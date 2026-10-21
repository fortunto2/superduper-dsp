//! Typed regions: the opcode strings from `parse` compiled into numbers the
//! engine can read on the audio thread without parsing anything.
//!
//! Supported is the subset real libraries use (measured on Salamander Grand
//! V3: 5 header kinds, ~45 opcodes). Anything else is collected in
//! `Instrument::unsupported` so a pack that sounds wrong says why instead of
//! failing silently.

use super::parse::{get, parse_key, Opcodes, SfzFile};

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum Trigger {
    #[default]
    Attack,
    Release,
    /// Only when no other key is held.
    First,
    /// Only when another key is held.
    Legato,
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum LoopMode {
    #[default]
    NoLoop,
    OneShot,
    Continuous,
    Sustain,
}

/// What a `<target>_onccN` opcode modulates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ModTarget {
    /// Multiplicative, in percent (sfizz semantics: `amplitude_oncc7=100`
    /// with CC7 at full = ×1.0, at zero = silence).
    Amplitude,
    /// Additive dB.
    Volume,
    /// Additive, −100..100.
    Pan,
    /// Additive percent.
    AmpVeltrack,
    /// Additive seconds.
    AmpegAttack,
    AmpegDecay,
    AmpegRelease,
    /// Additive percent.
    AmpegSustain,
    /// Additive frames.
    Offset,
    /// Additive cents.
    Tune,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CcMod {
    pub target: ModTarget,
    pub cc: u8,
    pub depth: f32,
    pub curve: u8,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CcRange {
    pub cc: u8,
    /// 0..127 inclusive, in CC units (fractional for hdcc).
    pub lo: f32,
    pub hi: f32,
}

impl CcRange {
    pub fn contains(&self, value01: f32) -> bool {
        let v = value01 * 127.0;
        v >= self.lo - 1e-3 && v <= self.hi + 1e-3
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Region {
    /// Path relative to the `.sfz` directory, `default_path` already applied,
    /// separators normalised to `/`.
    pub sample: String,
    /// Index into `Instrument::samples`, filled in by the loader.
    pub sample_index: usize,
    pub lokey: i32,
    pub hikey: i32,
    pub pitch_keycenter: i32,
    /// Cents per key.
    pub pitch_keytrack: f32,
    pub lovel: u8,
    pub hivel: u8,
    pub lorand: f32,
    pub hirand: f32,
    pub trigger: Trigger,
    /// All must hold for the region to play (`loccN` / `hiccN`).
    pub cc_conditions: Vec<CcRange>,
    /// `on_loccN` / `on_hiccN`: the region is started by a CC entering this
    /// range rather than by a key.
    pub on_cc: Option<CcRange>,
    /// Keyswitch this region answers to (`sw_last`).
    pub sw_last: Option<i32>,
    pub transpose: i32,
    pub tune_cents: f32,
    pub volume_db: f32,
    pub amplitude_pct: f32,
    pub pan: f32,
    pub amp_veltrack_pct: f32,
    pub mods: Vec<CcMod>,
    pub ampeg_delay: f32,
    pub ampeg_attack: f32,
    pub ampeg_hold: f32,
    pub ampeg_decay: f32,
    pub ampeg_sustain_pct: f32,
    pub ampeg_release: f32,
    /// dB lost per second the key was held (release triggers).
    pub rt_decay_db: f32,
    pub group: i64,
    pub off_by: Option<i64>,
    /// Fade length when this region's voice is cut by `off_by` / polyphony.
    pub off_time: f32,
    pub polyphony: Option<u32>,
    pub note_polyphony: Option<u32>,
    pub offset: u32,
    pub end: Option<u32>,
    pub loop_mode: Option<LoopMode>,
    pub loop_start: Option<u32>,
    pub loop_end: Option<u32>,
}

impl Default for Region {
    fn default() -> Self {
        Self {
            sample: String::new(),
            sample_index: 0,
            lokey: 0,
            hikey: 127,
            pitch_keycenter: 60,
            pitch_keytrack: 100.0,
            lovel: 1,
            hivel: 127,
            lorand: 0.0,
            hirand: 1.0,
            trigger: Trigger::Attack,
            cc_conditions: Vec::new(),
            on_cc: None,
            sw_last: None,
            transpose: 0,
            tune_cents: 0.0,
            volume_db: 0.0,
            amplitude_pct: 100.0,
            pan: 0.0,
            amp_veltrack_pct: 100.0,
            mods: Vec::new(),
            ampeg_delay: 0.0,
            ampeg_attack: 0.0,
            ampeg_hold: 0.0,
            ampeg_decay: 0.0,
            ampeg_sustain_pct: 100.0,
            ampeg_release: 0.001,
            rt_decay_db: 0.0,
            group: 0,
            off_by: None,
            off_time: 0.006,
            polyphony: None,
            note_polyphony: None,
            offset: 0,
            end: None,
            loop_mode: None,
            loop_start: None,
            loop_end: None,
        }
    }
}

impl Region {
    pub fn key_matches(&self, key: i32) -> bool {
        key >= self.lokey && key <= self.hikey
    }

    pub fn vel_matches(&self, vel: u8) -> bool {
        vel >= self.lovel && vel <= self.hivel
    }

    pub fn rand_matches(&self, r: f32) -> bool {
        // Half-open like sfizz, except the top bucket keeps 1.0.
        r >= self.lorand && (r < self.hirand || (self.hirand >= 1.0 && r <= 1.0))
    }

    pub fn effective_loop_mode(&self) -> LoopMode {
        match self.loop_mode {
            Some(m) => m,
            // SFZ: a region with loop points defaults to continuous.
            None if self.loop_start.is_some() && self.loop_end.is_some() => LoopMode::Continuous,
            None => LoopMode::NoLoop,
        }
    }
}

/// 128-point response curves, indexed by CC value. Indices 0..=6 are the
/// SFZ built-ins; `<curve>` blocks add or replace by `curve_index`.
#[derive(Debug, Clone)]
pub struct Curves {
    tables: Vec<Option<Box<[f32; 128]>>>,
}

impl Default for Curves {
    fn default() -> Self {
        let mut c = Curves { tables: Vec::new() };
        for idx in 0..=6u8 {
            let mut t = [0f32; 128];
            for (i, v) in t.iter_mut().enumerate() {
                let x = i as f32 / 127.0;
                *v = match idx {
                    0 => x,
                    1 => 2.0 * x - 1.0,
                    2 => 1.0 - x,
                    3 => 1.0 - 2.0 * x,
                    4 => x * x,
                    5 => x.sqrt(),
                    _ => (1.0 - x).sqrt(),
                };
            }
            c.set(idx as usize, t);
        }
        c
    }
}

impl Curves {
    fn set(&mut self, idx: usize, t: [f32; 128]) {
        if self.tables.len() <= idx { self.tables.resize(idx + 1, None); }
        self.tables[idx] = Some(Box::new(t));
    }

    /// Evaluate at a normalised CC value. Unknown curve index → linear.
    pub fn eval(&self, curve: u8, x01: f32) -> f32 {
        let x = x01.clamp(0.0, 1.0);
        match self.tables.get(curve as usize).and_then(|t| t.as_deref()) {
            Some(t) => {
                let pos = x * 127.0;
                let i = (pos as usize).min(126);
                let f = pos - i as f32;
                t[i] + (t[i + 1] - t[i]) * f
            }
            None => x,
        }
    }

    /// `<curve> curve_index=N v000=.. v127=..` — undefined points are linearly
    /// interpolated between the defined ones (endpoints default to 0 and 1).
    pub fn add_block(&mut self, ops: &Opcodes) {
        let Some(idx) = get(ops, "curve_index").and_then(|v| v.parse::<usize>().ok()) else { return };
        let mut pts: Vec<(usize, f32)> = ops
            .iter()
            .filter_map(|(k, v)| {
                let n = k.strip_prefix('v')?.parse::<usize>().ok()?;
                (n <= 127).then_some((n, v.parse::<f32>().ok()?))
            })
            .collect();
        if !pts.iter().any(|p| p.0 == 0) { pts.push((0, 0.0)); }
        if !pts.iter().any(|p| p.0 == 127) { pts.push((127, 1.0)); }
        pts.sort_by_key(|p| p.0);
        pts.dedup_by_key(|p| p.0);
        let mut t = [0f32; 128];
        for w in pts.windows(2) {
            let ((a, va), (b, vb)) = (w[0], w[1]);
            for (i, slot) in t.iter_mut().enumerate().take(b + 1).skip(a) {
                let f = if b == a { 0.0 } else { (i - a) as f32 / (b - a) as f32 };
                *slot = va + (vb - va) * f;
            }
        }
        self.set(idx, t);
    }
}

/// Everything instrument-wide: regions plus the `<control>` defaults and the
/// keyswitch range. Samples are attached by the loader.
#[derive(Debug, Clone, Default)]
pub struct InstrumentDef {
    pub regions: Vec<Region>,
    pub curves: Curves,
    /// CC values the instrument starts with (`set_ccN`/`set_hdccN`), 0..1.
    pub cc_defaults: Vec<(u8, f32)>,
    /// Keys that switch articulations instead of sounding.
    pub sw_range: Option<(i32, i32)>,
    pub sw_default: Option<i32>,
    /// Distinct sample paths, in first-use order; `Region::sample_index`
    /// points into this.
    pub sample_paths: Vec<String>,
    /// `opcode` names we parsed but do not implement, deduplicated.
    pub unsupported: Vec<String>,
}

/// Split `amplitude_oncc7` → ("amplitude_oncc", 7). Returns None when the
/// key does not end in digits after `prefix`.
fn cc_suffix<'a>(key: &'a str, marker: &str) -> Option<(&'a str, u8)> {
    let pos = key.find(marker)?;
    let num = key[pos + marker.len()..].parse::<u8>().ok()?;
    (num < 128).then_some((&key[..pos], num))
}

fn f(v: &str) -> Option<f32> { v.trim().parse::<f32>().ok() }

fn compile_region(ops: &Opcodes, default_path: &str, unsupported: &mut Vec<String>) -> Region {
    let mut r = Region::default();
    let mut on_lo: Vec<(u8, f32)> = Vec::new();
    let mut on_hi: Vec<(u8, f32)> = Vec::new();
    let mut lo: Vec<(u8, f32)> = Vec::new();
    let mut hi: Vec<(u8, f32)> = Vec::new();
    // Curves arrive as their own opcode, possibly before or after the depth.
    let mut curves: Vec<(String, u8, u8)> = Vec::new();
    let mut keycenter_set = false;

    for (k, v) in ops {
        let k = k.as_str();
        match k {
            "sample" => {
                let s = v.trim().replace('\\', "/");
                r.sample = if s.starts_with('*') { s } else { format!("{default_path}{s}") };
            }
            "key" => {
                if let Some(n) = parse_key(v) {
                    r.lokey = n;
                    r.hikey = n;
                    if !keycenter_set { r.pitch_keycenter = n; }
                }
            }
            "lokey" => if let Some(n) = parse_key(v) { r.lokey = n },
            "hikey" => if let Some(n) = parse_key(v) { r.hikey = n },
            "pitch_keycenter" => if let Some(n) = parse_key(v) { r.pitch_keycenter = n; keycenter_set = true },
            "pitch_keytrack" => if let Some(x) = f(v) { r.pitch_keytrack = x },
            "lovel" => if let Some(x) = f(v) { r.lovel = x.clamp(0.0, 127.0) as u8 },
            "hivel" => if let Some(x) = f(v) { r.hivel = x.clamp(0.0, 127.0) as u8 },
            "lorand" => if let Some(x) = f(v) { r.lorand = x },
            "hirand" => if let Some(x) = f(v) { r.hirand = x },
            "trigger" => r.trigger = match v.trim() {
                "release" | "release_key" => Trigger::Release,
                "first" => Trigger::First,
                "legato" => Trigger::Legato,
                _ => Trigger::Attack,
            },
            "sw_last" => r.sw_last = parse_key(v),
            "transpose" => if let Some(x) = f(v) { r.transpose = x as i32 },
            "tune" | "pitch" => if let Some(x) = f(v) { r.tune_cents = x },
            "volume" | "gain" => if let Some(x) = f(v) { r.volume_db = x },
            "amplitude" => if let Some(x) = f(v) { r.amplitude_pct = x },
            "pan" => if let Some(x) = f(v) { r.pan = x },
            "amp_veltrack" => if let Some(x) = f(v) { r.amp_veltrack_pct = x },
            "ampeg_delay" => if let Some(x) = f(v) { r.ampeg_delay = x },
            "ampeg_attack" => if let Some(x) = f(v) { r.ampeg_attack = x },
            "ampeg_hold" => if let Some(x) = f(v) { r.ampeg_hold = x },
            "ampeg_decay" => if let Some(x) = f(v) { r.ampeg_decay = x },
            "ampeg_sustain" => if let Some(x) = f(v) { r.ampeg_sustain_pct = x },
            "ampeg_release" => if let Some(x) = f(v) { r.ampeg_release = x },
            "rt_decay" => if let Some(x) = f(v) { r.rt_decay_db = x },
            "group" => if let Ok(x) = v.trim().parse::<i64>() { r.group = x },
            "off_by" => r.off_by = v.trim().parse::<i64>().ok(),
            "off_time" => if let Some(x) = f(v) { r.off_time = x.max(0.0) },
            "polyphony" => r.polyphony = v.trim().parse::<u32>().ok(),
            "note_polyphony" => r.note_polyphony = v.trim().parse::<u32>().ok(),
            "offset" => if let Some(x) = f(v) { r.offset = x.max(0.0) as u32 },
            "end" => r.end = f(v).map(|x| x.max(0.0) as u32),
            "loop_mode" | "loopmode" => r.loop_mode = match v.trim() {
                "one_shot" => Some(LoopMode::OneShot),
                "loop_continuous" => Some(LoopMode::Continuous),
                "loop_sustain" => Some(LoopMode::Sustain),
                _ => Some(LoopMode::NoLoop),
            },
            "loop_start" | "loopstart" => r.loop_start = f(v).map(|x| x.max(0.0) as u32),
            "loop_end" | "loopend" => r.loop_end = f(v).map(|x| x.max(0.0) as u32),
            // Labels and UI-only opcodes: harmless to ignore.
            _ if k.ends_with("_label") || k == "default_path" || k.starts_with("label_cc") => {}
            _ => {
                if let Some((_, n)) = cc_suffix(k, "on_locc") { if let Some(x) = f(v) { on_lo.push((n, x)); } continue; }
                if let Some((_, n)) = cc_suffix(k, "on_hicc") { if let Some(x) = f(v) { on_hi.push((n, x)); } continue; }
                if let Some((p, n)) = cc_suffix(k, "_curvecc") { if let Ok(c) = v.trim().parse::<u8>() { curves.push((p.to_string(), n, c)); } continue; }
                if let Some((p, n)) = cc_suffix(k, "_oncc").or_else(|| cc_suffix(k, "_cc")) {
                    let target = match p {
                        "amplitude" => Some(ModTarget::Amplitude),
                        "volume" | "gain" => Some(ModTarget::Volume),
                        "pan" => Some(ModTarget::Pan),
                        "amp_veltrack" => Some(ModTarget::AmpVeltrack),
                        "ampeg_attack" => Some(ModTarget::AmpegAttack),
                        "ampeg_decay" => Some(ModTarget::AmpegDecay),
                        "ampeg_release" => Some(ModTarget::AmpegRelease),
                        "ampeg_sustain" => Some(ModTarget::AmpegSustain),
                        "offset" => Some(ModTarget::Offset),
                        "tune" | "pitch" => Some(ModTarget::Tune),
                        _ => None,
                    };
                    match (target, f(v)) {
                        (Some(target), Some(depth)) => {
                            r.mods.retain(|m| !(m.target == target && m.cc == n));
                            r.mods.push(CcMod { target, cc: n, depth, curve: 0 });
                        }
                        _ => unsupported.push(k.to_string()),
                    }
                    continue;
                }
                if let Some((p, n)) = cc_suffix(k, "locc") { if p.is_empty() { if let Some(x) = f(v) { lo.push((n, x)); } continue; } }
                if let Some((p, n)) = cc_suffix(k, "hicc") { if p.is_empty() { if let Some(x) = f(v) { hi.push((n, x)); } continue; } }
                unsupported.push(k.to_string());
            }
        }
    }

    for (prefix, cc, curve) in curves {
        for m in r.mods.iter_mut().filter(|m| m.cc == cc) {
            let matches = match m.target {
                ModTarget::Amplitude => prefix == "amplitude",
                ModTarget::Volume => prefix == "volume" || prefix == "gain",
                ModTarget::Pan => prefix == "pan",
                ModTarget::AmpVeltrack => prefix == "amp_veltrack",
                ModTarget::AmpegAttack => prefix == "ampeg_attack",
                ModTarget::AmpegDecay => prefix == "ampeg_decay",
                ModTarget::AmpegRelease => prefix == "ampeg_release",
                ModTarget::AmpegSustain => prefix == "ampeg_sustain",
                ModTarget::Offset => prefix == "offset",
                ModTarget::Tune => prefix == "tune" || prefix == "pitch",
            };
            if matches { m.curve = curve; }
        }
    }

    let range = |los: &[(u8, f32)], his: &[(u8, f32)]| -> Vec<CcRange> {
        let mut ccs: Vec<u8> = los.iter().chain(his).map(|p| p.0).collect();
        ccs.sort_unstable();
        ccs.dedup();
        ccs.into_iter()
            .map(|cc| CcRange {
                cc,
                lo: los.iter().rev().find(|p| p.0 == cc).map_or(0.0, |p| p.1),
                hi: his.iter().rev().find(|p| p.0 == cc).map_or(127.0, |p| p.1),
            })
            .collect()
    };
    r.cc_conditions = range(&lo, &hi);
    r.on_cc = range(&on_lo, &on_hi).into_iter().next();
    r
}

/// Compile a parsed file. Regions without a `sample` are dropped (sfizz does
/// the same); `*sine`-style generators are dropped too, for now.
pub fn compile(file: &SfzFile) -> InstrumentDef {
    let mut def = InstrumentDef::default();
    let default_path = get(&file.control, "default_path")
        .map(|p| {
            let p = p.trim().replace('\\', "/");
            if p.is_empty() || p.ends_with('/') { p } else { format!("{p}/") }
        })
        .unwrap_or_default();

    for (k, v) in &file.control {
        if let Some((p, n)) = cc_suffix(k, "set_cc") {
            if p.is_empty() { if let Some(x) = f(v) { def.cc_defaults.push((n, (x / 127.0).clamp(0.0, 1.0))); } }
        } else if let Some((p, n)) = cc_suffix(k, "set_hdcc") {
            if p.is_empty() { if let Some(x) = f(v) { def.cc_defaults.push((n, x.clamp(0.0, 1.0))); } }
        }
    }
    for c in &file.curves { def.curves.add_block(c); }

    let mut unsupported = Vec::new();
    for ops in &file.regions {
        // Instrument-wide keyswitch range lives on regions in practice
        // (Salamander sets it in <global>), so read it from there.
        if let (Some(lo), Some(hi)) = (get(ops, "sw_lokey").and_then(parse_key), get(ops, "sw_hikey").and_then(parse_key)) {
            def.sw_range = Some((lo, hi));
        }
        if def.sw_default.is_none() {
            def.sw_default = get(ops, "sw_default").and_then(parse_key);
        }
        let mut r = compile_region(ops, &default_path, &mut unsupported);
        if r.sample.is_empty() || r.sample.starts_with('*') { continue; }
        r.sample_index = match def.sample_paths.iter().position(|p| *p == r.sample) {
            Some(i) => i,
            None => { def.sample_paths.push(r.sample.clone()); def.sample_paths.len() - 1 }
        };
        def.regions.push(r);
    }
    unsupported.retain(|k| !matches!(k.as_str(), "sw_lokey" | "sw_hikey" | "sw_default"));
    unsupported.sort();
    unsupported.dedup();
    def.unsupported = unsupported;
    def
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sfz::parse::parse_sfz;

    fn def(text: &str) -> InstrumentDef {
        let mut read = |_: &str| None;
        compile(&parse_sfz(text, &mut read).expect("parse"))
    }

    #[test]
    fn key_sets_range_and_center_and_note_names_work() {
        let d = def("<region> sample=a.wav key=c4 <region> sample=b.wav lokey=a0 hikey=b0 pitch_keycenter=a#0");
        assert_eq!((d.regions[0].lokey, d.regions[0].hikey, d.regions[0].pitch_keycenter), (60, 60, 60));
        assert_eq!((d.regions[1].lokey, d.regions[1].hikey, d.regions[1].pitch_keycenter), (21, 23, 22));
    }

    #[test]
    fn default_path_and_shared_sample_index() {
        let d = def("<control> default_path=Samples <region> sample=a.flac <region> sample=a.flac <region> sample=b.flac");
        assert_eq!(d.regions[0].sample, "Samples/a.flac");
        assert_eq!(d.sample_paths.len(), 2);
        assert_eq!(d.regions[1].sample_index, 0);
        assert_eq!(d.regions[2].sample_index, 1);
    }

    #[test]
    fn cc_mods_with_curves_and_conditions() {
        let d = def("<control> set_cc7=100 set_hdcc10=0.5 <global> amplitude_oncc7=100 amplitude_curvecc7=4 locc20=1 on_locc64=127 on_hicc64=127 <region> sample=a.wav");
        let r = &d.regions[0];
        assert_eq!(r.mods, vec![CcMod { target: ModTarget::Amplitude, cc: 7, depth: 100.0, curve: 4 }]);
        assert_eq!(r.cc_conditions, vec![CcRange { cc: 20, lo: 1.0, hi: 127.0 }]);
        assert_eq!(r.on_cc, Some(CcRange { cc: 64, lo: 127.0, hi: 127.0 }));
        assert!(d.cc_defaults.contains(&(7, 100.0 / 127.0)));
        assert!(d.cc_defaults.contains(&(10, 0.5)));
    }

    #[test]
    fn unknown_opcodes_are_reported_not_hidden() {
        let d = def("<region> sample=a.wav fil_type=lpf_2p cutoff=500");
        assert_eq!(d.unsupported, vec!["cutoff".to_string(), "fil_type".to_string()]);
    }

    #[test]
    fn custom_curve_interpolates_between_points() {
        let d = def("<curve> curve_index=7 v000=0 v001=1 v127=1");
        assert_eq!(d.curves.eval(7, 0.0), 0.0);
        assert!((d.curves.eval(7, 1.0 / 127.0) - 1.0).abs() < 1e-6);
        assert!((d.curves.eval(7, 0.5) - 1.0).abs() < 1e-6);
        assert!((d.curves.eval(4, 0.5) - 0.25).abs() < 0.01, "built-in x²");
    }

    #[test]
    fn keyswitch_range_from_global() {
        let d = def("<global> sw_lokey=c0 sw_hikey=c#0 sw_default=c0 <master> sw_last=c0 <region> sample=a.wav <master> sw_last=c#0 <region> sample=b.wav");
        assert_eq!(d.sw_range, Some((12, 13)));
        assert_eq!(d.sw_default, Some(12));
        assert_eq!(d.regions[1].sw_last, Some(13));
        assert!(d.unsupported.is_empty(), "{:?}", d.unsupported);
    }
}
