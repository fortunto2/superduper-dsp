//! The SuperDuper bus: every plugin instance publishes what it hears — level,
//! a 32-band spectrum, optionally a key — into one shared-memory file, and
//! any instance (or an outside tool, or an agent) reads all of them.
//!
//! Why a file and not a static: each plugin is its own dylib, so statics are
//! per plugin, and a CLI or a second host process cannot see them at all. A
//! memory-mapped file is the one thing they all share, at the cost of no
//! syscalls per update once it is mapped.
//!
//! Layout: a 64-byte header, then `SLOTS` fixed slots of `SLOT_BYTES`. A slot
//! is claimed by CAS on its `owner` word (an instance id, 0 = free), kept
//! alive by a heartbeat, and published under a seqlock (`seq` odd while
//! writing) so a reader never sees half an update. A slot whose heartbeat is
//! older than `STALE_MS` belongs to a crashed host and may be taken over.
//!
//! Nothing here runs on the audio thread: plugins publish from a background
//! thread (see `synth_core::gui::spawn_bus_publisher`).

use memmap2::MmapMut;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

pub const BANDS: usize = 32;
pub const SLOTS: usize = 128;
const SLOT_BYTES: usize = 512;
const HEADER_BYTES: usize = 64;
const MAGIC: u32 = u32::from_le_bytes(*b"SDBS");
const VERSION: u32 = 1;
/// A slot not refreshed for this long is free for the taking.
pub const STALE_MS: u64 = 3000;

// Slot field offsets (bytes).
const O_SEQ: usize = 0; // u32
const O_PID: usize = 4; // u32
const O_OWNER: usize = 8; // u64
const O_HEARTBEAT: usize = 16; // u64 unix ms
const O_SR: usize = 24; // f32
const O_RMS: usize = 28; // f32 dB
const O_PEAK: usize = 32; // f32 dB
const O_KEY: usize = 36; // i32 pitch class, −1 none
const O_KEY_CONF: usize = 40; // f32
const O_FLAGS: usize = 44; // u32 TRACK_* bits; 0 from writers that predate them
const O_PLUGIN: usize = 48; // [u8; 32]
const O_TRACK: usize = 80; // [u8; 64]
const O_BANDS: usize = 144; // [f32; BANDS] dB
const _: () = assert!(O_BANDS + BANDS * 4 <= SLOT_BYTES);

/// What kind of track the host says an instance sits on (CLAP track-info).
/// A master, group bus or return carries other tracks, so it is never one
/// side of a masking pair.
pub const TRACK_MASTER: u32 = 1;
pub const TRACK_BUS: u32 = 2;
pub const TRACK_RETURN: u32 = 4;

pub fn bus_path() -> PathBuf {
    std::env::var_os("SUPERDUPER_BUS").map(PathBuf::from).unwrap_or_else(|| {
        std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default().join(".superduper-dsp").join("bus")
    })
}

/// Band centre frequencies: 32 log-spaced bands, 20 Hz .. 20 kHz.
pub fn band_edges() -> [f32; BANDS + 1] {
    std::array::from_fn(|i| 20.0 * 1000f32.powf(i as f32 / BANDS as f32))
}

/// An instance id: odd (0 marks a free slot) and unique across instances,
/// restarts and processes. Time, pid and counter alone are not enough: every
/// .clap links its own copy of this crate and so its own counter, and plugins
/// of different kinds loading in one millisecond drew equal ids — they then
/// answered each other's mail and read each other as "self" for the key. A
/// RandomState salt is fresh per call and per binary (each copy of std seeds
/// its own keys from the OS).
fn instance_id(now: u64, pid: u32, counter: u64) -> u64 {
    use std::hash::{BuildHasher, Hasher};
    let salt = std::collections::hash_map::RandomState::new().build_hasher().finish();
    ((now << 20) ^ ((pid as u64) << 44) ^ (counter << 1) ^ salt) | 1
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

fn open_map() -> Option<MmapMut> {
    let path = bus_path();
    if let Some(dir) = path.parent() { let _ = std::fs::create_dir_all(dir); }
    let f = std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(&path).ok()?;
    let want = (HEADER_BYTES + SLOTS * SLOT_BYTES) as u64;
    if f.metadata().ok()?.len() < want { f.set_len(want).ok()?; }
    // SAFETY: the file is ours and fixed-size; concurrent writers only touch
    // it through the atomics and the seqlock below.
    let mut map = unsafe { MmapMut::map_mut(&f).ok()? };
    let magic = u32::from_le_bytes(map[0..4].try_into().ok()?);
    if magic != MAGIC {
        // Fresh (all zero) or foreign: initialise the header. Two processes
        // racing here write the same bytes.
        map[0..4].copy_from_slice(&MAGIC.to_le_bytes());
        map[4..8].copy_from_slice(&VERSION.to_le_bytes());
        map[8..12].copy_from_slice(&(SLOTS as u32).to_le_bytes());
        map[12..16].copy_from_slice(&(SLOT_BYTES as u32).to_le_bytes());
    }
    (u32::from_le_bytes(map[4..8].try_into().ok()?) == VERSION).then_some(map)
}

fn slot_base(slot: usize) -> usize { HEADER_BYTES + slot * SLOT_BYTES }

fn atomic_u32(map: &MmapMut, off: usize) -> &AtomicU32 {
    // SAFETY: offsets are 4-aligned within a page-aligned mapping.
    unsafe { &*(map.as_ptr().add(off) as *const AtomicU32) }
}

fn atomic_u64(map: &MmapMut, off: usize) -> &AtomicU64 {
    // SAFETY: offsets are 8-aligned within a page-aligned mapping.
    unsafe { &*(map.as_ptr().add(off) as *const AtomicU64) }
}

fn put_f32(map: &MmapMut, off: usize, v: f32) { atomic_u32(map, off).store(v.to_bits(), Ordering::Relaxed); }
fn get_f32(map: &MmapMut, off: usize) -> f32 { f32::from_bits(atomic_u32(map, off).load(Ordering::Relaxed)) }

fn put_str(map: &MmapMut, off: usize, len: usize, s: &str) {
    // Cut on a char boundary (64 bytes = 32 Cyrillic letters), never inside one.
    let mut end = s.len().min(len - 1);
    while end > 0 && !s.is_char_boundary(end) { end -= 1; }
    let bytes = &s.as_bytes()[..end];
    for i in 0..len / 4 {
        let mut w = [0u8; 4];
        for (j, b) in w.iter_mut().enumerate() { *b = *bytes.get(i * 4 + j).unwrap_or(&0); }
        atomic_u32(map, off + i * 4).store(u32::from_le_bytes(w), Ordering::Relaxed);
    }
}

fn get_str(map: &MmapMut, off: usize, len: usize) -> String {
    let mut out = Vec::with_capacity(len);
    for i in 0..len / 4 { out.extend_from_slice(&atomic_u32(map, off + i * 4).load(Ordering::Relaxed).to_le_bytes()); }
    let end = out.iter().position(|&b| b == 0).unwrap_or(out.len());
    String::from_utf8_lossy(&out[..end]).into_owned()
}

/// What one instance published.
#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    pub slot: usize,
    pub instance: u64,
    pub pid: u32,
    pub plugin: String,
    pub track: String,
    /// `TRACK_*` bits.
    pub track_flags: u32,
    pub sample_rate: f32,
    pub rms_db: f32,
    pub peak_db: f32,
    pub bands_db: [f32; BANDS],
    pub key: Option<u8>,
    pub key_confidence: f32,
    pub age_ms: u64,
}

impl Snapshot {
    /// Carries other tracks: flagged by the host, or unnamed (what hosts give
    /// a master, and all older writers publish for one).
    pub fn is_bus(&self) -> bool { self.track_flags != 0 || self.track.is_empty() }
}

/// One instance's slot. Dropping it frees the slot.
pub struct Publisher {
    map: MmapMut,
    slot: usize,
    id: u64,
}

impl Publisher {
    /// Claim a slot. None when the bus cannot be opened or all slots are live.
    pub fn open(plugin: &str) -> Option<Self> {
        let map = open_map()?;
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let id = instance_id(now_ms(), std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed));
        let now = now_ms();
        for slot in 0..SLOTS {
            let b = slot_base(slot);
            let owner = atomic_u64(&map, b + O_OWNER);
            let cur = owner.load(Ordering::Acquire);
            let stale = now.saturating_sub(atomic_u64(&map, b + O_HEARTBEAT).load(Ordering::Relaxed)) > STALE_MS;
            if (cur == 0 || stale) && owner.compare_exchange(cur, id, Ordering::AcqRel, Ordering::Relaxed).is_ok() {
                atomic_u64(&map, b + O_HEARTBEAT).store(now, Ordering::Release);
                atomic_u32(&map, b + O_PID).store(std::process::id(), Ordering::Relaxed);
                let p = Publisher { map, slot, id };
                p.write(|m, b| {
                    put_str(m, b + O_PLUGIN, 32, plugin);
                    put_str(m, b + O_TRACK, 64, "");
                    atomic_u32(m, b + O_FLAGS).store(0, Ordering::Relaxed);
                    atomic_u32(m, b + O_KEY).store(u32::MAX, Ordering::Relaxed);
                });
                return Some(p);
            }
        }
        None
    }

    pub fn slot(&self) -> usize { self.slot }
    pub fn instance(&self) -> u64 { self.id }

    fn write(&self, f: impl FnOnce(&MmapMut, usize)) {
        let b = slot_base(self.slot);
        // After a stall longer than STALE_MS (sleep/wake, a debugger) another
        // instance may have taken this slot; writing on would interleave two
        // spectra under its name. The slot is theirs now.
        if atomic_u64(&self.map, b + O_OWNER).load(Ordering::Acquire) != self.id { return; }
        let seq = atomic_u32(&self.map, b + O_SEQ);
        let s = seq.load(Ordering::Relaxed);
        seq.store(s.wrapping_add(1) | 1, Ordering::Release); // odd: writing
        std::sync::atomic::fence(Ordering::Release);
        f(&self.map, b);
        std::sync::atomic::fence(Ordering::Release);
        seq.store((s | 1).wrapping_add(1), Ordering::Release); // even: done
        atomic_u64(&self.map, b + O_HEARTBEAT).store(now_ms(), Ordering::Release);
    }

    pub fn set_track(&self, track: &str, flags: u32) {
        self.write(|m, b| {
            put_str(m, b + O_TRACK, 64, track);
            atomic_u32(m, b + O_FLAGS).store(flags, Ordering::Relaxed);
        });
    }

    pub fn publish(&self, sample_rate: f32, rms_db: f32, peak_db: f32, bands_db: &[f32; BANDS]) {
        self.write(|m, b| {
            put_f32(m, b + O_SR, sample_rate);
            put_f32(m, b + O_RMS, rms_db);
            put_f32(m, b + O_PEAK, peak_db);
            for (i, v) in bands_db.iter().enumerate() { put_f32(m, b + O_BANDS + i * 4, *v); }
        });
    }

    /// A key estimate (pitch class + 0..1 confidence), or None to clear it.
    pub fn publish_key(&self, key: Option<(u8, f32)>) {
        self.write(|m, b| {
            atomic_u32(m, b + O_KEY).store(key.map_or(u32::MAX, |k| k.0 as u32 % 12), Ordering::Relaxed);
            put_f32(m, b + O_KEY_CONF, key.map_or(0.0, |k| k.1));
        });
    }
}

impl Drop for Publisher {
    fn drop(&mut self) {
        let _ = atomic_u64(&self.map, slot_base(self.slot) + O_OWNER)
            .compare_exchange(self.id, 0, Ordering::AcqRel, Ordering::Relaxed);
    }
}

/// A read handle; cheap to keep around and poll.
pub struct Reader { map: MmapMut }

impl Reader {
    pub fn open() -> Option<Self> { open_map().map(|map| Reader { map }) }

    /// Every live slot (heartbeat within `STALE_MS`), consistent per slot.
    pub fn read_all(&self) -> Vec<Snapshot> {
        let now = now_ms();
        let mut out = Vec::new();
        for slot in 0..SLOTS {
            let b = slot_base(slot);
            let m = &self.map;
            let instance = atomic_u64(m, b + O_OWNER).load(Ordering::Acquire);
            if instance == 0 { continue; }
            let hb = atomic_u64(m, b + O_HEARTBEAT).load(Ordering::Acquire);
            if now.saturating_sub(hb) > STALE_MS { continue; }
            for _ in 0..8 {
                let s1 = atomic_u32(m, b + O_SEQ).load(Ordering::Acquire);
                if s1 & 1 == 1 { std::hint::spin_loop(); continue; }
                let key = atomic_u32(m, b + O_KEY).load(Ordering::Relaxed);
                let snap = Snapshot {
                    slot,
                    instance,
                    pid: atomic_u32(m, b + O_PID).load(Ordering::Relaxed),
                    plugin: get_str(m, b + O_PLUGIN, 32),
                    track: get_str(m, b + O_TRACK, 64),
                    track_flags: atomic_u32(m, b + O_FLAGS).load(Ordering::Relaxed),
                    sample_rate: get_f32(m, b + O_SR),
                    rms_db: get_f32(m, b + O_RMS),
                    peak_db: get_f32(m, b + O_PEAK),
                    bands_db: std::array::from_fn(|i| get_f32(m, b + O_BANDS + i * 4)),
                    key: (key < 12).then_some(key as u8),
                    key_confidence: get_f32(m, b + O_KEY_CONF),
                    age_ms: now.saturating_sub(hb),
                };
                std::sync::atomic::fence(Ordering::Acquire);
                if atomic_u32(m, b + O_SEQ).load(Ordering::Acquire) == s1 { out.push(snap); break; }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tests share one process and `SUPERDUPER_BUS` is process-wide, so they
    /// run one at a time (parallel, one test's publishers took the other's slots).
    static ONE_AT_A_TIME: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn with_bus<R>(tag: &str, f: impl FnOnce() -> R) -> R {
        let _serial = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
        let p = std::env::temp_dir().join(format!("sdsp-bus-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_file(&p);
        std::env::set_var("SUPERDUPER_BUS", &p);
        f()
    }

    #[test]
    fn back_to_back_publishers_never_share_an_id() {
        with_bus("ids", || {
            let ps: Vec<Publisher> = (0..8).map(|_| Publisher::open("SuperDuper Soothe").expect("open")).collect();
            let mut ids: Vec<u64> = ps.iter().map(|p| p.instance()).collect();
            ids.sort();
            ids.dedup();
            assert_eq!(ids.len(), 8, "eight instances in one millisecond, eight ids");
            assert!(ps.iter().all(|p| p.instance() & 1 == 1), "odd, never 0 (= free slot)");
        });
    }

    #[test]
    fn two_plugin_binaries_in_one_millisecond_never_share_an_id() {
        // Each .clap links its own copy of this crate, so each has its own
        // counter: Spectrum, Limiter and Soothe loading in one millisecond all
        // drew counter 2 and the same id (REAPER, 2026-10-22: duck_under on
        // Pad's Soothe was answered by the master's Spectrum). Same clock, pid
        // and counter must still give different ids.
        let (now, pid) = (now_ms(), std::process::id());
        let ids: std::collections::HashSet<u64> = (0..64).map(|_| instance_id(now, pid, 2)).collect();
        assert_eq!(ids.len(), 64, "64 binaries, one clock tick, one counter value: 64 ids");
        assert!(ids.iter().all(|id| id & 1 == 1), "odd, never 0 (= free slot)");
    }

    #[test]
    fn publish_read_release() {
        with_bus("a", || {
            let a = Publisher::open("SuperDuper Compressor").expect("open");
            let b = Publisher::open("SuperDuper Piano").expect("open");
            assert_ne!(a.slot(), b.slot());
            a.set_track("Vocal", 0);
            let mut bands = [-90.0f32; BANDS];
            bands[10] = -12.5;
            a.publish(48_000.0, -18.0, -3.0, &bands);
            b.publish_key(Some((9, 0.8)));
            let r = Reader::open().expect("reader");
            let all = r.read_all();
            let va = all.iter().find(|s| s.instance == a.instance()).expect("a visible");
            assert_eq!((va.plugin.as_str(), va.track.as_str()), ("SuperDuper Compressor", "Vocal"));
            assert_eq!((va.rms_db, va.peak_db, va.bands_db[10]), (-18.0, -3.0, -12.5));
            assert_eq!(all.iter().find(|s| s.instance == b.instance()).and_then(|s| s.key), Some(9));
            assert!(!va.is_bus(), "a named ordinary track is a track");
            assert!(all.iter().find(|s| s.instance == b.instance()).is_some_and(|s| s.is_bus()), "unnamed reads as a bus (old writers' master)");
            b.set_track("Drums", TRACK_BUS);
            let vb = r.read_all().into_iter().find(|s| s.instance == b.instance()).expect("b");
            assert_eq!((vb.track.as_str(), vb.track_flags, vb.is_bus()), ("Drums", TRACK_BUS, true), "the host's kind, not the name, makes a bus");
            let freed = a.slot();
            drop(a);
            assert!(r.read_all().iter().all(|s| s.slot != freed), "dropped publisher's slot is gone");
            let c = Publisher::open("again").expect("reuse");
            assert_eq!(c.slot(), freed, "the freed slot is reused");
        });
    }
}
