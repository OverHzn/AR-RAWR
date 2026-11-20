//! Agent hook registry — concurrency-safe hook slots used by tool assembly,
//! prompt rendering and settlement wiring (`agent/assembly.go`, `agent/prompt.go`).
//!
//! Server wiring installs hooks while restored tasks may already read them on
//! other goroutines, so every access goes through a shared RwLock.

use std::sync::{Arc, RwLock};

/// A replaceable hook. Cloning shares the same slot (like Go's package globals).
#[derive(Debug)]
pub struct HookSlot<T> {
    inner: Arc<RwLock<Option<Arc<T>>>>,
}

impl<T> Clone for HookSlot<T> {
    fn clone(&self) -> Self {
        HookSlot {
            inner: self.inner.clone(),
        }
    }
}

impl<T> Default for HookSlot<T> {
    fn default() -> Self {
        HookSlot {
            inner: Arc::new(RwLock::new(None)),
        }
    }
}

impl<T> HookSlot<T> {
    pub fn new() -> Self {
        Self::default()
    }

    /// Install (or clear with `None`) the hook.
    pub fn set(&self, hook: Option<Arc<T>>) {
        *self.inner.write().unwrap() = hook;
    }

    /// Current hook, cloned out so callers never hold the lock while using it.
    pub fn get(&self) -> Option<Arc<T>> {
        self.inner.read().unwrap().clone()
    }

    pub fn is_set(&self) -> bool {
        self.inner.read().unwrap().is_some()
    }
}

/// The canonical hook set used by the agent.
#[derive(Debug, Clone, Default)]
pub struct AgentHooks {
    pub augment: HookSlot<()>,
    pub resolve: HookSlot<()>,
    pub prompt: HookSlot<()>,
    pub wrapup: HookSlot<()>,
}

impl AgentHooks {
    /// Clear every slot (used between tests / on server reconfigure).
    pub fn clear_all(&self) {
        self.augment.set(None);
        self.resolve.set(None);
        self.prompt.set(None);
        self.wrapup.set(None);
    }
}
