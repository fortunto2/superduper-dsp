//! The bus key on the song timeline. Every plugin's audio thread turns what
//! it plays into 32-band frames stamped with the song time
//! (`superduper_dsp_sdk::bus_frames`); a plugin keyed on another track takes,
//! at the start of each block, that track's frame for the block's song time.
//!
//! Before this the key went through the publisher thread on the wall clock
//! (~20×/s), so a render faster than real time keyed on whatever the other
//! track had played when the thread woke: of two full-speed REAPER renders
//! of one project one did not duck at all and one ducked −11 dB through the
//! whole song, where a 1x render ducked −11.3 dB on the key and 0.0 off it
//! (2026-10-22). With frames all five renders, full speed and 1x, came out
//! bit-identical: −11.3 dB on the key, 0.0 off, onset 20–30 ms (was 40–120
//! ms jittering in 1x).
//!
//! One rule for playback and render, so a render sounds like playback: the
//! key for a block is the newest frame whose last sample is at or before the
//! block's start. In real time the other track usually has that frame
//! already; when it does not, the block takes the one before (a block
//! later, never a wait on the audio callback). Faster than real time nobody
//! is listening, so a block waits for a key track that is behind. It stops
//! waiting when that track sits one block behind and makes no progress (the
//! host runs both on one thread, the key track second: its frame for this
//! block cannot appear until this block returns), or makes none at all for
//! 100 ms (not played); any later progress turns waiting back on.
//!
//! Without a transport (stopped, monitoring) or with a key track whose
//! plugin predates frames, the publisher thread's wall-clock copy stays in
//! charge, as before.

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use clack_plugin::events::event_types::{TransportEvent, TransportFlags};
use realfft::num_complex::Complex;
use realfft::{RealFftPlanner, RealToComplex};
use superduper_dsp_sdk::bus::{band_edges, BANDS};
use superduper_dsp_sdk::bus_frames::{Frame, Frames};

use crate::gui::LiveScope;

/// Analysis window (43 ms at 48 kHz). Shorter than the 4096 the wall-clock
/// spectrum uses: half the time smear, and band levels add up to the same
/// total either way (band RMS, Parseval).
pub const WINDOW: usize = 2048;
/// One frame per hop (5.3 ms at 48 kHz).
pub const HOP: usize = 256;
/// Samples the scope ring must hold so a block of up to this many samples
/// still has every hop's window when the block ends.
pub const MIN_RING: usize = 16_384;
/// A frame within this of the block start is the block's frame: two hops at
/// 44.1 kHz covers any producer rate and block size.
const GOOD_S: f64 = 2.0 * HOP as f64 / 44_100.0;
/// The key track's newest frame older than this means it stopped playing.
const STALE_S: f64 = 0.5;
/// Producers analyse only while a reader asked within this.
const WANTED_MS: u64 = 3000;
/// Offline: a key track one block behind that makes no progress for STALL
/// (long enough that an OS preemption of its render thread is not mistaken
/// for it: 10 ms misfired about one run in six under `cargo test`)
/// runs after us on our thread; one that makes none for WAIT_MAX is not
/// being played at all (muted, or not rendered). Either way stop waiting —
/// until it moves again.
const STALL: Duration = Duration::from_millis(40);
const WAIT_MAX: Duration = Duration::from_millis(100);
/// Blocks arriving in under this fraction of their own length, this many
/// in a row, mean the host is rendering faster than real time.
const FAST_RATIO: f64 = 0.5;
const FAST_BLOCKS: u32 = 3;

pub struct Timeline {
    frames: OnceLock<Frames>,
    own_slot: AtomicUsize,
    own: AtomicU64,
    key_slot: AtomicUsize,
    key_owner: AtomicU64,
    clock_ms: AtomicU64,
    wanted: AtomicBool,
    key_blocks: AtomicU64,
    state: parking_lot::Mutex<State>,
}

struct State {
    fft: Arc<dyn RealToComplex<f32>>,
    x: Vec<f32>,
    spec: Vec<Complex<f32>>,
    scratch: Vec<Complex<f32>>,
    win: Vec<f32>,
    /// Scope head up to which frames are done.
    flushed: usize,
    /// Song time and scope head at the current block's start.
    t0: f64,
    h0: usize,
    last_call: Option<Instant>,
    /// Length of the last block (s), from what it pushed.
    block_s: f64,
    fast: u32,
    serial: bool,
    absent: bool,
    /// The key track's frame count when last looked at.
    key_seen: u64,
}

impl Default for Timeline {
    fn default() -> Self { Self::new() }
}

impl Timeline {
    /// Plans the FFT: allocates, so main thread (plugin construction).
    pub fn new() -> Self {
        let fft = RealFftPlanner::<f32>::new().plan_fft_forward(WINDOW);
        let win = (0..WINDOW).map(|i| 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / (WINDOW - 1) as f32).cos()).collect();
        let state = State {
            x: fft.make_input_vec(),
            spec: fft.make_output_vec(),
            scratch: fft.make_scratch_vec(),
            fft,
            win,
            flushed: 0,
            t0: f64::NAN,
            h0: 0,
            last_call: None,
            block_s: 0.0,
            fast: 0,
            serial: false,
            absent: false,
            key_seen: 0,
        };
        Self {
            frames: OnceLock::new(),
            own_slot: AtomicUsize::new(0),
            own: AtomicU64::new(0),
            key_slot: AtomicUsize::new(0),
            key_owner: AtomicU64::new(0),
            clock_ms: AtomicU64::new(0),
            wanted: AtomicBool::new(false),
            key_blocks: AtomicU64::new(0),
            state: parking_lot::Mutex::new(state),
        }
    }

    // --- publisher thread -------------------------------------------------

    /// Hand the frames file and this instance's bus slot to the audio thread.
    pub fn attach(&self, frames: Frames, slot: usize, owner: u64) {
        frames.claim(slot, owner);
        if self.frames.set(frames).is_ok() {
            self.own_slot.store(slot, Ordering::Relaxed);
            self.own.store(owner, Ordering::Release);
        }
    }

    /// Once per publisher tick: the coarse clock readers stamp their
    /// requests with, and whether anyone is keyed on this instance.
    pub fn tick(&self, now_ms: u64) {
        self.clock_ms.store(now_ms, Ordering::Relaxed);
        if let Some(fr) = self.frames.get() {
            let asked = fr.wanted_ms(self.own_slot.load(Ordering::Relaxed));
            self.wanted.store(now_ms.saturating_sub(asked) < WANTED_MS, Ordering::Relaxed);
        }
    }

    /// The key track's instance (bus slot + id), or None for no key.
    pub fn set_key_source(&self, src: Option<(usize, u64)>) {
        let (slot, owner) = src.unwrap_or((0, 0));
        self.key_slot.store(slot, Ordering::Relaxed);
        self.key_owner.store(owner, Ordering::Release);
    }

    /// Counts blocks whose key came from frames. While it moves, the audio
    /// thread owns `key_bands` and the publisher thread leaves them alone.
    pub fn key_blocks(&self) -> u64 { self.key_blocks.load(Ordering::Relaxed) }

    // --- audio thread -----------------------------------------------------

    /// Start of `process()`: frames for what the last block pushed, the new
    /// block's song time, and the key for this block. Atomics plus an
    /// uncontended `try_lock`; allocation-free.
    pub fn begin_block(&self, scope: &LiveScope, transport: Option<&TransportEvent>) {
        let Some(mut st) = self.state.try_lock() else { return };
        let head = scope.head.load(Ordering::Relaxed);
        let sr = scope.sample_rate.load(Ordering::Relaxed).max(8000.0) as f64;
        self.flush(scope, &mut st, head, sr);

        let now = Instant::now();
        let pushed_s = head.wrapping_sub(st.h0) as f64 / sr;
        if pushed_s > 0.0 { st.block_s = pushed_s; }
        if let Some(last) = st.last_call {
            if pushed_s > 0.0 {
                st.fast = if now.duration_since(last).as_secs_f64() < FAST_RATIO * pushed_s { (st.fast + 1).min(FAST_BLOCKS) } else { 0 };
            }
        }
        st.last_call = Some(now);

        let playing = transport.filter(|t| t.flags.contains(TransportFlags::IS_PLAYING | TransportFlags::HAS_SECONDS_TIMELINE));
        st.t0 = playing.map_or(f64::NAN, |t| t.song_pos_seconds.to_float());
        st.h0 = head;
        let t0 = st.t0;
        self.key_for_block(scope, &mut st, t0);
    }

    /// End of `process()`: frames for this block now, so a reader on
    /// another thread need not wait for our next block.
    pub fn end_block(&self, scope: &LiveScope) {
        let Some(mut st) = self.state.try_lock() else { return };
        let head = scope.head.load(Ordering::Relaxed);
        let sr = scope.sample_rate.load(Ordering::Relaxed).max(8000.0) as f64;
        self.flush(scope, &mut st, head, sr);
    }

    fn flush(&self, scope: &LiveScope, st: &mut State, head: usize, sr: f64) {
        let owner = self.own.load(Ordering::Acquire);
        let cap = scope.buf.len();
        let ready = owner != 0 && st.t0.is_finite() && cap >= WINDOW && self.wanted.load(Ordering::Relaxed);
        let (Some(fr), true) = (self.frames.get(), ready) else { st.flushed = head; return };
        let slot = self.own_slot.load(Ordering::Relaxed);
        // First hop boundary not done yet, skipping windows the ring lost.
        let oldest = head.saturating_sub(cap) + WINDOW;
        let mut b = (st.flushed.max(oldest.saturating_sub(1)) / HOP + 1) * HOP;
        while b <= head {
            let start = b - WINDOW;
            for (i, (x, w)) in st.x.iter_mut().zip(st.win.iter()).enumerate() {
                *x = scope.buf[(start + i) % cap].load(Ordering::Relaxed) * w;
            }
            let State { fft, x, spec, scratch, .. } = &mut *st;
            if fft.process_with_scratch(x, spec, scratch).is_ok() {
                let bands = bands_from_spectrum(spec, WINDOW, sr as f32);
                let t = st.t0 + (b as f64 - st.h0 as f64) / sr;
                fr.write(slot, owner, t, &bands);
            }
            b += HOP;
        }
        st.flushed = head;
    }

    fn key_for_block(&self, scope: &LiveScope, st: &mut State, t: f64) {
        let owner = self.key_owner.load(Ordering::Acquire);
        let Some(fr) = self.frames.get() else { return };
        if owner == 0 || !t.is_finite() { return }
        let slot = self.key_slot.load(Ordering::Relaxed);
        fr.want(slot, self.clock_ms.load(Ordering::Relaxed));
        // A key track whose plugin writes no frames: the wall-clock copy.
        let Some(written) = fr.written(slot, owner) else { return };
        if written != st.key_seen { st.key_seen = written; st.absent = false; }
        let good = |f: &Option<Frame>| f.is_some_and(|f| t - f.t <= GOOD_S);
        let mut pick = fr.at_or_before(slot, owner, t);
        // Ahead of us without the frame we need (lapped the ring): waiting
        // cannot bring it back.
        let ahead = fr.newest_t(slot, owner).is_some_and(|n| n > t);
        if good(&pick) {
            st.serial = false;
        } else if st.fast >= FAST_BLOCKS && !st.serial && !st.absent && !ahead {
            let start = Instant::now();
            let (mut seen, mut moved) = (written, start);
            loop {
                std::thread::yield_now();
                pick = fr.at_or_before(slot, owner, t);
                if good(&pick) { break }
                let w = fr.written(slot, owner).unwrap_or(seen);
                if w != seen { seen = w; moved = Instant::now(); }
                let stalled = moved.elapsed();
                let one_block_behind = fr.newest_t(slot, owner).is_some_and(|n| t - n <= 2.0 * st.block_s + GOOD_S);
                if one_block_behind && stalled > STALL { st.serial = true; break }
                if stalled > WAIT_MAX || start.elapsed() > 4 * WAIT_MAX { st.absent = true; break }
            }
            st.key_seen = seen;
        }
        let bands = match pick { Some(f) if t - f.t <= STALE_S => f.bands, _ => [-120.0; BANDS] };
        for (a, v) in scope.key_bands.iter().zip(bands) { a.store(v, Ordering::Relaxed); }
        self.key_blocks.fetch_add(1, Ordering::Relaxed);
    }
}

impl Drop for Timeline {
    fn drop(&mut self) {
        if let Some(fr) = self.frames.get() {
            fr.release(self.own_slot.load(Ordering::Relaxed), self.own.load(Ordering::Relaxed));
        }
    }
}

/// Band RMS levels (dB) of a Hann-windowed `n`-point real spectrum on the
/// bus's 32 bands, so bands add up to the total level. Parseval with a Hann
/// window: rms² in a set of bins = 16 / (3 N²) · Σ|X_k|² (one-sided). A
/// peak-bin reading instead sags by up to ~2 dB when a tone falls between
/// bins. Allocation-free.
pub fn bands_from_spectrum(spec: &[Complex<f32>], n: usize, sr: f32) -> [f32; BANDS] {
    let bin_hz = sr / n as f32;
    let norm = 16.0 / (3.0 * (n as f32) * (n as f32));
    let edges = band_edges();
    std::array::from_fn(|b| {
        let (lo, hi) = (edges[b] / bin_hz, edges[b + 1] / bin_hz);
        let (i0, i1) = (lo.ceil() as usize, hi.ceil() as usize);
        let p: f32 = if i1 > i0 {
            (i0..i1.min(spec.len())).map(|i| spec[i].norm_sqr()).sum()
        } else {
            // Narrower than a bin (low end): that bin's power, shared by width.
            spec.get(lo.round() as usize).map_or(0.0, |c| c.norm_sqr()) * (hi - lo)
        };
        10.0 * (p * norm).max(1e-12).log10()
    })
}
