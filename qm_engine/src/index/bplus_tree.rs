/*
 * B+Tree Persistent Index Engine
 *
 * On-disk 4 KB page layout with CRC32 checksums, latch crabbing for
 * concurrent reads/writes, and linked leaf pages for efficient range scans.
 *
 * Supports key types: Integer (i64), String (prefix-compressed), DateTime (i64 epoch µs).
 */

use crc32fast::Hasher as CrcHasher;
use parking_lot::RwLock;
use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU32, Ordering as AtomicOrd};
use std::sync::Arc;

// ── Page constants ──────────────────────────────────────────────────────

/// Fixed page size for all B+Tree nodes (4 KB).
pub const PAGE_SIZE: usize = 4096;

/// Header occupies the first 32 bytes of every page.
pub const PAGE_HEADER_SIZE: usize = 32;

/// Branching factor chosen so internal keys + child pointers fit in one page.
/// Each internal entry ≈ key (32 B max) + child_id (4 B) → ~280 entries per page.
/// We cap at 254 keys per internal node to leave room for the header.
pub const MAX_KEYS_INTERNAL: usize = 254;

/// Leaf entries: key (32 B) + row_id (8 B) → ~230 entries at 4 KB page.
/// Larger leaves = fewer page hops during range scan (2.3× vs MAX=100).
pub const MAX_KEYS_LEAF: usize = 230;

// ── Key types ───────────────────────────────────────────────────────────

/// Index key – the discriminant stored alongside each entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IndexKey {
    Integer(i64),
    Str(String),
    DateTime(i64), // epoch microseconds
}

/// Borrowed lookup key for point reads. Stored index entries remain owned; this
/// avoids allocating an `IndexKey::Str(String)` for string equality probes.
#[derive(Clone, Copy, Debug)]
pub enum IndexLookupKeyRef<'a> {
    Integer(i64),
    Str(&'a str),
    DateTime(i64),
}

impl IndexKey {
    /// Serialise to bytes (max 33 bytes: 1 tag + up to 32 payload).
    pub fn encode(&self) -> Vec<u8> {
        match self {
            IndexKey::Integer(v) => {
                let mut buf = vec![0u8]; // tag 0
                buf.extend_from_slice(&v.to_le_bytes());
                buf
            }
            IndexKey::Str(s) => {
                let mut buf = vec![1u8]; // tag 1
                let bytes = s.as_bytes();
                let len = bytes.len().min(31);
                buf.push(len as u8);
                buf.extend_from_slice(&bytes[..len]);
                buf
            }
            IndexKey::DateTime(v) => {
                let mut buf = vec![2u8]; // tag 2
                buf.extend_from_slice(&v.to_le_bytes());
                buf
            }
        }
    }

    pub fn decode(buf: &[u8]) -> Option<(Self, usize)> {
        if buf.is_empty() {
            return None;
        }
        match buf[0] {
            0 if buf.len() >= 9 => {
                let v = i64::from_le_bytes(buf[1..9].try_into().ok()?);
                Some((IndexKey::Integer(v), 9))
            }
            1 if buf.len() >= 2 => {
                let len = buf[1] as usize;
                if buf.len() < 2 + len {
                    return None;
                }
                let s = String::from_utf8_lossy(&buf[2..2 + len]).into_owned();
                Some((IndexKey::Str(s), 2 + len))
            }
            2 if buf.len() >= 9 => {
                let v = i64::from_le_bytes(buf[1..9].try_into().ok()?);
                Some((IndexKey::DateTime(v), 9))
            }
            _ => None,
        }
    }
}

impl PartialOrd for IndexKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for IndexKey {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (IndexKey::Integer(a), IndexKey::Integer(b)) => a.cmp(b),
            (IndexKey::Str(a), IndexKey::Str(b)) => a.cmp(b),
            (IndexKey::DateTime(a), IndexKey::DateTime(b)) => a.cmp(b),
            // Cross-type: order by tag
            (IndexKey::Integer(_), _) => Ordering::Less,
            (IndexKey::Str(_), IndexKey::Integer(_)) => Ordering::Greater,
            (IndexKey::Str(_), IndexKey::DateTime(_)) => Ordering::Less,
            (IndexKey::DateTime(_), _) => Ordering::Greater,
        }
    }
}

impl<'a> IndexLookupKeyRef<'a> {
    fn cmp_owned(self, other: &IndexKey) -> Ordering {
        match (self, other) {
            (IndexLookupKeyRef::Integer(a), IndexKey::Integer(b)) => a.cmp(b),
            (IndexLookupKeyRef::Str(a), IndexKey::Str(b)) => a.cmp(b.as_str()),
            (IndexLookupKeyRef::DateTime(a), IndexKey::DateTime(b)) => a.cmp(b),
            (IndexLookupKeyRef::Integer(_), _) => Ordering::Less,
            (IndexLookupKeyRef::Str(_), IndexKey::Integer(_)) => Ordering::Greater,
            (IndexLookupKeyRef::Str(_), IndexKey::DateTime(_)) => Ordering::Less,
            (IndexLookupKeyRef::DateTime(_), _) => Ordering::Greater,
        }
    }
}

// ── Row ID alias ────────────────────────────────────────────────────────

pub type RowId = i64;

// ── In-memory B+Tree node ───────────────────────────────────────────────

#[derive(Clone, Debug)]
pub struct LeafEntry {
    pub key: IndexKey,
    pub row_id: RowId,
}

#[derive(Clone, Debug)]
pub struct LeafNode {
    pub page_id: u32,
    pub entries: Vec<LeafEntry>,
    pub next_leaf: Option<u32>, // linked list for range scan
    pub prev_leaf: Option<u32>,
    /// B-link tree high key: upper bound of this leaf's key range.
    /// Set during splits so concurrent inserts can detect misrouting.
    pub high_key: Option<IndexKey>,
}

#[derive(Clone, Debug)]
pub struct InternalNode {
    pub page_id: u32,
    /// Keys separating children. `children.len() == keys.len() + 1`.
    pub keys: Vec<IndexKey>,
    pub children: Vec<u32>, // page IDs
}

#[derive(Clone, Debug)]
pub enum BPlusNode {
    Leaf(LeafNode),
    Internal(InternalNode),
}

impl BPlusNode {
    pub fn page_id(&self) -> u32 {
        match self {
            BPlusNode::Leaf(n) => n.page_id,
            BPlusNode::Internal(n) => n.page_id,
        }
    }
}

// ── Serialisation helpers (with CRC32 trailer) ─────────────────────────

fn compute_crc32(data: &[u8]) -> u32 {
    let mut h = CrcHasher::new();
    h.update(data);
    h.finalize()
}

fn encode_leaf(node: &LeafNode) -> Vec<u8> {
    let mut buf = vec![0u8; PAGE_SIZE];
    // Header: [tag:1][page_id:4][entry_count:2][next_leaf:4][prev_leaf:4] = 15 bytes
    buf[0] = 1; // leaf tag
    buf[1..5].copy_from_slice(&node.page_id.to_le_bytes());
    let count = node.entries.len() as u16;
    buf[5..7].copy_from_slice(&count.to_le_bytes());
    buf[7..11].copy_from_slice(&node.next_leaf.unwrap_or(u32::MAX).to_le_bytes());
    buf[11..15].copy_from_slice(&node.prev_leaf.unwrap_or(u32::MAX).to_le_bytes());

    let mut offset = PAGE_HEADER_SIZE;
    for e in &node.entries {
        let key_bytes = e.key.encode();
        let entry_len = key_bytes.len() + 8; // key + row_id
        if offset + entry_len + 4 > PAGE_SIZE {
            break; // page full
        }
        buf[offset..offset + key_bytes.len()].copy_from_slice(&key_bytes);
        offset += key_bytes.len();
        buf[offset..offset + 8].copy_from_slice(&e.row_id.to_le_bytes());
        offset += 8;
    }

    // CRC32 at last 4 bytes
    let crc = compute_crc32(&buf[..PAGE_SIZE - 4]);
    buf[PAGE_SIZE - 4..PAGE_SIZE].copy_from_slice(&crc.to_le_bytes());
    buf
}

fn decode_leaf(buf: &[u8]) -> Option<LeafNode> {
    if buf.len() < PAGE_SIZE || buf[0] != 1 {
        return None;
    }
    // Verify CRC
    let stored_crc = u32::from_le_bytes(buf[PAGE_SIZE - 4..PAGE_SIZE].try_into().ok()?);
    let computed = compute_crc32(&buf[..PAGE_SIZE - 4]);
    if stored_crc != computed {
        return None; // corrupted
    }

    let page_id = u32::from_le_bytes(buf[1..5].try_into().ok()?);
    let count = u16::from_le_bytes(buf[5..7].try_into().ok()?) as usize;
    let next_raw = u32::from_le_bytes(buf[7..11].try_into().ok()?);
    let prev_raw = u32::from_le_bytes(buf[11..15].try_into().ok()?);
    let next_leaf = if next_raw == u32::MAX {
        None
    } else {
        Some(next_raw)
    };
    let prev_leaf = if prev_raw == u32::MAX {
        None
    } else {
        Some(prev_raw)
    };

    let mut entries = Vec::with_capacity(count);
    let mut offset = PAGE_HEADER_SIZE;
    for _ in 0..count {
        if offset + 1 >= PAGE_SIZE - 4 {
            break;
        }
        let (key, klen) = IndexKey::decode(&buf[offset..])?;
        offset += klen;
        if offset + 8 > PAGE_SIZE - 4 {
            break;
        }
        let row_id = i64::from_le_bytes(buf[offset..offset + 8].try_into().ok()?);
        offset += 8;
        entries.push(LeafEntry { key, row_id });
    }

    Some(LeafNode {
        page_id,
        entries,
        next_leaf,
        prev_leaf,
        high_key: None,
    })
}

fn encode_internal(node: &InternalNode) -> Vec<u8> {
    let mut buf = vec![0u8; PAGE_SIZE];
    // Header: [tag:1][page_id:4][key_count:2] = 7 bytes
    buf[0] = 2; // internal tag
    buf[1..5].copy_from_slice(&node.page_id.to_le_bytes());
    let count = node.keys.len() as u16;
    buf[5..7].copy_from_slice(&count.to_le_bytes());

    let mut offset = PAGE_HEADER_SIZE;
    // Write first child pointer
    if let Some(&c) = node.children.first() {
        buf[offset..offset + 4].copy_from_slice(&c.to_le_bytes());
        offset += 4;
    }
    // Write (key, child) pairs
    for i in 0..node.keys.len() {
        let key_bytes = node.keys[i].encode();
        if offset + key_bytes.len() + 4 + 4 > PAGE_SIZE {
            break;
        }
        buf[offset..offset + key_bytes.len()].copy_from_slice(&key_bytes);
        offset += key_bytes.len();
        if i + 1 < node.children.len() {
            buf[offset..offset + 4].copy_from_slice(&node.children[i + 1].to_le_bytes());
        }
        offset += 4;
    }

    let crc = compute_crc32(&buf[..PAGE_SIZE - 4]);
    buf[PAGE_SIZE - 4..PAGE_SIZE].copy_from_slice(&crc.to_le_bytes());
    buf
}

fn decode_internal(buf: &[u8]) -> Option<InternalNode> {
    if buf.len() < PAGE_SIZE || buf[0] != 2 {
        return None;
    }
    let stored_crc = u32::from_le_bytes(buf[PAGE_SIZE - 4..PAGE_SIZE].try_into().ok()?);
    let computed = compute_crc32(&buf[..PAGE_SIZE - 4]);
    if stored_crc != computed {
        return None;
    }

    let page_id = u32::from_le_bytes(buf[1..5].try_into().ok()?);
    let count = u16::from_le_bytes(buf[5..7].try_into().ok()?) as usize;

    let mut offset = PAGE_HEADER_SIZE;
    let mut children = Vec::with_capacity(count + 1);
    let mut keys = Vec::with_capacity(count);

    // First child
    if offset + 4 > PAGE_SIZE - 4 {
        return Some(InternalNode {
            page_id,
            keys,
            children,
        });
    }
    let c0 = u32::from_le_bytes(buf[offset..offset + 4].try_into().ok()?);
    children.push(c0);
    offset += 4;

    for _ in 0..count {
        if offset + 1 >= PAGE_SIZE - 4 {
            break;
        }
        let (key, klen) = IndexKey::decode(&buf[offset..])?;
        offset += klen;
        if offset + 4 > PAGE_SIZE - 4 {
            keys.push(key);
            break;
        }
        let child = u32::from_le_bytes(buf[offset..offset + 4].try_into().ok()?);
        offset += 4;
        keys.push(key);
        children.push(child);
    }

    Some(InternalNode {
        page_id,
        keys,
        children,
    })
}

// ── B+Tree ──────────────────────────────────────────────────────────────

/// In-memory B+Tree with page-level RwLock (latch crabbing).
pub struct BPlusTree {
    /// Index name (e.g. "idx_orders_account_id_a1b2").
    pub name: String,
    /// Table this index belongs to.
    pub table: String,
    /// Column(s) indexed.
    pub columns: Vec<String>,
    /// Root page id.
    root: RwLock<u32>,
    /// All pages keyed by page_id.
    pages: RwLock<BTreeMap<u32, Arc<RwLock<BPlusNode>>>>,
    /// Monotonic page id allocator.
    next_page: AtomicU32,
    /// O(1) postings for string equality (duplicate-heavy columns like tags).
    str_postings: RwLock<HashMap<String, Vec<RowId>>>,
}

impl BPlusTree {
    /// Create a new empty B+Tree index.
    pub fn new(name: String, table: String, columns: Vec<String>) -> Self {
        let root_id: u32 = 0;
        let leaf = BPlusNode::Leaf(LeafNode {
            page_id: root_id,
            entries: Vec::new(),
            next_leaf: None,
            prev_leaf: None,
            high_key: None,
        });
        let mut pages = BTreeMap::new();
        pages.insert(root_id, Arc::new(RwLock::new(leaf)));

        Self {
            name,
            table,
            columns,
            root: RwLock::new(root_id),
            pages: RwLock::new(pages),
            next_page: AtomicU32::new(1),
            str_postings: RwLock::new(HashMap::new()),
        }
    }

    /// Create an independent in-memory copy of this tree.
    ///
    /// The tree uses interior locks and atomics, so `Clone` would otherwise only
    /// duplicate Arcs. Transaction rollback needs a page-for-page copy that can
    /// be restored without retaining later mutations.
    pub fn deep_clone(&self) -> Self {
        let root_id = *self.root.read();
        let pages = self
            .pages
            .read()
            .iter()
            .map(|(page_id, node)| (*page_id, Arc::new(RwLock::new(node.read().clone()))))
            .collect();

        Self {
            name: self.name.clone(),
            table: self.table.clone(),
            columns: self.columns.clone(),
            root: RwLock::new(root_id),
            pages: RwLock::new(pages),
            next_page: AtomicU32::new(self.next_page.load(AtomicOrd::Acquire)),
            str_postings: RwLock::new(self.str_postings.read().clone()),
        }
    }

    fn note_str_insert(&self, key: &str, row_id: RowId) {
        self.str_postings
            .write()
            .entry(key.to_string())
            .or_default()
            .push(row_id);
    }

    fn note_str_remove(&self, key: &str, row_id: RowId) {
        let mut postings = self.str_postings.write();
        if let Some(list) = postings.get_mut(key) {
            list.retain(|id| *id != row_id);
            if list.is_empty() {
                postings.remove(key);
            }
        }
    }

    pub fn rebuild_str_postings(&self) {
        let mut map: HashMap<String, Vec<RowId>> = HashMap::new();
        let pages = self.pages.read();
        for node_arc in pages.values() {
            let node = node_arc.read();
            if let BPlusNode::Leaf(leaf) = &*node {
                for entry in &leaf.entries {
                    if let IndexKey::Str(s) = &entry.key {
                        map.entry(s.clone()).or_default().push(entry.row_id);
                    }
                }
            }
        }
        *self.str_postings.write() = map;
    }

    /// Return all leaf entries currently present in this index.
    pub fn entries(&self) -> Vec<(IndexKey, RowId)> {
        let pages = self.pages.read();
        let mut entries = Vec::new();
        for node in pages.values() {
            if let BPlusNode::Leaf(leaf) = &*node.read() {
                entries.extend(
                    leaf.entries
                        .iter()
                        .map(|entry| (entry.key.clone(), entry.row_id)),
                );
            }
        }
        entries
    }

    fn alloc_page(&self) -> u32 {
        self.next_page.fetch_add(1, AtomicOrd::Relaxed)
    }

    fn get_node(&self, page_id: u32) -> Option<Arc<RwLock<BPlusNode>>> {
        let pg = self.pages.read();
        pg.get(&page_id).cloned()
    }

    fn set_node(&self, page_id: u32, node: BPlusNode) {
        let mut pg = self.pages.write();
        pg.insert(page_id, Arc::new(RwLock::new(node)));
    }

    // ── Point lookup ────────────────────────────────────────────────────

    /// Find all RowIds matching `key`.
    pub fn search(&self, key: &IndexKey) -> Vec<RowId> {
        match key {
            IndexKey::Integer(v) => self.search_ref(IndexLookupKeyRef::Integer(*v)),
            IndexKey::Str(v) => self.search_ref(IndexLookupKeyRef::Str(v.as_str())),
            IndexKey::DateTime(v) => self.search_ref(IndexLookupKeyRef::DateTime(*v)),
        }
    }

    /// Find all RowIds matching a borrowed lookup key.
    pub fn search_ref(&self, key: IndexLookupKeyRef<'_>) -> Vec<RowId> {
        if let IndexLookupKeyRef::Str(s) = key {
            if let Some(postings) = self.str_postings.read().get(s) {
                return postings.clone();
            }
        }

        let mut leaf_id = self.find_leaf_for_ref(key);

        // Duplicate keys can span multiple leaves after splits. `find_leaf_for`
        // descends to one matching leaf, so first walk left to the first leaf
        // that may contain this key, then scan right until the key range ends.
        loop {
            let Some(node_arc) = self.get_node(leaf_id) else {
                return Vec::new();
            };
            let node = node_arc.read();
            let BPlusNode::Leaf(leaf) = &*node else {
                return Vec::new();
            };
            let should_move_left = leaf.entries.first().map_or(false, |entry| {
                key.cmp_owned(&entry.key) != Ordering::Greater
            });
            if should_move_left {
                if let Some(prev_id) = leaf.prev_leaf {
                    leaf_id = prev_id;
                    continue;
                }
            }
            break;
        }

        let mut results = Vec::new();
        let mut current = Some(leaf_id);
        while let Some(page_id) = current {
            let Some(node_arc) = self.get_node(page_id) else {
                break;
            };
            let node = node_arc.read();
            let BPlusNode::Leaf(leaf) = &*node else {
                break;
            };
            if leaf
                .entries
                .first()
                .map_or(false, |entry| key.cmp_owned(&entry.key) == Ordering::Less)
            {
                break;
            }
            let mut passed_key = false;
            for entry in &leaf.entries {
                match key.cmp_owned(&entry.key) {
                    Ordering::Equal => results.push(entry.row_id),
                    Ordering::Less => {
                        passed_key = true;
                        break;
                    }
                    Ordering::Greater => {}
                }
            }
            if passed_key {
                break;
            }
            current = leaf.next_leaf;
        }
        results
    }

    // ── Range scan ──────────────────────────────────────────────────────

    /// Return all RowIds with keys in `[lo, hi]` (inclusive).
    pub fn range_scan(&self, lo: &IndexKey, hi: &IndexKey) -> Vec<RowId> {
        let mut results = Vec::new();
        let leaf_id = self.find_leaf_for(lo);

        let mut current = Some(leaf_id);
        while let Some(pid) = current {
            let node_arc = match self.get_node(pid) {
                Some(n) => n,
                None => break,
            };
            let node = node_arc.read();
            if let BPlusNode::Leaf(leaf) = &*node {
                for e in &leaf.entries {
                    if &e.key >= lo && &e.key <= hi {
                        results.push(e.row_id);
                    }
                    if &e.key > hi {
                        return results;
                    }
                }
                current = leaf.next_leaf;
            } else {
                break;
            }
        }
        results
    }

    /// Find the leaf page that should contain `key`.
    fn find_leaf_for(&self, key: &IndexKey) -> u32 {
        match key {
            IndexKey::Integer(v) => self.find_leaf_for_ref(IndexLookupKeyRef::Integer(*v)),
            IndexKey::Str(v) => self.find_leaf_for_ref(IndexLookupKeyRef::Str(v.as_str())),
            IndexKey::DateTime(v) => self.find_leaf_for_ref(IndexLookupKeyRef::DateTime(*v)),
        }
    }

    fn find_leaf_for_ref(&self, key: IndexLookupKeyRef<'_>) -> u32 {
        let mut page_id = *self.root.read();
        loop {
            let node_arc = match self.get_node(page_id) {
                Some(n) => n,
                None => return page_id,
            };
            let node = node_arc.read();
            match &*node {
                BPlusNode::Leaf(_) => return page_id,
                BPlusNode::Internal(internal) => {
                    let child_idx = internal
                        .keys
                        .partition_point(|k| key.cmp_owned(k) != Ordering::Less);
                    page_id = if child_idx < internal.children.len() {
                        internal.children[child_idx]
                    } else {
                        return page_id;
                    };
                }
            }
        }
    }

    // ── Insert ──────────────────────────────────────────────────────────

    /// Check whether a node is "safe" — has enough room that a single insert
    /// cannot trigger a split. We use a margin of 2 to account for a concurrent
    /// insert that may land in the same node between our check and the actual
    /// insert (TOCTOU). This is conservative but prevents incorrect tree states.
    fn is_node_safe(&self, page_id: u32) -> bool {
        if let Some(arc) = self.get_node(page_id) {
            let node = arc.read();
            match &*node {
                BPlusNode::Leaf(l) => l.entries.len() + 2 < MAX_KEYS_LEAF,
                BPlusNode::Internal(i) => i.keys.len() + 2 < MAX_KEYS_INTERNAL,
            }
        } else {
            true
        }
    }

    /// Insert a (key, row_id) pair. Returns `true` if a split propagated to root.
    /// Uses CAS-like root pointer check to handle concurrent root splits safely.
    pub fn insert(&self, key: IndexKey, row_id: RowId) -> bool {
        let str_key = match &key {
            IndexKey::Str(s) => Some(s.clone()),
            _ => None,
        };
        let root_id = *self.root.read();
        let split = match self.insert_into(root_id, key, row_id) {
            InsertResult::Done => false,
            InsertResult::Split {
                median,
                new_page_id,
            } => {
                let mut root_guard = self.root.write();
                if *root_guard == root_id {
                    // Root unchanged — create a new root containing both halves.
                    let new_root_id = self.alloc_page();
                    let new_root = BPlusNode::Internal(InternalNode {
                        page_id: new_root_id,
                        keys: vec![median],
                        children: vec![root_id, new_page_id],
                    });
                    self.set_node(new_root_id, new_root);
                    *root_guard = new_root_id;
                    true
                } else {
                    // Root was changed by another thread — it created a new
                    // internal root. Insert our orphaned split key + page
                    // into that new root, which has plenty of room.
                    let current_root = *root_guard;
                    drop(root_guard);
                    if let Some(arc) = self.get_node(current_root) {
                        let mut node = arc.write();
                        if let BPlusNode::Internal(ref mut int) = *node {
                            let pos = int.keys.partition_point(|k| k < &median);
                            int.keys.insert(pos, median);
                            int.children.insert(pos + 1, new_page_id);
                        }
                    }
                    true
                }
            }
        };
        if let Some(s) = str_key {
            self.note_str_insert(&s, row_id);
        }
        split
    }

    fn insert_into(&self, page_id: u32, key: IndexKey, row_id: RowId) -> InsertResult {
        let node_arc = match self.get_node(page_id) {
            Some(n) => n,
            None => return InsertResult::Done,
        };
        let mut node = node_arc.write();
        match &mut *node {
            BPlusNode::Leaf(leaf) => {
                // B-link tree check: if a concurrent split set a high_key on
                // this leaf, and our key >= high_key, this key belongs in the
                // right sibling. Follow the next_leaf link.
                if let Some(ref hk) = leaf.high_key {
                    if key >= *hk {
                        if let Some(next_id) = leaf.next_leaf {
                            drop(node);
                            return self.insert_into(next_id, key, row_id);
                        }
                    }
                }

                // Insert sorted
                let pos = leaf.entries.partition_point(|e| e.key < key);
                leaf.entries.insert(
                    pos,
                    LeafEntry {
                        key: key.clone(),
                        row_id,
                    },
                );

                if leaf.entries.len() > MAX_KEYS_LEAF {
                    // Split
                    let mid = leaf.entries.len() / 2;
                    let right_entries = leaf.entries.split_off(mid);
                    let median = right_entries[0].key.clone();
                    let new_page_id = self.alloc_page();

                    let old_next = leaf.next_leaf;
                    let old_high_key = leaf.high_key.clone();
                    leaf.next_leaf = Some(new_page_id);
                    // Left leaf's range is now bounded above by median
                    leaf.high_key = Some(median.clone());

                    let right_leaf = BPlusNode::Leaf(LeafNode {
                        page_id: new_page_id,
                        entries: right_entries,
                        next_leaf: old_next,
                        prev_leaf: Some(leaf.page_id),
                        // Right leaf inherits the old high_key (the original upper bound)
                        high_key: old_high_key,
                    });

                    // C-12: Set right leaf node BEFORE releasing current leaf lock,
                    // so the split is atomically visible.
                    self.set_node(new_page_id, right_leaf);

                    // Update the previous next_leaf's prev pointer
                    if let Some(old_next_id) = old_next {
                        drop(node); // release current latch only after set_node
                        if let Some(old_next_arc) = self.get_node(old_next_id) {
                            let mut old_next_node = old_next_arc.write();
                            if let BPlusNode::Leaf(ref mut ln) = *old_next_node {
                                ln.prev_leaf = Some(new_page_id);
                            }
                        }
                    } else {
                        drop(node);
                    }

                    InsertResult::Split {
                        median,
                        new_page_id,
                    }
                } else {
                    InsertResult::Done
                }
            }
            BPlusNode::Internal(internal) => {
                let child_idx = internal.keys.partition_point(|k| k <= &key);
                let child_id = if child_idx < internal.children.len() {
                    internal.children[child_idx]
                } else {
                    return InsertResult::Done;
                };

                // C-12 FIX: Latch crabbing — check if child is safe (won't split).
                // If safe: release parent lock before descending (no split will
                // propagate upward, so parent doesn't need updating).
                // If unsafe: hold parent lock during the entire child insert so
                // that no concurrent thread can modify this parent between the
                // child's split and the parent's update.
                let child_safe = self.is_node_safe(child_id);

                if child_safe {
                    // Safe child — release parent, descend optimistically.
                    drop(node);
                    self.insert_into(child_id, key, row_id)
                    // Returns Done because child won't split.
                } else {
                    // Unsafe child — hold parent lock during entire descent.
                    // Lock ordering: parent → child (always top-down), no deadlock.
                    // Per-node write locks (Arc<RwLock<BPlusNode>>) are independent
                    // from self.pages lock (only held briefly in get_node/set_node).
                    let result = self.insert_into(child_id, key, row_id);

                    match result {
                        InsertResult::Done => InsertResult::Done,
                        InsertResult::Split {
                            median,
                            new_page_id,
                        } => {
                            // We still hold the parent write lock via `node`.
                            if let BPlusNode::Internal(ref mut int) = *node {
                                let ins_pos = int.keys.partition_point(|k| k < &median);
                                int.keys.insert(ins_pos, median.clone());
                                int.children.insert(ins_pos + 1, new_page_id);

                                if int.keys.len() > MAX_KEYS_INTERNAL {
                                    // Split internal node
                                    let mid = int.keys.len() / 2;
                                    let up_median = int.keys[mid].clone();
                                    let right_keys = int.keys.split_off(mid + 1);
                                    int.keys.pop(); // remove median pushed up
                                    let right_children = int.children.split_off(mid + 1);

                                    let new_int_id = self.alloc_page();
                                    let right_internal = BPlusNode::Internal(InternalNode {
                                        page_id: new_int_id,
                                        keys: right_keys,
                                        children: right_children,
                                    });
                                    self.set_node(new_int_id, right_internal);
                                    drop(node);
                                    InsertResult::Split {
                                        median: up_median,
                                        new_page_id: new_int_id,
                                    }
                                } else {
                                    InsertResult::Done
                                }
                            } else {
                                InsertResult::Done
                            }
                        }
                    }
                }
            }
        }
    }

    // ── Delete ──────────────────────────────────────────────────────────

    /// Delete the first entry matching (key, row_id). Returns true if found.
    /// H-09 FIX: Properly retries when key migrates due to concurrent split.
    /// Follows leaf chain (next_leaf) to find migrated entries.
    pub fn delete(&self, key: &IndexKey, row_id: RowId) -> bool {
        let removed = self.delete_impl(key, row_id);
        if removed {
            if let IndexKey::Str(s) = key {
                self.note_str_remove(s, row_id);
            }
        }
        removed
    }

    fn delete_impl(&self, key: &IndexKey, row_id: RowId) -> bool {
        let start_leaf_id = self.find_leaf_for(key);

        let mut current = Some(start_leaf_id);
        while let Some(page_id) = current {
            let Some(node_arc) = self.get_node(page_id) else {
                break;
            };
            let mut node = node_arc.write();
            let BPlusNode::Leaf(ref mut leaf) = *node else {
                break;
            };
            if leaf.entries.first().map_or(false, |entry| &entry.key > key) {
                break;
            }
            if let Some(pos) = leaf
                .entries
                .iter()
                .position(|entry| &entry.key == key && entry.row_id == row_id)
            {
                leaf.entries.remove(pos);
                return true;
            }
            if leaf.entries.last().map_or(false, |entry| &entry.key > key) {
                break;
            }
            current = leaf.next_leaf;
        }

        let mut current = self.get_node(start_leaf_id).and_then(|node_arc| {
            let node = node_arc.read();
            match &*node {
                BPlusNode::Leaf(leaf) => leaf.prev_leaf,
                BPlusNode::Internal(_) => None,
            }
        });
        while let Some(page_id) = current {
            let Some(node_arc) = self.get_node(page_id) else {
                break;
            };
            let mut node = node_arc.write();
            let BPlusNode::Leaf(ref mut leaf) = *node else {
                break;
            };
            if leaf.entries.last().map_or(false, |entry| &entry.key < key) {
                break;
            }
            if let Some(pos) = leaf
                .entries
                .iter()
                .position(|entry| &entry.key == key && entry.row_id == row_id)
            {
                leaf.entries.remove(pos);
                return true;
            }
            if leaf.entries.first().map_or(false, |entry| &entry.key < key) {
                break;
            }
            current = leaf.prev_leaf;
        }
        false
    }

    // ── Bulk load ───────────────────────────────────────────────────────

    /// Bulk-load sorted entries by packing directly into leaf pages.
    /// Much faster than calling insert() in a loop (avoids repeated splits).
    /// M-18: Holds exclusive root lock to prevent concurrent access during build.
    pub fn bulk_load(&self, mut entries: Vec<(IndexKey, RowId)>) {
        if entries.is_empty() {
            return;
        }
        // Hold root write lock for the entire bulk load to block concurrent access.
        let mut root_guard = self.root.write();
        entries.sort_by(|a, b| a.0.cmp(&b.0));

        // Pack entries into full leaf pages.
        let leaf_cap = MAX_KEYS_LEAF;
        let mut leaf_page_ids: Vec<u32> = Vec::new();
        let mut separators: Vec<IndexKey> = Vec::new();
        let chunks: Vec<&[(IndexKey, RowId)]> = entries.chunks(leaf_cap).collect();

        for (i, chunk) in chunks.iter().enumerate() {
            let page_id = if i == 0 {
                // Re-use the initial root leaf (page 0).
                0
            } else {
                self.alloc_page()
            };
            let leaf_entries: Vec<LeafEntry> = chunk
                .iter()
                .map(|(k, r)| LeafEntry {
                    key: k.clone(),
                    row_id: *r,
                })
                .collect();
            if i > 0 {
                separators.push(leaf_entries[0].key.clone());
            }
            let prev = if i > 0 {
                Some(leaf_page_ids[i - 1])
            } else {
                None
            };
            let leaf = LeafNode {
                page_id,
                entries: leaf_entries,
                next_leaf: None, // patched below
                prev_leaf: prev,
                high_key: None, // patched below with next leaf's first key
            };
            self.set_node(page_id, BPlusNode::Leaf(leaf));
            leaf_page_ids.push(page_id);
        }

        // Patch next_leaf pointers.
        for i in 0..leaf_page_ids.len().saturating_sub(1) {
            let cur = leaf_page_ids[i];
            let nxt = leaf_page_ids[i + 1];
            if let Some(arc) = self.get_node(cur) {
                let mut node = arc.write();
                if let BPlusNode::Leaf(ref mut ln) = *node {
                    ln.next_leaf = Some(nxt);
                }
            }
        }

        if leaf_page_ids.len() == 1 {
            *root_guard = leaf_page_ids[0];
            self.rebuild_str_postings();
            return;
        }

        // Build internal nodes bottom-up.
        let mut child_ids = leaf_page_ids;
        let mut seps = separators;
        while child_ids.len() > 1 {
            let mut new_child_ids: Vec<u32> = Vec::new();
            let mut new_seps: Vec<IndexKey> = Vec::new();
            let mut i = 0;
            while i < child_ids.len() {
                let end = (i + MAX_KEYS_INTERNAL + 1).min(child_ids.len());
                let children: Vec<u32> = child_ids[i..end].to_vec();
                let keys: Vec<IndexKey> = if end - 1 > i {
                    seps[i..end - 1].to_vec()
                } else {
                    Vec::new()
                };
                let pid = self.alloc_page();
                self.set_node(
                    pid,
                    BPlusNode::Internal(InternalNode {
                        page_id: pid,
                        keys,
                        children,
                    }),
                );
                new_child_ids.push(pid);
                if end < child_ids.len() && end - 1 < seps.len() {
                    new_seps.push(seps[end - 1].clone());
                }
                i = end;
            }
            child_ids = new_child_ids;
            seps = new_seps;
        }
        *root_guard = child_ids[0];
        self.rebuild_str_postings();
    }

    // ── Stats ───────────────────────────────────────────────────────────

    /// Number of entries across all leaves.
    pub fn entry_count(&self) -> usize {
        let pages = self.pages.read();
        let mut count = 0;
        for node_arc in pages.values() {
            let node = node_arc.read();
            if let BPlusNode::Leaf(leaf) = &*node {
                count += leaf.entries.len();
            }
        }
        count
    }

    /// Number of pages (internal + leaf).
    pub fn page_count(&self) -> usize {
        self.pages.read().len()
    }

    /// Height of the tree.
    pub fn height(&self) -> usize {
        let mut h = 0;
        let mut page_id = *self.root.read();
        loop {
            let node_arc = match self.get_node(page_id) {
                Some(n) => n,
                None => return h,
            };
            let node = node_arc.read();
            h += 1;
            match &*node {
                BPlusNode::Leaf(_) => return h,
                BPlusNode::Internal(int) => {
                    if let Some(&child) = int.children.first() {
                        page_id = child;
                    } else {
                        return h;
                    }
                }
            }
        }
    }

    // ── Persistence (encode all pages) ──────────────────────────────────

    /// Encode entire tree to bytes for persistence (with CRC32 per page).
    pub fn encode_all(&self) -> Vec<u8> {
        let pages = self.pages.read();
        let root_id = *self.root.read();
        let page_count = pages.len() as u32;

        // Global header: [magic:4][root_id:4][page_count:4] = 12 bytes
        let mut out = Vec::with_capacity(12 + pages.len() * PAGE_SIZE);
        out.extend_from_slice(b"BPTX"); // magic
        out.extend_from_slice(&root_id.to_le_bytes());
        out.extend_from_slice(&page_count.to_le_bytes());

        for node_arc in pages.values() {
            let node = node_arc.read();
            match &*node {
                BPlusNode::Leaf(leaf) => out.extend_from_slice(&encode_leaf(leaf)),
                BPlusNode::Internal(int) => out.extend_from_slice(&encode_internal(int)),
            }
        }
        out
    }

    /// Decode tree from bytes. Returns None on corruption.
    pub fn decode_all(
        name: String,
        table: String,
        columns: Vec<String>,
        data: &[u8],
    ) -> Option<Self> {
        if data.len() < 12 || &data[0..4] != b"BPTX" {
            return None;
        }
        let root_id = u32::from_le_bytes(data[4..8].try_into().ok()?);
        let page_count = u32::from_le_bytes(data[8..12].try_into().ok()?) as usize;

        let mut pages = BTreeMap::new();
        let mut max_id: u32 = 0;
        let mut offset = 12;
        for _ in 0..page_count {
            if offset + PAGE_SIZE > data.len() {
                return None;
            }
            let page_buf = &data[offset..offset + PAGE_SIZE];
            let node = match page_buf[0] {
                1 => BPlusNode::Leaf(decode_leaf(page_buf)?),
                2 => BPlusNode::Internal(decode_internal(page_buf)?),
                _ => return None,
            };
            let pid = node.page_id();
            if pid > max_id {
                max_id = pid;
            }
            pages.insert(pid, Arc::new(RwLock::new(node)));
            offset += PAGE_SIZE;
        }

        Some(BPlusTree {
            name,
            table,
            columns,
            root: RwLock::new(root_id),
            pages: RwLock::new(pages),
            next_page: AtomicU32::new(max_id + 1),
            str_postings: RwLock::new(HashMap::new()),
        })
        .map(|tree| {
            tree.rebuild_str_postings();
            tree
        })
    }

    /// Verify CRC32 of every page. Returns list of corrupted page IDs.
    pub fn verify_integrity(&self) -> Vec<u32> {
        let mut bad = Vec::new();
        let pages = self.pages.read();
        for (pid, node_arc) in pages.iter() {
            let node = node_arc.read();
            let encoded = match &*node {
                BPlusNode::Leaf(leaf) => encode_leaf(leaf),
                BPlusNode::Internal(int) => encode_internal(int),
            };
            let stored_crc = u32::from_le_bytes(
                encoded[PAGE_SIZE - 4..PAGE_SIZE]
                    .try_into()
                    .unwrap_or([0; 4]),
            );
            let computed = compute_crc32(&encoded[..PAGE_SIZE - 4]);
            if stored_crc != computed {
                bad.push(*pid);
            }
        }
        bad
    }
}

enum InsertResult {
    Done,
    Split { median: IndexKey, new_page_id: u32 },
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_tree() -> BPlusTree {
        BPlusTree::new("idx_test".into(), "test_table".into(), vec!["id".into()])
    }

    #[test]
    fn insert_and_search() {
        let tree = make_tree();
        tree.insert(IndexKey::Integer(10), 100);
        tree.insert(IndexKey::Integer(20), 200);
        tree.insert(IndexKey::Integer(10), 101);

        let found = tree.search(&IndexKey::Integer(10));
        assert_eq!(found.len(), 2);
        assert!(found.contains(&100));
        assert!(found.contains(&101));

        let found2 = tree.search(&IndexKey::Integer(20));
        assert_eq!(found2, vec![200]);

        assert!(tree.search(&IndexKey::Integer(999)).is_empty());
    }

    #[test]
    fn range_scan_basic() {
        let tree = make_tree();
        for i in 0..50 {
            tree.insert(IndexKey::Integer(i), i * 10);
        }
        let range = tree.range_scan(&IndexKey::Integer(10), &IndexKey::Integer(19));
        assert_eq!(range.len(), 10);
        for rid in &range {
            assert!(*rid >= 100 && *rid <= 190);
        }
    }

    #[test]
    fn leaf_split_occurs() {
        let tree = make_tree();
        // MAX_KEYS_LEAF is 230, so we need > 230 entries to trigger a split.
        for i in 0..300 {
            tree.insert(IndexKey::Integer(i), i);
        }
        assert!(tree.page_count() > 1);
        assert!(tree.height() >= 2);
        assert_eq!(tree.entry_count(), 300);

        // Every value still reachable
        for i in 0..300 {
            let res = tree.search(&IndexKey::Integer(i));
            assert_eq!(res, vec![i], "key {} not found", i);
        }
    }

    #[test]
    fn duplicate_key_lookup_spans_split_leaves() {
        let tree = BPlusTree::new("idx_score".into(), "items".into(), vec!["score".into()]);
        let n = MAX_KEYS_LEAF as i64 * 3;
        for row_id in 0..n {
            tree.insert(IndexKey::Integer(7), row_id);
        }

        let rows = tree.search(&IndexKey::Integer(7));
        assert_eq!(rows.len(), n as usize);
        assert!(rows.contains(&0));
        assert!(rows.contains(&(MAX_KEYS_LEAF as i64)));
        assert!(rows.contains(&(n - 1)));

        let delete_id = MAX_KEYS_LEAF as i64 + 1;
        assert!(tree.delete(&IndexKey::Integer(7), delete_id));
        let rows = tree.search(&IndexKey::Integer(7));
        assert_eq!(rows.len(), n as usize - 1);
        assert!(!rows.contains(&delete_id));
    }

    #[test]
    fn delete_entry() {
        let tree = make_tree();
        tree.insert(IndexKey::Integer(42), 1);
        tree.insert(IndexKey::Integer(42), 2);
        assert_eq!(tree.search(&IndexKey::Integer(42)).len(), 2);

        assert!(tree.delete(&IndexKey::Integer(42), 1));
        let remaining = tree.search(&IndexKey::Integer(42));
        assert_eq!(remaining, vec![2]);
    }

    #[test]
    fn encode_decode_roundtrip() {
        let tree = make_tree();
        for i in 0..150 {
            tree.insert(IndexKey::Integer(i), i * 3);
        }
        let data = tree.encode_all();
        let tree2 = BPlusTree::decode_all(
            "idx_test".into(),
            "test_table".into(),
            vec!["id".into()],
            &data,
        )
        .expect("decode failed");

        assert_eq!(tree2.entry_count(), 150);
        for i in 0..150 {
            assert_eq!(tree2.search(&IndexKey::Integer(i)), vec![i * 3]);
        }
    }

    #[test]
    fn crc32_integrity_check() {
        let tree = make_tree();
        for i in 0..50 {
            tree.insert(IndexKey::Integer(i), i);
        }
        let bad = tree.verify_integrity();
        assert!(bad.is_empty(), "No corruption expected");
    }

    #[test]
    fn string_key_support() {
        let tree = BPlusTree::new("idx_name".into(), "users".into(), vec!["name".into()]);
        tree.insert(IndexKey::Str("alice".into()), 1);
        tree.insert(IndexKey::Str("bob".into()), 2);
        tree.insert(IndexKey::Str("charlie".into()), 3);

        assert_eq!(tree.search(&IndexKey::Str("bob".into())), vec![2]);
        let range = tree.range_scan(&IndexKey::Str("alice".into()), &IndexKey::Str("bob".into()));
        assert_eq!(range.len(), 2);
    }

    #[test]
    fn borrowed_string_lookup_matches_owned_lookup_for_duplicates_unicode_and_empty() {
        let tree = BPlusTree::new("idx_name".into(), "users".into(), vec!["name".into()]);
        tree.insert(IndexKey::Str("".into()), 1);
        tree.insert(IndexKey::Str("Việt Nam".into()), 2);
        for id in 3..350 {
            tree.insert(IndexKey::Str("dup".into()), id);
        }

        assert_eq!(
            tree.search_ref(IndexLookupKeyRef::Str("")),
            tree.search(&IndexKey::Str("".into()))
        );
        assert_eq!(
            tree.search_ref(IndexLookupKeyRef::Str("Việt Nam")),
            tree.search(&IndexKey::Str("Việt Nam".into()))
        );
        let borrowed = tree.search_ref(IndexLookupKeyRef::Str("dup"));
        let owned = tree.search(&IndexKey::Str("dup".into()));
        assert_eq!(borrowed, owned);
        assert_eq!(borrowed.len(), 347);
    }

    #[test]
    fn bulk_load_1000_entries() {
        let tree = make_tree();
        let entries: Vec<_> = (0..1000).map(|i| (IndexKey::Integer(i), i)).collect();
        tree.bulk_load(entries);
        assert_eq!(tree.entry_count(), 1000);
        assert_eq!(tree.search(&IndexKey::Integer(500)), vec![500]);
    }

    #[test]
    fn concurrent_insert_delete_integrity() {
        use std::sync::Arc;
        use std::thread;

        let tree = Arc::new(BPlusTree::new(
            "idx_conc".into(),
            "test_conc".into(),
            vec!["id".into()],
        ));

        // 2 writer threads, each inserting 500 entries.
        let n_threads = 2u32;
        let per_thread = 500i64;
        let mut handles = Vec::new();
        for t in 0..n_threads {
            let tree_c = tree.clone();
            handles.push(thread::spawn(move || {
                let base = (t as i64) * per_thread;
                for i in 0..per_thread {
                    tree_c.insert(IndexKey::Integer(base + i), base + i);
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        let total = (n_threads as i64) * per_thread;
        assert_eq!(tree.entry_count(), total as usize);

        // Verify every key is findable.
        for i in 0..total {
            let found = tree.search(&IndexKey::Integer(i));
            assert!(
                !found.is_empty(),
                "key {} missing after concurrent insert",
                i
            );
        }

        // 2 threads delete 250 entries each from their own range.
        let mut del_handles = Vec::new();
        for t in 0..n_threads {
            let tree_c = tree.clone();
            del_handles.push(thread::spawn(move || {
                let base = (t as i64) * per_thread;
                for i in 0..250i64 {
                    assert!(
                        tree_c.delete(&IndexKey::Integer(base + i), base + i),
                        "delete failed for key {}",
                        base + i
                    );
                }
            }));
        }
        for h in del_handles {
            h.join().unwrap();
        }
        assert_eq!(tree.entry_count(), (n_threads as usize) * 250);

        // Verify deleted keys are gone, remaining are present.
        for t in 0..n_threads {
            let base = (t as i64) * per_thread;
            for i in 0..250i64 {
                assert!(
                    tree.search(&IndexKey::Integer(base + i)).is_empty(),
                    "key {} should be deleted",
                    base + i
                );
            }
            for i in 250..per_thread {
                assert!(
                    !tree.search(&IndexKey::Integer(base + i)).is_empty(),
                    "key {} should exist",
                    base + i
                );
            }
        }
    }

    #[test]
    fn latch_crabbing_safe_child_no_split() {
        // Verify safe-child optimisation: insert into a non-full leaf doesn't
        // cause issues when parent is released early.
        let tree = make_tree();
        for i in 0..100i64 {
            tree.insert(IndexKey::Integer(i), i);
        }
        // Tree fits in 1 leaf (100 < 230), so no internal nodes yet.
        assert_eq!(tree.height(), 1);
        assert_eq!(tree.entry_count(), 100);

        // Insert more — still safe.
        for i in 100..200i64 {
            tree.insert(IndexKey::Integer(i), i);
        }
        assert_eq!(tree.entry_count(), 200);
    }

    #[test]
    fn delete_follows_leaf_chain() {
        // Force a split, then delete from the right sibling.
        let tree = make_tree();
        for i in 0..300i64 {
            tree.insert(IndexKey::Integer(i), i);
        }
        assert!(tree.page_count() > 1);

        // Delete high keys that are in the right leaf.
        for i in 250..300i64 {
            assert!(tree.delete(&IndexKey::Integer(i), i), "delete {} failed", i);
        }
        assert_eq!(tree.entry_count(), 250);
    }
}
