//! Quorum-replicated memory: a small Dynamo for AI memory on unreliable nodes.
//!
//! The standalone distributed pieces (consistent placement, versioning,
//! conflict-free merge) become a system here. Each memory is replicated to a
//! preference list of `rf` nodes chosen by hashing. A write lands on every
//! reachable replica and succeeds once `w` of them acknowledge; a read gathers
//! `r` responses and returns the highest-versioned value, repairing any stale
//! replica it passed. When `r + w > rf`, every read quorum overlaps every write
//! quorum, so a read always sees the latest acknowledged write, and the cluster
//! keeps serving through node failures: a downed node blocks neither writes nor
//! reads as long as a quorum survives, and a healed node is caught up by
//! read-repair. The whole thing is a deterministic simulation, so the
//! availability and consistency claims are testable, not asserted.

use std::collections::HashMap;

/// A stored value with the version that wrote it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Stored {
    value: Vec<u8>,
    version: u64,
}

#[derive(Clone, Debug, Default)]
struct Replica {
    up: bool,
    store: HashMap<u64, Stored>,
}

/// Why a quorum operation failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuorumError {
    /// Fewer than the required replicas were reachable.
    NotEnoughReplicas {
        /// How many responded.
        got: usize,
        /// How many were needed.
        need: usize,
    },
}

const fn splitmix(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

/// A replicated key-value cluster with tunable read/write quorums.
#[derive(Clone, Debug)]
pub struct Cluster {
    replicas: Vec<Replica>,
    rf: usize,
    r: usize,
    w: usize,
    version: u64,
}

impl Cluster {
    /// A cluster of `n` nodes, replication factor `rf`, read quorum `r`, write
    /// quorum `w`.
    ///
    /// # Panics
    /// Panics if `rf` exceeds `n` or a quorum exceeds `rf`.
    #[must_use]
    pub fn new(n: usize, rf: usize, r: usize, w: usize) -> Self {
        assert!(rf >= 1 && rf <= n, "rf in 1..=n");
        assert!(r >= 1 && r <= rf && w >= 1 && w <= rf, "quorums in 1..=rf");
        Self {
            replicas: vec![
                Replica {
                    up: true,
                    store: HashMap::new()
                };
                n
            ],
            rf,
            r,
            w,
            version: 0,
        }
    }

    /// The preference list for `key`: `rf` node indices, by descending hash.
    #[must_use]
    pub fn preference(&self, key: u64) -> Vec<usize> {
        let mut nodes: Vec<usize> = (0..self.replicas.len()).collect();
        nodes.sort_by_key(|&node| std::cmp::Reverse(splitmix(key ^ splitmix(node as u64))));
        nodes.truncate(self.rf);
        nodes
    }

    /// Mark a node down.
    pub fn fail(&mut self, node: usize) {
        self.replicas[node].up = false;
    }

    /// Mark a node up (it may be stale until read-repair touches it).
    pub fn heal(&mut self, node: usize) {
        self.replicas[node].up = true;
    }

    /// Write a memory. Lands on every reachable preference replica; succeeds once
    /// `w` acknowledge.
    ///
    /// # Errors
    /// Returns [`QuorumError`] if fewer than `w` replicas are reachable.
    pub fn write(&mut self, key: u64, value: &[u8]) -> Result<u64, QuorumError> {
        self.version += 1;
        let version = self.version;
        let pref = self.preference(key);
        let mut acks = 0;
        for node in pref {
            if self.replicas[node].up {
                self.replicas[node].store.insert(
                    key,
                    Stored {
                        value: value.to_vec(),
                        version,
                    },
                );
                acks += 1;
            }
        }
        if acks >= self.w {
            Ok(version)
        } else {
            Err(QuorumError::NotEnoughReplicas {
                got: acks,
                need: self.w,
            })
        }
    }

    /// Read a memory: gather `r` responses from the preference replicas, return
    /// the highest-versioned value, and repair any stale replica passed.
    ///
    /// # Errors
    /// Returns [`QuorumError`] if fewer than `r` replicas are reachable.
    pub fn read(&mut self, key: u64) -> Result<Option<Vec<u8>>, QuorumError> {
        let pref = self.preference(key);
        // Gather up to r responses from reachable replicas, in preference order.
        let mut responses: Vec<(usize, Option<Stored>)> = Vec::new();
        for &node in &pref {
            if self.replicas[node].up {
                responses.push((node, self.replicas[node].store.get(&key).cloned()));
                if responses.len() == self.r {
                    break;
                }
            }
        }
        if responses.len() < self.r {
            return Err(QuorumError::NotEnoughReplicas {
                got: responses.len(),
                need: self.r,
            });
        }
        let winner = responses
            .iter()
            .filter_map(|(_, s)| s.clone())
            .max_by_key(|s| s.version);
        // Read-repair: bring every reachable preference replica up to the winner.
        if let Some(latest) = &winner {
            for &node in &pref {
                if self.replicas[node].up {
                    let stale = self.replicas[node]
                        .store
                        .get(&key)
                        .map_or(true, |s| s.version < latest.version);
                    if stale {
                        self.replicas[node].store.insert(key, latest.clone());
                    }
                }
            }
        }
        Ok(winner.map(|s| s.value))
    }

    /// How many preference replicas for `key` hold the latest version (for
    /// convergence checks).
    #[must_use]
    pub fn replicas_in_sync(&self, key: u64) -> usize {
        let pref = self.preference(key);
        let latest = pref
            .iter()
            .filter_map(|&n| self.replicas[n].store.get(&key).map(|s| s.version))
            .max()
            .unwrap_or(0);
        pref.iter()
            .filter(|&&n| {
                self.replicas[n]
                    .store
                    .get(&key)
                    .is_some_and(|s| s.version == latest)
            })
            .count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_read_sees_the_latest_write_through_a_failure() {
        // rf=3, r=2, w=2 -> r + w = 4 > 3, so quorums overlap.
        let mut c = Cluster::new(5, 3, 2, 2);
        let key = 12345;
        c.write(key, b"v1").unwrap();
        // A preference node fails, then a new value is written to the survivors.
        let pref = c.preference(key);
        c.fail(pref[0]);
        c.write(key, b"v2").unwrap();
        // Heal the stale node and read: the overlap guarantees we see v2.
        c.heal(pref[0]);
        assert_eq!(c.read(key).unwrap(), Some(b"v2".to_vec()));
    }

    #[test]
    fn a_write_fails_without_a_write_quorum() {
        let mut c = Cluster::new(5, 3, 2, 2);
        let key = 9;
        let pref = c.preference(key);
        c.fail(pref[0]);
        c.fail(pref[1]); // only one preference replica left, below w=2
        assert!(matches!(
            c.write(key, b"x"),
            Err(QuorumError::NotEnoughReplicas { .. })
        ));
    }

    #[test]
    fn read_repair_converges_a_healed_node() {
        let mut c = Cluster::new(5, 3, 2, 2);
        let key = 77;
        let pref = c.preference(key);
        c.fail(pref[2]);
        c.write(key, b"fresh").unwrap(); // misses the down node
        c.heal(pref[2]); // back, but stale (no value)
        assert_eq!(
            c.replicas_in_sync(key),
            2,
            "only the two survivors are current"
        );
        c.read(key).unwrap(); // read-repair touches the healed node
        assert_eq!(
            c.replicas_in_sync(key),
            3,
            "all three preference replicas converged"
        );
    }

    #[test]
    fn the_cluster_stays_consistent_across_many_writes() {
        let mut c = Cluster::new(7, 3, 2, 2);
        let key = 4242;
        for i in 0..50u64 {
            c.write(key, format!("value-{i}").as_bytes()).unwrap();
            assert_eq!(
                c.read(key).unwrap(),
                Some(format!("value-{i}").into_bytes()),
                "read-your-writes"
            );
        }
    }

    #[test]
    fn a_read_fails_without_a_read_quorum() {
        let mut c = Cluster::new(5, 3, 2, 2);
        let key = 1;
        c.write(key, b"v").unwrap();
        let pref = c.preference(key);
        c.fail(pref[0]);
        c.fail(pref[1]);
        assert!(matches!(
            c.read(key),
            Err(QuorumError::NotEnoughReplicas { .. })
        ));
    }
}
