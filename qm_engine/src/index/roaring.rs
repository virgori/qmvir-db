/*
 * Roaring Bitmap Index
 *
 * Compressed bitmap index using three container types for optimal space/speed:
 *   • ArrayContainer  – sorted u16 array (cardinality ≤ 4096)
 *   • BitmapContainer – fixed 8 KB bitmap (cardinality > 4096)
 *   • RunContainer    – RLE-encoded runs (consecutive ranges)
 *
 * Use cases: analytics, low-cardinality columns, facets, tag filtering.
 *
 * Complexity:
 *   • AND/OR/XOR: O(n) where n = container count
 *   • Contains:    O(log n) array / O(1) bitmap
 *   • Cardinality: O(1) cached
 */

use std::collections::BTreeMap;

// ── Container types ───────────────────────────────────────────────────

const ARRAY_MAX: usize = 4096;

#[derive(Clone, Debug)]
struct Run {
    start: u16,
    length: u16, // inclusive: covers [start, start+length]
}

#[derive(Clone, Debug)]
enum Container {
    Array(Vec<u16>),
    Bitmap(Box<[u64; 1024]>), // 1024 × 64 = 65536 bits = 8 KB
    Run(Vec<Run>),
}

impl Container {
    fn new_array() -> Self {
        Container::Array(Vec::new())
    }

    fn cardinality(&self) -> usize {
        match self {
            Container::Array(v) => v.len(),
            Container::Bitmap(b) => b.iter().map(|w| w.count_ones() as usize).sum(),
            Container::Run(runs) => runs.iter().map(|r| r.length as usize + 1).sum(),
        }
    }

    fn contains(&self, val: u16) -> bool {
        match self {
            Container::Array(v) => v.binary_search(&val).is_ok(),
            Container::Bitmap(b) => {
                let word = val as usize / 64;
                let bit = val as u64 % 64;
                b[word] & (1u64 << bit) != 0
            }
            Container::Run(runs) => runs
                .iter()
                .any(|r| val >= r.start && val <= r.start + r.length),
        }
    }

    fn insert(&mut self, val: u16) -> bool {
        match self {
            Container::Array(v) => match v.binary_search(&val) {
                Ok(_) => false,
                Err(pos) => {
                    v.insert(pos, val);
                    true
                }
            },
            Container::Bitmap(b) => {
                let word = val as usize / 64;
                let bit = val as u64 % 64;
                let was_set = b[word] & (1u64 << bit) != 0;
                b[word] |= 1u64 << bit;
                !was_set
            }
            Container::Run(_) => {
                // Convert to array, insert, then caller may re-optimize
                let mut arr = self.to_array_vec();
                let changed = match arr.binary_search(&val) {
                    Ok(_) => false,
                    Err(pos) => {
                        arr.insert(pos, val);
                        true
                    }
                };
                *self = if arr.len() > ARRAY_MAX {
                    let mut bm = Box::new([0u64; 1024]);
                    for &v in &arr {
                        bm[v as usize / 64] |= 1u64 << (v as u64 % 64);
                    }
                    Container::Bitmap(bm)
                } else {
                    Container::Array(arr)
                };
                changed
            }
        }
    }

    fn remove(&mut self, val: u16) -> bool {
        match self {
            Container::Array(v) => {
                if let Ok(pos) = v.binary_search(&val) {
                    v.remove(pos);
                    true
                } else {
                    false
                }
            }
            Container::Bitmap(b) => {
                let word = val as usize / 64;
                let bit = val as u64 % 64;
                let was_set = b[word] & (1u64 << bit) != 0;
                b[word] &= !(1u64 << bit);
                was_set
            }
            Container::Run(_) => {
                let mut arr = self.to_array_vec();
                let changed = if let Ok(pos) = arr.binary_search(&val) {
                    arr.remove(pos);
                    true
                } else {
                    false
                };
                *self = Container::Array(arr);
                changed
            }
        }
    }

    fn to_array_vec(&self) -> Vec<u16> {
        match self {
            Container::Array(v) => v.clone(),
            Container::Bitmap(b) => {
                let mut out = Vec::new();
                for (i, &word) in b.iter().enumerate() {
                    let mut w = word;
                    while w != 0 {
                        let bit = w.trailing_zeros();
                        out.push((i * 64 + bit as usize) as u16);
                        w &= w - 1;
                    }
                }
                out
            }
            Container::Run(runs) => {
                let mut out = Vec::new();
                for r in runs {
                    for v in r.start..=r.start + r.length {
                        out.push(v);
                    }
                }
                out
            }
        }
    }

    /// Promote Array → Bitmap when cardinality exceeds threshold
    fn maybe_promote(&mut self) {
        if let Container::Array(v) = self {
            if v.len() > ARRAY_MAX {
                let mut bm = Box::new([0u64; 1024]);
                for &val in v.iter() {
                    bm[val as usize / 64] |= 1u64 << (val as u64 % 64);
                }
                *self = Container::Bitmap(bm);
            }
        }
    }

    /// Demote Bitmap → Array when cardinality drops below threshold
    fn maybe_demote(&mut self) {
        if let Container::Bitmap(_) = self {
            if self.cardinality() <= ARRAY_MAX {
                let arr = self.to_array_vec();
                *self = Container::Array(arr);
            }
        }
    }

    // ── Set operations ────────────────────────────────────────────────

    fn and(&self, other: &Container) -> Container {
        let a = self.to_array_vec();
        let b = other.to_array_vec();
        let mut result = Vec::new();
        let (mut i, mut j) = (0, 0);
        while i < a.len() && j < b.len() {
            match a[i].cmp(&b[j]) {
                std::cmp::Ordering::Equal => {
                    result.push(a[i]);
                    i += 1;
                    j += 1;
                }
                std::cmp::Ordering::Less => i += 1,
                std::cmp::Ordering::Greater => j += 1,
            }
        }
        if result.len() > ARRAY_MAX {
            let mut bm = Box::new([0u64; 1024]);
            for &v in &result {
                bm[v as usize / 64] |= 1u64 << (v as u64 % 64);
            }
            Container::Bitmap(bm)
        } else {
            Container::Array(result)
        }
    }

    fn or(&self, other: &Container) -> Container {
        let a = self.to_array_vec();
        let b = other.to_array_vec();
        let mut result = Vec::new();
        let (mut i, mut j) = (0, 0);
        while i < a.len() && j < b.len() {
            match a[i].cmp(&b[j]) {
                std::cmp::Ordering::Equal => {
                    result.push(a[i]);
                    i += 1;
                    j += 1;
                }
                std::cmp::Ordering::Less => {
                    result.push(a[i]);
                    i += 1;
                }
                std::cmp::Ordering::Greater => {
                    result.push(b[j]);
                    j += 1;
                }
            }
        }
        result.extend_from_slice(&a[i..]);
        result.extend_from_slice(&b[j..]);
        if result.len() > ARRAY_MAX {
            let mut bm = Box::new([0u64; 1024]);
            for &v in &result {
                bm[v as usize / 64] |= 1u64 << (v as u64 % 64);
            }
            Container::Bitmap(bm)
        } else {
            Container::Array(result)
        }
    }

    fn xor(&self, other: &Container) -> Container {
        let a = self.to_array_vec();
        let b = other.to_array_vec();
        let mut result = Vec::new();
        let (mut i, mut j) = (0, 0);
        while i < a.len() && j < b.len() {
            match a[i].cmp(&b[j]) {
                std::cmp::Ordering::Equal => {
                    i += 1;
                    j += 1;
                }
                std::cmp::Ordering::Less => {
                    result.push(a[i]);
                    i += 1;
                }
                std::cmp::Ordering::Greater => {
                    result.push(b[j]);
                    j += 1;
                }
            }
        }
        result.extend_from_slice(&a[i..]);
        result.extend_from_slice(&b[j..]);
        if result.len() > ARRAY_MAX {
            let mut bm = Box::new([0u64; 1024]);
            for &v in &result {
                bm[v as usize / 64] |= 1u64 << (v as u64 % 64);
            }
            Container::Bitmap(bm)
        } else {
            Container::Array(result)
        }
    }

    fn and_not(&self, other: &Container) -> Container {
        let a = self.to_array_vec();
        let b = other.to_array_vec();
        let mut result = Vec::new();
        let (mut i, mut j) = (0, 0);
        while i < a.len() && j < b.len() {
            match a[i].cmp(&b[j]) {
                std::cmp::Ordering::Equal => {
                    i += 1;
                    j += 1;
                }
                std::cmp::Ordering::Less => {
                    result.push(a[i]);
                    i += 1;
                }
                std::cmp::Ordering::Greater => {
                    j += 1;
                }
            }
        }
        result.extend_from_slice(&a[i..]);
        Container::Array(result)
    }

    /// Convert to Run-Length Encoding if beneficial
    fn optimize(&mut self) {
        let arr = self.to_array_vec();
        if arr.is_empty() {
            *self = Container::Array(Vec::new());
            return;
        }
        // Build runs
        let mut runs = Vec::new();
        let mut start = arr[0];
        let mut length: u16 = 0;
        for &val in &arr[1..] {
            if val == start + length + 1 {
                length += 1;
            } else {
                runs.push(Run { start, length });
                start = val;
                length = 0;
            }
        }
        runs.push(Run { start, length });

        // Pick smallest representation
        let array_bytes = arr.len() * 2;
        let run_bytes = runs.len() * 4;
        let bitmap_bytes = 8192; // fixed 8 KB

        if run_bytes <= array_bytes && run_bytes <= bitmap_bytes {
            *self = Container::Run(runs);
        } else if arr.len() <= ARRAY_MAX {
            *self = Container::Array(arr);
        } else {
            let mut bm = Box::new([0u64; 1024]);
            for &v in &arr {
                bm[v as usize / 64] |= 1u64 << (v as u64 % 64);
            }
            *self = Container::Bitmap(bm);
        }
    }
}

// ── Roaring Bitmap ──────────────────────────────────────────────────────

/// Roaring Bitmap — compressed bitmap with three container types.
///
/// Values are split into high 16 bits (chunk key) and low 16 bits (container value).
/// Each chunk uses the most space-efficient container type.
#[derive(Clone, Debug)]
pub struct RoaringBitmap {
    /// Map from high-16 chunk key → container
    containers: BTreeMap<u16, Container>,
    /// Cached cardinality
    cached_cardinality: usize,
}

impl RoaringBitmap {
    pub fn new() -> Self {
        Self {
            containers: BTreeMap::new(),
            cached_cardinality: 0,
        }
    }

    /// Insert a value. Returns true if the value was newly inserted.
    pub fn insert(&mut self, val: u32) -> bool {
        let hi = (val >> 16) as u16;
        let lo = val as u16;
        let container = self
            .containers
            .entry(hi)
            .or_insert_with(Container::new_array);
        let inserted = container.insert(lo);
        container.maybe_promote();
        if inserted {
            self.cached_cardinality += 1;
        }
        inserted
    }

    /// Remove a value. Returns true if the value was present.
    pub fn remove(&mut self, val: u32) -> bool {
        let hi = (val >> 16) as u16;
        let lo = val as u16;
        if let Some(container) = self.containers.get_mut(&hi) {
            let removed = container.remove(lo);
            if removed {
                self.cached_cardinality -= 1;
                container.maybe_demote();
                if container.cardinality() == 0 {
                    self.containers.remove(&hi);
                }
            }
            removed
        } else {
            false
        }
    }

    /// Check if a value is present.
    pub fn contains(&self, val: u32) -> bool {
        let hi = (val >> 16) as u16;
        let lo = val as u16;
        self.containers.get(&hi).map_or(false, |c| c.contains(lo))
    }

    /// Total number of set bits.
    pub fn cardinality(&self) -> usize {
        self.cached_cardinality
    }

    /// Return true if empty.
    pub fn is_empty(&self) -> bool {
        self.cached_cardinality == 0
    }

    /// Insert a range [start, end) of values.
    pub fn insert_range(&mut self, start: u32, end: u32) {
        for val in start..end {
            self.insert(val);
        }
    }

    /// Convert all values to a sorted vector.
    pub fn to_vec(&self) -> Vec<u32> {
        let mut result = Vec::with_capacity(self.cached_cardinality);
        for (&hi, container) in &self.containers {
            let base = (hi as u32) << 16;
            for lo in container.to_array_vec() {
                result.push(base | lo as u32);
            }
        }
        result
    }

    /// Iterator over all set values in ascending order.
    pub fn iter(&self) -> impl Iterator<Item = u32> + '_ {
        self.containers.iter().flat_map(|(&hi, container)| {
            let base = (hi as u32) << 16;
            container
                .to_array_vec()
                .into_iter()
                .map(move |lo| base | lo as u32)
        })
    }

    // ── Set operations ──────────────────────────────────────────────

    /// Bitwise AND (intersection)
    pub fn and(&self, other: &RoaringBitmap) -> RoaringBitmap {
        let mut result = RoaringBitmap::new();
        for (&hi, c1) in &self.containers {
            if let Some(c2) = other.containers.get(&hi) {
                let merged = c1.and(c2);
                let card = merged.cardinality();
                if card > 0 {
                    result.cached_cardinality += card;
                    result.containers.insert(hi, merged);
                }
            }
        }
        result
    }

    /// Bitwise OR (union)
    pub fn or(&self, other: &RoaringBitmap) -> RoaringBitmap {
        let mut result = self.clone();
        for (&hi, c2) in &other.containers {
            if let Some(c1) = result.containers.get(&hi) {
                let merged = c1.or(c2);
                let card = merged.cardinality();
                result.cached_cardinality = result.cached_cardinality
                    - result.containers.get(&hi).map_or(0, |c| c.cardinality())
                    + card;
                result.containers.insert(hi, merged);
            } else {
                let card = c2.cardinality();
                result.cached_cardinality += card;
                result.containers.insert(hi, c2.clone());
            }
        }
        result
    }

    /// Bitwise XOR (symmetric difference)
    pub fn xor(&self, other: &RoaringBitmap) -> RoaringBitmap {
        let mut result = RoaringBitmap::new();
        let all_keys: std::collections::BTreeSet<u16> = self
            .containers
            .keys()
            .chain(other.containers.keys())
            .cloned()
            .collect();
        for hi in all_keys {
            let merged = match (self.containers.get(&hi), other.containers.get(&hi)) {
                (Some(c1), Some(c2)) => c1.xor(c2),
                (Some(c), None) | (None, Some(c)) => c.clone(),
                (None, None) => continue,
            };
            let card = merged.cardinality();
            if card > 0 {
                result.cached_cardinality += card;
                result.containers.insert(hi, merged);
            }
        }
        result
    }

    /// AND-NOT (difference: self minus other)
    pub fn and_not(&self, other: &RoaringBitmap) -> RoaringBitmap {
        let mut result = RoaringBitmap::new();
        for (&hi, c1) in &self.containers {
            let merged = if let Some(c2) = other.containers.get(&hi) {
                c1.and_not(c2)
            } else {
                c1.clone()
            };
            let card = merged.cardinality();
            if card > 0 {
                result.cached_cardinality += card;
                result.containers.insert(hi, merged);
            }
        }
        result
    }

    /// Optimize container storage (pick best representation per chunk).
    pub fn optimize(&mut self) {
        self.cached_cardinality = 0;
        for container in self.containers.values_mut() {
            container.optimize();
            self.cached_cardinality += container.cardinality();
        }
    }

    /// Serialized size estimate in bytes.
    pub fn size_in_bytes(&self) -> usize {
        let mut total = 8; // header
        for container in self.containers.values() {
            total += 4; // chunk key + type tag
            match container {
                Container::Array(v) => total += v.len() * 2,
                Container::Bitmap(_) => total += 8192,
                Container::Run(runs) => total += runs.len() * 4,
            }
        }
        total
    }
}

impl Default for RoaringBitmap {
    fn default() -> Self {
        Self::new()
    }
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_insert_contains() {
        let mut bm = RoaringBitmap::new();
        assert!(bm.insert(42));
        assert!(!bm.insert(42)); // duplicate
        assert!(bm.contains(42));
        assert!(!bm.contains(43));
        assert_eq!(bm.cardinality(), 1);
    }

    #[test]
    fn test_remove() {
        let mut bm = RoaringBitmap::new();
        bm.insert(10);
        bm.insert(20);
        assert!(bm.remove(10));
        assert!(!bm.remove(10));
        assert!(!bm.contains(10));
        assert_eq!(bm.cardinality(), 1);
    }

    #[test]
    fn test_high_cardinality_promotion() {
        let mut bm = RoaringBitmap::new();
        for i in 0..5000u32 {
            bm.insert(i);
        }
        assert_eq!(bm.cardinality(), 5000);
        assert!(bm.contains(0));
        assert!(bm.contains(4999));
        assert!(!bm.contains(5000));
    }

    #[test]
    fn test_cross_chunk() {
        let mut bm = RoaringBitmap::new();
        bm.insert(0);
        bm.insert(65536); // second chunk
        bm.insert(131072); // third chunk
        assert_eq!(bm.cardinality(), 3);
        assert_eq!(bm.containers.len(), 3);
    }

    #[test]
    fn test_and_or_xor() {
        let mut a = RoaringBitmap::new();
        let mut b = RoaringBitmap::new();
        for i in 0..100u32 {
            a.insert(i);
        }
        for i in 50..150u32 {
            b.insert(i);
        }

        let intersection = a.and(&b);
        assert_eq!(intersection.cardinality(), 50); // 50..100

        let union = a.or(&b);
        assert_eq!(union.cardinality(), 150); // 0..150

        let xor = a.xor(&b);
        assert_eq!(xor.cardinality(), 100); // 0..50 + 100..150

        let diff = a.and_not(&b);
        assert_eq!(diff.cardinality(), 50); // 0..50
    }

    #[test]
    fn test_optimize_run_encoding() {
        let mut bm = RoaringBitmap::new();
        for i in 0..1000u32 {
            bm.insert(i);
        }
        bm.optimize();
        assert_eq!(bm.cardinality(), 1000);
        // After optimization, should use Run container for consecutive range
        for (&_hi, container) in &bm.containers {
            assert!(matches!(container, Container::Run(_)));
        }
    }

    #[test]
    fn test_to_vec() {
        let mut bm = RoaringBitmap::new();
        bm.insert(5);
        bm.insert(3);
        bm.insert(100);
        bm.insert(1);
        let v = bm.to_vec();
        assert_eq!(v, vec![1, 3, 5, 100]);
    }
}
