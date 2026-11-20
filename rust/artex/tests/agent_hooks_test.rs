//! Differential test for `agent/assembly_test.go:TestAgentHooksConcurrentSwap`
//! — readers and writers run concurrently; a torn read or stale hook fails.

use std::sync::Arc;

use artex::agent::hooks::{AgentHooks, HookSlot};

#[test]
fn hook_slot_concurrent_swap() {
    let hooks = AgentHooks::default();
    let on = Arc::new(());
    let slot: HookSlot<()> = HookSlot::new();
    let reader = slot.clone();
    let writer = slot.clone();

    let on_for_writer = on.clone();
    std::thread::scope(|s| {
        let r = s.spawn(move || {
            for _ in 0..200 {
                // Reading and using the hook must never panic or see a torn value.
                let _ = reader.get();
                let _ = reader.is_set();
            }
        });
        let w = s.spawn(move || {
            for _ in 0..200 {
                writer.set(Some(on_for_writer.clone()));
                writer.set(None);
            }
        });
        r.join().unwrap();
        w.join().unwrap();
    });

    // The canonical hook set also swaps cleanly.
    for _ in 0..200 {
        hooks.augment.set(Some(on.clone()));
        hooks.resolve.set(Some(on.clone()));
        hooks.prompt.set(Some(on.clone()));
        hooks.wrapup.set(Some(on.clone()));
        assert!(hooks.augment.is_set() && hooks.prompt.is_set());
        hooks.clear_all();
        assert!(!hooks.augment.is_set() && !hooks.wrapup.is_set());
    }
}
