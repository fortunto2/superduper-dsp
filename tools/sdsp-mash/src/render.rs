//! Render orchestration — parse → decode → per-stem processing → section
//! transitions → auto-align → placed stems + timeline FX + mix settings.
//!
//! Per-stem order: trim → time-stretch (beats only) → high-pass → compressor
//! → place. Sections give each part its own beat + vocals joined by a
//! `transition`; the flat `[[track]]` list still works (one global grid).

use std::path::Path;

use crate::align::auto_align;
use crate::config::{
    seconds_per_beat, FxConfig, MashConfig, Role, SectionConfig, TrackConfig,
};
use crate::duck::{db_to_lin, DuckParams};
use crate::fx::{FxKind, ResolvedFx};
use crate::mix::{mix, MixSettings, Stem};
use crate::phrase::{build_edl, build_pingpong, SourceAudio};
use crate::stretch::time_stretch_stereo;
use crate::sweep::SweepParams;
use crate::track_fx::{apply_comp, apply_highpass};
use crate::wav_io::{decode_any, StereoWav};

/// 4/4 assumed for bar→beat and the auto-align window.
const BEATS_PER_BAR: f64 = 4.0;
const MIN_ALIGN_CONF: f32 = 0.30;

#[derive(Debug)]
pub enum RenderError {
    Decode { path: String, msg: String },
    SampleRateMismatch { path: String, stem_sr: u32, project_sr: u32 },
    Phrase(String),
    NoStems,
}

impl std::fmt::Display for RenderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RenderError::Decode { path, msg } => write!(f, "failed to read '{path}': {msg}"),
            RenderError::SampleRateMismatch { path, stem_sr, project_sr } => write!(
                f,
                "'{path}' is {stem_sr} Hz but the project is {project_sr} Hz — v0 has no \
                 resampler; render all stems at the same rate (set [sample_rate] or match sources)"
            ),
            RenderError::Phrase(msg) => write!(f, "phrase engine: {msg}"),
            RenderError::NoStems => write!(f, "no stems produced any audio"),
        }
    }
}
impl std::error::Error for RenderError {}

pub struct AlignReport {
    pub path: String,
    pub nominal_ms: f64,
    pub shift_ms: f64,
    pub score: f32,
    pub applied: bool,
}

/// A stem after decode + trim + stretch + FX, before final placement.
struct Proc {
    role: Role,
    offset: i64,
    gain: f32,
    fade_in: usize,
    fade_out: usize,
    auto_align: bool,
    section: Option<usize>,
    path: String,
    l: Vec<f32>,
    r: Vec<f32>,
}

impl Proc {
    fn frames(&self) -> usize {
        self.l.len().min(self.r.len())
    }
}

pub struct Prepared {
    pub sr: u32,
    pub stems: Vec<Stem>,
    pub settings: MixSettings,
    pub align_reports: Vec<AlignReport>,
    pub fx: Vec<ResolvedFx>,
}

/// A track plus the section context it was authored in.
struct Planned<'a> {
    cfg: &'a TrackConfig,
    /// Section start on the global timeline, in seconds.
    base_sec: f64,
    /// Section-local tempo for interpreting this track's `offset_beats`.
    local_bpm: f64,
    section: Option<usize>,
}

fn beats_to_samples(beats: f64, bpm: f64, sr: u32) -> i64 {
    (beats * seconds_per_beat(bpm) * sr as f64).round() as i64
}

fn sec_to_samples(sec: f64, sr: u32) -> i64 {
    (sec * sr as f64).round() as i64
}

/// Absolute start of a section on the timeline, in seconds.
fn section_start_sec(sec: &SectionConfig, global_bpm: f64) -> f64 {
    sec.start_sec
        .unwrap_or(sec.start_beat * seconds_per_beat(global_bpm))
}

fn section_bpm(sec: &SectionConfig, global_bpm: f64) -> f64 {
    sec.bpm.unwrap_or(global_bpm)
}

fn role_from_keep(k: &str) -> Option<Role> {
    match k {
        "hats" => Some(Role::BeatDrums),
        "melody" => Some(Role::BeatOther),
        "vocal" => Some(Role::Vocal),
        _ => None,
    }
}

/// Default crossfade/build length (beats) for a transition when unset.
fn xfade_beats(sec: &SectionConfig) -> f64 {
    if sec.xfade_beats > 0.0 {
        return sec.xfade_beats;
    }
    match sec.transition.as_deref() {
        Some("drop") => 8.0,
        Some("cut") => 1.0,
        Some("crossfade") | Some("bass_swap") | Some("filter_sweep") | Some("breakdown")
        | Some("fade_except") => 4.0,
        _ => 0.0,
    }
}

pub fn prepare(cfg: &MashConfig) -> Result<Prepared, RenderError> {
    // `preset = "wild"` (v2.3) turns on the Fred-again "life" defaults.
    let is_wild = cfg.preset.as_deref() == Some("wild");
    // ---- Collect every track (flat + per-section) with its base beat -----
    let mut planned: Vec<Planned> = Vec::new();
    for t in &cfg.tracks {
        planned.push(Planned { cfg: t, base_sec: 0.0, local_bpm: cfg.bpm, section: None });
    }
    for (si, sec) in cfg.sections.iter().enumerate() {
        let base_sec = section_start_sec(sec, cfg.bpm);
        let local_bpm = section_bpm(sec, cfg.bpm);
        for t in &sec.tracks {
            planned.push(Planned { cfg: t, base_sec, local_bpm, section: Some(si) });
        }
    }
    if planned.is_empty() {
        return Err(RenderError::NoStems);
    }

    // ---- Decode all, settle project sample rate --------------------------
    let mut decoded: Vec<StereoWav> = Vec::with_capacity(planned.len());
    for p in &planned {
        let w = decode_any(Path::new(&p.cfg.path)).map_err(|msg| RenderError::Decode {
            path: p.cfg.path.clone(),
            msg,
        })?;
        decoded.push(w);
    }
    let project_sr = cfg.sample_rate.unwrap_or(decoded[0].sample_rate);

    // ---- Pass 1: per-stem processing -------------------------------------
    let mut procs: Vec<Proc> = Vec::with_capacity(planned.len());
    for (p, w) in planned.iter().zip(decoded.into_iter()) {
        if w.sample_rate != project_sr {
            return Err(RenderError::SampleRateMismatch {
                path: p.cfg.path.clone(),
                stem_sr: w.sample_rate,
                project_sr,
            });
        }
        let t = p.cfg;
        let start = ((t.start_sec.max(0.0) * project_sr as f64).round() as usize).min(w.frames());
        let take = match t.len_sec {
            Some(s) => ((s.max(0.0) * project_sr as f64).round() as usize).min(w.frames() - start),
            None => w.frames() - start,
        };
        let mut l = w.l[start..start + take].to_vec();
        let mut r = w.r[start..start + take].to_vec();

        if (t.tempo_ratio - 1.0).abs() > 1e-9 {
            if MashConfig::role_of(t) == Role::Vocal {
                eprintln!(
                    "warning: stretching vocal '{}' by {:+.1}% (tempo_ratio {}) — kept ≤5% \
                     for a counter-stretch; watch for warble",
                    t.path,
                    (t.tempo_ratio - 1.0) * 100.0,
                    t.tempo_ratio
                );
            }
            let (sl, srr) = time_stretch_stereo(&l, &r, t.tempo_ratio, project_sr);
            l = sl;
            r = srr;
        }
        if let Some(hz) = t.highpass_hz {
            apply_highpass(&mut l, &mut r, project_sr, hz);
        }
        if let Some(c) = &t.comp {
            apply_comp(&mut l, &mut r, project_sr, c);
        }
        if let Some(db) = t.presence_db {
            crate::track_fx::apply_presence(&mut l, &mut r, project_sr, t.presence_hz, db);
        }
        let role = MashConfig::role_of(t);
        // Fat bass: explicit `saturate_db`, or the wild preset default on bass.
        let drive = t.saturate_db.or((is_wild && role == Role::BeatBass).then_some(4.0));
        if let Some(drive) = drive {
            crate::track_fx::apply_saturate(&mut l, &mut r, drive);
        }
        // Vocal delay-throw: explicit `delay_throw`, or the wild preset on vocals.
        if role == Role::Vocal && (t.delay_throw || is_wild) {
            crate::track_fx::apply_delay_throw(&mut l, &mut r, project_sr, p.local_bpm, 0.5);
        }

        let offset = sec_to_samples(p.base_sec, project_sr)
            + beats_to_samples(t.offset_beats, p.local_bpm, project_sr);
        procs.push(Proc {
            role: MashConfig::role_of(t),
            offset,
            gain: db_to_lin(t.gain_db as f32),
            fade_in: 0,
            fade_out: 0,
            auto_align: t.auto_align,
            section: p.section,
            path: t.path.clone(),
            l,
            r,
        });
    }

    // ---- Pass 2: section transitions (fades + beat trims + FX) -----------
    let mut fx: Vec<ResolvedFx> = Vec::new();
    apply_section_transitions(cfg, &mut procs, &mut fx, project_sr);
    // Section loudness is levelled on the mixed pre-master (see
    // `level_premaster_sections`), not here — raw-stem RMS can't see ducking.

    // ---- Pass 3: auto-align vocals against the beat-drums bus -------------
    let search = beats_to_samples(2.0, cfg.bpm, project_sr);
    let timeline = procs
        .iter()
        .map(|p| p.offset + p.frames() as i64)
        .max()
        .unwrap_or(0)
        .max(0) as usize
        + search.max(0) as usize;
    let (drums_l, drums_r) = build_drums_bus(&procs, timeline);
    let have_drums = drums_l.iter().any(|&x| x != 0.0);

    let mut align_reports = Vec::new();
    for p in procs.iter_mut() {
        if p.role == Role::Vocal && p.auto_align && have_drums {
            let res = auto_align(&p.l, &p.r, &drums_l, &drums_r, project_sr, p.offset, search);
            let nominal_ms = 1000.0 * p.offset as f64 / project_sr as f64;
            let applied = res.score >= MIN_ALIGN_CONF;
            if applied {
                p.offset = (p.offset + res.shift_samples).max(0);
            }
            align_reports.push(AlignReport {
                path: p.path.clone(),
                nominal_ms,
                shift_ms: res.shift_ms,
                score: res.score,
                applied,
            });
        }
    }

    // ---- Finalise (with an anti-click micro-fade floor on every stem) ----
    let micro = (MICRO_FADE_MS * 0.001 * project_sr as f64).round() as usize;
    let mut stems: Vec<Stem> = procs
        .into_iter()
        .map(|p| Stem {
            role: p.role,
            offset_samples: p.offset.max(0) as usize,
            gain: p.gain,
            fade_in: p.fade_in.max(micro),
            fade_out: p.fade_out.max(micro),
            l: p.l,
            r: p.r,
        })
        .collect();

    // ---- v3 phrase engine: EDL dialogue or auto ping-pong on the grid ----
    stems.extend(build_phrase_stems(cfg, project_sr, micro)?);

    let duck = cfg.duck.as_ref().map(|d| DuckParams {
        threshold_db: d.threshold_db as f32,
        ratio: d.ratio as f32,
        attack_ms: d.attack_ms as f32,
        release_ms: d.release_ms as f32,
        knee_db: d.knee_db as f32,
    });
    let sweep = cfg.intro_sweep.as_ref().map(|s| {
        let secs = s.bars * BEATS_PER_BAR * seconds_per_beat(cfg.bpm);
        SweepParams {
            len_samples: (secs * project_sr as f64).round().max(0.0) as usize,
            from_hz: s.from_hz as f32,
            to_hz: s.to_hz as f32,
        }
    });

    // ---- v2.2 "life" generators: whole-mix pump + filter breathing --------
    let content_end = stems
        .iter()
        .map(|s| s.offset_samples + s.l.len().min(s.r.len()))
        .max()
        .unwrap_or(0);

    // `preset = "wild"` fills in the Fred-again "life" defaults unless the
    // config sets its own [pump] / [breath].
    let eff_pump = cfg.pump.clone().or_else(|| {
        is_wild.then(|| crate::config::PumpConfig { depth_db: 6.0, release_ms: 120.0 })
    });
    let eff_breath = cfg.breath.clone().or_else(|| {
        is_wild.then(|| crate::config::BreathConfig {
            every_bars: 8.0,
            len_bars: 4.0,
            from_hz: 30.0,
            to_hz: 600.0,
        })
    });

    // Breathing pump on the whole mix — per section so its beat-zero lands on
    // each section downbeat (kick_pump keys off `start`).
    if let Some(p) = &eff_pump {
        let (depth, rel) = (p.depth_db as f32, p.release_ms as f32);
        let spans: Vec<(usize, usize)> = if cfg.sections.is_empty() {
            vec![(0, content_end)]
        } else {
            let mut starts: Vec<usize> = cfg
                .sections
                .iter()
                .map(|s| {
                    (sec_to_samples(section_start_sec(s, cfg.bpm), project_sr).max(0) as usize)
                        .min(content_end)
                })
                .collect();
            starts.sort_unstable();
            starts.dedup();
            let mut v = Vec::new();
            for i in 0..starts.len() {
                let end = *starts.get(i + 1).unwrap_or(&content_end);
                v.push((starts[i], end));
            }
            v
        };
        for (s, e) in spans {
            if e > s {
                let mut f = fx_at(FxKind::KickPump, s as i64, (e - s) as i64);
                f.depth_db = depth;
                f.release_ms = rel;
                fx.push(f);
            }
        }
    }

    // Filter breathing: a rising HP every `every_bars`, `len_bars` long.
    if let Some(b) = &eff_breath {
        let bar = beats_to_samples(BEATS_PER_BAR, cfg.bpm, project_sr).max(1);
        let every = beats_to_samples(b.every_bars * BEATS_PER_BAR, cfg.bpm, project_sr).max(bar);
        let len = beats_to_samples(b.len_bars * BEATS_PER_BAR, cfg.bpm, project_sr).max(1);
        // First breath after the intro; the build ends on a downbeat.
        let mut at = every;
        while (at as usize) + (len as usize) < content_end {
            let mut f = fx_at(FxKind::HpSweep, at - len, len);
            f.from_hz = b.from_hz;
            f.to_hz = b.to_hz;
            fx.push(f);
            at += every;
        }
    }

    for f in &cfg.fx {
        fx.push(resolve_fx(f, cfg.bpm, project_sr));
    }

    Ok(Prepared {
        sr: project_sr,
        stems,
        settings: MixSettings { sr: project_sr, duck, sweep },
        align_reports,
        fx,
    })
}

/// Anti-click fade floor applied to every stem edge (v2.1).
const MICRO_FADE_MS: f64 = 22.0;

fn fx_at(kind: FxKind, start: i64, len: i64) -> ResolvedFx {
    ResolvedFx {
        kind,
        start: start.max(0) as usize,
        len: len.max(0) as usize,
        from_hz: 200.0,
        to_hz: 12000.0,
        feedback: 0.6,
        delay_samples: 0,
        depth_db: 9.0,
        release_ms: 120.0,
        peak: 0.4,
    }
}

/// Mutate section beat stems' fades/lengths per the transition model, and push
/// transition FX. v2.1: every seam gets a lead-in/lead-out fade (default 1
/// beat); `cut` gets a lead-out (closing lowpass + volume ramp), an auto
/// beat-repeat stutter, and an echo tail (fb 0.4) ringing into the pause;
/// `breakdown` filters+fades the beat down over 2 beats; `drop` gets an
/// auto beat-repeat lead-out plus its riser.
fn apply_section_transitions(
    cfg: &MashConfig,
    procs: &mut [Proc],
    fx: &mut Vec<ResolvedFx>,
    sr: u32,
) {
    let sections = &cfg.sections;
    if sections.is_empty() {
        return;
    }
    let micro = (MICRO_FADE_MS * 0.001 * sr as f64).round() as i64;
    let start_samp_of =
        |sec: &SectionConfig| sec_to_samples(section_start_sec(sec, cfg.bpm), sr).max(0);
    let xf_samp_of = |sec: &SectionConfig| {
        beats_to_samples(xfade_beats(sec), section_bpm(sec, cfg.bpm), sr).max(0)
    };

    let mut order: Vec<usize> = (0..sections.len()).collect();
    order.sort_by(|&a, &b| {
        section_start_sec(&sections[a], cfg.bpm)
            .partial_cmp(&section_start_sec(&sections[b], cfg.bpm))
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    for (pos, &si) in order.iter().enumerate() {
        let sec = &sections[si];
        let this_start = start_samp_of(sec);
        let this_xf = xf_samp_of(sec);
        let this_bpm = section_bpm(sec, cfg.bpm);
        let beat1 = beats_to_samples(1.0, this_bpm, sr).max(1);
        let bar = 4 * beat1;
        let lead_out = sec
            .lead_out_beats
            .map(|b| beats_to_samples(b, this_bpm, sr))
            .unwrap_or(beat1)
            .max(micro);
        let lead_in = sec
            .lead_in_beats
            .map(|b| beats_to_samples(b, this_bpm, sr))
            .unwrap_or(beat1)
            .max(micro);
        let next = order.get(pos + 1).map(|&ni| &sections[ni]);

        // Fade-in for this section's beat.
        let fade_in_samp = match sec.transition.as_deref() {
            Some("crossfade") | Some("bass_swap") | Some("filter_sweep") => this_xf.max(micro),
            // Hard hits (after a riser / clean jingle start) get only anti-click.
            Some("drop") | Some("cut") => micro,
            // Everything else eases in over the lead-in.
            _ => lead_in,
        };

        // How this section's beat ends, driven by the NEXT section's transition.
        let (beat_len_samp, fade_out_samp): (Option<i64>, i64) = match next {
            None => (None, lead_out),
            Some(ns) => {
                let gap = (start_samp_of(ns) - this_start).max(0);
                let nxf = xf_samp_of(ns);
                let nbeat = beats_to_samples(1.0, section_bpm(ns, cfg.bpm), sr).max(1);
                match ns.transition.as_deref() {
                    Some("crossfade") | Some("bass_swap") | Some("filter_sweep") => {
                        (Some(gap + nxf), nxf.max(micro))
                    }
                    Some("breakdown") => (Some(gap), (2 * nbeat).max(lead_out)),
                    Some("fade_except") => (Some(gap), nxf.max(micro)),
                    // cut / drop / default: soft lead-out (no hard stop).
                    _ => (Some(gap), lead_out),
                }
            }
        };

        let keep_next: Option<Role> = match next {
            Some(ns) if ns.transition.as_deref() == Some("fade_except") => {
                ns.keep.as_deref().and_then(role_from_keep)
            }
            _ => None,
        };

        for p in procs.iter_mut() {
            if p.section != Some(si) || !p.role.is_beat() {
                continue;
            }
            if let Some(lb) = beat_len_samp {
                let want = lb.max(0) as usize;
                if want < p.frames() {
                    p.l.truncate(want);
                    p.r.truncate(want);
                }
            }
            p.fade_in = p.fade_in.max(fade_in_samp.max(0) as usize);
            let fades_out = keep_next.map_or(true, |k| k != p.role);
            if fades_out {
                p.fade_out = p.fade_out.max(fade_out_samp.max(0) as usize);
            }
        }

        // ── Entry FX for THIS section ──────────────────────────────────────
        match sec.transition.as_deref() {
            Some("drop") if this_xf > 0 => {
                // Angrier build into the drop.
                let mut r = fx_at(FxKind::Riser, this_start - this_xf, this_xf);
                r.from_hz = 200.0;
                r.to_hz = 16000.0;
                r.peak = 0.6;
                fx.push(r);
                // Sub-drop chest-hit ON the downbeat (half a bar, 55→45 Hz).
                let mut sd = fx_at(FxKind::SubDrop, this_start, 2 * beat1);
                sd.from_hz = 55.0;
                sd.to_hz = 45.0;
                sd.peak = 0.85; // v2.3 +3 dB
                fx.push(sd);
            }
            Some("filter_sweep") if this_xf > 0 => {
                let mut fs = fx_at(FxKind::FilterSweep, this_start, this_xf);
                fs.from_hz = 300.0;
                fs.to_hz = 18000.0;
                fx.push(fs);
            }
            _ => {}
        }

        // ── Lead-out FX for LEAVING this section (driven by NEXT) ──────────
        if let Some(ns) = next {
            let boundary = start_samp_of(ns);
            match ns.transition.as_deref() {
                Some("cut") => {
                    // Stutter the last bar into the pause, close the lowpass
                    // over the last beat, and ring an echo tail (fb 0.4).
                    fx.push(fx_at(FxKind::BeatRepeat, boundary - bar, bar));
                    let mut close = fx_at(FxKind::FilterSweep, boundary - lead_out, lead_out);
                    close.from_hz = 16000.0;
                    close.to_hz = 400.0;
                    fx.push(close);
                    let mut echo = fx_at(FxKind::EchoOut, boundary, 4 * beat1);
                    echo.feedback = 0.4;
                    echo.delay_samples = (beat1 / 2).max(1) as usize;
                    fx.push(echo);
                }
                Some("drop") => {
                    // Auto beat-repeat lead-out into the drop.
                    fx.push(fx_at(FxKind::BeatRepeat, boundary - bar, bar));
                }
                Some("breakdown") => {
                    // Close the lowpass over the last 2 beats as the beat dies.
                    let mut close = fx_at(FxKind::FilterSweep, boundary - 2 * beat1, 2 * beat1);
                    close.from_hz = 16000.0;
                    close.to_hz = 500.0;
                    fx.push(close);
                }
                _ => {}
            }
        }
    }
}

/// Resolve an explicit `[[fx]]` into sample-domain params with musical defaults.
fn resolve_fx(f: &FxConfig, bpm: f64, sr: u32) -> ResolvedFx {
    let beat = beats_to_samples(1.0, bpm, sr).max(1) as usize;
    let start = match f.at_sec {
        Some(s) => sec_to_samples(s, sr).max(0) as usize,
        None => beats_to_samples(f.at_beat, bpm, sr).max(0) as usize,
    };
    let len_beats = if f.len_beats > 0.0 {
        f.len_beats
    } else {
        match f.kind.as_str() {
            "tape_stop" | "beat_repeat" | "backspin" => BEATS_PER_BAR,
            "kick_pump" | "riser" | "echo_out" => 2.0 * BEATS_PER_BAR,
            _ => BEATS_PER_BAR,
        }
    };
    let len = beats_to_samples(len_beats, bpm, sr).max(0) as usize;
    let kind = match f.kind.as_str() {
        "tape_stop" => FxKind::TapeStop,
        "beat_repeat" => FxKind::BeatRepeat,
        "echo_out" => FxKind::EchoOut,
        "kick_pump" => FxKind::KickPump,
        "riser" => FxKind::Riser,
        "filter_sweep" => FxKind::FilterSweep,
        "backspin" => FxKind::Backspin,
        "sub_drop" => FxKind::SubDrop,
        "hp_sweep" => FxKind::HpSweep,
        _ => FxKind::Riser,
    };
    let delay_samples = match f.delay_ms {
        Some(ms) => (ms * 0.001 * sr as f64).round() as usize,
        None => beat / 2,
    };
    ResolvedFx {
        kind,
        start,
        len,
        from_hz: f.from_hz.unwrap_or(200.0),
        to_hz: f.to_hz.unwrap_or(12000.0),
        feedback: f.feedback.unwrap_or(0.6) as f32,
        delay_samples,
        depth_db: f.depth_db.unwrap_or(9.0) as f32,
        release_ms: f.release_ms.unwrap_or(120.0) as f32,
        peak: f.peak.unwrap_or(0.4) as f32,
    }
}

fn build_drums_bus(procs: &[Proc], len: usize) -> (Vec<f32>, Vec<f32>) {
    let mut bl = vec![0.0f32; len];
    let mut br = vec![0.0f32; len];
    for p in procs.iter().filter(|p| p.role == Role::BeatDrums) {
        let frames = p.frames();
        for i in 0..frames {
            let dst = p.offset + i as i64;
            if dst >= 0 && (dst as usize) < len {
                bl[dst as usize] += p.l[i] * p.gain;
                br[dst as usize] += p.r[i] * p.gain;
            }
        }
    }
    (bl, br)
}

/// Even out section loudness on the **mastered** output — the only place the
/// balance is reliable, since it's after the ducking *and* the master limiter's
/// density response (both of which raw-stem or pre-master RMS can't predict).
/// Attenuation only (never boost, so nothing is pushed over the ceiling): every
/// section above the median level is pulled down to it via a smoothed gain
/// envelope (≈0.3 s ramps, no clicks). No-op unless `balance_sections` and ≥2
/// sections. Returns per-section dB adjustments (all ≤ 0).
pub fn level_premaster_sections(
    cfg: &MashConfig,
    sr: u32,
    l: &mut [f32],
    r: &mut [f32],
) -> Vec<f32> {
    let n = l.len().min(r.len());
    if !cfg.balance_sections || cfg.sections.len() < 2 || n == 0 {
        return Vec::new();
    }
    // Section boundaries (sample indices), in start order.
    let mut starts: Vec<usize> = cfg
        .sections
        .iter()
        .map(|s| (sec_to_samples(section_start_sec(s, cfg.bpm), sr).max(0) as usize).min(n))
        .collect();
    starts.sort_unstable();
    starts.dedup();
    let mut bounds = starts.clone();
    if *bounds.last().unwrap_or(&0) < n {
        bounds.push(n);
    }
    if bounds.len() < 3 {
        return Vec::new(); // need at least 2 spans
    }

    let seg_rms = |a: usize, b: usize| -> f32 {
        if b <= a {
            return 0.0;
        }
        let mut sq = 0.0f64;
        for i in a..b {
            sq += (l[i] as f64) * (l[i] as f64) + (r[i] as f64) * (r[i] as f64);
        }
        (sq / (2 * (b - a)) as f64).sqrt() as f32
    };

    let n_seg = bounds.len() - 1;
    let rmss: Vec<f32> = (0..n_seg).map(|i| seg_rms(bounds[i], bounds[i + 1])).collect();
    let mut nz: Vec<f32> = rmss.iter().copied().filter(|&x| x > 1e-6).collect();
    if nz.len() < 2 {
        return Vec::new();
    }
    nz.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mid = nz.len() / 2;
    let target = if nz.len() % 2 == 0 {
        0.5 * (nz[mid - 1] + nz[mid])
    } else {
        nz[mid]
    };

    // Per-section gain, then a per-sample step envelope.
    // Deadband: a section may sit up to DEADBAND_DB above the median untouched
    // — that headroom is the *drop's payoff* (an energetic section SHOULD lift
    // over the intro). Only the excess beyond the deadband is pulled back, so
    // gross outliers (a +7 dB finale) still get tamed but drops stay punchy.
    const DEADBAND_DB: f32 = 4.5;
    let mut env = vec![1.0f32; n];
    let mut adj_db = Vec::with_capacity(n_seg);
    for i in 0..n_seg {
        let g = if rmss[i] > 1e-6 {
            let excess_db = 20.0 * (rmss[i] / target).log10();
            let cut_db = (excess_db - DEADBAND_DB).max(0.0).min(9.0);
            10f32.powf(-cut_db / 20.0)
        } else {
            1.0
        };
        adj_db.push(20.0 * g.log10());
        for e in env.iter_mut().take(bounds[i + 1]).skip(bounds[i]) {
            *e = g;
        }
    }

    // Smooth the gain steps into ~0.3 s ramps (fwd + back one-pole = zero-phase).
    let a = (-1.0f32 / (0.12 * sr as f32)).exp();
    let mut y = env[0];
    for e in env.iter_mut() {
        y = a * y + (1.0 - a) * *e;
        *e = y;
    }
    let mut y = *env.last().unwrap();
    for e in env.iter_mut().rev() {
        y = a * y + (1.0 - a) * *e;
        *e = y;
    }

    for i in 0..n {
        l[i] *= env[i];
        r[i] *= env[i];
    }
    adj_db
}

/// prepare + mix → the pre-master stereo bus (FX not applied — used by tests).
#[allow(dead_code)]
pub fn render_premaster(cfg: &MashConfig) -> Result<(u32, Vec<f32>, Vec<f32>), RenderError> {
    let prep = prepare(cfg)?;
    let (l, r) = mix(&prep.stems, &prep.settings);
    if l.is_empty() {
        return Err(RenderError::NoStems);
    }
    Ok((prep.sr, l, r))
}

/// Decode the `[[source]]` vocals (pre-stretched by their `tempo_ratio`), then
/// run the phrase engine: an explicit `[[phrase]]` EDL if present, otherwise
/// the auto `[pingpong]`. Both emit placed vocal phrases summed into the grid.
/// Returns them as `Role::Vocal` stems with the anti-click micro-fade floor.
fn build_phrase_stems(cfg: &MashConfig, sr: u32, micro: usize) -> Result<Vec<Stem>, RenderError> {
    let want_phrases = !cfg.phrases.is_empty() || cfg.pingpong.is_some();
    if cfg.sources.is_empty() || !want_phrases {
        return Ok(Vec::new());
    }

    // Decode + counter-stretch each named source to the grid.
    let mut srcs: Vec<SourceAudio> = Vec::with_capacity(cfg.sources.len());
    for s in &cfg.sources {
        let w = decode_any(Path::new(&s.path)).map_err(|msg| RenderError::Decode {
            path: s.path.clone(),
            msg,
        })?;
        if w.sample_rate != sr {
            return Err(RenderError::SampleRateMismatch {
                path: s.path.clone(),
                stem_sr: w.sample_rate,
                project_sr: sr,
            });
        }
        // Trim to the chosen verse first (so we don't stretch/segment 3 min).
        let start = ((s.start_sec.max(0.0) * sr as f64).round() as usize).min(w.frames());
        let take = match s.len_sec {
            Some(t) => ((t.max(0.0) * sr as f64).round() as usize).min(w.frames() - start),
            None => w.frames() - start,
        };
        let (tl, tr) = (w.l[start..start + take].to_vec(), w.r[start..start + take].to_vec());
        let (l, r) = if (s.tempo_ratio - 1.0).abs() > 1e-9 {
            time_stretch_stereo(&tl, &tr, s.tempo_ratio, sr)
        } else {
            (tl, tr)
        };
        srcs.push(SourceAudio { name: s.name.clone(), l, r });
    }

    // Explicit EDL disables the auto pingpong (hand-authored dialogue wins).
    let placed = if !cfg.phrases.is_empty() {
        build_edl(&cfg.phrases, &srcs, cfg.bpm, sr).map_err(RenderError::Phrase)?
    } else if let Some(pp) = &cfg.pingpong {
        build_pingpong(pp, &srcs, cfg.bpm, sr).map_err(RenderError::Phrase)?
    } else {
        Vec::new()
    };

    Ok(placed
        .into_iter()
        .map(|p| Stem {
            role: Role::Vocal,
            offset_samples: p.offset,
            gain: p.gain,
            fade_in: micro,
            fade_out: micro,
            l: p.l,
            r: p.r,
        })
        .collect())
}

pub fn role_counts(cfg: &MashConfig) -> (usize, usize) {
    let mut vocals = 0;
    let mut others = 0;
    let all = cfg
        .tracks
        .iter()
        .chain(cfg.sections.iter().flat_map(|s| s.tracks.iter()));
    for t in all {
        match MashConfig::role_of(t) {
            Role::Vocal => vocals += 1,
            Role::BeatOther => others += 1,
            _ => {}
        }
    }
    // Phrase-engine sources are vocals too — count them so `[duck]` next to a
    // `[pingpong]`/`[[phrase]]` isn't falsely flagged as "no vocal key".
    if !cfg.phrases.is_empty() || cfg.pingpong.is_some() {
        vocals += cfg.sources.len();
    }
    (vocals, others)
}
