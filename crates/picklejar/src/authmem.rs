//! Authenticated memory: verifiable nearest-neighbor search for a node you do
//! not trust.
//!
//! The premise of this engine is memory for hardware you cannot reach. This
//! module pushes that one step further, to hardware you cannot *trust*: a remote
//! or radiation-exposed node can return a nearest-neighbor result together with
//! a cryptographic proof, and a thin client that holds only a small pinned root
//! can verify the answer without re-running the query and without trusting the
//! server.
//!
//! What the proof establishes (soundness):
//!
//! - **Authenticity.** Every returned memory is a genuine row of the committed
//!   state the root commits to. A fabricated or altered vector fails its
//!   inclusion proof, because the leaf is hashed with [`sha256`].
//! - **Correct scoring.** The client recomputes each distance from the
//!   authenticated vector, so a server cannot misreport how close a result is.
//! - **Ordering.** The returned results are checked to be sorted by distance.
//! - **Tenant isolation.** The tenant is part of the authenticated leaf, so a
//!   server cannot pass another tenant's row off as the caller's, even though
//!   that row really is committed.
//!
//! What it does not establish (the honest frontier): **completeness.** These
//! proofs show every returned answer is real, correctly scored, and correctly
//! ordered. They do not by themselves prove the server did not withhold a closer
//! memory. Verifiable completeness for approximate nearest-neighbor search is an
//! open problem; soundness against a tampering or corrupted node is what this
//! delivers, and it is already enough to catch fabrication, mis-scoring,
//! reordering, and cross-tenant leakage.

/// A from-scratch SHA-256, so the authenticated layer rests on a
/// collision-resistant hash rather than the engine's 32-bit page checksums
/// (which catch accidental bit-flips but not an adversary).
pub mod sha256 {
    const K: [u32; 64] = [
        0x428a_2f98,
        0x7137_4491,
        0xb5c0_fbcf,
        0xe9b5_dba5,
        0x3956_c25b,
        0x59f1_11f1,
        0x923f_82a4,
        0xab1c_5ed5,
        0xd807_aa98,
        0x1283_5b01,
        0x2431_85be,
        0x550c_7dc3,
        0x72be_5d74,
        0x80de_b1fe,
        0x9bdc_06a7,
        0xc19b_f174,
        0xe49b_69c1,
        0xefbe_4786,
        0x0fc1_9dc6,
        0x240c_a1cc,
        0x2de9_2c6f,
        0x4a74_84aa,
        0x5cb0_a9dc,
        0x76f9_88da,
        0x983e_5152,
        0xa831_c66d,
        0xb003_27c8,
        0xbf59_7fc7,
        0xc6e0_0bf3,
        0xd5a7_9147,
        0x06ca_6351,
        0x1429_2967,
        0x27b7_0a85,
        0x2e1b_2138,
        0x4d2c_6dfc,
        0x5338_0d13,
        0x650a_7354,
        0x766a_0abb,
        0x81c2_c92e,
        0x9272_2c85,
        0xa2bf_e8a1,
        0xa81a_664b,
        0xc24b_8b70,
        0xc76c_51a3,
        0xd192_e819,
        0xd699_0624,
        0xf40e_3585,
        0x106a_a070,
        0x19a4_c116,
        0x1e37_6c08,
        0x2748_774c,
        0x34b0_bcb5,
        0x391c_0cb3,
        0x4ed8_aa4a,
        0x5b9c_ca4f,
        0x682e_6ff3,
        0x748f_82ee,
        0x78a5_636f,
        0x84c8_7814,
        0x8cc7_0208,
        0x90be_fffa,
        0xa450_6ceb,
        0xbef9_a3f7,
        0xc671_78f2,
    ];

    const H0: [u32; 8] = [
        0x6a09_e667,
        0xbb67_ae85,
        0x3c6e_f372,
        0xa54f_f53a,
        0x510e_527f,
        0x9b05_688c,
        0x1f83_d9ab,
        0x5be0_cd19,
    ];

    /// The SHA-256 digest of `data`.
    #[must_use]
    #[allow(clippy::many_single_char_names)] // a..h are the standard SHA-256 working variables
    pub fn hash(data: &[u8]) -> [u8; 32] {
        let mut h = H0;

        // Pad: 0x80, then zeros, then the 64-bit big-endian bit length, to a
        // multiple of 64 bytes.
        let bit_len = (data.len() as u64).wrapping_mul(8);
        let mut msg = data.to_vec();
        msg.push(0x80);
        while msg.len() % 64 != 56 {
            msg.push(0);
        }
        msg.extend_from_slice(&bit_len.to_be_bytes());

        for chunk in msg.chunks_exact(64) {
            let mut w = [0u32; 64];
            for (i, word) in w.iter_mut().take(16).enumerate() {
                let b = i * 4;
                *word = u32::from_be_bytes([chunk[b], chunk[b + 1], chunk[b + 2], chunk[b + 3]]);
            }
            for i in 16..64 {
                let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
                let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
                w[i] = w[i - 16]
                    .wrapping_add(s0)
                    .wrapping_add(w[i - 7])
                    .wrapping_add(s1);
            }

            let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
            for i in 0..64 {
                let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
                let ch = (e & f) ^ ((!e) & g);
                let t1 = hh
                    .wrapping_add(s1)
                    .wrapping_add(ch)
                    .wrapping_add(K[i])
                    .wrapping_add(w[i]);
                let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
                let maj = (a & b) ^ (a & c) ^ (b & c);
                let t2 = s0.wrapping_add(maj);
                hh = g;
                g = f;
                f = e;
                e = d.wrapping_add(t1);
                d = c;
                c = b;
                b = a;
                a = t1.wrapping_add(t2);
            }
            for (slot, v) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
                *slot = slot.wrapping_add(v);
            }
        }

        let mut out = [0u8; 32];
        for (i, word) in h.iter().enumerate() {
            out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
        }
        out
    }
}

/// One committed memory the root commits to: a row id, the owning tenant, and
/// the embedding. Together these are exactly what a verifier needs to trust.
#[derive(Clone, Debug, PartialEq)]
pub struct MemoryRecord {
    /// The row id (primary key) of the memory.
    pub rowid: u64,
    /// The tenant that owns the memory.
    pub tenant: String,
    /// The embedding.
    pub vector: Vec<f32>,
}

impl MemoryRecord {
    /// A canonical, length-prefixed byte encoding, so the leaf hash binds the
    /// row id, the tenant, and the exact embedding with no ambiguity.
    fn encode(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(20 + self.tenant.len() + self.vector.len() * 4);
        buf.extend_from_slice(&self.rowid.to_be_bytes());
        buf.extend_from_slice(&(self.tenant.len() as u64).to_be_bytes());
        buf.extend_from_slice(self.tenant.as_bytes());
        buf.extend_from_slice(&(self.vector.len() as u64).to_be_bytes());
        for x in &self.vector {
            buf.extend_from_slice(&x.to_bits().to_be_bytes());
        }
        buf
    }
}

/// Domain separation: leaves and internal nodes hash a distinct prefix so a leaf
/// can never be reinterpreted as an internal node.
const LEAF_PREFIX: u8 = 0x00;
const NODE_PREFIX: u8 = 0x01;
const EMPTY_PREFIX: u8 = 0x02;

fn leaf_hash(record: &MemoryRecord) -> [u8; 32] {
    let mut buf = vec![LEAF_PREFIX];
    buf.extend_from_slice(&record.encode());
    sha256::hash(&buf)
}

fn node_hash(left: &[u8; 32], right: &[u8; 32]) -> [u8; 32] {
    let mut buf = [0u8; 65];
    buf[0] = NODE_PREFIX;
    buf[1..33].copy_from_slice(left);
    buf[33..65].copy_from_slice(right);
    sha256::hash(&buf)
}

/// The pinned commitment to a committed memory set: a single 32-byte root a thin
/// client can hold and trust.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemoryRoot(pub [u8; 32]);

impl MemoryRoot {
    /// The root as a short hex string, for display and pinning.
    #[must_use]
    pub fn hex(&self) -> String {
        use std::fmt::Write as _;
        let mut s = String::with_capacity(64);
        for b in self.0 {
            let _ = write!(s, "{b:02x}");
        }
        s
    }
}

/// A Merkle inclusion proof: the sibling hashes from a leaf up to the root, plus
/// the leaf's index, which encodes whether each sibling is on the left or right.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InclusionProof {
    /// The leaf's position in the canonical (row-id-sorted) ordering.
    pub leaf_index: usize,
    /// The total number of leaves, needed to replay the odd-node duplication.
    pub leaf_count: usize,
    /// Sibling hashes from the bottom level up.
    pub siblings: Vec<[u8; 32]>,
}

/// A single authenticated nearest-neighbor result.
#[derive(Clone, Debug, PartialEq)]
pub struct AuthHit {
    /// The authenticated memory.
    pub record: MemoryRecord,
    /// The squared L2 distance the server reports. The verifier recomputes it
    /// from `record.vector`, so a wrong value here is caught.
    pub distance: f64,
    /// The proof that `record` is in the committed set the root commits to.
    pub proof: InclusionProof,
}

/// Squared L2 distance. `f64::from` keeps the f32 widening lossless.
fn l2_sq(a: &[f32], b: &[f32]) -> f64 {
    a.iter()
        .zip(b)
        .map(|(x, y)| {
            let d = f64::from(*x) - f64::from(*y);
            d * d
        })
        .sum()
}

/// Build one level of parents from a level of nodes, duplicating the last node
/// when the count is odd.
fn parent_level(level: &[[u8; 32]]) -> Vec<[u8; 32]> {
    let mut next = Vec::with_capacity(level.len().div_ceil(2));
    let mut i = 0;
    while i < level.len() {
        let left = &level[i];
        let right = if i + 1 < level.len() {
            &level[i + 1]
        } else {
            left
        };
        next.push(node_hash(left, right));
        i += 2;
    }
    next
}

/// The canonical leaves of a record set: sorted by row id, then leaf-hashed.
fn canonical_leaves(records: &[MemoryRecord]) -> (Vec<MemoryRecord>, Vec<[u8; 32]>) {
    let mut sorted = records.to_vec();
    sorted.sort_by_key(|r| r.rowid);
    let leaves = sorted.iter().map(leaf_hash).collect();
    (sorted, leaves)
}

fn root_of(leaves: &[[u8; 32]]) -> MemoryRoot {
    if leaves.is_empty() {
        return MemoryRoot(sha256::hash(&[EMPTY_PREFIX]));
    }
    let mut level = leaves.to_vec();
    while level.len() > 1 {
        level = parent_level(&level);
    }
    MemoryRoot(level[0])
}

/// Commit to a committed memory set, producing the root a client pins.
#[must_use]
pub fn commit(records: &[MemoryRecord]) -> MemoryRoot {
    let (_, leaves) = canonical_leaves(records);
    root_of(&leaves)
}

/// The inclusion proof for the leaf at `index` in a canonical leaf list.
fn prove(leaves: &[[u8; 32]], index: usize) -> InclusionProof {
    let leaf_count = leaves.len();
    let mut siblings = Vec::new();
    let mut level = leaves.to_vec();
    let mut idx = index;
    while level.len() > 1 {
        let sib = if idx % 2 == 0 {
            // Left node: sibling is the right one, or itself when duplicated.
            if idx + 1 < level.len() {
                level[idx + 1]
            } else {
                level[idx]
            }
        } else {
            level[idx - 1]
        };
        siblings.push(sib);
        level = parent_level(&level);
        idx /= 2;
    }
    InclusionProof {
        leaf_index: index,
        leaf_count,
        siblings,
    }
}

/// Replay an inclusion proof from a leaf hash up to a root.
fn replay(leaf: [u8; 32], proof: &InclusionProof) -> MemoryRoot {
    let mut node = leaf;
    let mut idx = proof.leaf_index;
    for sib in &proof.siblings {
        node = if idx % 2 == 0 {
            node_hash(&node, sib)
        } else {
            node_hash(sib, &node)
        };
        idx /= 2;
    }
    MemoryRoot(node)
}

/// Answer a k-nearest-neighbor query with proofs, on the server side.
///
/// Commits to the whole set, finds the querying tenant's nearest memories, and
/// attaches an inclusion proof to each. Returns the root (which the client is
/// assumed to already trust) and the proven hits.
#[must_use]
pub fn authenticated_knn(
    records: &[MemoryRecord],
    tenant: &str,
    query: &[f32],
    k: usize,
) -> (MemoryRoot, Vec<AuthHit>) {
    let (sorted, leaves) = canonical_leaves(records);
    let root = root_of(&leaves);

    // Score the querying tenant's rows by distance, keeping their canonical
    // index so the proof lines up with the committed tree.
    let mut scored: Vec<(usize, f64)> = sorted
        .iter()
        .enumerate()
        .filter(|(_, r)| r.tenant == tenant)
        .map(|(i, r)| (i, l2_sq(query, &r.vector)))
        .collect();
    scored.sort_by(|a, b| a.1.total_cmp(&b.1));

    let hits = scored
        .into_iter()
        .take(k)
        .map(|(i, dist)| AuthHit {
            record: sorted[i].clone(),
            distance: dist,
            proof: prove(&leaves, i),
        })
        .collect();
    (root, hits)
}

/// Why a verification failed. Each variant is a distinct attack a tampering or
/// corrupted server might attempt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VerifyError {
    /// A result is not in the committed set (fabricated or altered memory).
    InclusionFailed { rowid: u64 },
    /// The reported distance does not match the authenticated vector.
    DistanceMismatch { rowid: u64 },
    /// A result belongs to another tenant (cross-tenant leak).
    WrongTenant { rowid: u64, found: String },
    /// The results are not sorted by distance.
    OutOfOrder,
    /// More results than were asked for.
    TooMany { got: usize, k: usize },
}

impl std::fmt::Display for VerifyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InclusionFailed { rowid } => {
                write!(
                    f,
                    "row {rowid}: inclusion proof failed (not in committed state)"
                )
            }
            Self::DistanceMismatch { rowid } => {
                write!(
                    f,
                    "row {rowid}: reported distance does not match the vector"
                )
            }
            Self::WrongTenant { rowid, found } => {
                write!(
                    f,
                    "row {rowid}: belongs to tenant '{found}', not the caller"
                )
            }
            Self::OutOfOrder => write!(f, "results are not ordered by distance"),
            Self::TooMany { got, k } => write!(f, "got {got} results, more than k={k}"),
        }
    }
}

/// The thin-client verifier.
///
/// It holds only the pinned `root` and the query, with no access to the
/// database, and accepts the hits only if every one is authentic, correctly
/// scored, in order, and owned by `tenant`.
///
/// # Errors
/// Returns the first [`VerifyError`] that any result triggers.
pub fn verify_knn(
    root: MemoryRoot,
    tenant: &str,
    query: &[f32],
    hits: &[AuthHit],
    k: usize,
) -> Result<(), VerifyError> {
    if hits.len() > k {
        return Err(VerifyError::TooMany { got: hits.len(), k });
    }
    let mut last = f64::NEG_INFINITY;
    for hit in hits {
        // Tenant isolation: the tenant is authenticated as part of the leaf.
        if hit.record.tenant != tenant {
            return Err(VerifyError::WrongTenant {
                rowid: hit.record.rowid,
                found: hit.record.tenant.clone(),
            });
        }
        // Authenticity: the record must hash into the pinned root.
        if replay(leaf_hash(&hit.record), &hit.proof) != root {
            return Err(VerifyError::InclusionFailed {
                rowid: hit.record.rowid,
            });
        }
        // Correct scoring: recompute the distance from the authenticated vector.
        let recomputed = l2_sq(query, &hit.record.vector);
        if (recomputed - hit.distance).abs() > 1e-6 {
            return Err(VerifyError::DistanceMismatch {
                rowid: hit.record.rowid,
            });
        }
        // Ordering: distances must not decrease.
        if recomputed < last {
            return Err(VerifyError::OutOfOrder);
        }
        last = recomputed;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(bytes: &[u8]) -> String {
        use std::fmt::Write as _;
        let mut s = String::new();
        for b in bytes {
            let _ = write!(s, "{b:02x}");
        }
        s
    }

    #[test]
    fn sha256_matches_known_vectors() {
        // The canonical NIST test vectors.
        assert_eq!(
            hex(&sha256::hash(b"")),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            hex(&sha256::hash(b"abc")),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            hex(&sha256::hash(
                b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"
            )),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
    }

    fn sample() -> Vec<MemoryRecord> {
        vec![
            MemoryRecord {
                rowid: 1,
                tenant: "acme".into(),
                vector: vec![0.0, 0.0],
            },
            MemoryRecord {
                rowid: 2,
                tenant: "acme".into(),
                vector: vec![1.0, 1.0],
            },
            MemoryRecord {
                rowid: 3,
                tenant: "acme".into(),
                vector: vec![5.0, 5.0],
            },
            MemoryRecord {
                rowid: 4,
                tenant: "globex".into(),
                vector: vec![0.1, 0.1],
            },
            MemoryRecord {
                rowid: 5,
                tenant: "acme".into(),
                vector: vec![2.0, 2.0],
            },
        ]
    }

    #[test]
    fn honest_result_verifies() {
        let recs = sample();
        let query = [0.2, 0.2];
        let (root, hits) = authenticated_knn(&recs, "acme", &query, 3);
        // Nearest acme rows to (0.2,0.2): row 1 (0,0), then row 2 (1,1), then row 5 (2,2).
        assert_eq!(hits.len(), 3);
        assert_eq!(hits[0].record.rowid, 1);
        assert!(verify_knn(root, "acme", &query, &hits, 3).is_ok());
    }

    #[test]
    fn fabricated_vector_is_rejected() {
        let recs = sample();
        let query = [0.2, 0.2];
        let (root, mut hits) = authenticated_knn(&recs, "acme", &query, 3);
        // Tamper: change a vector the server claims is committed.
        hits[0].record.vector[0] = 99.0;
        hits[0].distance = l2_sq(&query, &hits[0].record.vector);
        assert!(matches!(
            verify_knn(root, "acme", &query, &hits, 3),
            Err(VerifyError::InclusionFailed { .. })
        ));
    }

    #[test]
    fn misreported_distance_is_rejected() {
        let recs = sample();
        let query = [0.2, 0.2];
        let (root, mut hits) = authenticated_knn(&recs, "acme", &query, 3);
        // Tamper: claim a result is closer than it is.
        hits[1].distance = 0.0;
        assert!(matches!(
            verify_knn(root, "acme", &query, &hits, 3),
            Err(VerifyError::DistanceMismatch { .. })
        ));
    }

    #[test]
    fn cross_tenant_row_is_rejected() {
        let recs = sample();
        let query = [0.2, 0.2];
        let (root, mut hits) = authenticated_knn(&recs, "acme", &query, 3);
        // Tamper: substitute globex's real, committed row (row 4) with a valid
        // inclusion proof. The proof passes, but the tenant check catches it.
        let (_, leaves) = canonical_leaves(&recs);
        let globex = MemoryRecord {
            rowid: 4,
            tenant: "globex".into(),
            vector: vec![0.1, 0.1],
        };
        hits[2] = AuthHit {
            distance: l2_sq(&query, &globex.vector),
            proof: prove(&leaves, 3),
            record: globex,
        };
        assert!(matches!(
            verify_knn(root, "acme", &query, &hits, 3),
            Err(VerifyError::WrongTenant { .. })
        ));
    }

    #[test]
    fn reordered_results_are_rejected() {
        let recs = sample();
        let query = [0.2, 0.2];
        let (root, mut hits) = authenticated_knn(&recs, "acme", &query, 3);
        hits.swap(0, 2);
        assert!(matches!(
            verify_knn(root, "acme", &query, &hits, 3),
            Err(VerifyError::OutOfOrder)
        ));
    }

    #[test]
    fn empty_set_has_a_stable_root() {
        assert_eq!(commit(&[]), commit(&[]));
        let (root, hits) = authenticated_knn(&[], "acme", &[0.0], 3);
        assert!(hits.is_empty());
        assert!(verify_knn(root, "acme", &[0.0], &hits, 3).is_ok());
    }
}
