//! Per-connection NativeSqlEngine sessions for isolated transactions.

use super::native_sql::NativeSqlEngine;
use dashmap::DashMap;
use std::sync::{Arc, OnceLock};

static POOL: OnceLock<SessionPool> = OnceLock::new();

pub fn global_session_pool() -> &'static SessionPool {
    POOL.get_or_init(SessionPool::default)
}

#[derive(Default)]
pub struct SessionPool {
    sessions: DashMap<u64, NativeSqlEngine>,
}

impl SessionPool {
    pub fn session_for_connection(&self, conn_id: u64, base: &Arc<NativeSqlEngine>) -> NativeSqlEngine {
        self.sessions
            .entry(conn_id)
            .or_insert_with(|| base.new_session())
            .clone()
    }

    pub fn remove_connection(&self, conn_id: u64) {
        self.sessions.remove(&conn_id);
    }
}
