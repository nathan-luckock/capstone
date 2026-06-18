//! A variable-length-key B+ tree for secondary indexes.
//!
//! The primary [`BTree`](crate::btree::BTree) keys on a fixed `u64`.
//!
//! That fits the rowid primary index and unique fixed-width columns but cannot
//! index `TEXT`, multiple columns, or a non-unique column (whose values repeat).
//! This tree keys on an arbitrary byte string instead, compared
//! lexicographically, so a caller that encodes its key order-preservingly gets
//! ordered lookups and range scans over anything.
//!
//! # Unique keys, by construction
//!
//! Secondary-index entries are never deleted (a stale entry is filtered by the
//! MVCC visibility check and the executor's residual predicate, see
//! [`crate::btree`] callers), so this tree needs only **insert**, **search**,
//! and **range scan** - no delete, merge, or rebalance, which is where a B+
//! tree's complexity and crash-safety risk concentrate. The engine appends the
//! row's unique id to every key, so even a non-unique *column* produces unique
//! *keys*: the tree stays a plain unique-key tree, and a value lookup is a range
//! scan over `[encode(value) .. encode(value) || 0xFF..]`.
//!
//! # Node layout
//!
//! Both node kinds use a slotted layout: a fixed header, then a sorted directory
//! of fixed-size slots growing up from the header, and a heap of the actual key
//! bytes growing down from the end of the page. The directory stays sorted by
//! key; the heap is append-only (no deletes means no compaction). The page
//! header's `slot_count` doubles as the key count and `free_space_ptr` as the
//! heap top.
//!
//! ```text
//! Leaf:      [header 24][next_leaf 8][slot*][   free   ][heap of keys]
//!            slot = key_off(2) key_len(2) val_page(8) val_slot(2)        = 14
//! Internal:  [header 24][first_child 8][slot*][   free   ][heap of keys]
//!            slot = key_off(2) key_len(2) right_child(8)                 = 12
//! ```
//!
//! Crash safety is inherited from the buffer pool: every mutation goes through a
//! [`PageWriteGuard`](crate::buffer::PageWriteGuard), which the WAL hook logs and
//! whose checksum the pool recomputes on flush, exactly as for the primary tree.

use std::cell::Cell;
use std::ops::Bound;

use crate::btree::TupleRef;
use crate::buffer::BufferPool;
use crate::error::{Result, StorageError};
use crate::header::{PageHeader, PageType, HEADER_SIZE};
use crate::heap::SlotId;
use crate::page::{Page, PageId, PAGE_SIZE};

/// Offset of the per-page `key_count` (reusing the header's `slot_count`).
const COUNT_OFF: usize = 14;
/// Offset of the per-page `heap_top` (reusing the header's `free_space_ptr`).
const HEAP_TOP_OFF: usize = 16;
/// A leaf's `next_leaf` sibling pointer sits right after the header.
const NEXT_LEAF_OFF: usize = HEADER_SIZE;
/// An internal node's left-most child pointer sits right after the header.
const FIRST_CHILD_OFF: usize = HEADER_SIZE;
/// Both node kinds put their slot directory after the 8-byte pointer field.
const SLOTS_OFF: usize = HEADER_SIZE + 8;
/// Bytes per leaf slot: `key_off(2) key_len(2) val_page(8) val_slot(2)`.
const LEAF_SLOT: usize = 14;
/// Bytes per internal slot: `key_off(2) key_len(2) right_child(8)`.
const INT_SLOT: usize = 12;

/// The largest key the tree accepts.
///
/// Bounded so that a node holding entries that fit one page, plus one more new
/// entry, can always be byte-split (see [`byte_split_index`]) into two halves
/// that each fit a page. Callers that key on long text encode a bounded prefix
/// (the residual predicate still filters), so this is never hit in practice.
pub const MAX_VAR_KEY: usize = 2000;

// --- raw field helpers (the buffer pool owns lsn/checksum, so we only ever
// touch bytes at offset >= 12) ---

fn get_u16(p: &Page, off: usize) -> u16 {
    u16::from_le_bytes(p[off..off + 2].try_into().expect("2 bytes"))
}
fn set_u16(p: &mut Page, off: usize, v: u16) {
    p[off..off + 2].copy_from_slice(&v.to_le_bytes());
}
fn get_u64(p: &Page, off: usize) -> u64 {
    u64::from_le_bytes(p[off..off + 8].try_into().expect("8 bytes"))
}
fn set_u64(p: &mut Page, off: usize, v: u64) {
    p[off..off + 8].copy_from_slice(&v.to_le_bytes());
}

fn key_count(p: &Page) -> usize {
    get_u16(p, COUNT_OFF) as usize
}
fn set_key_count(p: &mut Page, n: usize) {
    set_u16(p, COUNT_OFF, u16::try_from(n).expect("count fits u16"));
}
fn heap_top(p: &Page) -> usize {
    get_u16(p, HEAP_TOP_OFF) as usize
}
fn set_heap_top(p: &mut Page, v: usize) {
    set_u16(
        p,
        HEAP_TOP_OFF,
        u16::try_from(v).expect("heap top fits u16"),
    );
}

/// Initialize `buf` as an empty node of `kind`, with the 8-byte pointer field
/// (next-leaf for a leaf, first-child for an internal) set to `pointer`.
fn init_node(buf: &mut Page, kind: PageType, pointer: PageId) {
    buf.fill(0);
    let mut header = PageHeader::new_heap();
    header.page_type = kind;
    header.slot_count = 0;
    header.free_space_ptr = u16::try_from(PAGE_SIZE).expect("page size fits u16");
    header.write(buf);
    set_u64(buf, NEXT_LEAF_OFF, pointer.get());
}

/// Free bytes available for one more `(slot, key)` pair.
fn free_space(p: &Page, slot_size: usize) -> usize {
    heap_top(p).saturating_sub(SLOTS_OFF + key_count(p) * slot_size)
}

/// The key bytes of slot `i`, given the per-slot record size.
fn slot_key(p: &Page, i: usize, slot_size: usize) -> &[u8] {
    let base = SLOTS_OFF + i * slot_size;
    let off = get_u16(p, base) as usize;
    let len = get_u16(p, base + 2) as usize;
    &p[off..off + len]
}

/// Binary-search the sorted directory for `key`. Returns `Ok(i)` if slot `i`
/// holds exactly `key`, or `Err(i)` for the insertion point (the first slot
/// whose key is greater).
fn search_slots(p: &Page, key: &[u8], slot_size: usize) -> std::result::Result<usize, usize> {
    let mut lo = 0usize;
    let mut hi = key_count(p);
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        match slot_key(p, mid, slot_size).cmp(key) {
            std::cmp::Ordering::Less => lo = mid + 1,
            std::cmp::Ordering::Greater => hi = mid,
            std::cmp::Ordering::Equal => return Ok(mid),
        }
    }
    Err(lo)
}

/// Choose a split index by **cumulative byte size**, not entry count.
///
/// Keys vary in length, so an even count split can put far more bytes on one
/// side than a page holds. `sizes[i]` is the on-page cost of entry `i`
/// (`slot_size + key_len`). Returns the smallest index where the left side's
/// bytes reach half the total, clamped to `[1, n-1]` so both sides are
/// non-empty. Since the pre-split node fit in one page plus a single new entry,
/// this guarantees each half fits in a page.
fn byte_split_index(sizes: &[usize]) -> usize {
    let total: usize = sizes.iter().sum();
    let half = total / 2;
    let mut acc = 0;
    let mut idx = sizes.len();
    for (i, &s) in sizes.iter().enumerate() {
        acc += s;
        if acc >= half {
            idx = i + 1;
            break;
        }
    }
    idx.clamp(1, sizes.len().saturating_sub(1).max(1))
}

/// Append `key` to the heap and write a fresh slot at directory index `pos`,
/// shifting later slots right. The caller must have checked free space and the
/// non-duplicate position. `write_value` fills the slot's value bytes (those
/// after the 4-byte key reference).
fn insert_slot(
    p: &mut Page,
    pos: usize,
    key: &[u8],
    slot_size: usize,
    write_value: impl FnOnce(&mut [u8]),
) {
    let count = key_count(p);
    // Append the key bytes to the heap (grows down).
    let new_top = heap_top(p) - key.len();
    p[new_top..new_top + key.len()].copy_from_slice(key);
    set_heap_top(p, new_top);

    // Shift slots [pos, count) right by one slot.
    let pos_off = SLOTS_OFF + pos * slot_size;
    let end_off = SLOTS_OFF + count * slot_size;
    p.copy_within(pos_off..end_off, pos_off + slot_size);

    // Write the new slot: key reference, then the value.
    set_u16(p, pos_off, u16::try_from(new_top).expect("offset fits u16"));
    set_u16(
        p,
        pos_off + 2,
        u16::try_from(key.len()).expect("len fits u16"),
    );
    write_value(&mut p[pos_off + 4..pos_off + slot_size]);

    set_key_count(p, count + 1);
}

// --- leaf operations ---

fn leaf_value(p: &Page, i: usize) -> TupleRef {
    let base = SLOTS_OFF + i * LEAF_SLOT + 4;
    let page = get_u64(p, base);
    let slot = get_u16(p, base + 8);
    TupleRef::new(PageId::new(page), SlotId::new(slot))
}

fn leaf_next(p: &Page) -> PageId {
    PageId::new(get_u64(p, NEXT_LEAF_OFF))
}

/// Insert `(key, tuple)` into a leaf page. Returns `BTreeNodeFull` when the
/// page cannot fit the entry (the caller splits) and `DuplicateVarKey` if the
/// key is already present.
fn leaf_insert(p: &mut Page, key: &[u8], tuple: TupleRef) -> Result<()> {
    let Err(pos) = search_slots(p, key, LEAF_SLOT) else {
        return Err(StorageError::DuplicateVarKey);
    };
    if LEAF_SLOT + key.len() > free_space(p, LEAF_SLOT) {
        return Err(StorageError::BTreeNodeFull {
            key_count: u16::try_from(key_count(p)).unwrap_or(u16::MAX),
            capacity: 0,
        });
    }
    insert_slot(p, pos, key, LEAF_SLOT, |val| {
        val[0..8].copy_from_slice(&tuple.page_id.get().to_le_bytes());
        val[8..10].copy_from_slice(&tuple.slot_id.get().to_le_bytes());
    });
    Ok(())
}

// --- internal operations ---

fn internal_first_child(p: &Page) -> PageId {
    PageId::new(get_u64(p, FIRST_CHILD_OFF))
}

fn internal_child(p: &Page, i: usize) -> PageId {
    PageId::new(get_u64(p, SLOTS_OFF + i * INT_SLOT + 4))
}

/// The child to descend into for `query`: the right child of the largest key
/// `<= query`, or the first child if `query` precedes every key.
fn internal_find_child(p: &Page, query: &[u8]) -> PageId {
    let count = key_count(p);
    // Largest i with key[i] <= query == (upper_bound) - 1.
    let mut lo = 0usize;
    let mut hi = count;
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        if slot_key(p, mid, INT_SLOT) <= query {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    if lo == 0 {
        internal_first_child(p)
    } else {
        internal_child(p, lo - 1)
    }
}

fn internal_insert(p: &mut Page, key: &[u8], child: PageId) -> Result<()> {
    let Err(pos) = search_slots(p, key, INT_SLOT) else {
        return Err(StorageError::DuplicateVarKey);
    };
    if INT_SLOT + key.len() > free_space(p, INT_SLOT) {
        return Err(StorageError::BTreeNodeFull {
            key_count: u16::try_from(key_count(p)).unwrap_or(u16::MAX),
            capacity: 0,
        });
    }
    insert_slot(p, pos, key, INT_SLOT, |val| {
        val[0..8].copy_from_slice(&child.get().to_le_bytes());
    });
    Ok(())
}

/// A variable-length-key B+ tree over a [`BufferPool`].
///
/// The root id moves when the root splits, so it lives in a `Cell` and the tree
/// borrows the pool immutably (matching [`BTree`](crate::btree::BTree)).
pub struct VarBTree<'pool> {
    pool: &'pool BufferPool,
    root: Cell<PageId>,
}

impl std::fmt::Debug for VarBTree<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VarBTree")
            .field("root", &self.root.get())
            .finish_non_exhaustive()
    }
}

impl<'pool> VarBTree<'pool> {
    /// Create a new empty tree (one leaf page as the root).
    pub fn create(pool: &'pool BufferPool) -> Result<Self> {
        let (root_id, mut guard) = pool.new_page()?;
        init_node(guard.page_mut(), PageType::BTreeVarLeaf, PageId::INVALID);
        Ok(Self {
            pool,
            root: Cell::new(root_id),
        })
    }

    /// Open an existing tree rooted at `root`.
    #[must_use]
    pub const fn open(pool: &'pool BufferPool, root: PageId) -> Self {
        Self {
            pool,
            root: Cell::new(root),
        }
    }

    /// The current root page id (changes when the root splits).
    #[must_use]
    pub fn root_page(&self) -> PageId {
        self.root.get()
    }

    /// Look up `key`'s exact tuple reference, if present.
    pub fn search(&self, key: &[u8]) -> Result<Option<TupleRef>> {
        let leaf_id = self.descend_to_leaf(key)?;
        let guard = self.pool.fetch_page(leaf_id)?;
        let p = guard.page();
        Ok(search_slots(p, key, LEAF_SLOT)
            .ok()
            .map(|i| leaf_value(p, i)))
    }

    /// Insert `(key, tuple)`, splitting and growing the tree as needed. A key
    /// already present is left unchanged (an idempotent re-insert), so replaying
    /// the same row's index maintenance is harmless.
    pub fn insert(&self, key: &[u8], tuple: TupleRef) -> Result<()> {
        if key.len() > MAX_VAR_KEY {
            return Err(StorageError::VarKeyTooLarge(key.len()));
        }
        let root = self.root.get();
        if let Some((promoted, right)) = self.insert_descend(root, key, tuple)? {
            // The root split: grow a new root above it.
            let (new_root_id, mut guard) = self.pool.new_page()?;
            init_node(guard.page_mut(), PageType::BTreeVarInternal, root);
            internal_insert(guard.page_mut(), &promoted, right)?;
            drop(guard);
            self.root.set(new_root_id);
        }
        Ok(())
    }

    /// Iterate `(key, tuple)` entries with keys in `[lo, hi]` per the bounds.
    pub fn range_scan(&self, lo: Bound<&[u8]>, hi: Bound<&[u8]>) -> Result<VarRangeScan<'pool>> {
        let start_key: Vec<u8> = match lo {
            Bound::Included(k) | Bound::Excluded(k) => k.to_vec(),
            Bound::Unbounded => Vec::new(),
        };
        let leaf_id = self.descend_to_leaf(&start_key)?;
        let start_index = {
            let guard = self.pool.fetch_page(leaf_id)?;
            let p = guard.page();
            let mut idx = match search_slots(p, &start_key, LEAF_SLOT) {
                Ok(i) | Err(i) => i,
            };
            if let Bound::Excluded(k) = lo {
                if idx < key_count(p) && slot_key(p, idx, LEAF_SLOT) == k {
                    idx += 1;
                }
            }
            idx
        };
        Ok(VarRangeScan {
            pool: self.pool,
            leaf: leaf_id,
            index: start_index,
            hi: match hi {
                Bound::Included(k) => Bound::Included(k.to_vec()),
                Bound::Excluded(k) => Bound::Excluded(k.to_vec()),
                Bound::Unbounded => Bound::Unbounded,
            },
            done: false,
        })
    }

    /// Walk from the root to the leaf that would own `key`.
    fn descend_to_leaf(&self, key: &[u8]) -> Result<PageId> {
        let mut current = self.root.get();
        loop {
            let guard = self.pool.fetch_page(current)?;
            let p = guard.page();
            match PageHeader::read(p)?.page_type {
                PageType::BTreeVarLeaf => return Ok(current),
                PageType::BTreeVarInternal => {
                    let child = internal_find_child(p, key);
                    drop(guard);
                    current = child;
                }
                other => {
                    return Err(StorageError::WrongPageType {
                        expected: PageType::BTreeVarLeaf,
                        actual: other,
                    })
                }
            }
        }
    }

    /// Recursive insert: descend to the owning leaf, insert (splitting on the
    /// way back up), and return any `(separator, new_right)` this level promoted.
    fn insert_descend(
        &self,
        page_id: PageId,
        key: &[u8],
        tuple: TupleRef,
    ) -> Result<Option<(Vec<u8>, PageId)>> {
        let kind = {
            let guard = self.pool.fetch_page(page_id)?;
            PageHeader::read(guard.page())?.page_type
        };
        match kind {
            PageType::BTreeVarLeaf => self.insert_into_leaf(page_id, key, tuple),
            PageType::BTreeVarInternal => {
                let child = {
                    let guard = self.pool.fetch_page(page_id)?;
                    internal_find_child(guard.page(), key)
                };
                match self.insert_descend(child, key, tuple)? {
                    Some((sep, right)) => self.insert_into_internal(page_id, &sep, right),
                    None => Ok(None),
                }
            }
            other => Err(StorageError::WrongPageType {
                expected: PageType::BTreeVarLeaf,
                actual: other,
            }),
        }
    }

    fn insert_into_leaf(
        &self,
        page_id: PageId,
        key: &[u8],
        tuple: TupleRef,
    ) -> Result<Option<(Vec<u8>, PageId)>> {
        {
            let mut guard = self.pool.fetch_page_mut(page_id)?;
            match leaf_insert(guard.page_mut(), key, tuple) {
                // An absent key inserted, or a duplicate left as-is (idempotent).
                Ok(()) | Err(StorageError::DuplicateVarKey) => return Ok(None),
                Err(StorageError::BTreeNodeFull { .. }) => {}
                Err(e) => return Err(e),
            }
        }
        self.split_leaf(page_id, key, tuple).map(Some)
    }

    fn split_leaf(&self, old_id: PageId, key: &[u8], tuple: TupleRef) -> Result<(Vec<u8>, PageId)> {
        let (new_id, new_guard) = self.pool.new_page()?;
        drop(new_guard);

        // Snapshot the old leaf, splice in the new entry.
        let (mut entries, old_next) = {
            let guard = self.pool.fetch_page(old_id)?;
            let p = guard.page();
            let entries: Vec<(Vec<u8>, TupleRef)> = (0..key_count(p))
                .map(|i| (slot_key(p, i, LEAF_SLOT).to_vec(), leaf_value(p, i)))
                .collect();
            (entries, leaf_next(p))
        };
        let pos = entries.partition_point(|(k, _)| k.as_slice() < key);
        entries.insert(pos, (key.to_vec(), tuple));

        // Split by cumulative bytes so a mix of short and long keys still yields
        // two halves that each fit a page.
        let sizes: Vec<usize> = entries.iter().map(|(k, _)| LEAF_SLOT + k.len()).collect();
        let mid = byte_split_index(&sizes);
        let split_key = entries[mid].0.clone();

        // New (right) leaf gets the right half and the old sibling link.
        {
            let mut g = self.pool.fetch_page_mut(new_id)?;
            init_node(g.page_mut(), PageType::BTreeVarLeaf, old_next);
            for (k, t) in &entries[mid..] {
                leaf_insert(g.page_mut(), k, *t)?;
            }
        }
        // Old (left) leaf is rebuilt with the left half and points at the new one.
        {
            let mut g = self.pool.fetch_page_mut(old_id)?;
            init_node(g.page_mut(), PageType::BTreeVarLeaf, new_id);
            for (k, t) in &entries[..mid] {
                leaf_insert(g.page_mut(), k, *t)?;
            }
        }
        Ok((split_key, new_id))
    }

    fn insert_into_internal(
        &self,
        page_id: PageId,
        key: &[u8],
        child: PageId,
    ) -> Result<Option<(Vec<u8>, PageId)>> {
        {
            let mut guard = self.pool.fetch_page_mut(page_id)?;
            match internal_insert(guard.page_mut(), key, child) {
                Ok(()) => return Ok(None),
                Err(StorageError::BTreeNodeFull { .. }) => {}
                Err(e) => return Err(e),
            }
        }
        self.split_internal(page_id, key, child).map(Some)
    }

    fn split_internal(
        &self,
        old_id: PageId,
        key: &[u8],
        child: PageId,
    ) -> Result<(Vec<u8>, PageId)> {
        let (new_id, new_guard) = self.pool.new_page()?;
        drop(new_guard);

        let (mut entries, old_first) = {
            let guard = self.pool.fetch_page(old_id)?;
            let p = guard.page();
            let entries: Vec<(Vec<u8>, PageId)> = (0..key_count(p))
                .map(|i| (slot_key(p, i, INT_SLOT).to_vec(), internal_child(p, i)))
                .collect();
            (entries, internal_first_child(p))
        };
        let pos = entries.partition_point(|(k, _)| k.as_slice() < key);
        entries.insert(pos, (key.to_vec(), child));

        // Promote (and remove) the byte-balanced separator; its child becomes
        // the new node's first child. Splitting by bytes (not count) keeps each
        // half within a page even when separators vary in length.
        let sizes: Vec<usize> = entries.iter().map(|(k, _)| INT_SLOT + k.len()).collect();
        let mid = byte_split_index(&sizes);
        let promoted = entries[mid].0.clone();
        let new_first = entries[mid].1;

        {
            let mut g = self.pool.fetch_page_mut(new_id)?;
            init_node(g.page_mut(), PageType::BTreeVarInternal, new_first);
            for (k, c) in &entries[mid + 1..] {
                internal_insert(g.page_mut(), k, *c)?;
            }
        }
        {
            let mut g = self.pool.fetch_page_mut(old_id)?;
            init_node(g.page_mut(), PageType::BTreeVarInternal, old_first);
            for (k, c) in &entries[..mid] {
                internal_insert(g.page_mut(), k, *c)?;
            }
        }
        Ok((promoted, new_id))
    }
}

/// A lazy iterator over a [`VarBTree`] range scan. Each `next` may fault the
/// next sibling leaf through the buffer pool, so items are `Result`s.
pub struct VarRangeScan<'pool> {
    pool: &'pool BufferPool,
    leaf: PageId,
    index: usize,
    hi: Bound<Vec<u8>>,
    done: bool,
}

impl std::fmt::Debug for VarRangeScan<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VarRangeScan")
            .field("leaf", &self.leaf)
            .field("index", &self.index)
            .field("done", &self.done)
            .finish_non_exhaustive()
    }
}

impl Iterator for VarRangeScan<'_> {
    type Item = Result<(Vec<u8>, TupleRef)>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        loop {
            if self.leaf.is_invalid() {
                self.done = true;
                return None;
            }
            let guard = match self.pool.fetch_page(self.leaf) {
                Ok(g) => g,
                Err(e) => {
                    self.done = true;
                    return Some(Err(e));
                }
            };
            let p = guard.page();
            if self.index >= key_count(p) {
                self.leaf = leaf_next(p);
                self.index = 0;
                continue;
            }
            let key = slot_key(p, self.index, LEAF_SLOT);
            let within = match &self.hi {
                Bound::Included(h) => key <= h.as_slice(),
                Bound::Excluded(h) => key < h.as_slice(),
                Bound::Unbounded => true,
            };
            if !within {
                self.done = true;
                return None;
            }
            let item = (key.to_vec(), leaf_value(p, self.index));
            self.index += 1;
            return Some(Ok(item));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file::FileManager;

    fn pool() -> (tempfile::TempDir, BufferPool) {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = FileManager::open(dir.path().join("v.db")).expect("open");
        (dir, BufferPool::new(file, 64))
    }

    fn tref(n: u64) -> TupleRef {
        TupleRef::new(PageId::new(n), SlotId::new(0))
    }

    #[test]
    fn insert_search_round_trip() {
        let (_d, pool) = pool();
        let t = VarBTree::create(&pool).unwrap();
        t.insert(b"banana", tref(2)).unwrap();
        t.insert(b"apple", tref(1)).unwrap();
        t.insert(b"cherry", tref(3)).unwrap();
        assert_eq!(t.search(b"apple").unwrap(), Some(tref(1)));
        assert_eq!(t.search(b"banana").unwrap(), Some(tref(2)));
        assert_eq!(t.search(b"cherry").unwrap(), Some(tref(3)));
        assert_eq!(t.search(b"durian").unwrap(), None);
    }

    #[test]
    fn duplicate_insert_is_idempotent() {
        let (_d, pool) = pool();
        let t = VarBTree::create(&pool).unwrap();
        t.insert(b"k", tref(1)).unwrap();
        // A re-insert of the same key keeps the original value.
        t.insert(b"k", tref(9)).unwrap();
        assert_eq!(t.search(b"k").unwrap(), Some(tref(1)));
    }

    #[test]
    fn many_inserts_force_multi_level_splits_and_stay_searchable() {
        let (_d, pool) = pool();
        let t = VarBTree::create(&pool).unwrap();
        // 5000 variable-length keys force many leaf and internal splits.
        let n = 5000u64;
        for i in 0..n {
            let key = format!("key-{i:08}-{}", "x".repeat((i % 40) as usize));
            t.insert(key.as_bytes(), tref(i)).unwrap();
        }
        // The root is now an internal node (the tree is multi-level).
        let guard = pool.fetch_page(t.root_page()).unwrap();
        assert_eq!(
            PageHeader::read(guard.page()).unwrap().page_type,
            PageType::BTreeVarInternal
        );
        drop(guard);
        for i in 0..n {
            let key = format!("key-{i:08}-{}", "x".repeat((i % 40) as usize));
            assert_eq!(t.search(key.as_bytes()).unwrap(), Some(tref(i)), "key {i}");
        }
        assert_eq!(t.search(b"absent").unwrap(), None);
    }

    #[test]
    fn mixed_tiny_and_huge_keys_split_without_overflow() {
        // Regression: a count-based split could put far more *bytes* on one side
        // than a page holds when keys vary wildly in length, aborting a legal
        // insert. Interleave many ~MAX_VAR_KEY keys with tiny ones to force
        // byte-imbalanced splits at every level.
        let (_d, pool) = pool();
        let t = VarBTree::create(&pool).unwrap();
        let mut expected: Vec<Vec<u8>> = Vec::new();
        for i in 0..400u64 {
            let key = if i % 2 == 0 {
                // A tiny key.
                format!("t{i:04}").into_bytes()
            } else {
                // A near-maximal key (distinct by the rowid prefix).
                let mut k = format!("h{i:04}").into_bytes();
                k.extend(std::iter::repeat_n(b'x', MAX_VAR_KEY - k.len()));
                k
            };
            t.insert(&key, tref(i)).unwrap();
            expected.push(key);
        }
        // Every key is still findable after all those lopsided splits.
        for (i, key) in expected.iter().enumerate() {
            assert_eq!(
                t.search(key).unwrap(),
                Some(tref(i as u64)),
                "key index {i}"
            );
        }
        // And the scan stays globally sorted across the mixed sizes.
        let scanned: Vec<Vec<u8>> = t
            .range_scan(Bound::Unbounded, Bound::Unbounded)
            .unwrap()
            .map(|r| r.unwrap().0)
            .collect();
        let mut sorted = expected.clone();
        sorted.sort();
        assert_eq!(scanned, sorted);
    }

    #[test]
    fn range_scan_returns_sorted_window() {
        let (_d, pool) = pool();
        let t = VarBTree::create(&pool).unwrap();
        for i in 0..1000u64 {
            let key = format!("{i:05}");
            t.insert(key.as_bytes(), tref(i)).unwrap();
        }
        // Inclusive range [00100, 00105].
        let lo = b"00100".to_vec();
        let hi = b"00105".to_vec();
        let got: Vec<u64> = t
            .range_scan(
                Bound::Included(lo.as_slice()),
                Bound::Included(hi.as_slice()),
            )
            .unwrap()
            .map(|r| r.unwrap().1.page_id.get())
            .collect();
        assert_eq!(got, vec![100, 101, 102, 103, 104, 105]);

        // Exclusive lower bound skips the first.
        let got: Vec<u64> = t
            .range_scan(
                Bound::Excluded(lo.as_slice()),
                Bound::Included(hi.as_slice()),
            )
            .unwrap()
            .map(|r| r.unwrap().1.page_id.get())
            .collect();
        assert_eq!(got, vec![101, 102, 103, 104, 105]);
    }

    #[test]
    fn range_scan_over_shared_prefix_returns_all_duplicates() {
        // The non-unique use case: many keys share a value prefix, made unique
        // by an 8-byte big-endian rowid suffix. A range over the prefix returns
        // every rowid.
        let (_d, pool) = pool();
        let t = VarBTree::create(&pool).unwrap();
        for rowid in 0..200u64 {
            let mut key = b"active".to_vec();
            key.extend_from_slice(&rowid.to_be_bytes());
            t.insert(&key, tref(rowid)).unwrap();
        }
        let lo = {
            let mut k = b"active".to_vec();
            k.extend_from_slice(&0u64.to_be_bytes());
            k
        };
        let hi = {
            let mut k = b"active".to_vec();
            k.extend_from_slice(&u64::MAX.to_be_bytes());
            k
        };
        let got: Vec<u64> = t
            .range_scan(
                Bound::Included(lo.as_slice()),
                Bound::Included(hi.as_slice()),
            )
            .unwrap()
            .map(|r| r.unwrap().1.page_id.get())
            .collect();
        assert_eq!(got, (0..200).collect::<Vec<_>>());
    }

    use proptest::prelude::*;

    proptest! {
        #![proptest_config(ProptestConfig { cases: 64, ..ProptestConfig::default() })]

        /// Insert a random set of variable-length byte keys, then check every
        /// search and a full range scan against a `BTreeMap` oracle. Keys vary in
        /// length (0..40 bytes) to exercise the heap layout and splits.
        #[test]
        fn matches_btreemap_oracle(
            entries in prop::collection::vec(
                // Most keys are short, but ~1 in 6 is large (up to MAX_VAR_KEY)
                // so byte-imbalanced splits are exercised, not just count splits.
                (
                    prop_oneof![
                        5 => prop::collection::vec(any::<u8>(), 0..40),
                        1 => prop::collection::vec(any::<u8>(), 1500..=MAX_VAR_KEY),
                    ],
                    any::<u64>(),
                ),
                1..300,
            )
        ) {
            use std::collections::BTreeMap;
            let (_d, pool) = pool();
            let t = VarBTree::create(&pool).unwrap();
            let mut oracle: BTreeMap<Vec<u8>, u64> = BTreeMap::new();
            for (key, rowid) in entries {
                // The tree is idempotent on a duplicate key (first write wins);
                // mirror that in the oracle.
                if t.search(&key).unwrap().is_none() {
                    t.insert(&key, tref(rowid)).unwrap();
                    oracle.insert(key, rowid);
                }
            }
            // Every oracle key resolves to the same rowid.
            for (key, rowid) in &oracle {
                prop_assert_eq!(t.search(key).unwrap(), Some(tref(*rowid)));
            }
            // A full unbounded scan yields the keys in sorted order.
            let scanned: Vec<(Vec<u8>, u64)> = t
                .range_scan(Bound::Unbounded, Bound::Unbounded)
                .unwrap()
                .map(|r| { let (k, v) = r.unwrap(); (k, v.page_id.get()) })
                .collect();
            let expected: Vec<(Vec<u8>, u64)> =
                oracle.iter().map(|(k, v)| (k.clone(), *v)).collect();
            prop_assert_eq!(scanned, expected);
        }
    }

    #[test]
    fn survives_reopen_at_recorded_root() {
        let (_d, pool) = pool();
        let root = {
            let t = VarBTree::create(&pool).unwrap();
            for i in 0..2000u64 {
                t.insert(format!("k{i:06}").as_bytes(), tref(i)).unwrap();
            }
            t.root_page()
        };
        pool.flush_all().unwrap();
        // Re-open the same tree at its recorded root and read back.
        let t = VarBTree::open(&pool, root);
        for i in 0..2000u64 {
            assert_eq!(
                t.search(format!("k{i:06}").as_bytes()).unwrap(),
                Some(tref(i))
            );
        }
    }
}
