//! An allocator that counts what happens inside `process()`.
//!
//! `sdk/tests/rt_safety.rs` greps the text of each `process()` body. That
//! catches `vec![` written there and nothing else — not an allocation inside a
//! function it calls, which is where the real ones hid: NAM cloned a whole
//! WaveNet inside a helper, LinEQ called `build_fir`, the Looper's `vec!`s were
//! in `render_mono`. A lexical test would have missed all three.
//!
//! This one counts for real. The plugin's `process()` is bracketed by
//! `enter_rt` / `exit_rt`, and every allocation, reallocation and free in
//! between is tallied. Host-side buffer juggling happens outside the bracket
//! and is not counted.
//!
//! Each test binary opts in:
//!
//! ```ignore
//! #[global_allocator]
//! static ALLOC: sdsp_test_kit::alloc::CountingAllocator =
//!     sdsp_test_kit::alloc::CountingAllocator;
//! ```
//!
//! Without that line the counters stay at zero and the check silently passes,
//! so `assert_rt_clean` also fails when it sees no activity at all where some
//! was expected.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

// Thread-local, not global: cargo runs the tests in a binary concurrently, so a
// global flag made this count allocations belonging to whichever other test
// happened to be running — 16 phantom allocations for a plugin that makes none.
thread_local! {
    static IN_RT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}
static ALLOCS: AtomicUsize = AtomicUsize::new(0);
static FREES: AtomicUsize = AtomicUsize::new(0);
static BYTES: AtomicUsize = AtomicUsize::new(0);
/// Proves the allocator is actually installed — see the note above.
static TOTAL_SEEN: AtomicUsize = AtomicUsize::new(0);
/// Print a backtrace for the first few RT allocations — call `set_trace(true)`.
static TRACE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Turn backtraces on while hunting an allocation.
pub fn set_trace(on: bool) {
    TRACE.store(on, Ordering::Relaxed);
}

pub struct CountingAllocator;

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        TOTAL_SEEN.fetch_add(1, Ordering::Relaxed);
        if IN_RT.with(|f| f.get()) {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
            BYTES.fetch_add(layout.size(), Ordering::Relaxed);
            // SDSP_ALLOC_TRACE=1 prints where the first few came from. Printing
            // allocates, so the flag is cleared around it — otherwise the
            // allocator recurses into itself.
            // A plain AtomicBool, not an env lookup: std::env::var_os
            // allocates, so checking it here re-entered the allocator.
            if TRACE.load(Ordering::Relaxed) && ALLOCS.load(Ordering::Relaxed) <= 3 {
                IN_RT.with(|f| f.set(false));
                eprintln!(
                    "\n=== allocation of {} bytes inside process() ===\n{}",
                    layout.size(),
                    std::backtrace::Backtrace::force_capture()
                );
                IN_RT.with(|f| f.set(true));
            }
        }
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if IN_RT.with(|f| f.get()) {
            FREES.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        TOTAL_SEEN.fetch_add(1, Ordering::Relaxed);
        if IN_RT.with(|f| f.get()) {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
            BYTES.fetch_add(new_size, Ordering::Relaxed);
        }
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

/// Start counting. Called by the harness around the plugin's process().
pub fn enter_rt() {
    IN_RT.with(|f| f.set(true));
}

pub fn exit_rt() {
    IN_RT.with(|f| f.set(false));
}

pub fn reset() {
    ALLOCS.store(0, Ordering::SeqCst);
    FREES.store(0, Ordering::SeqCst);
    BYTES.store(0, Ordering::SeqCst);
}

/// (allocations, frees, bytes) seen inside process() since the last reset.
pub fn counts() -> (usize, usize, usize) {
    (
        ALLOCS.load(Ordering::SeqCst),
        FREES.load(Ordering::SeqCst),
        BYTES.load(Ordering::SeqCst),
    )
}

/// Fail if the plugin allocated on the audio thread.
///
/// `blocks` is only used for the message — "12 allocations over 40 blocks" is
/// easier to act on than a bare count.
pub fn assert_rt_clean(plugin: &str, blocks: usize) {
    assert!(
        TOTAL_SEEN.load(Ordering::Relaxed) > 0,
        "{plugin}: the counting allocator never saw a single allocation, which means it is \
         not installed. Add to this test file:\n\n    #[global_allocator]\n    static ALLOC: \
         sdsp_test_kit::alloc::CountingAllocator = sdsp_test_kit::alloc::CountingAllocator;\n"
    );
    let (allocs, frees, bytes) = counts();
    assert!(
        allocs == 0 && frees == 0,
        "{plugin} allocated on the audio thread: {allocs} allocations ({bytes} bytes) and \
         {frees} frees across {blocks} process() calls.\n\
         Pre-allocate in activate() instead — see lesson 11 in CLAUDE.md. If the allocation \
         is in a helper the lexical rt_safety test cannot see, this is exactly the case it \
         exists to catch."
    );
}
