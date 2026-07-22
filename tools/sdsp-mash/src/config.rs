//! Mashup config — the `mash.toml` schema + parsing + validation.
//!
//! A mashup is a shared BPM grid onto which stems from two (or more)
//! source songs are placed by `offset_beats`. Beat stems come from one
//! song, the vocal from another. See `example.toml`.

use serde::Deserialize;

/// What a stem contributes to the mix. Drives the ducking + sweep logic:
/// only `Vocal` is a sidechain key, only `BeatOther` is ducked, all three
/// beat roles get the intro sweep, `Vocal` never does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    BeatDrums,
    BeatBass,
    BeatOther,
    Vocal,
}

impl Role {
    pub fn is_beat(self) -> bool {
        matches!(self, Role::BeatDrums | Role::BeatBass | Role::BeatOther)
    }

    fn parse(s: &str) -> Result<Role, ConfigError> {
        match s {
            "beat-drums" => Ok(Role::BeatDrums),
            "beat-bass" => Ok(Role::BeatBass),
            "beat-other" => Ok(Role::BeatOther),
            "vocal" => Ok(Role::Vocal),
            other => Err(ConfigError::BadRole(other.to_string())),
        }
    }
}

#[derive(Debug)]
pub enum ConfigError {
    Parse(toml::de::Error),
    BadRole(String),
    /// Vocals are never time-stretched (formant/pitch artefacts); the beat is
    /// stretched under the vocal instead. `tempo_ratio` must be 1.0 for vocals.
    VocalStretchTooLarge { path: String, ratio: f64 },
    /// A non-positive or absurd stretch factor.
    TempoRatioOutOfRange { path: String, ratio: f64 },
    NoTracks,
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigError::Parse(e) => write!(f, "TOML parse error: {e}"),
            ConfigError::BadRole(r) => write!(
                f,
                "unknown track role '{r}' — expected beat-drums | beat-bass | beat-other | vocal"
            ),
            ConfigError::VocalStretchTooLarge { path, ratio } => write!(
                f,
                "tempo_ratio = {ratio} on vocal '{path}' stretches it by more than 5% — a pop \
                 vocal only tolerates ±5%; use counter-stretch (nudge the beat too) to meet in \
                 the middle, or leave the vocal at 1.0"
            ),
            ConfigError::TempoRatioOutOfRange { path, ratio } => write!(
                f,
                "tempo_ratio = {ratio} for '{path}' is out of range — must be within 0.25..4.0"
            ),
            ConfigError::NoTracks => write!(f, "config has no [[track]] entries"),
        }
    }
}

impl std::error::Error for ConfigError {}

fn default_tempo_ratio() -> f64 {
    1.0
}

fn default_true() -> bool {
    true
}

fn default_presence_hz() -> f64 {
    3000.0
}

/// One source stem placed on the grid.
#[derive(Debug, Clone, Deserialize)]
pub struct TrackConfig {
    /// Path to the source WAV (a demucs stem, typically).
    pub path: String,
    /// Role string — validated into [`Role`] by [`MashConfig::validated`].
    pub role: String,
    /// Level trim applied to the stem before summing.
    #[serde(default)]
    pub gain_db: f64,
    /// Where the stem starts on the grid, in beats (fractions allowed).
    #[serde(default)]
    pub offset_beats: f64,
    /// Trim the *source* file — skip this many seconds from its head.
    #[serde(default)]
    pub start_sec: f64,
    /// Optional source length in seconds (after `start_sec`). `None` = to EOF.
    #[serde(default)]
    pub len_sec: Option<f64>,
    /// Time-stretch factor applied to this stem (WSOLA, pitch-preserving).
    /// `2.0` = twice as long / half speed. Forbidden on vocals (see
    /// [`ConfigError::VocalStretchTooLarge`]). Applied *before* placement, so
    /// `offset_beats` still refers to grid position.
    #[serde(default = "default_tempo_ratio")]
    pub tempo_ratio: f64,
    /// Phase auto-alignment (vocals only): cross-correlate this stem's onset
    /// envelope against the beat-drums bus in a ±half-bar window around
    /// `offset_beats` and snap to the best-matching lag.
    #[serde(default)]
    pub auto_align: bool,
    /// Optional per-track high-pass (RBJ biquad) applied before placement —
    /// clears rumble / plosives from a vocal stem.
    #[serde(default)]
    pub highpass_hz: Option<f64>,
    /// Optional per-track compressor applied before placement (e.g. to even
    /// out a rap vocal).
    #[serde(default)]
    pub comp: Option<TrackCompConfig>,
    /// Optional tape-saturation drive (dB) applied to the stem — fattens a
    /// bass (v2.2). ~3–6 dB is musical.
    #[serde(default)]
    pub saturate_db: Option<f64>,
    /// Optional presence boost (dB) — a peaking bell at `presence_hz`
    /// lifting the 2–4 kHz intelligibility band of a vocal.
    #[serde(default)]
    pub presence_db: Option<f64>,
    /// Centre of the presence bell (default 3000 Hz).
    #[serde(default = "default_presence_hz")]
    pub presence_hz: f64,
    /// Vocal delay-throw (v2.3): echo the last word of each phrase into the
    /// following pause (dotted-quarter feedback). Vocals only.
    #[serde(default)]
    pub delay_throw: bool,
}

/// A per-track compressor (feed-forward, soft-knee). Reuses the same static
/// curve as the master compressor / sidechain ducker.
#[derive(Debug, Clone, Deserialize)]
pub struct TrackCompConfig {
    pub threshold_db: f64,
    pub ratio: f64,
    pub attack_ms: f64,
    pub release_ms: f64,
    /// Soft-knee width in dB (optional, default 6).
    #[serde(default = "default_knee")]
    pub knee_db: f64,
    /// Make-up gain in dB applied after compression (optional, default 0).
    #[serde(default)]
    pub makeup_db: f64,
}

/// Sidechain ducking — a vocal-driven compressor on the `beat-other` bus.
/// `bass` is deliberately never ducked (it fills the low end under the
/// vocal); drums punch through on their own.
#[derive(Debug, Clone, Deserialize)]
pub struct DuckConfig {
    pub threshold_db: f64,
    pub ratio: f64,
    pub attack_ms: f64,
    pub release_ms: f64,
    /// Soft-knee width in dB (optional, default 6).
    #[serde(default = "default_knee")]
    pub knee_db: f64,
}

fn default_knee() -> f64 {
    6.0
}

/// Intro lowpass sweep — the beat bus opens from `from_hz` to `to_hz`
/// exponentially over the first `bars` bars (4/4 assumed).
#[derive(Debug, Clone, Deserialize)]
pub struct IntroSweepConfig {
    pub bars: f64,
    pub from_hz: f64,
    pub to_hz: f64,
}

/// Fred-again "breathing" sidechain pump on the whole mix (v2.2) — the gain
/// dips on every beat and recovers, keyed to the grid.
#[derive(Debug, Clone, Deserialize)]
pub struct PumpConfig {
    /// Depth of the dip in dB (positive; ~4–5).
    pub depth_db: f64,
    /// Recovery time in ms (~120 at 130 BPM).
    #[serde(default = "default_pump_release")]
    pub release_ms: f64,
}

fn default_pump_release() -> f64 {
    120.0
}

/// Periodic "filter breathing" mini-build (v2.2): every `every_bars` bars a
/// rising high-pass over `len_bars`, snapping back on the downbeat.
#[derive(Debug, Clone, Deserialize)]
pub struct BreathConfig {
    #[serde(default = "default_breath_every")]
    pub every_bars: f64,
    #[serde(default = "default_breath_len")]
    pub len_bars: f64,
    #[serde(default = "default_breath_from")]
    pub from_hz: f64,
    #[serde(default = "default_breath_to")]
    pub to_hz: f64,
}

fn default_breath_every() -> f64 {
    16.0
}
fn default_breath_len() -> f64 {
    4.0
}
fn default_breath_from() -> f64 {
    30.0
}
fn default_breath_to() -> f64 {
    600.0
}

/// A master-chain stage — identical shape to sdsp-chain's `[[stage]]`.
#[derive(Debug, Clone, Deserialize)]
pub struct MasterStage {
    pub plugin: String,
    #[serde(default)]
    pub params: toml::Table,
}

/// A named vocal source for the phrase engine (v2.3).
#[derive(Debug, Clone, Deserialize)]
pub struct SourceConfig {
    pub name: String,
    pub path: String,
    /// Counter-stretch this vocal source to the grid before phrase-segmentation
    /// (WSOLA, pitch-preserving). Sources are vocals → validated to ±5% like a
    /// vocal `[[track]]`. `1.0` = no stretch. Direction: `native_bpm / grid_bpm`.
    #[serde(default = "default_tempo_ratio")]
    pub tempo_ratio: f64,
    /// Use only `[start_sec, start_sec+len_sec)` of the source — pick one punchy
    /// verse so the pingpong doesn't chop the whole 3-minute track. Trim happens
    /// before stretch + segmentation.
    #[serde(default)]
    pub start_sec: f64,
    #[serde(default)]
    pub len_sec: Option<f64>,
}

fn default_loops() -> u32 {
    1
}
fn default_phrase_beats() -> f64 {
    4.0
}

/// One EDL phrase (v2.3) — cut `[start_sec, end_sec)` of source `track`, pitch,
/// loop, throw, and place at `at_beat` on the global grid. Overlaps sum.
#[derive(Debug, Clone, Deserialize)]
pub struct PhraseConfig {
    pub track: String,
    pub start_sec: f64,
    pub end_sec: f64,
    pub at_beat: f64,
    #[serde(default)]
    pub pitch_semitones: f64,
    #[serde(default = "default_loops")]
    pub loops: u32,
    /// Per-repeat gain ramp in dB when `loops` > 1: repeat k plays at
    /// `k * loop_gain_step_db` (first repeat at 0). A looped hook bridge
    /// escalates toward its exit instead of treading water. 0 = flat.
    #[serde(default)]
    pub loop_gain_step_db: f64,
    #[serde(default)]
    pub throw: bool,
    #[serde(default)]
    pub gain_db: f64,
}

/// Auto vocal ping-pong (v2.3) — alternate phrases of two vocals on the grid,
/// with seeded random stutter-loops and chipmunk / slowed pitch answers.
#[derive(Debug, Clone, Deserialize)]
pub struct PingpongConfig {
    /// Exactly two source names.
    pub vocals: Vec<String>,
    #[serde(default = "default_phrase_beats")]
    pub phrase_beats: f64,
    #[serde(default)]
    pub loop_prob: f64,
    #[serde(default)]
    pub pitch_prob: f64,
    #[serde(default)]
    pub seed: u64,
    /// Where the dialogue starts, in global-BPM beats.
    #[serde(default)]
    pub start_beat: f64,
    /// Hard stop (global-BPM beats): no phrase starts at/after this point, so
    /// the dialogue can't outlive the beat into an a-cappella tail. None =
    /// place until both sources run dry.
    #[serde(default)]
    pub end_beat: Option<f64>,
    #[serde(default)]
    pub gain_db: f64,
    /// Breath gap reserved at the end of every slot (ms) so the ear registers
    /// each hand-over. Phrases are chunked to `slot − gap`, never flush.
    #[serde(default = "default_pp_gap_ms")]
    pub gap_ms: f64,
    /// Stereo split between the two voices (0..1 of full side): voice A sits
    /// slightly left, voice B slightly right. Balance law, centre stays unity.
    #[serde(default = "default_pp_voice_pan")]
    pub voice_pan: f64,
    /// High-shelf tilt @5 kHz: voice A −tilt (darker), voice B +tilt
    /// (brighter) — separates two similar rap timbres.
    #[serde(default = "default_pp_voice_tilt")]
    pub voice_tilt_db: f64,
    /// Per-phrase probability of a soft dub delay-throw tail (fb 0.35). The
    /// tail rings into the next voice's slot, so keep 0 for dense rap.
    #[serde(default)]
    pub throw_prob: f64,
}

fn default_pp_gap_ms() -> f64 {
    150.0
}
fn default_pp_voice_pan() -> f64 {
    0.18
}
fn default_pp_voice_tilt() -> f64 {
    1.0
}

/// A one-shot timeline effect applied to the pre-master mix at `at_beat`.
/// Params are optional — each effect falls back to a musical default.
#[derive(Debug, Clone, Deserialize)]
pub struct FxConfig {
    /// `tape_stop` | `beat_repeat` | `echo_out` | `kick_pump` | `riser`.
    pub kind: String,
    /// Start position in global-BPM beats. `at_sec` overrides it if set.
    #[serde(default)]
    pub at_beat: f64,
    /// Start position in seconds (handy for section-based megamixes).
    #[serde(default)]
    pub at_sec: Option<f64>,
    /// Effect duration in beats (0 = per-effect default).
    #[serde(default)]
    pub len_beats: f64,
    #[serde(default)]
    pub from_hz: Option<f64>,
    #[serde(default)]
    pub to_hz: Option<f64>,
    #[serde(default)]
    pub feedback: Option<f64>,
    #[serde(default)]
    pub delay_ms: Option<f64>,
    #[serde(default)]
    pub depth_db: Option<f64>,
    #[serde(default)]
    pub release_ms: Option<f64>,
    #[serde(default)]
    pub peak: Option<f64>,
}

/// A section of the arrangement — its own beat + vocals, placed at
/// `start_beat`, joined to the previous section by `transition`. Track
/// `offset_beats` are **relative to `start_beat`**.
#[derive(Debug, Clone, Deserialize)]
pub struct SectionConfig {
    #[serde(default)]
    pub name: Option<String>,
    /// Section start on the global timeline, in global-BPM beats (default 0;
    /// `start_sec` overrides).
    #[serde(default)]
    pub start_beat: f64,
    /// Explicit section start in seconds (overrides `start_beat` placement).
    #[serde(default)]
    pub start_sec: Option<f64>,
    /// Section-local tempo — the grid its beat+vocal sit on (stretch the beat
    /// to this via each beat track's `tempo_ratio`). Defaults to the global BPM.
    #[serde(default)]
    pub bpm: Option<f64>,
    /// Transition INTO this section: `crossfade` | `cut` | `drop` |
    /// `breakdown` | `fade_except` | `bass_swap` | `filter_sweep`.
    #[serde(default)]
    pub transition: Option<String>,
    /// Crossfade / build length in beats.
    #[serde(default)]
    pub xfade_beats: f64,
    /// For `fade_except`: which role survives — `vocal` | `hats` | `melody`.
    #[serde(default)]
    pub keep: Option<String>,
    /// Beats the section's beat fades/filters out at its end (default 1).
    #[serde(default)]
    pub lead_out_beats: Option<f64>,
    /// Beats the section's beat fades in at its start (default 1).
    #[serde(default)]
    pub lead_in_beats: Option<f64>,
    #[serde(default, rename = "track")]
    pub tracks: Vec<TrackConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MashConfig {
    pub bpm: f64,
    /// Output sample rate. `None` → inherit from the first stem loaded.
    #[serde(default)]
    pub sample_rate: Option<u32>,
    #[serde(default, rename = "track")]
    pub tracks: Vec<TrackConfig>,
    #[serde(default, rename = "section")]
    pub sections: Vec<SectionConfig>,
    /// Named vocal sources for the phrase engine (v3). Referenced by
    /// `[[phrase]].track` and `[pingpong].vocals`.
    #[serde(default, rename = "source")]
    pub sources: Vec<SourceConfig>,
    /// Explicit EDL phrase list (v3). If non-empty it **disables** the auto
    /// `[pingpong]` — you're driving the dialogue by hand.
    #[serde(default, rename = "phrase")]
    pub phrases: Vec<PhraseConfig>,
    /// Auto vocal ping-pong (v3) — used only when `[[phrase]]` is empty.
    #[serde(default)]
    pub pingpong: Option<PingpongConfig>,
    #[serde(default, rename = "fx")]
    pub fx: Vec<FxConfig>,
    /// Level every section's beat-bus RMS to the first section's (fixes a
    /// section grabbed from a louder part of the source sticking out). Default on.
    #[serde(default = "default_true")]
    pub balance_sections: bool,
    #[serde(default)]
    pub duck: Option<DuckConfig>,
    #[serde(default)]
    pub intro_sweep: Option<IntroSweepConfig>,
    /// Whole-mix breathing pump (v2.2).
    #[serde(default)]
    pub pump: Option<PumpConfig>,
    /// Periodic filter-breathing build (v2.2).
    #[serde(default)]
    pub breath: Option<BreathConfig>,
    /// Preset that fills in the "life" defaults (v2.3). `"wild"` = Fred-again
    /// energy: whole-mix pump + filter breathing turned on unless overridden.
    #[serde(default)]
    pub preset: Option<String>,
    #[serde(default, rename = "master")]
    pub master: Vec<MasterStage>,
}

impl MashConfig {
    /// Parse from TOML text and run validation (roles, tempo_ratio, at
    /// least one track).
    pub fn parse(text: &str) -> Result<MashConfig, ConfigError> {
        let cfg: MashConfig = toml::from_str(text).map_err(ConfigError::Parse)?;
        cfg.validate()?;
        Ok(cfg)
    }

    fn validate(&self) -> Result<(), ConfigError> {
        let section_tracks: usize = self.sections.iter().map(|s| s.tracks.len()).sum();
        if self.tracks.is_empty() && section_tracks == 0 {
            return Err(ConfigError::NoTracks);
        }
        let all = self
            .tracks
            .iter()
            .chain(self.sections.iter().flat_map(|s| s.tracks.iter()));
        for t in all {
            let role = Role::parse(&t.role)?;
            // Vocals tolerate a small stretch (±5%) — beyond that a pop vocal
            // gets audibly warbly, so it stays an error (use counter-stretch).
            if role == Role::Vocal && (t.tempo_ratio - 1.0).abs() > 0.05 {
                return Err(ConfigError::VocalStretchTooLarge {
                    path: t.path.clone(),
                    ratio: t.tempo_ratio,
                });
            }
            if t.tempo_ratio < 0.25 || t.tempo_ratio > 4.0 {
                return Err(ConfigError::TempoRatioOutOfRange {
                    path: t.path.clone(),
                    ratio: t.tempo_ratio,
                });
            }
        }
        // Phrase-engine sources are always vocals — hold them to the same ±5%
        // counter-stretch budget as a vocal `[[track]]`.
        for s in &self.sources {
            if (s.tempo_ratio - 1.0).abs() > 0.05 {
                return Err(ConfigError::VocalStretchTooLarge {
                    path: s.path.clone(),
                    ratio: s.tempo_ratio,
                });
            }
        }
        Ok(())
    }

    /// Parsed role for a track (infallible after [`validate`]).
    pub fn role_of(t: &TrackConfig) -> Role {
        Role::parse(&t.role).expect("role validated at parse time")
    }
}

/// Seconds per beat at a given tempo.
#[inline]
pub fn seconds_per_beat(bpm: f64) -> f64 {
    60.0 / bpm
}

/// Convert a grid offset in beats to a sample count at `sr`. Rounds to the
/// nearest sample so placement is sample-accurate. (Render uses its own i64
/// variant; this stays for the config-math unit tests.)
#[inline]
#[allow(dead_code)]
pub fn offset_to_samples(offset_beats: f64, bpm: f64, sr: u32) -> usize {
    let secs = offset_beats * seconds_per_beat(bpm);
    (secs * sr as f64).round().max(0.0) as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_config() {
        let text = r#"
            bpm = 120.0
            [[track]]
            path = "drums.wav"
            role = "beat-drums"
            [[track]]
            path = "vocal.wav"
            role = "vocal"
            offset_beats = 8.0
            gain_db = -2.0
        "#;
        let cfg = MashConfig::parse(text).expect("parse");
        assert_eq!(cfg.bpm, 120.0);
        assert_eq!(cfg.tracks.len(), 2);
        assert_eq!(MashConfig::role_of(&cfg.tracks[0]), Role::BeatDrums);
        assert_eq!(MashConfig::role_of(&cfg.tracks[1]), Role::Vocal);
        assert_eq!(cfg.tracks[1].offset_beats, 8.0);
        assert_eq!(cfg.tracks[1].gain_db, -2.0);
        // Defaults.
        assert_eq!(cfg.tracks[0].tempo_ratio, 1.0);
        assert_eq!(cfg.tracks[0].start_sec, 0.0);
        assert!(cfg.tracks[0].len_sec.is_none());
    }

    #[test]
    fn rejects_bad_role() {
        let text = r#"
            bpm = 120.0
            [[track]]
            path = "x.wav"
            role = "beat-guitar"
        "#;
        let err = MashConfig::parse(text).unwrap_err();
        assert!(matches!(err, ConfigError::BadRole(_)), "got {err:?}");
    }

    #[test]
    fn rejects_large_vocal_stretch() {
        // >5% on a vocal is refused.
        let text = r#"
            bpm = 120.0
            [[track]]
            path = "x.wav"
            role = "vocal"
            tempo_ratio = 1.25
        "#;
        let err = MashConfig::parse(text).unwrap_err();
        assert!(
            matches!(err, ConfigError::VocalStretchTooLarge { .. }),
            "got {err:?}"
        );
    }

    #[test]
    fn allows_small_vocal_stretch() {
        // ≤5% counter-stretch on a vocal is allowed (e.g. 125→130 = 0.9628).
        let text = r#"
            bpm = 130.0
            [[track]]
            path = "v.wav"
            role = "vocal"
            tempo_ratio = 0.9628
        "#;
        let cfg = MashConfig::parse(text).expect("±5% vocal stretch allowed");
        assert_eq!(cfg.tracks[0].tempo_ratio, 0.9628);
    }

    #[test]
    fn allows_beat_stretch() {
        let text = r#"
            bpm = 120.0
            [[track]]
            path = "drums.wav"
            role = "beat-drums"
            tempo_ratio = 1.05
        "#;
        let cfg = MashConfig::parse(text).expect("beat stretch allowed");
        assert_eq!(cfg.tracks[0].tempo_ratio, 1.05);
    }

    #[test]
    fn rejects_absurd_tempo_ratio() {
        let text = r#"
            bpm = 120.0
            [[track]]
            path = "drums.wav"
            role = "beat-drums"
            tempo_ratio = 8.0
        "#;
        let err = MashConfig::parse(text).unwrap_err();
        assert!(
            matches!(err, ConfigError::TempoRatioOutOfRange { .. }),
            "got {err:?}"
        );
    }

    #[test]
    fn parses_track_fx_and_align() {
        let text = r#"
            bpm = 120.0
            [[track]]
            path = "drums.wav"
            role = "beat-drums"
            [[track]]
            path = "vocal.wav"
            role = "vocal"
            auto_align = true
            highpass_hz = 120.0
            [track.comp]
            threshold_db = -18.0
            ratio = 3.0
            attack_ms = 5.0
            release_ms = 80.0
            makeup_db = 2.0
        "#;
        let cfg = MashConfig::parse(text).expect("parse");
        let v = &cfg.tracks[1];
        assert!(v.auto_align);
        assert_eq!(v.highpass_hz, Some(120.0));
        let comp = v.comp.as_ref().expect("comp");
        assert_eq!(comp.ratio, 3.0);
        assert_eq!(comp.makeup_db, 2.0);
        assert_eq!(comp.knee_db, 6.0); // default
    }

    #[test]
    fn rejects_empty() {
        let text = "bpm = 120.0\n";
        let err = MashConfig::parse(text).unwrap_err();
        assert!(matches!(err, ConfigError::NoTracks), "got {err:?}");
    }

    #[test]
    fn parses_duck_and_sweep_and_master() {
        let text = r#"
            bpm = 100.0
            [[track]]
            path = "o.wav"
            role = "beat-other"
            [duck]
            threshold_db = -28.0
            ratio = 4.0
            attack_ms = 5.0
            release_ms = 120.0
            [intro_sweep]
            bars = 8
            from_hz = 300.0
            to_hz = 18000.0
            [[master]]
            plugin = "eq"
            params = { "1" = 1.0 }
            [[master]]
            plugin = "limiter"
            params = { "1" = -1.0 }
        "#;
        let cfg = MashConfig::parse(text).expect("parse");
        let duck = cfg.duck.expect("duck");
        assert_eq!(duck.ratio, 4.0);
        assert_eq!(duck.knee_db, 6.0); // default
        let sweep = cfg.intro_sweep.expect("sweep");
        assert_eq!(sweep.bars, 8.0);
        assert_eq!(cfg.master.len(), 2);
        assert_eq!(cfg.master[1].plugin, "limiter");
    }

    #[test]
    fn offset_samples_is_sample_accurate() {
        // 120 BPM → 0.5 s/beat. 8 beats = 4 s. At 44.1k → 176_400 samples.
        assert_eq!(offset_to_samples(8.0, 120.0, 44_100), 176_400);
        // 4 beats at 48k, 120 BPM → 2 s → 96_000.
        assert_eq!(offset_to_samples(4.0, 120.0, 48_000), 96_000);
        // Fractional beat rounds to nearest sample.
        // 1 beat at 105.5 BPM = 60/105.5 s = 0.568720… s → ×44100 = 25080.6 → 25081.
        assert_eq!(offset_to_samples(1.0, 105.5, 44_100), 25_081);
        // Zero offset → zero.
        assert_eq!(offset_to_samples(0.0, 90.0, 44_100), 0);
    }
}
