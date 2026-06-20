//! Denormal-number protection for the audio thread.
//!
//! When a float reaches the denormal range (≈10⁻³⁸ for `f32`) CPUs
//! drop to a slow microcode path that can take 50-100× longer than a
//! normal multiply. Filter envelopes decaying to silence and feedback
//! loops in long reverbs are the usual culprits — they keep producing
//! exponentially smaller numbers forever instead of clamping to zero.
//!
//! On a real-time audio thread that means: process() suddenly takes
//! 10× longer than the buffer period, REAPER / Ableton can't fill the
//! audio device in time, you hear a **periodic tick** at the buffer
//! rate (~10 ms at 512 frames @ 48 kHz). Symptom is "ритмичные щелчки
//! одинакового тона" — exactly what the user reported.
//!
//! Fix: tell the CPU to **flush denormals to zero** ("FTZ" / "DAZ"
//! flags) at the top of every `process()` call. The flag is a global
//! per-thread mode, so we save / restore it via the RAII guard so we
//! never leak the mode change into the host's other audio plugins.
//!
//! Usage in a plugin's `process()`:
//! ```ignore
//! fn process(&mut self, ...) -> Result<...> {
//!     let _denormals = superduper_dsp_sdk::denormals::Guard::new();
//!     // …rest of the audio path…
//! }
//! ```
//!
//! The Drop impl restores whatever the host had set when our plugin
//! was called, so we leave no global state behind on return.

#[cfg(target_arch = "aarch64")]
mod arch {
    /// FPCR bit 24 = FZ (flush-to-zero, single + double precision).
    const FZ_BIT: u64 = 1 << 24;

    #[inline(always)]
    pub unsafe fn read() -> u64 {
        let mut v: u64;
        core::arch::asm!("mrs {0}, fpcr", out(reg) v, options(nomem, nostack));
        v
    }

    #[inline(always)]
    pub unsafe fn write(v: u64) {
        core::arch::asm!("msr fpcr, {0}", in(reg) v, options(nomem, nostack));
    }

    #[inline(always)]
    pub unsafe fn enable_ftz() -> u64 {
        let prev = read();
        write(prev | FZ_BIT);
        prev
    }
}

#[cfg(target_arch = "x86_64")]
mod arch {
    use core::arch::x86_64::{_mm_getcsr, _mm_setcsr};

    /// MXCSR bit 15 = FTZ (flush-to-zero on output).
    /// MXCSR bit  6 = DAZ (denormals-are-zero on input).
    const FTZ_BIT: u32 = 1 << 15;
    const DAZ_BIT: u32 = 1 << 6;

    #[inline(always)]
    pub unsafe fn read() -> u32 {
        _mm_getcsr()
    }

    #[inline(always)]
    pub unsafe fn write(v: u32) {
        _mm_setcsr(v);
    }

    #[inline(always)]
    pub unsafe fn enable_ftz() -> u32 {
        let prev = read();
        write(prev | FTZ_BIT | DAZ_BIT);
        prev
    }
}

#[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
mod arch {
    // No-op fallback for architectures we don't have intrinsics for —
    // denormals will still slow down audio but at least the plugin
    // builds.
    pub type Csr = u32;
    pub unsafe fn read() -> Csr { 0 }
    pub unsafe fn write(_v: Csr) {}
    pub unsafe fn enable_ftz() -> Csr { 0 }
}

/// RAII guard that flips the audio thread into flush-to-zero mode on
/// construction and restores the host's previous CSR/FPCR on drop.
/// Construct ONCE at the top of `process()`.
pub struct Guard {
    #[cfg(target_arch = "aarch64")]
    prev: u64,
    #[cfg(target_arch = "x86_64")]
    prev: u32,
    #[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
    prev: u32,
}

impl Guard {
    #[inline(always)]
    pub fn new() -> Self {
        // SAFETY: read/write MXCSR / FPCR are CPU register accesses
        // with no memory effect — safe on every supported target.
        let prev = unsafe { arch::enable_ftz() };
        Self { prev }
    }
}

impl Default for Guard {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for Guard {
    #[inline(always)]
    fn drop(&mut self) {
        // SAFETY: same as `new()`.
        unsafe { arch::write(self.prev) };
    }
}
