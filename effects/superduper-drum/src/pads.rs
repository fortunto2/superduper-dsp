//! Sample pads: the drum library, per-pad sample loading, kit export.
//!
//! Each of the six pads can play its synth voice, a sample, or both
//! (`<Pad> Src`). `<Pad> Smp` indexes the pad's slice of the library — kicks
//! for the kick pad, hats for the hat pads — so ◀ ▶ only ever offers the
//! right kind of sound.
//!
//! Same threading as SuperDuper Piano: a loader thread (idle until the first
//! `activate`, so a host's plugin scan never walks gigabytes) scans the
//! library and decodes, and hands each sample over through `pending`. The
//! audio thread parks a replaced sample in `retired`; the loader frees only
//! entries no playing hit still holds (`strong_count == 1`), so no sample is
//! ever freed on the audio thread.

use parking_lot::Mutex;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::Arc;
use superduper_synth_core::drumlib::{default_roots, scan_library, DrumSample};
use superduper_synth_core::sfz::load::decode_file;
use superduper_synth_core::sfz::SampleBuf;

use crate::{voice_param_idx, SharedParamsInner, KEY_AUTO, P_KEY, P_KIT_DICE, P_KIT_FILE, P_KIT_PACK, P_SMP0, P_SRC0};
use superduper_synth_core::drumlib::{is_tonal, parse_key_name, pitch_class, song_key_path};

/// Where kits are saved and listed.
pub fn kits_dir() -> PathBuf {
    superduper_synth_core::sfz::load::default_pack_root().join("Kits")
}

pub const PADS: usize = 6;
/// Retired samples the audio thread can park without allocating.
pub const RETIRE_CAPACITY: usize = 64;
/// GM notes the exported `.sfz` kit uses, per pad.
pub const KIT_KEYS: [u8; PADS] = [36, 38, 42, 46, 39, 56];
/// Samples handed to the audio thread in this process — lets a test wait.
pub static PAD_LOADS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

pub mod src {
    pub const SYNTH: u8 = 0;
    pub const SAMPLE: u8 = 1;
    pub const LAYER: u8 = 2;
}

pub struct PadBank {
    pub library: Mutex<Vec<DrumSample>>,
    /// Per pad: indices into `library`, in browse order.
    pub lists: Mutex<[Vec<usize>; PADS]>,
    pub scanned: AtomicBool,
    pub activated: AtomicBool,
    pub status: Mutex<String>,
    /// Pad-list index currently loaded (−1 none).
    pub loaded_idx: [AtomicI32; PADS],
    pub loaded_path: [Mutex<Option<PathBuf>>; PADS],
    /// A specific file asked for by project state; wins over the index.
    pub requested_path: [Mutex<Option<PathBuf>>; PADS],
    pub pending: [Mutex<Option<Arc<SampleBuf>>>; PADS],
    pub retired: Mutex<Vec<Arc<SampleBuf>>>,
    /// Fire the pad once its newly browsed sample arrives.
    pub audition_after_load: [AtomicBool; PADS],
    /// Kit Pack / Kit Dice / Kit File values already acted on.
    pub applied_kit: [AtomicI32; 3],
    /// Pitch class of the sample loaded on each pad (tonal classes only), −1 = none.
    pub pad_key: [AtomicI32; PADS],
    /// Key = Auto's answer: from the song-key file, else the passed-on notes. −1 = none.
    pub song_key_auto: AtomicI32,
    /// Pitch classes of the notes this kit forwards to a bass synth.
    pub note_hist: [std::sync::atomic::AtomicU32; 12],
    /// Loader passes, per instance — paces the song-key refresh.
    pub key_tick: std::sync::atomic::AtomicU32,
}

impl Default for PadBank {
    fn default() -> Self {
        Self {
            library: Mutex::new(Vec::new()),
            lists: Mutex::new(Default::default()),
            scanned: AtomicBool::new(false),
            activated: AtomicBool::new(false),
            status: Mutex::new(String::new()),
            loaded_idx: std::array::from_fn(|_| AtomicI32::new(-1)),
            loaded_path: std::array::from_fn(|_| Mutex::new(None)),
            requested_path: std::array::from_fn(|_| Mutex::new(None)),
            pending: std::array::from_fn(|_| Mutex::new(None)),
            retired: Mutex::new(Vec::with_capacity(RETIRE_CAPACITY)),
            audition_after_load: std::array::from_fn(|_| AtomicBool::new(false)),
            applied_kit: std::array::from_fn(|_| AtomicI32::new(0)),
            pad_key: std::array::from_fn(|_| AtomicI32::new(-1)),
            song_key_auto: AtomicI32::new(-1),
            note_hist: std::array::from_fn(|_| std::sync::atomic::AtomicU32::new(0)),
            key_tick: std::sync::atomic::AtomicU32::new(0),
        }
    }
}

/// Scan roots: `SUPERDUPER_DRUM_ROOT` (tests) or our sample folders plus any
/// mounted volume's `SuperDuper Samples`.
fn roots() -> Vec<PathBuf> {
    match std::env::var_os("SUPERDUPER_DRUM_ROOT") {
        Some(r) => vec![PathBuf::from(r)],
        None => default_roots(),
    }
}

pub fn rescan(shared: &SharedParamsInner) {
    let t0 = std::time::Instant::now();
    *shared.pads.status.lock() = "scanning sample folders…".into();
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).clamp(1, 8);
    let lib = scan_library(&roots(), threads);
    let mut lists: [Vec<usize>; PADS] = Default::default();
    for (i, s) in lib.iter().enumerate() { lists[s.class.pad()].push(i); }
    *shared.pads.status.lock() = format!("{} drum one-shots in {:.1} s", lib.len(), t0.elapsed().as_secs_f64());
    *shared.pads.library.lock() = lib;
    *shared.pads.lists.lock() = lists;
    // Indices may have moved: reload what is loaded by path.
    for pad in 0..PADS {
        if let Some(p) = shared.pads.loaded_path[pad].lock().take() {
            let mut req = shared.pads.requested_path[pad].lock();
            if req.is_none() { *req = Some(p); }
        }
        shared.pads.loaded_idx[pad].store(-1, Ordering::Relaxed);
    }
    shared.pads.scanned.store(true, Ordering::Relaxed);
}

/// The library entry at `idx` of `pad`'s list.
pub fn pad_entry(shared: &SharedParamsInner, pad: usize, idx: usize) -> Option<DrumSample> {
    let lists = shared.pads.lists.lock();
    let li = *lists.get(pad)?.get(idx)?;
    shared.pads.library.lock().get(li).cloned()
}

pub fn pad_len(shared: &SharedParamsInner, pad: usize) -> usize {
    shared.pads.lists.lock().get(pad).map_or(0, Vec::len)
}

/// One loader pass. Blocking; loader thread (and tests).
pub fn loader_step(shared: &SharedParamsInner) {
    shared.pads.retired.lock().retain(|a| Arc::strong_count(a) > 1);
    if !shared.pads.activated.load(Ordering::Relaxed) { return; }
    if !shared.pads.scanned.load(Ordering::Relaxed) { rescan(shared); }
    apply_kit_params(shared);
    // About once a second (loader ticks every 20 ms), and on the very first pass.
    if shared.pads.key_tick.fetch_add(1, Ordering::Relaxed) % 50 == 0 { refresh_song_key(shared); }

    for pad in 0..PADS {
        let src = shared.params[P_SRC0 + pad].load(Ordering::Relaxed).round() as u8;
        let req = shared.pads.requested_path[pad].lock().take();
        let n = pad_len(shared, pad);
        let (idx, path) = if let Some(p) = req {
            let lib = shared.pads.library.lock();
            let lists = shared.pads.lists.lock();
            let pos = lists[pad].iter().position(|&li| lib.get(li).is_some_and(|s| s.path == p));
            match pos {
                Some(i) => {
                    shared.params[P_SMP0 + pad].store(i as f32, Ordering::Relaxed);
                    (i as i32, p)
                }
                // Not in the library (drive unplugged, moved): load it anyway.
                None => (shared.params[P_SMP0 + pad].load(Ordering::Relaxed).round() as i32, p),
            }
        } else {
            if src == src::SYNTH || n == 0 { continue; }
            let want = (shared.params[P_SMP0 + pad].load(Ordering::Relaxed).round().max(0.0) as usize).min(n - 1) as i32;
            if want == shared.pads.loaded_idx[pad].load(Ordering::Relaxed) { continue; }
            match pad_entry(shared, pad, want as usize) {
                Some(e) => (want, e.path),
                None => continue,
            }
        };
        shared.pads.loaded_idx[pad].store(idx, Ordering::Relaxed);
        match decode_file(&path) {
            Ok(buf) => {
                // The note this sample sits on, for Key: the library's answer
                // (name first, then sound), or — for a file outside the
                // library on a tonal pad — measured now.
                let entry = shared.pads.library.lock().iter().find(|e| e.path == path).cloned();
                let key = match entry {
                    Some(e) if is_tonal(e.class) => e.key.map_or(-1, |k| k as i32),
                    Some(_) => -1,
                    None if pad == 0 => superduper_synth_core::drumlib::drum_pitch_hz(&superduper_synth_core::drumlib::mono_of(&buf), buf.sample_rate)
                        .map_or(-1, |hz| pitch_class(hz) as i32),
                    None => -1,
                };
                shared.pads.pad_key[pad].store(key, Ordering::Relaxed);
                *shared.pads.loaded_path[pad].lock() = Some(path);
                let stale = shared.pads.pending[pad].lock().replace(Arc::new(buf));
                drop(stale);
                PAD_LOADS.fetch_add(1, Ordering::SeqCst);
            }
            Err(e) => *shared.pads.status.lock() = format!("{}: {e}", path.display()),
        }
    }
}

fn kit_param(shared: &SharedParamsInner, i: usize) -> i32 {
    shared.params[[P_KIT_PACK, P_KIT_DICE, P_KIT_FILE][i]].load(Ordering::Relaxed).round() as i32
}

/// Treat the current kit params as done (project load), so the kit they
/// describe is not rebuilt over the pads saved with it.
pub fn mark_kit_applied(shared: &SharedParamsInner) {
    for i in 0..3 { shared.pads.applied_kit[i].store(kit_param(shared, i), Ordering::Relaxed); }
}

/// Act on Kit Pack / Kit Dice / Kit File when they changed. Loader thread.
pub fn apply_kit_params(shared: &SharedParamsInner) {
    let now: [i32; 3] = std::array::from_fn(|i| kit_param(shared, i));
    let was: [i32; 3] = std::array::from_fn(|i| shared.pads.applied_kit[i].load(Ordering::Relaxed));
    if now == was { return; }
    for i in 0..3 { shared.pads.applied_kit[i].store(now[i], Ordering::Relaxed); }
    if now[2] != was[2] && now[2] > 0 {
        if let Some(f) = kit_files(&kits_dir()).get(now[2] as usize - 1) {
            let res = load_kit_sfz(shared, f);
            *shared.pads.status.lock() = match res {
                Ok(n) => format!("kit {}: {n} pads", f.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()),
                Err(e) => e,
            };
        }
        return;
    }
    if (now[0] != was[0] || now[1] != was[1]) && now[0] > 0 {
        if let Some(pack) = packs(shared).get(now[0] as usize - 1).cloned() {
            kit_from_pack(shared, &pack, (now[1] > 0).then_some(now[1] as u32));
            *shared.pads.status.lock() = format!("kit from {pack}{}", if now[1] > 0 { format!(" (random #{})", now[1]) } else { String::new() });
        }
    }
}

/// GUI + agent share this path: set the params, the loader acts on them.
pub fn request_pack_kit(shared: &SharedParamsInner, pack_index: usize, dice: u32) {
    // Fill pressed again after hand edits must rebuild: forget the last pack.
    shared.pads.applied_kit[0].store(0, Ordering::Relaxed);
    for (i, v) in [(P_KIT_PACK, pack_index as f32 + 1.0), (P_KIT_DICE, dice as f32)] {
        shared.params[i].store(v, Ordering::Relaxed);
        shared.dirty_params[i].store(true, Ordering::Relaxed);
    }
}

pub fn request_kit_file(shared: &SharedParamsInner, file_index: usize) {
    // Re-picking the same file must reload it: step through 0 first.
    shared.pads.applied_kit[2].store(0, Ordering::Relaxed);
    shared.params[P_KIT_FILE].store(file_index as f32 + 1.0, Ordering::Relaxed);
    shared.dirty_params[P_KIT_FILE].store(true, Ordering::Relaxed);
}

/// The song key Auto would use right now.
pub fn song_key(shared: &SharedParamsInner) -> Option<u8> {
    let k = shared.pads.song_key_auto.load(Ordering::Relaxed);
    (0..12).contains(&k).then_some(k as u8)
}

/// The key the tonal pads follow: manual C..B, Auto, or None (Off).
pub fn effective_key(shared: &SharedParamsInner) -> Option<u8> {
    match shared.params[P_KEY].load(Ordering::Relaxed).round() as i32 {
        k @ 1..=12 => Some((k - 1) as u8),
        KEY_AUTO => song_key(shared),
        _ => None,
    }
}

/// Refresh Key = Auto: the song-key file wins; without one, the most
/// frequent pitch class among at least 8 forwarded notes (a bassline's root
/// is its most common note far more often than not).
pub fn refresh_song_key(shared: &SharedParamsInner) {
    let from_file = std::fs::read_to_string(song_key_path()).ok().and_then(|t| parse_key_name(&t));
    let from_notes = || {
        let h: Vec<u32> = shared.pads.note_hist.iter().map(|a| a.load(Ordering::Relaxed)).collect();
        let total: u32 = h.iter().sum();
        (total >= 8).then(|| h.iter().enumerate().max_by_key(|(_, &n)| n).map(|(i, _)| i as u8)).flatten()
    };
    let k = from_file.or_else(from_notes).map_or(-1, |k| k as i32);
    shared.pads.song_key_auto.store(k, Ordering::Relaxed);
}

pub fn spawn_loader(shared: &crate::SharedParams) {
    let weak = Arc::downgrade(shared);
    let _ = std::thread::Builder::new()
        .name("sdsp-drum-loader".into())
        .spawn(move || loop {
            match weak.upgrade() {
                Some(inner) => { loader_step(&inner); drop(inner); }
                None => break,
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        });
}

/// Put entry `idx` of `pad`'s list on the pad: switch it to Sample if it was
/// on Synth, and play the new sound as soon as it is loaded.
pub fn assign(shared: &SharedParamsInner, pad: usize, idx: usize) {
    assign_quiet(shared, pad, idx);
    shared.pads.audition_after_load[pad].store(true, Ordering::Relaxed);
}

/// `assign` without the audition — a whole kit arriving at once must not
/// fire six pads together.
pub fn assign_quiet(shared: &SharedParamsInner, pad: usize, idx: usize) {
    if idx >= pad_len(shared, pad) { return; }
    shared.params[P_SMP0 + pad].store(idx as f32, Ordering::Relaxed);
    shared.dirty_params[P_SMP0 + pad].store(true, Ordering::Relaxed);
    if shared.params[P_SRC0 + pad].load(Ordering::Relaxed).round() as u8 == src::SYNTH {
        set_source(shared, pad, src::SAMPLE);
    }
}

pub fn set_source(shared: &SharedParamsInner, pad: usize, value: u8) {
    shared.params[P_SRC0 + pad].store(value as f32, Ordering::Relaxed);
    shared.dirty_params[P_SRC0 + pad].store(true, Ordering::Relaxed);
}

/// Step through `pad`'s list (optionally only `positions`, a filtered view).
pub fn browse(shared: &SharedParamsInner, pad: usize, delta: i32) {
    let n = pad_len(shared, pad) as i32;
    if n == 0 { return; }
    let cur = shared.params[P_SMP0 + pad].load(Ordering::Relaxed).round() as i32;
    assign(shared, pad, (cur + delta).rem_euclid(n) as usize);
}

/// Packs that have at least one one-shot, sorted.
pub fn packs(shared: &SharedParamsInner) -> Vec<String> {
    let mut v: Vec<String> = shared.pads.library.lock().iter().map(|s| s.pack.clone()).collect();
    v.sort();
    v.dedup();
    v
}

/// Fill every pad from one pack: the first sound of each type, or a random
/// one. Pads the pack has nothing for go back to their synth voice.
pub fn kit_from_pack(shared: &SharedParamsInner, pack: &str, seed: Option<u32>) {
    let mut rng = superduper_synth_core::dsp_blocks::Xorshift::new(seed.unwrap_or(1));
    let choices: Vec<Vec<usize>> = {
        let lib = shared.pads.library.lock();
        let lists = shared.pads.lists.lock();
        (0..PADS)
            .map(|pad| lists[pad].iter().enumerate()
                .filter(|(_, &li)| lib.get(li).is_some_and(|s| s.pack == pack))
                .map(|(i, _)| i)
                .collect())
            .collect()
    };
    for (pad, opts) in choices.iter().enumerate() {
        if opts.is_empty() {
            set_source(shared, pad, src::SYNTH);
            continue;
        }
        let pick = if seed.is_some() { opts[rng.next_u32() as usize % opts.len()] } else { opts[0] };
        assign_quiet(shared, pad, pick);
        set_source(shared, pad, src::SAMPLE);
    }
}

/// `.sfz` kits saved by this plugin (or any GM-mapped kit) in `dir`.
pub fn kit_files(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir).map(|rd| rd.flatten().map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("sfz"))).collect()).unwrap_or_default();
    v.sort();
    v
}

/// Which pad a GM note belongs to (kit loading accepts the GM aliases too).
fn pad_for_key(key: i32) -> Option<usize> {
    match key {
        35 | 36 => Some(0),
        38 | 40 => Some(1),
        42 | 44 => Some(2),
        46 | 49 | 51 | 52 | 55 | 57 | 59 => Some(3),
        39 => Some(4),
        37 | 41 | 43 | 45 | 47 | 48 | 50 | 54 | 56 | 58 | 60..=81 => Some(5),
        _ => None,
    }
}

/// Load a kit `.sfz` onto the pads: each region's sample, tune, volume and
/// pan go to the pad its key maps to. Pads the kit does not name go back to
/// Synth. Returns how many pads were filled.
pub fn load_kit_sfz(shared: &SharedParamsInner, file: &Path) -> Result<usize, String> {
    use superduper_synth_core::sfz::{parse::parse_sfz, region::compile};
    let text = std::fs::read_to_string(file).map_err(|e| e.to_string())?;
    let dir = file.parent().unwrap_or(Path::new(".")).to_path_buf();
    let mut read = |p: &str| std::fs::read_to_string(dir.join(p)).ok();
    let def = compile(&parse_sfz(&text, &mut read).map_err(|e| e.to_string())?);
    let mut filled = [false; PADS];
    for r in &def.regions {
        let Some(pad) = pad_for_key(r.lokey) else { continue };
        if filled[pad] { continue; }
        filled[pad] = true;
        let path = if Path::new(&r.sample).is_absolute() { PathBuf::from(&r.sample) } else { dir.join(&r.sample) };
        *shared.pads.requested_path[pad].lock() = Some(path);
        let set = |o: usize, v: f32| {
            let i = voice_param_idx(pad, o);
            let d = &crate::PARAMS[i];
            shared.params[i].store(v.clamp(d.min as f32, d.max as f32), Ordering::Relaxed);
            shared.dirty_params[i].store(true, Ordering::Relaxed);
        };
        set(0, r.tune_cents / 100.0);
        set(2, 10f32.powf(r.volume_db / 20.0));
        set(3, r.pan / 100.0);
        set_source(shared, pad, src::SAMPLE);
    }
    for (pad, f) in filled.iter().enumerate() {
        if !f { set_source(shared, pad, src::SYNTH); }
    }
    Ok(filled.iter().filter(|f| **f).count())
}

/// Write the sample pads as an SFZ kit (GM keys, hat choke, the pad's tune,
/// level and pan baked in). Synth-only pads are left out — the kit is what
/// another sampler can play. Returns the file written.
pub fn save_kit_sfz(shared: &SharedParamsInner, dir: &Path, name: &str) -> Result<PathBuf, String> {
    let mut body = format!("// {name} — exported from SuperDuper Drum\n<global> loop_mode=one_shot\n");
    let mut n = 0;
    for pad in 0..PADS {
        let src = shared.params[P_SRC0 + pad].load(Ordering::Relaxed).round() as u8;
        if src == src::SYNTH { continue; }
        let Some(path) = shared.pads.loaded_path[pad].lock().clone() else { continue };
        let p = |o: usize| shared.params[voice_param_idx(pad, o)].load(Ordering::Relaxed);
        // Bake the Key shift into the kit so it sounds the same elsewhere.
        let pk = shared.pads.pad_key[pad].load(Ordering::Relaxed);
        let shift = match (effective_key(shared), pad == 0 || pad == 5) {
            (Some(song), true) if pk >= 0 => superduper_synth_core::drumlib::key_shift(pk as u8, song) as f32,
            _ => 0.0,
        };
        let tune = p(0) + shift;
        let vol = 20.0 * p(2).max(1e-4).log10();
        let choke = match pad { 2 => " group=1", 3 => " group=2 off_by=1", _ => "" };
        body.push_str(&format!(
            "<region> key={} pitch_keycenter={} tune={:.0} volume={:.1} pan={:.0}{choke} sample={}\n",
            KIT_KEYS[pad], KIT_KEYS[pad], tune * 100.0, vol, p(3) * 100.0, path.display(),
        ));
        n += 1;
    }
    if n == 0 { return Err("no pad is playing a sample".into()); }
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let file = dir.join(format!("{name}.sfz"));
    std::fs::write(&file, body).map_err(|e| e.to_string())?;
    Ok(file)
}
