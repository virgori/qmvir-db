//! Query cancellation registry (PostgreSQL CancelRequest backend).

use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

static REGISTRY: OnceLock<CancelRegistry> = OnceLock::new();

pub fn global_cancel_registry() -> &'static CancelRegistry {
    REGISTRY.get_or_init(CancelRegistry::default)
}

#[derive(Debug)]
pub struct CancelHandle {
    cancelled: Arc<AtomicBool>,
    process_id: i32,
    secret_key: i32,
}

impl CancelHandle {
    pub fn register(process_id: i32, secret_key: i32) -> Self {
        let cancelled = Arc::new(AtomicBool::new(false));
        global_cancel_registry()
            .sessions
            .lock()
            .insert((process_id, secret_key), Arc::clone(&cancelled));
        CancelHandle {
            cancelled,
            process_id,
            secret_key,
        }
    }

    pub fn flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.cancelled)
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
}

impl Drop for CancelHandle {
    fn drop(&mut self) {
        global_cancel_registry().unregister(self.process_id, self.secret_key);
    }
}

#[derive(Debug, Default)]
pub struct CancelRegistry {
    sessions: Mutex<HashMap<(i32, i32), Arc<AtomicBool>>>,
}

impl CancelRegistry {
    pub fn cancel(process_id: i32, secret_key: i32) -> bool {
        let sessions = global_cancel_registry().sessions.lock();
        if let Some(flag) = sessions.get(&(process_id, secret_key)) {
            flag.store(true, Ordering::Release);
            true
        } else {
            false
        }
    }

    pub fn unregister(&self, process_id: i32, secret_key: i32) {
        self.sessions.lock().remove(&(process_id, secret_key));
    }
}
