//! A storage-fault taxonomy and a detection-coverage simulator.
//!
//! The radiation model injects single-event upsets (bit flips). Real storage
//! fails three other ways too, and they are not the same fault: a **torn write**
//! lands only a prefix of a page, a **lost write** is acknowledged but never
//! reaches the platter, and a **misdirected write** lands a page at the wrong
//! location. Each defeats a different defense, so "we catch bit flips" does not
//! imply "we catch torn writes".
//!
//! This module injects all four and measures which of the engine's page-integrity
//! checks catches each, honestly. The page checksum (a CRC32 over the payload)
//! catches a bit flip and a torn write, because both leave payload bytes that
//! disagree with the stored checksum. It does not catch a lost write (the old page
//! is internally consistent) or a misdirected write (the displaced page is
//! internally consistent); those need, respectively, the page's LSN compared to
//! what the write-ahead log last logged for it, and a self-identifying page id.
//! Both guards are now in the engine: the LSN-versus-log comparison, and the
//! self-identifying page id stamped into every page's header
//! ([`picklejar_storage::stamp_page_id`], checked by
//! [`picklejar_storage::verify_page_id`]). A misdirected write carries the source
//! page's id, which does not match the location it landed at, so it is caught even
//! when its content is newer than the location expected. All four classes are
//! detected; there is no residual.

use picklejar_storage::{
    stamp_page_id, verify_checksum, verify_page_id, PageHeader, HEADER_SIZE, PAGE_SIZE,
};

/// `SplitMix64`: the shared deterministic PRNG, so a coverage run replays exactly.
struct Rng(u64);

impl Rng {
    const fn new(seed: u64) -> Self {
        Self(seed)
    }

    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: usize) -> usize {
        usize::try_from(self.next_u64() % n as u64).unwrap_or(0)
    }
}

/// One of the four storage-write fault classes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Fault {
    /// A single bit in the payload is flipped (a single-event upset or bit-rot).
    BitFlip,
    /// A page write was interrupted, so only a prefix of the new image landed and
    /// the rest is the old image (a torn page).
    TornWrite,
    /// A page write was acknowledged but never reached the disk, so the location
    /// still holds the previous, internally-consistent page (a lost update).
    LostWrite,
    /// A page write landed at the wrong location, so this location holds some other
    /// page's internally-consistent image.
    MisdirectedWrite,
}

impl Fault {
    /// All four classes, for sweeping.
    #[must_use]
    pub const fn all() -> [Self; 4] {
        [
            Self::BitFlip,
            Self::TornWrite,
            Self::LostWrite,
            Self::MisdirectedWrite,
        ]
    }

    /// The class name, for reports.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::BitFlip => "bit flip",
            Self::TornWrite => "torn write",
            Self::LostWrite => "lost write",
            Self::MisdirectedWrite => "misdirected write",
        }
    }
}

/// A well-formed heap page with random payload, stamped with page id `page_id`,
/// carrying a correct checksum and LSN `lsn`.
fn good_page(rng: &mut Rng, lsn: u64, page_id: u64) -> Box<[u8; PAGE_SIZE]> {
    let mut page = Box::new([0u8; PAGE_SIZE]);
    let mut h = PageHeader::new_heap();
    h.lsn = lsn;
    h.write(&mut page);
    for b in &mut page[HEADER_SIZE..] {
        *b = (rng.next_u64() & 0xFF) as u8;
    }
    stamp_page_id(&mut page, page_id);
    picklejar_storage::recompute_checksum(&mut page);
    page
}

/// Whether the engine's layered page check catches a page read from the slot
/// holding page `expected_id`, whose log reached `expected_lsn`.
///
/// Three layers, in order: the checksum catches a payload that disagrees with its
/// stored CRC (a bit flip or torn write); the page-id guard catches a page whose
/// stamped id is not this slot's (a misdirected write, whatever its content); the
/// LSN guard catches a page lagging the log (a stored LSN below what the log last
/// recorded for this slot, the signature of a lost write).
#[must_use]
pub fn caught(page: &[u8; PAGE_SIZE], expected_lsn: u64, expected_id: u64) -> bool {
    if !verify_checksum(page) {
        return true;
    }
    if !verify_page_id(page, expected_id) {
        return true;
    }
    let stored = PageHeader::read(page).map_or(0, |h| h.lsn);
    stored < expected_lsn
}

/// The detection rate of each fault class over a coverage run.
#[derive(Clone, Copy, Debug)]
pub struct FaultCoverage {
    /// Fraction of injected bit flips caught.
    pub bit_flip: f32,
    /// Fraction of injected torn writes caught.
    pub torn_write: f32,
    /// Fraction of injected lost writes caught.
    pub lost_write: f32,
    /// Fraction of injected misdirected writes caught. Every page carries a
    /// self-identifying id stamped in its header, so a displaced page (some other
    /// page's valid image) fails the page-id guard regardless of its content; this
    /// is now total, closing what used to be the residual.
    pub misdirected_write: f32,
    /// Trials per class.
    pub trials: usize,
}

/// Run a deterministic fault-coverage sweep from `seed`: inject each class
/// `per_class` times into well-formed pages and measure the engine's detection
/// rate under its layered checksum-and-LSN check.
#[must_use]
#[allow(clippy::cast_precision_loss)] // counts are small; the ratio is exact
pub fn run_fault_coverage(seed: u64, per_class: usize) -> FaultCoverage {
    const SLOT: u64 = 7;
    let mut rng = Rng::new(seed);
    // The write-ahead log says every live slot has reached this LSN; a correct
    // page at the slot carries exactly it. The slot under test holds page `SLOT`.
    let expected: u64 = 1000;

    let mut hits = [0usize; 4];
    for (idx, fault) in Fault::all().into_iter().enumerate() {
        for _ in 0..per_class {
            let detected = match fault {
                Fault::BitFlip => {
                    let mut page = good_page(&mut rng, expected, SLOT);
                    let offset = HEADER_SIZE + rng.below(PAGE_SIZE - HEADER_SIZE);
                    page[offset] ^= 1u8 << (rng.below(8));
                    caught(&page, expected, SLOT)
                }
                Fault::TornWrite => {
                    // Only a prefix of the new page landed; the suffix is the old
                    // page. The new header (with the new checksum) is in the
                    // prefix, so the stale suffix fails the checksum.
                    let old = good_page(&mut rng, expected - 1, SLOT);
                    let cut = HEADER_SIZE + 1 + rng.below(PAGE_SIZE - HEADER_SIZE - 1);
                    let mut torn = good_page(&mut rng, expected, SLOT);
                    torn[cut..].copy_from_slice(&old[cut..]);
                    caught(&torn, expected, SLOT)
                }
                Fault::LostWrite => {
                    // The new write never landed; the slot keeps the previous,
                    // internally-consistent page, which carries an older LSN.
                    let behind = 1 + rng.below(50) as u64;
                    let stale = good_page(&mut rng, expected - behind, SLOT);
                    caught(&stale, expected, SLOT)
                }
                Fault::MisdirectedWrite => {
                    // Some other page's correct image landed at this slot, with a
                    // content LSN unrelated to this slot's (it ranges over
                    // `expected +/- 50`, so the LSN guard alone would miss the
                    // newer half). The displaced page is stamped with its own id,
                    // never SLOT, so the page-id guard catches every one.
                    let other_lsn = (expected + rng.below(101) as u64).saturating_sub(50);
                    let other_id = SLOT + 1 + rng.below(4096) as u64;
                    let other = good_page(&mut rng, other_lsn, other_id);
                    caught(&other, expected, SLOT)
                }
            };
            if detected {
                hits[idx] += 1;
            }
        }
    }

    let rate = |i: usize| hits[i] as f32 / per_class as f32;
    FaultCoverage {
        bit_flip: rate(0),
        torn_write: rate(1),
        lost_write: rate(2),
        misdirected_write: rate(3),
        trials: per_class,
    }
}

#[cfg(test)]
mod tests {
    use super::{caught, good_page, run_fault_coverage, Fault, Rng};
    use picklejar_storage::{HEADER_SIZE, PAGE_SIZE};

    #[test]
    fn a_correct_page_at_its_slot_is_not_flagged() {
        let mut rng = Rng::new(1);
        let page = good_page(&mut rng, 1000, 7);
        assert!(!caught(&page, 1000, 7), "a current, intact page must pass");
    }

    #[test]
    fn the_checksum_catches_bit_flip_and_torn_write() {
        let cov = run_fault_coverage(42, 400);
        assert!(cov.bit_flip >= 1.0, "every bit flip must be caught");
        assert!(cov.torn_write >= 1.0, "every torn write must be caught");
    }

    #[test]
    fn the_lsn_guard_catches_every_lost_write() {
        let cov = run_fault_coverage(7, 400);
        assert!(
            cov.lost_write >= 1.0,
            "a page lagging the log must always be caught"
        );
    }

    #[test]
    fn the_page_id_guard_catches_every_misdirected_write() {
        // With a self-identifying page id stamped in every header, a displaced
        // page fails the page-id guard regardless of its content, so detection is
        // total: the last residual is closed.
        for seed in [99, 1234, 0xFA17] {
            let cov = run_fault_coverage(seed, 400);
            assert!(
                cov.misdirected_write >= 1.0,
                "every misdirected write must be caught, got {} at seed {seed}",
                cov.misdirected_write
            );
        }
    }

    #[test]
    fn all_four_fault_classes_are_fully_detected() {
        let cov = run_fault_coverage(0x00C0_FFEE, 500);
        assert!(cov.bit_flip >= 1.0);
        assert!(cov.torn_write >= 1.0);
        assert!(cov.lost_write >= 1.0);
        assert!(cov.misdirected_write >= 1.0);
    }

    #[test]
    fn fault_classes_are_named() {
        assert_eq!(Fault::all().len(), 4);
        assert_eq!(Fault::BitFlip.name(), "bit flip");
    }

    #[test]
    fn a_torn_suffix_always_disagrees_with_the_new_checksum() {
        // A direct construction, independent of the sweep: a new page with one old
        // suffix byte fails the checksum.
        let mut rng = Rng::new(3);
        let mut torn = good_page(&mut rng, 10, 7);
        torn[PAGE_SIZE - 1] ^= 0xFF; // a stale last byte
        assert!(caught(&torn, 10, 7), "a torn suffix must fail the checksum");
        let _ = HEADER_SIZE;
    }
}
