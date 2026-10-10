//! Isolated allocation regressions. Count only the measured test thread, not
//! test harness activity or setup, and bound bytes rather than allocator-specific
//! allocation counts.
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use mcp_guest::protocol::{CreateElicitationResult, TaskOrResult, ToolInfo};
use serde_json::json;

struct CountingAllocator;

thread_local! {
    static ALLOCATED: Cell<Option<usize>> = const { Cell::new(None) };
}

fn record(bytes: usize) {
    let _ = ALLOCATED.try_with(|counter| {
        if let Some(total) = counter.get() {
            counter.set(Some(total.saturating_add(bytes)));
        }
    });
}

// SAFETY: All memory operations are delegated unchanged to the system allocator.
// Thread-local accounting does not allocate or access the allocated memory.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        record(layout.size());
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        record(layout.size());
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        record(size);
        unsafe { System.realloc(ptr, layout, size) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

fn measured<T>(f: impl FnOnce() -> T) -> (T, usize) {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            ALLOCATED.with(|counter| counter.set(None));
        }
    }
    ALLOCATED.with(|counter| counter.set(Some(0)));
    let reset = Reset;
    let result = f();
    let bytes = ALLOCATED.with(|counter| match counter.get() {
        Some(bytes) => bytes,
        None => unreachable!("measurement guard keeps accounting enabled"),
    });
    drop(reset);
    (result, bytes)
}

const PAYLOAD_BYTES: usize = 1024 * 1024;

#[test]
fn ui_parsing_does_not_copy_ignored_metadata() {
    let tool: ToolInfo = serde_json::from_value(json!({
        "name": "view",
        "inputSchema": {},
        "_meta": {"ui": {
            "resourceUri": "ui://test/view.html",
            "vendorPayload": "x".repeat(PAYLOAD_BYTES)
        }}
    }))
    .unwrap();
    let (ui, bytes) = measured(|| tool.ui().unwrap().unwrap());
    eprintln!("UI metadata parsing: {bytes} allocated bytes for {PAYLOAD_BYTES} ignored bytes");
    assert_eq!(ui.resource_uri.as_deref(), Some("ui://test/view.html"));
    assert!(
        bytes < PAYLOAD_BYTES / 2,
        "copied ignored payload: {bytes} bytes"
    );
    assert_eq!(
        tool.meta.as_ref().unwrap()["ui"]["vendorPayload"]
            .as_str()
            .unwrap()
            .len(),
        PAYLOAD_BYTES
    );
}

#[test]
fn untagged_response_parsing_does_not_clone_the_json_tree() {
    let value = json!({"action": "decline", "vendorPayload": "x".repeat(PAYLOAD_BYTES)});
    let (result, bytes) = measured(|| {
        serde_json::from_value::<TaskOrResult<CreateElicitationResult>>(value).unwrap()
    });
    eprintln!(
        "Untagged response parsing: {bytes} allocated bytes for {PAYLOAD_BYTES} ignored bytes"
    );
    assert!(result.as_result().is_some());
    assert!(
        bytes < PAYLOAD_BYTES / 2,
        "copied ignored payload: {bytes} bytes"
    );
}
