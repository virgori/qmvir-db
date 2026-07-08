//! Per-table concurrent storage — avoids global catalog lock on hot read paths.

use super::native_sql::NativeTable;
use dashmap::DashMap;
use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::Arc;

pub type SharedTable = Arc<RwLock<NativeTable>>;

/// Snapshot write guard — commits the full map on drop (legacy HashMap write API).
pub struct TableStoreWriteGuard<'a> {
    store: &'a TableStore,
    map: HashMap<String, NativeTable>,
}

impl<'a> std::ops::Deref for TableStoreWriteGuard<'a> {
    type Target = HashMap<String, NativeTable>;

    fn deref(&self) -> &Self::Target {
        &self.map
    }
}

impl<'a> std::ops::DerefMut for TableStoreWriteGuard<'a> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.map
    }
}

impl Drop for TableStoreWriteGuard<'_> {
    fn drop(&mut self) {
        let map = std::mem::take(&mut self.map);
        self.store.replace_all(map);
    }
}

#[derive(Debug)]
pub struct TableStore {
    map: DashMap<String, SharedTable>,
}

impl Default for TableStore {
    fn default() -> Self {
        Self::new()
    }
}

impl TableStore {
    pub fn new() -> Self {
        Self {
            map: DashMap::new(),
        }
    }

    pub fn from_native_map(tables: HashMap<String, NativeTable>) -> Self {
        let store = Self::new();
        store.replace_all(tables);
        store
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    pub fn contains_key(&self, table: &str) -> bool {
        self.map.contains_key(table)
    }

    pub fn get_shared(&self, table: &str) -> Option<SharedTable> {
        self.map.get(table).map(|e| Arc::clone(e.value()))
    }

    pub fn insert_native(&self, name: String, table: NativeTable) {
        self.map.insert(name, Arc::new(RwLock::new(table)));
    }

    pub fn remove(&self, name: &str) -> Option<SharedTable> {
        self.map.remove(name).map(|(_, v)| v)
    }

    pub fn replace_all(&self, tables: HashMap<String, NativeTable>) {
        self.map.clear();
        for (name, table) in tables {
            self.insert_native(name, table);
        }
    }

    /// Run a closure against a full snapshot; persists changes atomically.
    pub fn update_all<R>(&self, f: impl FnOnce(&mut HashMap<String, NativeTable>) -> R) -> R {
        let mut map = self.to_native_map();
        let result = f(&mut map);
        self.replace_all(map);
        result
    }

    pub fn to_native_map(&self) -> HashMap<String, NativeTable> {
        let mut out = HashMap::with_capacity(self.map.len());
        for entry in self.map.iter() {
            out.insert(entry.key().clone(), entry.value().read().clone());
        }
        out
    }

    /// Clone only named tables (one lock at a time) — preferred for checkpoint.
    pub fn clone_named(&self, names: &[String]) -> HashMap<String, NativeTable> {
        let mut out = HashMap::with_capacity(names.len());
        for name in names {
            if let Some(shared) = self.get_shared(name) {
                out.insert(name.clone(), shared.read().clone());
            }
        }
        out
    }

    /// Best-effort clone that never blocks DML — returns None if any table is
    /// write-locked or too large to clone under a shared lock (would stall writers).
    pub fn try_clone_named(&self, names: &[String]) -> Option<HashMap<String, NativeTable>> {
        const MAX_ROWS_UNDER_SHARED_LOCK: usize = 2_000;
        let mut out = HashMap::with_capacity(names.len());
        for name in names {
            let shared = self.get_shared(name)?;
            let guard = shared.try_read()?;
            if guard.rows.len() > MAX_ROWS_UNDER_SHARED_LOCK {
                return None;
            }
            out.insert(name.clone(), guard.clone());
        }
        Some(out)
    }

    pub fn row_count(&self, table: &str) -> Option<usize> {
        self.get_shared(table)
            .map(|shared| shared.read().rows.len())
    }

    pub fn with_read_opt<R>(&self, table: &str, f: impl FnOnce(&NativeTable) -> R) -> Option<R> {
        let shared = self.get_shared(table)?;
        let guard = shared.read();
        Some(f(&guard))
    }

    /// Snapshot read (legacy — clones every table; avoid on hot paths).
    #[deprecated(note = "use with_read / lock_table_read / row_count instead")]
    pub fn read(&self) -> HashMap<String, NativeTable> {
        self.to_native_map()
    }

    /// Snapshot write (legacy HashMap write API); commits on drop.
    pub fn write(&self) -> TableStoreWriteGuard<'_> {
        TableStoreWriteGuard {
            store: self,
            map: self.to_native_map(),
        }
    }

    pub fn table_names(&self) -> Vec<String> {
        self.map.iter().map(|e| e.key().clone()).collect()
    }

    pub fn with_read<R>(
        &self,
        table: &str,
        f: impl FnOnce(&NativeTable) -> R,
    ) -> Result<R, String> {
        let shared = self
            .get_shared(table)
            .ok_or_else(|| format!("table \"{}\" does not exist", table))?;
        let guard = shared.read();
        Ok(f(&guard))
    }

    pub fn with_write<R>(
        &self,
        table: &str,
        f: impl FnOnce(&mut NativeTable) -> R,
    ) -> Result<R, String> {
        let shared = self
            .get_shared(table)
            .ok_or_else(|| format!("table \"{}\" does not exist", table))?;
        let mut guard = shared.write();
        Ok(f(&mut guard))
    }

    pub fn with_write_or_create<R>(
        &self,
        table: &str,
        create: impl FnOnce() -> NativeTable,
        f: impl FnOnce(&mut NativeTable) -> R,
    ) -> R {
        let shared = self
            .map
            .entry(table.to_string())
            .or_insert_with(|| Arc::new(RwLock::new(create())))
            .clone();
        let mut guard = shared.write();
        f(&mut guard)
    }

    /// Clone Arc then release map shard lock before taking per-table read lock.
    pub fn lock_table_read(&self, table: &str) -> Result<SharedTable, String> {
        self.get_shared(table)
            .ok_or_else(|| format!("table \"{}\" does not exist", table))
    }

    pub fn lock_table_write(&self, table: &str) -> Result<SharedTable, String> {
        self.get_shared(table)
            .ok_or_else(|| format!("table \"{}\" does not exist", table))
    }

    pub fn for_each_read<F>(&self, mut f: F)
    where
        F: FnMut(&str, &NativeTable),
    {
        for entry in self.map.iter() {
            let guard = entry.value().read();
            f(entry.key(), &guard);
        }
    }

    pub fn for_each_read_collect<T, F>(&self, mut f: F) -> Vec<T>
    where
        F: FnMut(&str, &NativeTable) -> T,
    {
        let mut out = Vec::with_capacity(self.map.len());
        for entry in self.map.iter() {
            let guard = entry.value().read();
            out.push(f(entry.key(), &guard));
        }
        out
    }
}
