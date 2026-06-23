//! Mass-efficient self-healing for the page heap, built on the erasure code.
//!
//! This is the engine-level companion to [`crate::erasure`] and
//! [`crate::resilient`]. It protects an on-disk page heap with Reed-Solomon
//! parity stored in a sidecar file, then repairs corrupt pages from that parity,
//! so a node nobody can reach heals its own storage instead of returning a
//! checksum error and stopping.
//!
//! Two operations:
//!
//! - [`write_parity`] takes a point-in-time image of every heap page and writes a
//!   parity sidecar: for each stripe of `k` pages it stores `m` parity pages.
//!   Surviving any `m` bad pages per stripe then reconstructs the rest.
//! - [`heal_file`] reads the heap and the sidecar, finds pages whose checksum
//!   fails, reconstructs them from the stripe's survivors, and writes them back.
//!   It runs on the raw file with no buffer pool attached, so it never fights the
//!   checksum-enforcing read path: a deployment heals before it opens.
//!
//! The protection is a snapshot. A page changed after the last [`write_parity`]
//! is covered for crashes by the write-ahead log, but its parity is refreshed
//! only at the next protect. For a memory layer that writes embeddings once and
//! reads them many times, with a periodic protect, that is exactly the model: a
//! corrupt page heals to its protected content, and the log replays anything
//! committed since. The parity sidecar carries its own header checksum and a
//! checksum per parity page, so corruption in the sidecar itself is detected too.

use std::fs;
use std::io;
use std::path::Path;

use crate::crc32::crc32;
use crate::erasure::ReedSolomon;
use crate::file::FileManager;
use crate::header::{verify_checksum, verify_page_id};
use crate::page::{Page, PageId, PAGE_SIZE};

/// Magic bytes at the head of a parity sidecar.
const MAGIC: &[u8; 4] = b"PJEC";
/// Sidecar format version.
const VERSION: u8 = 1;
/// Header bytes covered by the header checksum, laid out as
/// `magic(4) version(1) k(1) m(1) reserved(1) page_size(4) num_pages(8)`.
const HEADER_BODY: usize = 20;
/// Total header length, including the trailing header checksum.
const HEADER_LEN: usize = HEADER_BODY + 4;
/// One parity page on disk: a checksum followed by the page bytes.
const SHARD_FRAME: usize = 4 + PAGE_SIZE;

/// What a heal pass did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HealReport {
    /// Heap pages whose checksum was verified.
    pub pages_checked: u64,
    /// Corrupt pages reconstructed from parity and rewritten.
    pub pages_repaired: u64,
    /// Stripes with more bad shards than parity, left as they were (the data is
    /// genuinely lost, and stays detectably corrupt rather than silently wrong).
    pub stripes_unrecoverable: u64,
    /// The id of each page that was repaired, for the fault log.
    pub repaired_pages: Vec<u64>,
    /// The first page id of each stripe that could not be recovered.
    pub unrecoverable_stripes: Vec<u64>,
}

/// An `InvalidData` error describing a corrupt or unreadable parity sidecar.
fn bad_parity(why: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("parity sidecar: {why}"))
}

/// Write a parity sidecar protecting `pages` with a `(k, m)` Reed-Solomon code.
///
/// Every stripe of `k` pages gains `m` parity pages, so any `m` bad pages per
/// stripe can be reconstructed. The write is atomic (temp file then rename).
///
/// # Errors
///
/// Returns an error if `k`/`m` are out of range for the code, or if the sidecar
/// cannot be written.
pub fn write_parity(parity_path: &Path, pages: &[Page], k: usize, m: usize) -> io::Result<()> {
    if k == 0 || m == 0 || k > 255 || m > 255 || k + m > 256 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("parity shape k={k}, m={m} out of range"),
        ));
    }
    let rs = ReedSolomon::new(k, m).map_err(|e| bad_parity(&e.to_string()))?;
    let num_pages = pages.len();

    let mut out = Vec::with_capacity(HEADER_LEN + num_pages.div_ceil(k) * m * SHARD_FRAME);
    out.extend_from_slice(MAGIC);
    out.push(VERSION);
    out.push(u8::try_from(k).expect("k <= 255"));
    out.push(u8::try_from(m).expect("m <= 255"));
    out.push(0); // reserved
    out.extend_from_slice(
        &u32::try_from(PAGE_SIZE)
            .expect("page size fits u32")
            .to_le_bytes(),
    );
    out.extend_from_slice(&(num_pages as u64).to_le_bytes());
    let header_crc = crc32(&out);
    out.extend_from_slice(&header_crc.to_le_bytes());

    let num_stripes = num_pages.div_ceil(k);
    for s in 0..num_stripes {
        let mut shards: Vec<Vec<u8>> = (0..k)
            .map(|i| {
                let id = s * k + i;
                if id < num_pages {
                    pages[id].to_vec()
                } else {
                    vec![0u8; PAGE_SIZE]
                }
            })
            .collect();
        shards.extend((0..m).map(|_| vec![0u8; PAGE_SIZE]));
        rs.encode(&mut shards)
            .map_err(|e| bad_parity(&e.to_string()))?;
        for shard in shards.iter().skip(k) {
            out.extend_from_slice(&crc32(shard).to_le_bytes());
            out.extend_from_slice(shard);
        }
    }

    let tmp = parity_path.with_extension("parity.tmp");
    fs::write(&tmp, &out)?;
    fs::rename(&tmp, parity_path)?;
    Ok(())
}

/// Read the heap and the parity, reconstruct any page whose checksum fails.
///
/// Operates on the raw file, so it must run with no buffer pool holding the heap
/// (a deployment heals before it opens the database).
///
/// # Errors
///
/// Returns an error if the parity sidecar is missing, corrupt in its header, or
/// shaped for a different page size, or if the heap cannot be read or written.
pub fn heal_file(heap_path: &Path, parity_path: &Path) -> io::Result<HealReport> {
    let sidecar = fs::read(parity_path)?;
    let (k, m, num_pages) = parse_header(&sidecar)?;
    let rs = ReedSolomon::new(k, m).map_err(|e| bad_parity(&e.to_string()))?;

    let mut fm = FileManager::open(heap_path).map_err(|e| io::Error::other(e.to_string()))?;
    let page_count = usize::try_from(fm.page_count()).expect("page count fits usize");
    let num_stripes = num_pages.div_ceil(k);
    let mut report = HealReport::default();

    let mut off = HEADER_LEN;
    for s in 0..num_stripes {
        // The m parity pages for this stripe, each checked against its checksum.
        let mut shards: Vec<Vec<u8>> = Vec::with_capacity(k + m);
        let mut present = vec![true; k + m];
        let mut page_ids: Vec<Option<usize>> = Vec::with_capacity(k);

        // `i` is the stripe-relative page index; it drives the heap id and several
        // parallel per-shard vectors, so a range loop is the clear form here.
        #[allow(clippy::needless_range_loop)]
        for i in 0..k {
            let id = s * k + i;
            if id < num_pages && id < page_count {
                let mut page: Page = [0u8; PAGE_SIZE];
                fm.read_page(PageId::new(id as u64), &mut page)
                    .map_err(|e| io::Error::other(e.to_string()))?;
                report.pages_checked += 1;
                // A failed checksum (bit flip, torn write) or a page whose
                // self-identifying id does not match this location (a
                // misdirected write) is corrupt: reconstruct it from parity.
                if !verify_checksum(&page) || !verify_page_id(&page, id as u64) {
                    present[i] = false;
                }
                shards.push(page.to_vec());
                page_ids.push(Some(id));
            } else {
                // Padding past the protected page count is defined zeros; a real
                // page missing because the heap shrank is an erasure we cannot
                // write back, but can still use to recover its stripe-mates.
                shards.push(vec![0u8; PAGE_SIZE]);
                if id < num_pages && id >= page_count {
                    present[i] = false;
                }
                page_ids.push(None);
            }
        }

        for j in 0..m {
            let end = off + SHARD_FRAME;
            if end > sidecar.len() {
                return Err(bad_parity("truncated parity body"));
            }
            let stored = u32::from_le_bytes(sidecar[off..off + 4].try_into().expect("4 bytes"));
            let payload = &sidecar[off + 4..end];
            if crc32(payload) != stored {
                present[k + j] = false;
            }
            shards.push(payload.to_vec());
            off = end;
        }

        let bad = present.iter().filter(|p| !**p).count();
        if bad == 0 {
            continue;
        }
        if bad > m || rs.reconstruct(&mut shards, &present).is_err() {
            report.stripes_unrecoverable += 1;
            report.unrecoverable_stripes.push(s as u64 * k as u64);
            continue;
        }
        // Write back the data pages that were corrupt and are real and in range.
        #[allow(clippy::needless_range_loop)]
        for i in 0..k {
            if !present[i] {
                if let Some(id) = page_ids[i] {
                    let mut page: Page = [0u8; PAGE_SIZE];
                    page.copy_from_slice(&shards[i]);
                    fm.write_page(PageId::new(id as u64), &page)
                        .map_err(|e| io::Error::other(e.to_string()))?;
                    report.pages_repaired += 1;
                    report.repaired_pages.push(id as u64);
                }
            }
        }
    }

    fm.fsync().map_err(|e| io::Error::other(e.to_string()))?;
    Ok(report)
}

/// Parse and verify the sidecar header, returning `(k, m, num_pages)`.
fn parse_header(bytes: &[u8]) -> io::Result<(usize, usize, usize)> {
    if bytes.len() < HEADER_LEN {
        return Err(bad_parity("shorter than a header"));
    }
    if &bytes[0..4] != MAGIC {
        return Err(bad_parity("bad magic"));
    }
    if bytes[4] != VERSION {
        return Err(bad_parity("unsupported version"));
    }
    let stored = u32::from_le_bytes(bytes[HEADER_BODY..HEADER_LEN].try_into().expect("4 bytes"));
    if crc32(&bytes[0..HEADER_BODY]) != stored {
        return Err(bad_parity("corrupt header"));
    }
    let k = bytes[5] as usize;
    let m = bytes[6] as usize;
    let page_size = u32::from_le_bytes(bytes[8..12].try_into().expect("4 bytes")) as usize;
    if page_size != PAGE_SIZE {
        return Err(bad_parity("different page size"));
    }
    let num_pages = usize::try_from(u64::from_le_bytes(
        bytes[12..20].try_into().expect("8 bytes"),
    ))
    .map_err(|_| bad_parity("page count too large"))?;
    Ok((k, m, num_pages))
}

#[cfg(test)]
mod tests {
    use super::{heal_file, write_parity};
    use crate::file::FileManager;
    use crate::header::recompute_checksum;
    use crate::page::{Page, PageId, PAGE_SIZE};
    use std::io::{Read, Seek, SeekFrom, Write};

    /// `SplitMix64`, so corruption patterns replay exactly.
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^ (z >> 31)
        }
        fn below(&mut self, n: u64) -> u64 {
            self.next() % n
        }
    }

    /// A page whose payload is derived from its id, stamped with its own page
    /// id, and carrying a valid checksum.
    fn page_for(id: u64) -> Page {
        let mut p: Page = [0u8; PAGE_SIZE];
        for (i, b) in p.iter_mut().enumerate().skip(12) {
            *b = u8::try_from((i as u64 ^ id.wrapping_mul(2_654_435_761)) & 0xFF).expect("masked");
        }
        crate::header::stamp_page_id(&mut p, id);
        recompute_checksum(&mut p);
        p
    }

    /// Build a heap file of `n` valid pages; return its dir and path.
    fn build_heap(n: u64) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("h.db");
        let mut fm = FileManager::open(&path).expect("open");
        for id in 0..n {
            fm.allocate_page().expect("allocate");
            fm.write_page(PageId::new(id), &page_for(id))
                .expect("write");
        }
        fm.fsync().expect("fsync");
        (dir, path)
    }

    /// Corrupt one byte inside the checksum-covered region of `page` in the file.
    fn corrupt(path: &std::path::Path, page: u64, rng: &mut Rng) {
        let pos = page * PAGE_SIZE as u64 + 12 + rng.below(PAGE_SIZE as u64 - 12);
        let mut f = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .expect("open");
        f.seek(SeekFrom::Start(pos)).unwrap();
        let mut b = [0u8; 1];
        f.read_exact(&mut b).unwrap();
        b[0] ^= 0xFF;
        f.seek(SeekFrom::Start(pos)).unwrap();
        f.write_all(&b).unwrap();
    }

    fn read_page(path: &std::path::Path, id: u64) -> Page {
        let mut fm = FileManager::open(path).expect("open");
        let mut p: Page = [0u8; PAGE_SIZE];
        fm.read_page(PageId::new(id), &mut p).expect("read");
        p
    }

    #[test]
    fn heals_up_to_m_corrupt_pages_per_stripe() {
        let (k, m) = (6usize, 3usize);
        let pages = 40u64; // not a whole number of stripes, to exercise padding
        let mut rng = Rng(0x1234);
        for _trial in 0..20 {
            let (dir, path) = build_heap(pages);
            let parity = dir.path().join("h.parity");
            let image: Vec<Page> = (0..pages).map(page_for).collect();
            write_parity(&parity, &image, k, m).expect("protect");

            // Corrupt up to m pages in each stripe.
            let stripes = usize::try_from(pages).expect("small").div_ceil(k);
            for s in 0..stripes {
                let bad = rng.below(m as u64 + 1);
                let mut hit = std::collections::HashSet::new();
                while (hit.len() as u64) < bad {
                    let i = rng.below(k as u64);
                    let id = s as u64 * k as u64 + i;
                    if id < pages {
                        hit.insert(id);
                    } else {
                        break;
                    }
                }
                for &id in &hit {
                    corrupt(&path, id, &mut rng);
                }
            }

            let report = heal_file(&path, &parity).expect("heal");
            assert_eq!(report.stripes_unrecoverable, 0);
            // Every page is back to its original, valid content.
            for id in 0..pages {
                assert_eq!(read_page(&path, id), page_for(id), "page {id} not healed");
            }
        }
    }

    #[test]
    fn more_than_m_in_a_stripe_is_unrecoverable_not_silently_wrong() {
        let (k, m) = (5usize, 2usize);
        let pages = 5u64; // one stripe
        let (dir, path) = build_heap(pages);
        let parity = dir.path().join("h.parity");
        let image: Vec<Page> = (0..pages).map(page_for).collect();
        write_parity(&parity, &image, k, m).expect("protect");

        let mut rng = Rng(9);
        for id in 0..3 {
            // three corrupt pages, only two parity
            corrupt(&path, id, &mut rng);
        }
        let report = heal_file(&path, &parity).expect("heal");
        assert_eq!(report.stripes_unrecoverable, 1);
        // The corrupt pages were left corrupt (detectable), never rewritten wrong.
        let healthy = (0..pages)
            .filter(|&id| crate::header::verify_checksum(&read_page(&path, id)))
            .count();
        assert!(healthy >= 2, "intact pages must remain intact");
    }

    #[test]
    fn a_corrupt_parity_header_is_rejected() {
        let (k, m) = (4usize, 2usize);
        let (dir, path) = build_heap(8);
        let parity = dir.path().join("h.parity");
        let image: Vec<Page> = (0..8).map(page_for).collect();
        write_parity(&parity, &image, k, m).expect("protect");
        // Flip a header byte.
        let mut bytes = std::fs::read(&parity).unwrap();
        bytes[10] ^= 0xFF;
        std::fs::write(&parity, &bytes).unwrap();
        assert!(heal_file(&path, &parity).is_err());
    }

    #[test]
    fn a_corrupt_parity_page_still_heals_a_corrupt_data_page() {
        // One bad data page and one bad parity page in a stripe with m=2: still
        // within tolerance, so the data page heals.
        let (k, m) = (6usize, 2usize);
        let pages = 6u64;
        let (dir, path) = build_heap(pages);
        let parity = dir.path().join("h.parity");
        let image: Vec<Page> = (0..pages).map(page_for).collect();
        write_parity(&parity, &image, k, m).expect("protect");

        let mut rng = Rng(0xABCD);
        corrupt(&path, 2, &mut rng); // a data page
                                     // Corrupt one parity page's payload in the sidecar.
        let mut bytes = std::fs::read(&parity).unwrap();
        let parity_payload_start = super::HEADER_LEN + 4;
        bytes[parity_payload_start + 7] ^= 0xFF;
        std::fs::write(&parity, &bytes).unwrap();

        let report = heal_file(&path, &parity).expect("heal");
        assert_eq!(report.stripes_unrecoverable, 0);
        assert_eq!(read_page(&path, 2), page_for(2));
    }

    #[test]
    fn a_misdirected_write_with_a_valid_checksum_is_healed() {
        // The hardest corruption to catch: another page's complete, valid image
        // lands at the wrong location. Its checksum verifies, so only the
        // self-identifying page id reveals it is misplaced. Heal must reconstruct
        // the right page from parity.
        let (k, m) = (6usize, 2usize);
        let pages = 12u64;
        let (dir, path) = build_heap(pages);
        let parity = dir.path().join("h.parity");
        let image: Vec<Page> = (0..pages).map(page_for).collect();
        write_parity(&parity, &image, k, m).expect("protect");

        // Overwrite page 3 with page 9's fully valid image (a misdirected write).
        let displaced = page_for(9);
        assert!(
            crate::header::verify_checksum(&displaced),
            "the intruder is internally valid"
        );
        let mut fm = FileManager::open(&path).expect("open");
        fm.write_page(PageId::new(3), &displaced).expect("write");
        fm.fsync().expect("fsync");
        drop(fm);

        let report = heal_file(&path, &parity).expect("heal");
        assert_eq!(
            report.pages_repaired, 1,
            "the misplaced page must be repaired"
        );
        assert_eq!(report.stripes_unrecoverable, 0);
        // Page 3 is back to its own content, and still verifies at its location.
        assert_eq!(read_page(&path, 3), page_for(3));
        assert!(crate::header::verify_page_id(&read_page(&path, 3), 3));
    }
}
