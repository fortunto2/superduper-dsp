//! Lock-free vocoder-activity snapshot shared audio-thread → GUI.
//!
//! The audio thread writes the current vocoding state once per block; the GUI
//! samples it at ~30–60 Hz to paint the activity display. Same philosophy as
//! `core_gui::LiveScope`: an array of `AtomicF32` slots plus small atomic
//! scalars, no locks, no allocation on either side (all slots pre-allocated).
//!
//! Two payloads, one live at a time depending on `Mode`:
//! - **Classic** — `bars`: per-band envelope levels (first `active` used), the
//!   iconic bouncing hardware-vocoder columns.
//! - **Spectral** — `curve`: the modulator's formant envelope, log-frequency
//!   resampled, that shapes the carrier.

use atomic_float::AtomicF32;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

use crate::dsp::{MAX_BANDS, MODE_CLASSIC, MODE_SPECTRAL};

/// Resolution of the Spectral formant-envelope curve.
pub const VIZ_CURVE: usize = 96;

pub struct VocViz {
    /// Which payload is live — `MODE_CLASSIC` (bars) or `MODE_SPECTRAL` (curve).
    mode: AtomicU32,
    /// Active band count for the Classic bars.
    active: AtomicUsize,
    bars: [AtomicF32; MAX_BANDS],
    curve: [AtomicF32; VIZ_CURVE],
}

impl Default for VocViz {
    fn default() -> Self {
        Self::new()
    }
}

impl VocViz {
    pub fn new() -> Self {
        Self {
            mode: AtomicU32::new(MODE_CLASSIC),
            active: AtomicUsize::new(16),
            bars: std::array::from_fn(|_| AtomicF32::new(0.0)),
            curve: std::array::from_fn(|_| AtomicF32::new(0.0)),
        }
    }

    // ---- audio thread (writer) --------------------------------------------

    /// Publish the Classic per-band activity meter (`bars[..active]`).
    pub fn write_bars(&self, bars: &[f32], active: usize) {
        let active = active.min(MAX_BANDS);
        for (i, slot) in self.bars.iter().enumerate() {
            slot.store(if i < active { bars[i] } else { 0.0 }, Ordering::Relaxed);
        }
        self.active.store(active, Ordering::Relaxed);
        self.mode.store(MODE_CLASSIC, Ordering::Relaxed);
    }

    /// Publish the Spectral formant-envelope curve.
    pub fn write_curve(&self, curve: &[f32]) {
        for (slot, &v) in self.curve.iter().zip(curve.iter()) {
            slot.store(v, Ordering::Relaxed);
        }
        self.mode.store(MODE_SPECTRAL, Ordering::Relaxed);
    }

    // ---- GUI (reader) ------------------------------------------------------

    pub fn mode(&self) -> u32 {
        self.mode.load(Ordering::Relaxed)
    }

    /// Read the Classic bars into `out`; returns the active band count.
    pub fn read_bars(&self, out: &mut [f32; MAX_BANDS]) -> usize {
        for (o, slot) in out.iter_mut().zip(self.bars.iter()) {
            *o = slot.load(Ordering::Relaxed);
        }
        self.active.load(Ordering::Relaxed)
    }

    /// Read the Spectral formant curve into `out`.
    pub fn read_curve(&self, out: &mut [f32; VIZ_CURVE]) {
        for (o, slot) in out.iter_mut().zip(self.curve.iter()) {
            *o = slot.load(Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bars_roundtrip_and_zero_inactive() {
        let v = VocViz::new();
        let mut src = [0.0f32; MAX_BANDS];
        for (i, s) in src.iter_mut().enumerate() {
            *s = i as f32 + 1.0;
        }
        v.write_bars(&src, 11);
        assert_eq!(v.mode(), MODE_CLASSIC);
        let mut out = [0.0f32; MAX_BANDS];
        let active = v.read_bars(&mut out);
        assert_eq!(active, 11);
        assert_eq!(out[10], 11.0);
        // Slots past the active count are cleared, not stale.
        assert_eq!(out[11], 0.0);
    }

    #[test]
    fn curve_roundtrip_sets_spectral_mode() {
        let v = VocViz::new();
        let mut src = [0.0f32; VIZ_CURVE];
        src[42] = 3.14;
        v.write_curve(&src);
        assert_eq!(v.mode(), MODE_SPECTRAL);
        let mut out = [0.0f32; VIZ_CURVE];
        v.read_curve(&mut out);
        assert_eq!(out[42], 3.14);
    }
}
