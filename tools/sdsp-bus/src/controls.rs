//! Known-answer controls for the bus meter.
//!
//! The rule (from the research harness): never accept a zero from an
//! instrument you have not seen produce a non-zero, and it must find nothing
//! where nothing exists. So before trusting `mix_masking` or `mix_phase` on
//! a real session, run `sdsp-bus controls`: it plants known 32-band spectra
//! on a temporary bus through real `Publisher`s (the live
//! `~/.superduper-dsp/bus` is never opened), runs the same pure functions
//! the MCP tools run, and checks each answer against what was planted.
//! `tests/controls.rs` pins the same list, one test per control.
//!
//! Each control is a scene (`scene_*`) and a judge (`judge_*`); the judges
//! are public so a test can prove they go red on the wrong answer.

use std::sync::{Mutex, MutexGuard};

use superduper_dsp_sdk::bus::{band_edges, Publisher, Reader, Snapshot, BANDS, TRACK_MASTER};

use crate::serve::{masking_report, phase_report, ClashOut, FaderDb, MaskingOut, PhaseOut, Prefer};

/// `SUPERDUPER_BUS` is process-wide, so scenes run one at a time (the same
/// discipline as the tests in sdk/src/bus.rs).
static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

/// What a plugin publishes in a band where nothing plays.
pub const FLOOR_DB: f32 = -120.0;
/// Bus reads per scene; the tools take ten a second.
pub const FRAMES: usize = 20;
/// 267–331 Hz: where the planted clash sits.
pub const CLASH_BAND: usize = 12;
/// 113–140 Hz: where the planted cancellation sits.
pub const CANCEL_BAND: usize = 8;

/// A temporary bus with real publishers on it. Dropping it frees the slots
/// and deletes the file; the lock is held for its whole life.
pub struct Bus {
    publishers: Vec<Publisher>,
    reader: Reader,
    path: std::path::PathBuf,
    _lock: MutexGuard<'static, ()>,
}

impl Bus {
    pub fn new(tag: &str) -> Self {
        let lock = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
        let path = std::env::temp_dir().join(format!("sdsp-controls-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_file(&path);
        std::env::set_var("SUPERDUPER_BUS", &path);
        let reader = Reader::open().expect("open a fresh temp bus");
        Bus { publishers: Vec::new(), reader, path, _lock: lock }
    }

    pub fn path(&self) -> &std::path::Path { &self.path }

    /// One plugin instance on `track` publishing `bands` (rms = their power
    /// sum, as a plugin would); returns its index for `republish`.
    pub fn instance(&mut self, track: &str, flags: u32, plugin: &str, bands: &[f32; BANDS]) -> usize {
        let p = Publisher::open(plugin).expect("a free slot on a fresh bus");
        p.set_track(track, flags);
        let rms = rms_of(bands);
        p.publish(48_000.0, rms, rms + 3.0, bands);
        self.publishers.push(p);
        self.publishers.len() - 1
    }

    pub fn republish(&self, i: usize, bands: &[f32; BANDS]) {
        let rms = rms_of(bands);
        self.publishers[i].publish(48_000.0, rms, rms + 3.0, bands);
    }

    /// Drop every instance (frees the slots) so another scene can use the
    /// same bus; the lock is not reentrant, so a second `Bus` would deadlock.
    pub fn clear(&mut self) { self.publishers.clear(); }

    /// `n` bus reads — what the tools collect at 100 ms each. No sleep: the
    /// scene is static between republishes.
    pub fn frames(&self, n: usize) -> Vec<Vec<Snapshot>> {
        (0..n).map(|_| self.reader.read_all()).collect()
    }
}

impl Drop for Bus {
    fn drop(&mut self) {
        self.publishers.clear();
        let _ = std::fs::remove_file(&self.path);
    }
}

/// A spectrum at `level` dB in the given bands, the floor elsewhere.
pub fn spectrum(level: f32, on: &[std::ops::RangeInclusive<usize>]) -> [f32; BANDS] {
    let mut s = [FLOOR_DB; BANDS];
    for r in on { for b in r.clone() { s[b] = level; } }
    s
}

pub fn with_band(mut s: [f32; BANDS], band: usize, level: f32) -> [f32; BANDS] {
    s[band] = level;
    s
}

/// Power sum of the bands: the rms a plugin publishes next to them.
pub fn rms_of(bands: &[f32; BANDS]) -> f32 {
    10.0 * bands.iter().map(|b| 10f32.powf(b / 10.0)).sum::<f32>().log10()
}

/// Per band, the power sum of two spectra: what a master hears from two
/// tracks that add without interacting.
pub fn power_sum(a: &[f32; BANDS], b: &[f32; BANDS]) -> [f32; BANDS] {
    std::array::from_fn(|i| 10.0 * (10f32.powf(a[i] / 10.0) + 10f32.powf(b[i] / 10.0)).log10())
}

pub fn masking(bus: &Bus, prefer: &Prefer, fader: Option<&FaderDb>) -> Result<MaskingOut, String> {
    masking_report(&bus.frames(FRAMES), prefer, fader)
}

pub fn phase(bus: &Bus) -> Result<PhaseOut, String> {
    phase_report(&bus.frames(FRAMES), &Prefer::new(), None, None)
}

// ---- scenes ---------------------------------------------------------------

fn bass() -> [f32; BANDS] { spectrum(-20.0, &[0..=6]) }
fn keys() -> [f32; BANDS] { spectrum(-20.0, &[14..=22]) }

/// Bass in bands 0–6, keys in 14–22, both at −20 dB: nothing to find.
pub fn scene_disjoint(bus: &mut Bus) {
    bus.instance("Bass", 0, "SuperDuper Sampler", &bass());
    bus.instance("Keys", 0, "SuperDuper Piano", &keys());
}

/// The same two, plus the bass at −20 dB and the keys at `keys_db` in band
/// 12. Returns the keys' instance index.
pub fn scene_planted(bus: &mut Bus, keys_db: f32) -> usize {
    bus.instance("Bass", 0, "SuperDuper Sampler", &with_band(bass(), CLASH_BAND, -20.0));
    bus.instance("Keys", 0, "SuperDuper Piano", &with_band(keys(), CLASH_BAND, keys_db))
}

/// Keys with two instances: the Piano at −20 and, after it, an EQ that cut
/// it to −35 — both with band 12; the bass at −20 in band 12.
pub fn scene_two_instances(bus: &mut Bus) {
    bus.instance("Bass", 0, "SuperDuper Sampler", &with_band(bass(), CLASH_BAND, -20.0));
    bus.instance("Keys", 0, "SuperDuper Piano", &with_band(keys(), CLASH_BAND, -20.0));
    bus.instance("Keys", 0, "SuperDuper EQ", &with_band(spectrum(-35.0, &[14..=22]), CLASH_BAND, -35.0));
}

/// Bass and Vox clashing in band 12, a "Vox Reverb" return carrying the
/// vox's spectrum, and a host-flagged master at −10 dB everywhere.
pub fn scene_returns(bus: &mut Bus) {
    let vox = with_band(keys(), CLASH_BAND, -20.0);
    bus.instance("Bass", 0, "SuperDuper Sampler", &with_band(bass(), CLASH_BAND, -20.0));
    bus.instance("Vox", 0, "SuperDuper Vocal", &vox);
    bus.instance("Vox Reverb", 0, "SuperDuper Reverb", &vox);
    bus.instance("Master", TRACK_MASTER, "SuperDuper Spectrum", &spectrum(-10.0, &[0..=31]));
}

/// Kick and bass at −30 dB everywhere, −20 / −23 in band 8; the master is
/// their power sum with `shift_db` added per band (`shift(b)`).
pub fn scene_phase(bus: &mut Bus, shift: impl Fn(usize) -> f32) {
    let kick = with_band(spectrum(-30.0, &[0..=31]), CANCEL_BAND, -20.0);
    let bass = with_band(spectrum(-30.0, &[0..=31]), CANCEL_BAND, -23.0);
    let sum = power_sum(&kick, &bass);
    let master: [f32; BANDS] = std::array::from_fn(|b| sum[b] + shift(b));
    bus.instance("Kick", 0, "SuperDuper Drum", &kick);
    bus.instance("Bass", 0, "SuperDuper Sampler", &bass);
    bus.instance("Master", TRACK_MASTER, "SuperDuper Spectrum", &master);
}

// ---- judges ---------------------------------------------------------------

fn hz(band: usize) -> String {
    let e = band_edges();
    format!("{:.0}–{:.0} Hz", e[band], e[band + 1])
}

fn clash_str(c: &ClashOut) -> String {
    format!("{}×{} {:.0}–{:.0} Hz share {:.2} ({} +{:.1} dB)", c.a, c.b, c.low_hz, c.high_hz, c.share, c.louder, c.by_db)
}

fn describe(out: &MaskingOut) -> String {
    let clashes: Vec<String> = out.clashes.iter().map(clash_str).collect();
    let buried: Vec<String> = out.buried.iter().map(|b| format!("{} under {} {:.0}–{:.0} Hz share {:.2}", b.quiet, b.under, b.low_hz, b.high_hz, b.share)).collect();
    format!("tracks {:?}, clashes [{}], buried [{}]", out.tracks, clashes.join("; "), buried.join("; "))
}

fn level(out: &MaskingOut, track: &str) -> Result<f32, String> {
    out.levels.iter().find(|l| l.track == track).map(|l| l.rms_db).ok_or_else(|| format!("no level for {track}: {}", describe(out)))
}

fn near(got: f32, want: f32, tol: f32, what: &str) -> Result<(), String> {
    if (got - want).abs() <= tol { Ok(()) } else { Err(format!("{what} {got:.2}, expected {want:.2} ± {tol}")) }
}

/// No clash, no buried, and exactly these (non-bus) tracks.
pub fn judge_nothing(out: &MaskingOut, tracks: &[&str]) -> Result<String, String> {
    if !out.clashes.is_empty() || !out.buried.is_empty() {
        return Err(format!("expected nothing, got {}", describe(out)));
    }
    if out.tracks != tracks {
        return Err(format!("expected tracks {tracks:?}, got {:?}", out.tracks));
    }
    Ok(format!("0 clashes, 0 buried, tracks {:?}", out.tracks))
}

/// Exactly one clash: this pair, this band, at least this share, nothing buried.
pub fn judge_one_clash(out: &MaskingOut, a: &str, b: &str, band: usize, min_share: f32) -> Result<String, String> {
    let [c] = out.clashes.as_slice() else {
        return Err(format!("expected exactly 1 clash {a}×{b} {}, got {}", hz(band), describe(out)));
    };
    let edges = band_edges();
    if (c.a.as_str(), c.b.as_str()) != (a, b) || c.low_hz != edges[band] || c.share < min_share {
        return Err(format!("expected {a}×{b} {} share ≥ {min_share}, got {}", hz(band), clash_str(c)));
    }
    if !out.buried.is_empty() {
        return Err(format!("expected nothing buried next to the clash, got {}", describe(out)));
    }
    Ok(clash_str(c))
}

/// No clash, exactly one buried entry: `quiet` under `under` in this band.
pub fn judge_one_buried(out: &MaskingOut, quiet: &str, under: &str, band: usize) -> Result<String, String> {
    if !out.clashes.is_empty() {
        return Err(format!("expected no clash, got {}", describe(out)));
    }
    let [b] = out.buried.as_slice() else {
        return Err(format!("expected exactly 1 buried ({quiet} under {under} {}), got {}", hz(band), describe(out)));
    };
    let edges = band_edges();
    if (b.quiet.as_str(), b.under.as_str()) != (quiet, under) || b.low_hz != edges[band] || b.share < 0.95 {
        return Err(format!("expected {quiet} under {under} {} share ≥ 0.95, got {} under {} {:.0}–{:.0} Hz share {:.2}", hz(band), b.quiet, b.under, b.low_hz, b.high_hz, b.share));
    }
    Ok(format!("{} under {} {:.0}–{:.0} Hz share {:.2}", b.quiet, b.under, b.low_hz, b.high_hz, b.share))
}

/// Exactly one cancelling band with this deficit and these suspects, in order.
pub fn judge_one_cancel(out: &PhaseOut, band: usize, deficit_db: f32, suspects: &[&str]) -> Result<String, String> {
    let [c] = out.cancelling.as_slice() else {
        return Err(format!("expected exactly 1 cancelling band ({}), got {}", hz(band), describe_phase(out)));
    };
    let edges = band_edges();
    if c.low_hz != edges[band] { return Err(format!("expected {}, got {}", hz(band), describe_phase(out))); }
    near(c.deficit_db, deficit_db, 0.1, "deficit")?;
    if c.tracks != suspects { return Err(format!("expected suspects {suspects:?}, got {:?}", c.tracks)); }
    if !out.not_on_bus_hz.is_empty() { return Err(format!("nothing is off the bus, yet not_on_bus_hz = {:?}", out.not_on_bus_hz)); }
    Ok(format!("{} deficit {:.2} dB suspects {:?}, master {}", hz(band), c.deficit_db, c.tracks, out.master))
}

/// No cancelling band and every deficit within 0.05 dB of zero.
pub fn judge_flat(out: &PhaseOut) -> Result<String, String> {
    if !out.cancelling.is_empty() { return Err(format!("expected no cancelling band, got {}", describe_phase(out))); }
    let worst = out.deficit_db.iter().copied().fold(0f32, |m, d| if d.abs() > m.abs() { d } else { m });
    near(worst, 0.0, 0.05, "worst deficit")?;
    Ok(format!("0 cancelling bands, worst deficit {worst:+.3} dB, master {}", out.master))
}

fn describe_phase(out: &PhaseOut) -> String {
    let c: Vec<String> = out.cancelling.iter().map(|c| format!("{:.0}–{:.0} Hz {:+.2} dB {:?}", c.low_hz, c.high_hz, c.deficit_db, c.tracks)).collect();
    format!("cancelling [{}], not_on_bus_hz {:?}", c.join("; "), out.not_on_bus_hz)
}

fn expect_err<T>(r: Result<T, String>, starts_with: &str) -> Result<String, String> {
    match r {
        Err(e) if e.starts_with(starts_with) => Ok(format!("error: {}…", e.chars().take(60).collect::<String>())),
        Err(e) => Err(format!("wrong error: {e}")),
        Ok(_) => Err(format!("a report came back where an error starting '{starts_with}' was due")),
    }
}

// ---- the list -------------------------------------------------------------

pub struct Control {
    /// Short handle, what a test pins.
    pub id: &'static str,
    /// Scene → what a correct meter must say.
    pub name: &'static str,
    pub run: fn(&mut Bus) -> Result<String, String>,
}

fn none() -> Prefer { Prefer::new() }

fn masking_controls() -> Vec<Control> {
    vec![
        Control { id: "null-alone", name: "one track alone → no clash, no buried", run: |bus| {
            bus.instance("Bass", 0, "SuperDuper Sampler", &bass());
            judge_nothing(&masking(bus, &none(), None)?, &["Bass"])
        } },
        Control { id: "null-disjoint", name: "bass 0–6, keys 14–22, both −20 dB → no clash, no buried", run: |bus| {
            scene_disjoint(bus);
            judge_nothing(&masking(bus, &none(), None)?, &["Bass", "Keys"])
        } },
        Control { id: "planted-clash", name: "both −20 dB in band 12 (267–331 Hz), every frame → 1 clash Bass×Keys there, share ≥ 0.95", run: |bus| {
            scene_planted(bus, -20.0);
            judge_one_clash(&masking(bus, &none(), None)?, "Bass", "Keys", CLASH_BAND, 0.95)
        } },
        Control { id: "planted-half", name: "the same clash in half the frames → share 0.50", run: |bus| {
            let keys_idx = scene_planted(bus, -20.0);
            let mut frames = bus.frames(FRAMES / 2);
            bus.republish(keys_idx, &keys());
            frames.extend(bus.frames(FRAMES / 2));
            let out = masking_report(&frames, &none(), None)?;
            let s = judge_one_clash(&out, "Bass", "Keys", CLASH_BAND, 0.45)?;
            near(out.clashes[0].share, 0.5, 0.02, "share")?;
            // Equal parts while they clash: no side louder — a whole-run mean
            // said "Bass +3.0 dB" because it counted the keys' silent frames.
            near(out.clashes[0].by_db, 0.0, 0.05, "by_db")?;
            if !out.clashes[0].louder.is_empty() { return Err(format!("louder should be empty at equal level, got {:?}", out.clashes[0].louder)); }
            if out.tracks != ["Bass", "Keys"] { return Err(format!("expected tracks [Bass, Keys] though Keys stopped early, got {}", describe(&out))); }
            Ok(format!("{s}, by {:+.1} dB, tracks {:?}", out.clashes[0].by_db, out.tracks))
        } },
        Control { id: "planted-buried", name: "keys 12 dB under the bass in band 12 → no clash, 1 buried: Keys under Bass there", run: |bus| {
            scene_planted(bus, -32.0);
            judge_one_buried(&masking(bus, &none(), None)?, "Keys", "Bass", CLASH_BAND)
        } },
        Control { id: "prefer-default", name: "Keys = Piano −20 + EQ −35 after it, no prefer → the loud Piano is measured (clash, Keys −10 dB)", run: |bus| {
            scene_two_instances(bus);
            let out = masking(bus, &none(), None)?;
            let s = judge_one_clash(&out, "Bass", "Keys", CLASH_BAND, 0.95)?;
            near(level(&out, "Keys")?, -10.0, 0.05, "Keys level")?;
            Ok(format!("{s}, Keys {:.2} dB", level(&out, "Keys")?))
        } },
        Control { id: "prefer-eq", name: "the same with prefer {Keys: EQ} → the EQ is measured (buried 15 dB under, Keys −25 dB)", run: |bus| {
            scene_two_instances(bus);
            let out = masking(bus, &Prefer::from([("Keys".to_string(), "EQ".to_string())]), None)?;
            let s = judge_one_buried(&out, "Keys", "Bass", CLASH_BAND)?;
            near(level(&out, "Keys")?, -25.0, 0.05, "Keys level")?;
            Ok(format!("{s}, Keys {:.2} dB", level(&out, "Keys")?))
        } },
        Control { id: "fader", name: "planted clash with fader_db {Keys: −12} → the clash becomes a burial, Keys level −12 dB lower", run: |bus| {
            scene_planted(bus, -20.0);
            let before = level(&masking(bus, &none(), None)?, "Keys")?;
            let out = masking(bus, &none(), Some(&FaderDb::from([("Keys".to_string(), -12.0)])))?;
            let s = judge_one_buried(&out, "Keys", "Bass", CLASH_BAND)?;
            near(level(&out, "Keys")? - before, -12.0, 0.05, "Keys level change")?;
            Ok(format!("{s}, Keys {before:.2} → {:.2} dB", level(&out, "Keys")?))
        } },
        Control { id: "fader-partial", name: "\"Gtr\" and \"Gtr DI\" clashing, fader_db {Gtr: −12} → only Gtr moves (Gtr buried under Gtr DI, Gtr DI level unchanged)", run: |bus| {
            bus.instance("Gtr", 0, "SuperDuper NAM", &with_band(bass(), CLASH_BAND, -20.0));
            bus.instance("Gtr DI", 0, "SuperDuper EQ", &with_band(keys(), CLASH_BAND, -20.0));
            let flat = masking(bus, &none(), None)?;
            let (di_before, gtr_before) = (level(&flat, "Gtr DI")?, level(&flat, "Gtr")?);
            let out = masking(bus, &none(), Some(&FaderDb::from([("Gtr".to_string(), -12.0)])))?;
            let s = judge_one_buried(&out, "Gtr", "Gtr DI", CLASH_BAND)?;
            near(level(&out, "Gtr DI")? - di_before, 0.0, 0.05, "Gtr DI level change")?;
            near(level(&out, "Gtr")? - gtr_before, -12.0, 0.05, "Gtr level change")?;
            Ok(format!("{s}, Gtr DI {di_before:.2} dB unchanged, Gtr {gtr_before:.2} → {:.2} dB", level(&out, "Gtr")?))
        } },
        Control { id: "returns", name: "\"Vox Reverb\" (same spectrum as Vox) and a TRACK_MASTER slot → never in a pair; the Bass×Vox clash still found", run: |bus| {
            scene_returns(bus);
            let out = masking(bus, &none(), None)?;
            if out.tracks != ["Bass", "Vox"] { return Err(format!("expected tracks [Bass, Vox], got {}", describe(&out))); }
            judge_one_clash(&out, "Bass", "Vox", CLASH_BAND, 0.95)
        } },
        Control { id: "empty-masking", name: "no named track (one unnamed instance; an empty bus) → an error, not an empty report", run: |bus| {
            let a = expect_err(masking(bus, &none(), None), "nothing to measure")?;
            bus.instance("", 0, "SuperDuper Spectrum", &spectrum(-20.0, &[0..=31]));
            let b = expect_err(masking(bus, &none(), None), "nothing to measure")?;
            Ok(format!("empty bus: {a}; unnamed instance: {b}"))
        } },
    ]
}

fn phase_controls() -> Vec<Control> {
    vec![
        Control { id: "phase-cancel", name: "master = Kick+Bass in power, 9 dB short in band 8 (113–140 Hz) → that band alone, deficit −9 dB, suspects [Kick, Bass]", run: |bus| {
            scene_phase(bus, |b| if b == CANCEL_BAND { -9.0 } else { 0.0 });
            judge_one_cancel(&phase(bus)?, CANCEL_BAND, -9.0, &["Kick", "Bass"])
        } },
        Control { id: "phase-sum", name: "master = the sum everywhere → no cancelling band", run: |bus| {
            scene_phase(bus, |_| 0.0);
            judge_flat(&phase(bus)?)
        } },
        Control { id: "phase-limiter", name: "master 3 dB short in every band (a limiter) → no cancelling band", run: |bus| {
            scene_phase(bus, |_| -3.0);
            judge_flat(&phase(bus)?)
        } },
        Control { id: "empty-phase", name: "tracks without a master, a master without named tracks → errors", run: |bus| {
            scene_disjoint(bus);
            let a = expect_err(phase(bus), "no SuperDuper plugin on the master")?;
            bus.clear();
            bus.instance("Master", TRACK_MASTER, "SuperDuper Spectrum", &spectrum(-10.0, &[0..=31]));
            let b = expect_err(phase(bus), "no named tracks")?;
            Ok(format!("no master: {a}; no tracks: {b}"))
        } },
    ]
}

pub fn all() -> Vec<Control> {
    let mut v = masking_controls();
    v.extend(phase_controls());
    v
}

/// Run one control by id; the measured line on success.
pub fn check(id: &str) -> Result<String, String> {
    let c = all().into_iter().find(|c| c.id == id).ok_or_else(|| format!("no control '{id}'"))?;
    let mut bus = Bus::new(id);
    (c.run)(&mut bus)
}

/// Run every control, print the table, say whether all passed.
pub fn run(w: &mut dyn std::io::Write) -> std::io::Result<bool> {
    let controls = all();
    let mut failed = 0;
    writeln!(w, "sdsp-bus controls — known answers planted on a temp bus through real publishers; the live bus is not opened")?;
    writeln!(w)?;
    for c in &controls {
        let res = (c.run)(&mut Bus::new(c.id));
        match res {
            Ok(m) => writeln!(w, "PASS  {}\n      measured: {m}", c.name)?,
            Err(e) => { failed += 1; writeln!(w, "FAIL  {}\n      {e}", c.name)?; }
        }
    }
    writeln!(w)?;
    let n = controls.len();
    if failed == 0 {
        writeln!(w, "{n} of {n} controls passed: the meter finds what was planted and nothing where nothing is. A zero from the live bus can be read as a zero.")?;
    } else {
        writeln!(w, "{failed} of {n} controls FAILED: do not read anything from the live bus until this is fixed.")?;
    }
    Ok(failed == 0)
}

/// `sdsp-bus controls`: exit 0 when every control passes, 1 otherwise.
pub fn main() -> i32 {
    let mut out = std::io::stdout().lock();
    match run(&mut out) {
        Ok(true) => 0,
        Ok(false) => 1,
        Err(e) => { eprintln!("controls: {e}"); 2 }
    }
}
