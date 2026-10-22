//! Key frames: each instance's 32-band spectrum on the **song timeline**, so a
//! plugin keyed on another track reads that track at its own song position.
//!
//! The bus slot (`bus.rs`) holds one spectrum refreshed ~20×/s by a thread on
//! the wall clock. That is fine while a song plays in real time and useless
//! in a faster-than-real-time render: at 20× one refresh spans a second of
//! audio, and which second depends on where the render threads happened to
//! be (REAPER, 2026-10-22: of two full-speed renders of one project one did
//! not duck at all and one ducked −11 dB through the whole song, where a 1x
//! render ducked −11.3 dB on the key and 0.0 dB off it). Here the producer's audio thread writes a frame every hop
//! stamped with the song time of its last sample, and the reader picks the
//! frame for its block, whatever the wall clock did.
//!
//! Layout: a sibling file of the bus (`<bus>.frames`), one slot per bus slot
//! (same index, same owner id), each a ring of `FRAMES` frames under per-frame
//! seqlocks. Writing and reading are atomics only: both sides run on audio
//! threads. Opening, claiming and the "wanted" heartbeat run elsewhere.

use crate::bus::{bus_path, BANDS, SLOTS};
use memmap2::MmapMut;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

/// Frames per slot: 1024 hops of 256 samples = 5.5 s at 48 kHz, room for a
/// host that renders one track well ahead of another. A key track further
/// ahead than that has overwritten the frames its reader needs.
pub const FRAMES: usize = 1024;
const HEADER_BYTES: usize = 64;
// Slot header.
const O_OWNER: usize = 0; // u64 instance id, as on the bus
const O_WRITTEN: usize = 8; // u64 frames written since the claim
const O_WANTED: usize = 16; // u64 unix ms a reader last asked for this slot
const SLOT_HDR: usize = 64;
// Frame.
const F_SEQ: usize = 0; // u32, odd while writing
const F_T: usize = 8; // f64 song seconds of the frame's last sample; NaN = no transport
const F_BANDS: usize = 16; // [f32; BANDS] dB
const FRAME_BYTES: usize = F_BANDS + BANDS * 4;
const SLOT_BYTES: usize = SLOT_HDR + FRAMES * FRAME_BYTES;
const MAGIC: u32 = u32::from_le_bytes(*b"SDKF");
const VERSION: u32 = 1;
const _: () = assert!(FRAME_BYTES.is_multiple_of(8) && SLOT_BYTES.is_multiple_of(8));

pub fn frames_path() -> PathBuf {
    let mut p = bus_path().into_os_string();
    p.push(".frames");
    PathBuf::from(p)
}

/// The mapped frames file. One per plugin instance is plenty; the audio
/// thread reaches it through a reference the publisher thread hands over.
pub struct Frames { map: MmapMut }

/// The newest frame at or before a time.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Frame {
    pub t: f64,
    pub bands: [f32; BANDS],
}

impl Frames {
    pub fn open() -> Option<Self> {
        let path = frames_path();
        if let Some(dir) = path.parent() { let _ = std::fs::create_dir_all(dir); }
        let f = std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(&path).ok()?;
        let want = (HEADER_BYTES + SLOTS * SLOT_BYTES) as u64;
        // Sparse: pages of slots nobody writes are never touched.
        if f.metadata().ok()?.len() < want { f.set_len(want).ok()?; }
        // SAFETY: fixed-size file, touched only through the atomics below.
        let mut map = unsafe { MmapMut::map_mut(&f).ok()? };
        if u32::from_le_bytes(map[0..4].try_into().ok()?) != MAGIC {
            map[0..4].copy_from_slice(&MAGIC.to_le_bytes());
            map[4..8].copy_from_slice(&VERSION.to_le_bytes());
        }
        (u32::from_le_bytes(map[4..8].try_into().ok()?) == VERSION).then_some(Frames { map })
    }

    fn base(slot: usize) -> usize { HEADER_BYTES + slot.min(SLOTS - 1) * SLOT_BYTES }
    fn frame(slot: usize, i: u64) -> usize { Self::base(slot) + SLOT_HDR + (i % FRAMES as u64) as usize * FRAME_BYTES }

    fn u32_at(&self, off: usize) -> &AtomicU32 {
        // SAFETY: 4-aligned offset inside a page-aligned mapping.
        unsafe { &*(self.map.as_ptr().add(off) as *const AtomicU32) }
    }
    fn u64_at(&self, off: usize) -> &AtomicU64 {
        // SAFETY: 8-aligned offset inside a page-aligned mapping.
        unsafe { &*(self.map.as_ptr().add(off) as *const AtomicU64) }
    }

    /// Take `slot` for `owner` (the bus instance id). Frames a previous owner
    /// left are voided first, or a reader could match their song times.
    /// Publisher thread, once.
    pub fn claim(&self, slot: usize, owner: u64) {
        let b = Self::base(slot);
        self.u64_at(b + O_OWNER).store(0, Ordering::Release);
        for i in 0..FRAMES as u64 {
            self.u64_at(Self::frame(slot, i) + F_T).store(f64::NAN.to_bits(), Ordering::Relaxed);
        }
        self.u64_at(b + O_WRITTEN).store(0, Ordering::Relaxed);
        self.u64_at(b + O_OWNER).store(owner, Ordering::Release);
    }

    /// Give the slot up (instance dropped), unless someone took it over.
    pub fn release(&self, slot: usize, owner: u64) {
        let _ = self.u64_at(Self::base(slot) + O_OWNER).compare_exchange(owner, 0, Ordering::AcqRel, Ordering::Relaxed);
    }

    /// Append one frame. Audio thread: atomics only. Does nothing once the
    /// slot belongs to someone else.
    pub fn write(&self, slot: usize, owner: u64, t: f64, bands: &[f32; BANDS]) {
        let b = Self::base(slot);
        if self.u64_at(b + O_OWNER).load(Ordering::Acquire) != owner { return; }
        let n = self.u64_at(b + O_WRITTEN).load(Ordering::Relaxed);
        let f = Self::frame(slot, n);
        let seq = self.u32_at(f + F_SEQ);
        let s = seq.load(Ordering::Relaxed);
        seq.store(s.wrapping_add(1) | 1, Ordering::Release);
        std::sync::atomic::fence(Ordering::Release);
        self.u64_at(f + F_T).store(t.to_bits(), Ordering::Relaxed);
        for (i, v) in bands.iter().enumerate() { self.u32_at(f + F_BANDS + i * 4).store(v.to_bits(), Ordering::Relaxed); }
        std::sync::atomic::fence(Ordering::Release);
        seq.store((s | 1).wrapping_add(1), Ordering::Release);
        self.u64_at(b + O_WRITTEN).store(n + 1, Ordering::Release);
    }

    /// Frames written into `slot` by `owner`; None when the slot is not
    /// theirs (a producer from before key frames, or gone).
    pub fn written(&self, slot: usize, owner: u64) -> Option<u64> {
        let b = Self::base(slot);
        (owner != 0 && self.u64_at(b + O_OWNER).load(Ordering::Acquire) == owner)
            .then(|| self.u64_at(b + O_WRITTEN).load(Ordering::Acquire))
    }

    /// A reader keyed on `slot` says so; producers only analyse while wanted.
    pub fn want(&self, slot: usize, now_ms: u64) { self.u64_at(Self::base(slot) + O_WANTED).store(now_ms, Ordering::Relaxed); }
    pub fn wanted_ms(&self, slot: usize) -> u64 { self.u64_at(Self::base(slot) + O_WANTED).load(Ordering::Relaxed) }

    /// Song time of the newest frame (NaN when it had no transport).
    pub fn newest_t(&self, slot: usize, owner: u64) -> Option<f64> {
        let n = self.written(slot, owner)?;
        (n > 0).then(|| f64::from_bits(self.u64_at(Self::frame(slot, n - 1) + F_T).load(Ordering::Acquire)))
    }

    /// The newest frame whose song time is at or before `t` (frames are
    /// stamped with their last sample, so this is what had been heard by
    /// `t`). Among equal times — a loop played again — the most recently
    /// written wins. Audio thread: atomics only, no allocation.
    pub fn at_or_before(&self, slot: usize, owner: u64, t: f64) -> Option<Frame> {
        let n = self.written(slot, owner)?;
        let lo = n.saturating_sub(FRAMES as u64);
        // Half a sample at 192 kHz: song times computed in two plugins from
        // one transport differ only by rounding.
        let t = t + 2.5e-6;
        for _ in 0..4 {
            let mut best: Option<(u64, f64)> = None;
            for i in (lo..n).rev() {
                let ft = f64::from_bits(self.u64_at(Self::frame(slot, i) + F_T).load(Ordering::Acquire));
                if ft <= t && best.is_none_or(|(_, bt)| ft > bt) { best = Some((i, ft)); }
            }
            let (i, _) = best?;
            let f = Self::frame(slot, i);
            let s1 = self.u32_at(f + F_SEQ).load(Ordering::Acquire);
            if s1 & 1 == 1 { continue; }
            let ft = f64::from_bits(self.u64_at(f + F_T).load(Ordering::Relaxed));
            let bands = std::array::from_fn(|k| f32::from_bits(self.u32_at(f + F_BANDS + k * 4).load(Ordering::Relaxed)));
            std::sync::atomic::fence(Ordering::Acquire);
            // Overwritten while read (the ring lapped): look again.
            if self.u32_at(f + F_SEQ).load(Ordering::Acquire) == s1 && ft <= t { return Some(Frame { t: ft, bands }); }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_frames<R>(tag: &str, f: impl FnOnce(Frames) -> R) -> R {
        // Its own file: SUPERDUPER_BUS is process-wide and bus.rs tests set it.
        let p = std::env::temp_dir().join(format!("sdsp-frames-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_file(&p);
        let fr = open_at(&p).expect("open");
        let r = f(fr);
        let _ = std::fs::remove_file(&p);
        r
    }

    fn open_at(p: &std::path::Path) -> Option<Frames> {
        let f = std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(p).ok()?;
        f.set_len((HEADER_BYTES + SLOTS * SLOT_BYTES) as u64).ok()?;
        // SAFETY: test-private file.
        Some(Frames { map: unsafe { MmapMut::map_mut(&f).ok()? } })
    }

    fn bands(v: f32) -> [f32; BANDS] { [v; BANDS] }

    #[test]
    fn reads_the_frame_for_a_song_time_not_the_latest() {
        with_frames("time", |fr| {
            fr.claim(3, 11);
            // A producer rendered two seconds ahead of the reader.
            for k in 0..200 { fr.write(3, 11, k as f64 * 0.01, &bands(-(k as f32))); }
            let f = fr.at_or_before(3, 11, 0.505).expect("frame");
            assert_eq!((f.t, f.bands[0]), (0.5, -50.0), "the frame heard by 0.505 s, not the newest (1.99 s)");
            assert!(fr.at_or_before(3, 11, -0.1).is_none(), "nothing before the first frame");
            assert_eq!(fr.newest_t(3, 11), Some(1.99));
        });
    }

    #[test]
    fn a_loop_played_again_reads_the_newer_pass() {
        with_frames("loop", |fr| {
            fr.claim(0, 5);
            fr.write(0, 5, 1.0, &bands(-60.0));
            fr.write(0, 5, 2.0, &bands(-60.0));
            fr.write(0, 5, 1.0, &bands(-10.0)); // second pass over the same spot
            assert_eq!(fr.at_or_before(0, 5, 1.2).map(|f| f.bands[0]), Some(-10.0));
        });
    }

    #[test]
    fn frames_without_transport_never_match_a_time() {
        with_frames("nan", |fr| {
            fr.claim(1, 7);
            fr.write(1, 7, f64::NAN, &bands(-5.0));
            assert!(fr.at_or_before(1, 7, 10.0).is_none());
            assert_eq!(fr.written(1, 7), Some(1));
        });
    }

    #[test]
    fn another_owners_slot_reads_as_absent_and_a_claim_voids_old_frames() {
        with_frames("own", |fr| {
            fr.claim(2, 9);
            fr.write(2, 9, 1.0, &bands(-1.0));
            assert!(fr.written(2, 8).is_none(), "wrong owner: not theirs");
            fr.write(2, 8, 1.5, &bands(-2.0));
            assert_eq!(fr.written(2, 9), Some(1), "a stranger cannot write");
            fr.claim(2, 13);
            assert!(fr.at_or_before(2, 13, 5.0).is_none(), "the previous owner's frames are void");
            fr.release(2, 9);
            assert_eq!(fr.written(2, 13), Some(0), "release by a former owner does not free it");
        });
    }

    #[test]
    fn the_ring_keeps_the_last_frames_only() {
        with_frames("ring", |fr| {
            fr.claim(4, 3);
            for k in 0..(FRAMES as u64 + 10) { fr.write(4, 3, k as f64, &bands(k as f32)); }
            assert!(fr.at_or_before(4, 3, 5.0).is_none(), "lapped: 5 s is gone");
            assert_eq!(fr.at_or_before(4, 3, 100.0).map(|f| f.t), Some(100.0));
        });
    }
}
