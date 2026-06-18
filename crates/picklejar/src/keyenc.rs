//! Order-preserving key encoding for the variable-length secondary index.
//!
//! [`VarBTree`](picklejar_storage::VarBTree) compares keys as raw byte strings,
//! so to get value-ordered lookups the engine must encode each value to bytes
//! whose lexicographic order matches the value's order. This module does that,
//! and concatenates fields for a composite key.
//!
//! # Properties
//!
//! - **Order-preserving.** For two values `a <= b` of the same type,
//!   `encode(a) <= encode(b)` byte-wise. So a `WHERE col > x` range maps to a
//!   contiguous key range and a `WHERE col = x` lookup to a key prefix.
//! - **Self-delimiting.** Each field encodes to a sequence that the next field
//!   cannot be confused with, so a composite key `(a, b)` is unambiguous and a
//!   lookup on a leading column is a clean prefix of the full key.
//! - **Total, including NULL.** A leading presence byte (`0` for NULL, `1`
//!   otherwise) keeps every row encodable, so the index holds every row and a
//!   non-NULL equality range never returns a NULL row.
//!
//! The engine appends the row id after the encoded value (see
//! [`Index`](crate::index)), which makes every key unique even for a non-unique
//! column, so the tree never sees a duplicate key.

use picklejar_sql::Value;

/// Cap on the bytes of a `TEXT` value that go into a key. Two values sharing
/// this prefix collide in the index and are separated by the executor's
/// residual predicate, so a long string is never a correctness problem - only a
/// slightly wider candidate scan. Bounds the key well under
/// [`MAX_VAR_KEY`](picklejar_storage::MAX_VAR_KEY).
const TEXT_PREFIX_CAP: usize = 1900;

/// Append the order-preserving encoding of one `value` to `out`.
///
/// Returns `false` (leaving `out` unchanged) for a type this index does not key
/// on, so the caller can decline to build a physical index for it.
#[must_use]
pub fn encode_field(value: &Value, out: &mut Vec<u8>) -> bool {
    match value {
        // NULL: presence byte 0, no payload. Sorts before any present value and
        // can never equal a non-NULL lookup key.
        Value::Null => {
            out.push(0);
            true
        }
        // i64-backed: flip the sign bit so signed order matches unsigned-byte
        // (big-endian) order.
        Value::Int(n) | Value::Date(n) | Value::Timestamp(n) => {
            out.push(1);
            let bits = u64::from_ne_bytes(n.to_ne_bytes()) ^ (1 << 63);
            out.extend_from_slice(&bits.to_be_bytes());
            true
        }
        Value::Bool(b) => {
            out.push(1);
            out.push(u8::from(*b));
            true
        }
        Value::Text(s) => {
            out.push(1);
            encode_text(s, out);
            true
        }
        // FLOAT, DECIMAL, and JSON are not keyed by this index (FLOAT has NaN;
        // DECIMAL is not bijective into bytes across scales). A column of these
        // types simply falls back to a sequential scan, which is still correct.
        Value::Float(_) | Value::Decimal(..) | Value::Json(_) => false,
    }
}

/// Encode a `TEXT` value: a `0x00`-terminated, `0x00`-escaped byte string,
/// capped to [`TEXT_PREFIX_CAP`]. Escaping `0x00` as `0x00 0x01` keeps the
/// terminator (`0x00 0x00`) ordering before any real continuation, so the
/// encoding stays order-preserving and self-delimiting.
fn encode_text(s: &str, out: &mut Vec<u8>) {
    let bytes = s.as_bytes();
    let capped = &bytes[..bytes.len().min(TEXT_PREFIX_CAP)];
    for &b in capped {
        if b == 0 {
            out.push(0);
            out.push(1);
        } else {
            out.push(b);
        }
    }
    out.push(0);
    out.push(0);
}

/// Encode a tuple of `values` (one per indexed column, in order) into a single
/// key prefix. Returns `None` if any field's type is not indexable.
#[must_use]
pub fn encode_key(values: &[&Value]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    for v in values {
        if !encode_field(v, &mut out) {
            return None;
        }
    }
    Some(out)
}

/// The smallest byte string strictly greater than every string having `prefix`
/// as a prefix, for an exclusive upper bound on a prefix scan.
///
/// `None` means the prefix is all `0xFF` (no successor: the scan runs to the
/// end).
#[must_use]
pub fn prefix_successor(prefix: &[u8]) -> Option<Vec<u8>> {
    let mut out = prefix.to_vec();
    while let Some(&last) = out.last() {
        if last == 0xFF {
            out.pop();
        } else {
            *out.last_mut().expect("non-empty") = last + 1;
            return Some(out);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enc(v: &Value) -> Vec<u8> {
        let mut out = Vec::new();
        assert!(encode_field(v, &mut out));
        out
    }

    #[test]
    fn ints_encode_in_signed_order() {
        let encoded: Vec<Vec<u8>> = [-100i64, -1, 0, 1, 100]
            .iter()
            .map(|n| enc(&Value::Int(*n)))
            .collect();
        for w in encoded.windows(2) {
            assert!(w[0] < w[1], "{:?} !< {:?}", w[0], w[1]);
        }
    }

    #[test]
    fn null_sorts_before_present_values() {
        assert!(enc(&Value::Null) < enc(&Value::Int(i64::MIN)));
        assert!(enc(&Value::Null) < enc(&Value::Text(String::new())));
    }

    #[test]
    fn text_encodes_in_lexicographic_order_and_is_self_delimiting() {
        assert!(enc(&Value::Text("a".into())) < enc(&Value::Text("ab".into())));
        assert!(enc(&Value::Text("ab".into())) < enc(&Value::Text("b".into())));
        // "a" sorts before "a\0b": the terminator orders before the escaped null.
        assert!(enc(&Value::Text("a".into())) < enc(&Value::Text("a\0b".into())));
        // A composite key (text, int) is unambiguous: the text terminator
        // separates the fields regardless of the text's bytes.
        let k1 = encode_key(&[&Value::Text("a".into()), &Value::Int(2)]).unwrap();
        let k2 = encode_key(&[&Value::Text("a".into()), &Value::Int(10)]).unwrap();
        let k3 = encode_key(&[&Value::Text("ab".into()), &Value::Int(1)]).unwrap();
        assert!(k1 < k2, "same text, 2 < 10");
        assert!(k2 < k3, "text 'a' < 'ab' dominates the second field");
    }

    #[test]
    fn composite_order_matches_tuple_order() {
        let rows = [
            encode_key(&[&Value::Int(1), &Value::Int(1)]).unwrap(),
            encode_key(&[&Value::Int(1), &Value::Int(2)]).unwrap(),
            encode_key(&[&Value::Int(2), &Value::Int(1)]).unwrap(),
            encode_key(&[&Value::Int(2), &Value::Int(2)]).unwrap(),
        ];
        for w in rows.windows(2) {
            assert!(w[0] < w[1], "{:?} !< {:?}", w[0], w[1]);
        }
    }

    #[test]
    fn unindexable_types_are_declined() {
        let mut out = Vec::new();
        assert!(!encode_field(&Value::Float(1.0), &mut out));
        assert!(out.is_empty());
        assert!(encode_key(&[&Value::Int(1), &Value::Float(2.0)]).is_none());
    }

    #[test]
    fn prefix_successor_bounds_a_prefix_scan() {
        assert_eq!(prefix_successor(&[1, 2, 3]), Some(vec![1, 2, 4]));
        assert_eq!(prefix_successor(&[1, 2, 0xFF]), Some(vec![1, 3]));
        assert_eq!(prefix_successor(&[0xFF, 0xFF]), None);
        // Every string with the prefix sorts below the successor.
        let p = vec![5u8, 0xFF];
        let succ = prefix_successor(&p).unwrap();
        let mut longer = p;
        longer.extend_from_slice(&[0xFF, 0xFF, 0xFF]);
        assert!(longer < succ);
    }
}
