//! Property-based tests for the WAL.
//!
//! Covers record round-trip across all variants, single-bit corruption,
//! writer-reader durability under random workloads, and tail truncation
//! at arbitrary offsets.

use picklejar_wal::{LogRecord, Lsn, TxnId, WalReader, WalWriter};
use proptest::prelude::*;
use std::io::Write;

// --- strategies ---

fn lsn_strategy() -> impl Strategy<Value = Lsn> {
    any::<u64>().prop_map(Lsn::new)
}

fn txn_strategy() -> impl Strategy<Value = TxnId> {
    any::<u64>().prop_map(TxnId::new)
}

fn small_blob() -> impl Strategy<Value = Vec<u8>> {
    prop::collection::vec(any::<u8>(), 0..=256)
}

fn record_strategy() -> impl Strategy<Value = LogRecord> {
    prop_oneof![
        Just(LogRecord::Begin),
        Just(LogRecord::Commit),
        Just(LogRecord::Abort),
        (any::<u64>(), any::<u16>(), small_blob(), small_blob()).prop_map(
            |(page_id, slot_id, before, after)| LogRecord::Update {
                page_id,
                slot_id,
                before,
                after,
            }
        ),
    ]
}

// --- test 1: record round-trip across all variants ---

proptest! {
    #[test]
    fn record_round_trip(
        rec in record_strategy(),
        lsn in lsn_strategy(),
        prev in lsn_strategy(),
        txn in txn_strategy()
    ) {
        let mut buf = Vec::new();
        rec.write(lsn, prev, txn, &mut buf);
        let (hdr, decoded) = LogRecord::read(&buf).expect("decode");
        prop_assert_eq!(hdr.lsn, lsn);
        prop_assert_eq!(hdr.prev_lsn, prev);
        prop_assert_eq!(hdr.txn, txn);
        prop_assert_eq!(decoded, rec);
    }
}

// --- test 2: checksum catches single-bit flips ---

proptest! {
    #![proptest_config(ProptestConfig { cases: 32, ..ProptestConfig::default() })]

    #[test]
    fn record_checksum_catches_single_bit_flip(
        rec in record_strategy(),
        flip_offset in any::<usize>()
    ) {
        let mut buf = Vec::new();
        rec.write(Lsn::new(1), Lsn::INVALID, TxnId::new(1), &mut buf);
        // Flip a bit at a deterministic position derived from the proptest input.
        // Exclude the last 4 bytes (checksum field) so the checksum itself
        // changes via the payload mutation, not by us re-rolling it.
        let trailer_start = buf.len() - 4;
        let target = flip_offset % trailer_start;
        let original = buf[target];
        buf[target] ^= 0x01;
        prop_assert_ne!(buf[target], original);
        // Either the length field, type byte, or payload was perturbed.
        // The checksum must catch it.
        let err = LogRecord::read(&buf);
        prop_assert!(
            err.is_err(),
            "decode should fail after bit flip at offset {target}",
        );
    }
}

// --- test 3: writer + reader durability under random workloads ---

#[derive(Debug, Clone)]
enum Op {
    Append(LogRecord),
    FsyncAll,
}

fn op_strategy() -> impl Strategy<Value = Op> {
    prop_oneof![
        4 => record_strategy().prop_map(Op::Append),
        1 => Just(Op::FsyncAll),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 24, ..ProptestConfig::default() })]

    #[test]
    fn writer_reader_round_trip_under_random_ops(
        ops in prop::collection::vec(op_strategy(), 1..=64)
    ) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("wal.log");
        let mut w = WalWriter::open(&path).expect("open");
        let mut expected = Vec::new();
        for op in ops {
            match op {
                Op::Append(rec) => {
                    let lsn = w
                        .append(&rec, TxnId::new(1), Lsn::INVALID)
                        .expect("append");
                    expected.push((lsn, rec));
                }
                Op::FsyncAll => {
                    w.fsync_all().expect("fsync");
                }
            }
        }
        w.fsync_all().expect("final fsync");
        drop(w);

        let r = WalReader::open(&path).expect("open read");
        let got: Vec<(Lsn, LogRecord)> = r
            .map(|item| {
                let (h, rec) = item.expect("ok");
                (h.lsn, rec)
            })
            .collect();
        prop_assert_eq!(got, expected);
    }
}

// --- test 4: tail truncation at arbitrary offsets stops cleanly ---

proptest! {
    #![proptest_config(ProptestConfig { cases: 24, ..ProptestConfig::default() })]

    #[test]
    fn arbitrary_tail_truncation_stops_cleanly(
        ops in prop::collection::vec(record_strategy(), 5..=40),
        truncate_to in any::<u32>()
    ) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("wal.log");
        let mut w = WalWriter::open(&path).expect("open");
        let mut record_offsets: Vec<(usize, Lsn, LogRecord)> = Vec::new();
        let mut cursor = 0usize;
        for op in ops {
            let lsn = w
                .append(&op, TxnId::new(1), Lsn::INVALID)
                .expect("append");
            // Record the byte offset where this record ENDS (after writer's
            // buffer alone, since file is empty until fsync_all).
            // We compute via serializing once to scratch.
            let mut tmp = Vec::new();
            op.write(lsn, Lsn::INVALID, TxnId::new(1), &mut tmp);
            cursor += tmp.len();
            record_offsets.push((cursor, lsn, op));
        }
        w.fsync_all().expect("fsync");
        drop(w);

        // Truncate at a random offset.
        let full = std::fs::read(&path).expect("read");
        let truncate_at = (truncate_to as usize) % (full.len() + 1);
        std::fs::write(&path, &full[..truncate_at]).expect("write");

        // The reader must yield every complete record before truncate_at
        // and then stop cleanly (no error).
        let r = WalReader::open(&path).expect("open read");
        let mut got = Vec::new();
        for item in r {
            match item {
                Ok((h, rec)) => got.push((h.lsn, rec)),
                Err(e) => prop_assert!(false, "unexpected error after truncation: {e}"),
            }
        }
        // Expected: every record whose END offset <= truncate_at.
        let want: Vec<(Lsn, LogRecord)> = record_offsets
            .iter()
            .filter(|(end, _, _)| *end <= truncate_at)
            .map(|(_, lsn, rec)| (*lsn, rec.clone()))
            .collect();
        prop_assert_eq!(got, want);
    }
}

// --- test 5: garbage appended at tail does not corrupt earlier records ---

proptest! {
    #![proptest_config(ProptestConfig { cases: 16, ..ProptestConfig::default() })]

    #[test]
    fn garbage_appended_does_not_corrupt_earlier_records(
        ops in prop::collection::vec(record_strategy(), 1..=20),
        garbage in prop::collection::vec(any::<u8>(), 1..=64)
    ) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("wal.log");
        let mut w = WalWriter::open(&path).expect("open");
        let mut expected = Vec::new();
        for op in ops {
            let lsn = w
                .append(&op, TxnId::new(1), Lsn::INVALID)
                .expect("append");
            expected.push((lsn, op));
        }
        w.fsync_all().expect("fsync");
        drop(w);

        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .expect("open append");
        f.write_all(&garbage).expect("append garbage");
        drop(f);

        let r = WalReader::open(&path).expect("open read");
        let mut got = Vec::new();
        for item in r {
            match item {
                Ok((h, rec)) => got.push((h.lsn, rec)),
                Err(_) => {
                    // The reader is allowed to yield an error once on
                    // garbage that happens to pass the length-prefix
                    // sanity check but fails the checksum. After that
                    // it must yield None.
                    break;
                }
            }
        }
        // Every expected record must be present at the head of got.
        prop_assert_eq!(&got[..expected.len().min(got.len())], &expected[..got.len().min(expected.len())]);
        prop_assert!(got.len() >= expected.len(),
            "garbage at tail must not drop earlier records: got {} records, expected at least {}",
            got.len(), expected.len());
    }
}
