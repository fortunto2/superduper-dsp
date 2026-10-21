//! `sdsp-bus serve` — the bus as an MCP server, for agents.
//!
//! Over stdio (`--stdio`, what the project's .mcp.json uses: the client
//! starts it, nothing to keep running) or streamable HTTP on
//! 127.0.0.1:7892/mcp. Not 7891: that is each plugin's own MCP server.
//! Reads the bus (levels, spectra, masking, phase) and writes into plugins
//! through their mailboxes: parameters by name, documents (wavetables,
//! harmonics, captured timbres), the bus key, the song key.

use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    schemars,
    transport::streamable_http_server::{session::local::LocalSessionManager, tower::StreamableHttpServerConfig, StreamableHttpService},
    ErrorData as McpError, ServerHandler,
};
// Names the prompt_handler macro expands to.
use rmcp::model::{GetPromptRequestParams, GetPromptResult, ListPromptsResult, PaginatedRequestParams};
use rmcp::{service::RequestContext, RoleServer};
use serde::{Deserialize, Serialize};
use superduper_dsp_sdk::bus::{band_edges, Reader, Snapshot, BANDS, TRACK_MASTER};
use superduper_synth_core::drumlib::{parse_key_name, song_key_path};
use superduper_synth_core::melody::NOTE_NAMES;
use superduper_synth_core::mixmap::{loudest_per_track, sum_deficit_db, MaskingTracker, TrackSpectrum};

/// Track name → plugin that stands for the track's output.
pub type Prefer = std::collections::HashMap<String, String>;
/// Track name → its fader gain in dB (the bus measures pre-fader).
pub type FaderDb = std::collections::HashMap<String, f32>;

/// A per-track map entry for `track`, matched like every other track input:
/// exact, then case-insensitive, then containment either way (the bus cuts
/// names at 64 bytes, so the DAW's full name may be longer than the bus's).
/// `track` as the agent's map (`prefer`, `fader_db`) names it: exact, then
/// case-insensitive, then containment either way when that names one entry.
/// `on_bus` is every track name in the frame: an entry that is exactly some
/// other track's name belongs to it alone, so {"Gtr": …} with both "Gtr"
/// and "Gtr DI" on the bus never moves "Gtr DI" (measured 2026-10-21: a
/// partial map silently moved the wrong track).
fn lookup<'a, V>(map: &'a std::collections::HashMap<String, V>, track: &str, on_bus: &[&str]) -> Option<&'a V> {
    let t = track.to_lowercase();
    map.get(track)
        .or_else(|| map.iter().find(|(k, _)| k.to_lowercase() == t).map(|(_, v)| v))
        .or_else(|| {
            let owned = |k: &str| on_bus.iter().any(|n| n.eq_ignore_ascii_case(k) && !n.eq_ignore_ascii_case(track));
            let hits: Vec<&V> = map.iter()
                .filter(|(k, _)| { let kl = k.to_lowercase(); (t.contains(&kl) || kl.contains(&t)) && !owned(k) })
                .map(|(_, v)| v).collect();
            if hits.len() == 1 { Some(hits[0]) } else { None }
        })
}

/// Distinct track names in one bus read, for `lookup`.
fn names_of(rows: &[Snapshot]) -> Vec<&str> {
    let mut v: Vec<&str> = rows.iter().map(|s| s.track.as_str()).filter(|t| !t.is_empty()).collect();
    v.sort_unstable();
    v.dedup();
    v
}

/// The bus reads silence from an instance that has had no samples for 500 ms.
fn silent_db(db: f32) -> bool { db <= -119.0 }

/// One spectrum per track. The bus has no chain order, so by default a track
/// is its loudest instance — wrong once a cut follows the instrument (the EQ
/// after it is quieter than it). `prefer` names the instance that is last in
/// the chain, which the agent reads from the DAW.
fn tracks(rows: &[Snapshot], prefer: &Prefer) -> Vec<TrackSpectrum> {
    // Track names in `prefer` match like everywhere else (case-insensitive,
    // substring); a preferred plugin that is not on the track falls back to
    // the loudest instance instead of silently dropping the track.
    let on_bus = names_of(rows);
    let pref_for = |track: &str| lookup(prefer, track, &on_bus).map(|p| p.to_lowercase());
    // A bypassed plugin publishes nothing (its bypass path skips the scope),
    // so a preferred instance reading as bus silence is not the track's
    // output either: fall back to the loudest instance then.
    let silent = |s: &Snapshot| silent_db(s.rms_db);
    let keep = |s: &&Snapshot| match pref_for(&s.track) {
        Some(p) => {
            let usable = |o: &Snapshot| o.track == s.track && o.plugin.to_lowercase().contains(&p) && !silent(o);
            s.plugin.to_lowercase().contains(&p) && !silent(s) || !rows.iter().any(usable)
        }
        None => true,
    };
    use superduper_synth_core::mixmap::is_return_name;
    loudest_per_track(rows.iter().filter(keep).map(|s| (s.track.as_str(), s.is_bus() || is_return_name(&s.track), s.rms_db, &s.bands_db)))
}

fn key_out(p: &std::path::Path, key: Option<String>) -> rmcp::Json<KeyOut> {
    rmcp::Json(KeyOut { key, file: p.display().to_string() })
}

fn reader() -> Result<Reader, McpError> {
    Reader::open().ok_or_else(|| McpError::internal_error(format!("cannot open {}", superduper_dsp_sdk::bus::bus_path().display()), None))
}

/// `n` reads of the bus, 100 ms apart: one frame per tick, what the
/// listening tools collect before they measure anything.
async fn listen(r: &Reader, n: usize) -> Vec<Vec<Snapshot>> {
    let mut frames = Vec::with_capacity(n);
    for _ in 0..n {
        frames.push(r.read_all());
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    frames
}

const NO_TRACKS_MASKING: &str = "nothing to measure: no named track on the bus (transport stopped, or no CLAP SuperDuper plugin on a track) — this is not 'no clashes'";
const NO_MASTER: &str = "no SuperDuper plugin on the master: put one there (Spectrum or EQ, first in the chain)";
const NO_TRACKS_PHASE: &str = "no named tracks on the bus — the host gives no track names (CLAP track-info) or every SuperDuper plugin is a wrapped AU/VST3; use the CLAP versions";

/// `mix_masking` with the listening already done: `frames` is one bus read
/// per 100 ms tick. Pure, so the known-answer controls (`controls.rs`,
/// `sdsp-bus controls`) can plant a bus and check what the meter says of
/// it. `seconds` comes out as frames/10 and `label` empty; the tool
/// overwrites both with what it was asked for.
pub fn masking_report(frames: &[Vec<Snapshot>], prefer: &Prefer, fader_db: Option<&FaderDb>) -> Result<MaskingOut, String> {
    let mut tracker = MaskingTracker::mean();
    // Per track, power sums over the frames it was audible in — per band for
    // `by_db`, broadband for `levels` — so a part that plays half the run
    // reads at its playing level, not 3 dB under it (what a whole-run mean
    // said; measured by `sdsp-bus controls`).
    #[derive(Default)]
    struct Acc { band: [f64; BANDS], band_n: [f64; BANDS], rms: f64, rms_n: f64 }
    let mut sums: std::collections::HashMap<String, Acc> = Default::default();
    let mut names: Vec<String> = Vec::new();
    for rows in frames {
        let mut tracks = tracks(rows, prefer);
        let on_bus = names_of(rows);
        if let Some(f) = fader_db {
            for t in &mut tracks {
                if let Some(g) = lookup(f, &t.name, &on_bus) {
                    t.rms_db += g;
                    for b in t.bands_db.iter_mut() { *b += g; }
                }
            }
        }
        for t in &tracks {
            if !t.bus && !names.contains(&t.name) { names.push(t.name.clone()); }
            if silent_db(t.rms_db) { continue; }
            let a = sums.entry(t.name.clone()).or_default();
            a.rms += 10f64.powf(t.rms_db as f64 / 10.0);
            a.rms_n += 1.0;
            for (i, v) in t.bands_db.iter().enumerate() {
                if *v > -100.0 {
                    a.band[i] += 10f64.powf(*v as f64 / 10.0);
                    a.band_n[i] += 1.0;
                }
            }
        }
        tracker.update(&tracks);
    }
    let edges = band_edges();
    let avg = |name: &str, b: usize| sums.get(name).map_or(-120.0, |a| 10.0 * (a.band[b] / a.band_n[b].max(1.0) + 1e-12).log10()) as f32;
    let clashes = tracker.top(8).into_iter().filter(|c| c.share > 0.05).map(|c| {
        let (la, lb) = (avg(&c.a, c.band), avg(&c.b, c.band));
        // Within half a dB neither side is louder; the field says so.
        let (louder, by) = if (la - lb).abs() < 0.5 { (String::new(), (la - lb).abs()) }
            else if la > lb { (c.a.clone(), la - lb) } else { (c.b.clone(), lb - la) };
        ClashOut { low_hz: edges[c.band], high_hz: edges[c.band + 1], share: c.share, louder, by_db: by, a: c.a, b: c.b }
    }).collect();
    if names.is_empty() {
        return Err(NO_TRACKS_MASKING.into());
    }
    let mut levels: Vec<TrackLevel> = names.iter()
        .map(|t| TrackLevel { track: t.clone(), rms_db: sums.get(t).map_or(-120.0, |a| (10.0 * (a.rms / a.rms_n.max(1.0) + 1e-12).log10()) as f32) })
        .collect();
    levels.sort_by(|a, b| a.track.cmp(&b.track));
    let buried = tracker.top_buried(8, None).into_iter().filter(|b| b.share > 0.2).map(|b| BuriedOut {
        low_hz: edges[b.band], high_hz: edges[b.band + 1], share: b.share, quiet: b.quiet, under: b.loud,
    }).collect();
    Ok(MaskingOut { label: None, seconds: frames.len() as f32 / 10.0, tracks: names, clashes, buried, levels })
}

/// The master instance a phase run reads: flagged by the host or named
/// "Master", narrowed to `plugin` (substring) when given; the loudest wins.
fn master_of<'a>(rows: &'a [Snapshot], plugin: Option<&str>) -> Option<&'a Snapshot> {
    let want = plugin.map(str::to_lowercase);
    rows.iter()
        .filter(|s| s.track_flags & TRACK_MASTER != 0 || s.track.eq_ignore_ascii_case("master"))
        .filter(|s| want.as_ref().is_none_or(|p| s.plugin.to_lowercase().contains(p.as_str())))
        .max_by(|a, b| a.rms_db.total_cmp(&b.rms_db))
}

/// `mix_phase` with the listening already done (see `masking_report`).
/// Every frame must have a master instance; `seconds` comes out as frames/10.
pub fn phase_report(frames: &[Vec<Snapshot>], prefer: &Prefer, fader_db: Option<&FaderDb>, master_plugin: Option<&str>) -> Result<PhaseOut, String> {
    let (mut tp, mut mp) = ([0f64; BANDS], [0f64; BANDS]);
    let mut master_name = String::new();
    let mut contrib: std::collections::HashMap<String, [f64; BANDS]> = Default::default();
    for rows in frames {
        let m = master_of(rows, master_plugin).ok_or_else(|| NO_MASTER.to_string())?;
        master_name = m.plugin.clone();
        for (b, v) in m.bands_db.iter().enumerate() { mp[b] += 10f64.powf(*v as f64 / 10.0); }
        let on_bus = names_of(rows);
        for t in tracks(rows, prefer).into_iter().filter(|t| !t.bus) {
            let g = fader_db.and_then(|f| lookup(f, &t.name, &on_bus)).copied().unwrap_or(0.0);
            let c = contrib.entry(t.name.clone()).or_insert([0.0; BANDS]);
            for (b, v) in t.bands_db.iter().enumerate() {
                let p = 10f64.powf((*v + g) as f64 / 10.0);
                tp[b] += p;
                c[b] += p;
            }
        }
    }
    if contrib.is_empty() {
        // Every instance read as a bus: unnamed tracks (a host without CLAP
        // track-info, or AU/VST3 wrappers). All zeros would read as "fine".
        return Err(NO_TRACKS_PHASE.into());
    }
    let d = sum_deficit_db(&tp, &mp);
    let edges = band_edges();
    let nf = frames.len() as f64;
    let mut cancelling: Vec<PhaseBand> = (0..BANDS).filter(|&b| d[b] < -2.0 && tp[b] > 1e-9 * nf).map(|b| {
        let mut who: Vec<(&String, f64)> = contrib.iter().map(|(n, c)| (n, c[b])).collect();
        who.sort_by(|x, y| y.1.total_cmp(&x.1));
        PhaseBand { low_hz: edges[b], high_hz: edges[b + 1], deficit_db: d[b], tracks: who.into_iter().take(3).filter(|w| w.1 > 0.0).map(|w| w.0.clone()).collect() }
    }).collect();
    cancelling.sort_by(|a, b| a.deficit_db.total_cmp(&b.deficit_db));
    let not_on_bus_hz = (0..BANDS).filter(|&b| d[b] > 6.0).map(|b| (edges[b] * edges[b + 1]).sqrt()).collect();
    Ok(PhaseOut { seconds: frames.len() as f32 / 10.0, master: master_name, cancelling, deficit_db: d.to_vec(), not_on_bus_hz })
}

#[derive(Serialize, schemars::JsonSchema)]
pub struct Instance {
    /// Host track name; "Master" for the master; empty if the host gave none.
    pub track: String,
    /// Master, group bus or return: carries other tracks, never masked.
    pub bus: bool,
    pub plugin: String,
    pub rms_db: f32,
    pub peak_db: f32,
    /// Key the plugin detected, if it detects one.
    pub key: Option<String>,
}

#[derive(Serialize, schemars::JsonSchema)]
pub struct BusList { pub instances: Vec<Instance>, pub tracks: Vec<String> }

#[derive(Deserialize, schemars::JsonSchema)]
pub struct TrackInput {
    /// Track name as bus_list shows it (case-insensitive, substring ok).
    pub track: String,
}

#[derive(Serialize, schemars::JsonSchema)]
pub struct TrackSpectrumOut {
    pub track: String,
    pub rms_db: f32,
    /// 33 edges, 20 Hz .. 20 kHz, log-spaced.
    pub band_edges_hz: Vec<f32>,
    /// 32 band RMS levels in dB; they sum (in power) to rms_db.
    pub bands_db: Vec<f32>,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct MaskingInput {
    /// How long to listen while the song plays (default 6, max 60).
    pub seconds: Option<f32>,
    /// Keep this measurement under a name ("before", "cut-300-gtr") for
    /// mix_compare. Play the same section for every labelled run.
    pub label: Option<String>,
    /// Track → plugin (substring, e.g. "EQ") to measure the track at: the
    /// last enabled SuperDuper plugin in its chain, from the DAW. Without it
    /// a track is its loudest instance, which misses a cut placed after the
    /// instrument. Use the same map for runs you compare.
    pub prefer: Option<Prefer>,
    /// Track → its fader gain in dB (and any post-plugin volume). The bus
    /// measures where the plugin sits, before the fader: a guitar pulled
    /// down 10 dB still reads loud without this. Read it from the DAW
    /// (REAPER: track D_VOL → 20·log10) and pass the same map for runs you compare.
    pub fader_db: Option<FaderDb>,
}

#[derive(Serialize, Deserialize, Clone, schemars::JsonSchema)]
pub struct ClashOut {
    pub a: String,
    pub b: String,
    pub low_hz: f32,
    pub high_hz: f32,
    /// Share of the listened time that band clashed (both > −50 dB, within 6 dB).
    pub share: f32,
    /// Which of the two is louder there while it plays, and by how much;
    /// empty when they are within 0.5 dB.
    pub louder: String,
    pub by_db: f32,
}

#[derive(Serialize, Deserialize, Clone, schemars::JsonSchema)]
pub struct BuriedOut {
    /// The part that is playing but cannot be heard there.
    pub quiet: String,
    /// The part on top of it.
    pub under: String,
    pub low_hz: f32,
    pub high_hz: f32,
    /// Share of the listened time it sat 6–20 dB under `under` in this band.
    pub share: f32,
}

#[derive(Serialize, Deserialize, schemars::JsonSchema)]
pub struct TrackLevel { pub track: String, pub rms_db: f32 }

#[derive(Serialize, Deserialize, schemars::JsonSchema)]
pub struct MaskingOut {
    pub label: Option<String>,
    pub seconds: f32,
    pub tracks: Vec<String>,
    pub clashes: Vec<ClashOut>,
    /// The other masking: a playing part 6–20 dB under a louder one in a band.
    /// Only a problem for a part that has to be heard there — check the ones
    /// that matter (the lead vocal), not the list.
    #[serde(default)]
    pub buried: Vec<BuriedOut>,
    /// Each track's average level over the run (power mean).
    pub levels: Vec<TrackLevel>,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct CompareInput {
    /// Label of the earlier mix_masking run.
    pub before: String,
    /// Label of the later one.
    pub after: String,
}

#[derive(Serialize, schemars::JsonSchema)]
pub struct ClashDelta {
    pub a: String,
    pub b: String,
    pub low_hz: f32,
    pub high_hz: f32,
    /// Share of time clashing in each run; 0 = not in that run's list.
    pub before: f32,
    pub after: f32,
}

#[derive(Serialize, schemars::JsonSchema)]
pub struct LevelDelta { pub track: String, pub before_db: Option<f32>, pub after_db: Option<f32> }

#[derive(Serialize, schemars::JsonSchema)]
pub struct CompareOut { pub clashes: Vec<ClashDelta>, pub levels: Vec<LevelDelta> }

/// Labelled mix_masking runs, one JSON per line; the latest run of a label wins.
fn mix_log_path() -> std::path::PathBuf {
    song_key_path().with_file_name("mix-log.jsonl")
}

fn load_run(label: &str) -> Result<MaskingOut, McpError> {
    let text = std::fs::read_to_string(mix_log_path()).unwrap_or_default();
    let runs: Vec<MaskingOut> = text.lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
    let labels: Vec<&str> = runs.iter().filter_map(|r| r.label.as_deref()).collect();
    let msg = format!("no run labelled '{label}'; have: {}", labels.join(", "));
    runs.iter().rposition(|r| r.label.as_deref() == Some(label))
        .map(|i| runs.into_iter().nth(i).expect("index from rposition"))
        .ok_or_else(|| McpError::invalid_params(msg, None))
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct KeyInput {
    /// "A", "F#m", "Bb major" … or "" to clear.
    pub key: String,
}

#[derive(Serialize, schemars::JsonSchema)]
pub struct KeyOut { pub key: Option<String>, pub file: String }

#[derive(Deserialize, schemars::JsonSchema)]
pub struct DocInput {
    /// Track name as bus_list shows it (case-insensitive, substring ok).
    pub track: String,
    /// Plugin name or part of it ("Wave", "Kubyz").
    pub plugin: String,
    /// Document name from plugin_docs ("wavetable"); empty for plugin_docs.
    #[serde(default)]
    pub doc: String,
    /// For plugin_set: the new document, in the shape plugin_docs describes.
    #[serde(default)]
    pub value: serde_json::Value,
}

/// The one live instance of `plugin` on `track`, by its bus id.
fn find_instance(track: &str, plugin: &str) -> Result<u64, McpError> {
    let rows = reader()?.read_all();
    let (t, p) = (track.to_lowercase(), plugin.to_lowercase());
    // An exact track name wins over substring matches ("Vox" must be
    // nameable next to "Vox Delay"), then the same for the plugin name.
    let exact: Vec<&Snapshot> = rows.iter().filter(|s| s.track.to_lowercase() == t).collect();
    let on_track: Vec<&Snapshot> = if exact.is_empty() { rows.iter().filter(|s| s.track.to_lowercase().contains(&t)).collect() } else { exact };
    let plugin_exact: Vec<&Snapshot> = on_track.iter().copied().filter(|s| s.plugin.to_lowercase() == p || s.plugin.to_lowercase() == format!("superduper {p}")).collect();
    let hits: Vec<&Snapshot> = if plugin_exact.is_empty() { on_track.into_iter().filter(|s| s.plugin.to_lowercase().contains(&p)).collect() } else { plugin_exact };
    match hits.as_slice() {
        [one] => Ok(one.instance),
        [] => Err(McpError::invalid_params(format!("no '{plugin}' on a track matching '{track}'"), None)),
        many => Err(McpError::invalid_params(format!(
            "{} instances match: {} — name the track exactly",
            many.len(),
            many.iter().map(|s| format!("{} / {}", s.track, s.plugin)).collect::<Vec<_>>().join("; ")
        ), None)),
    }
}

/// MCP wants an object schema at the root, so the document rides in `value`.
#[derive(Serialize, schemars::JsonSchema)]
pub struct DocOut { pub value: serde_json::Value }

async fn mail(track: &str, plugin: &str, doc: &str, op: &str, value: serde_json::Value) -> Result<rmcp::Json<DocOut>, McpError> {
    let id = find_instance(track, plugin)?;
    let (doc, op) = (doc.to_string(), op.to_string());
    let out = tokio::task::spawn_blocking(move || {
        superduper_synth_core::bus_mail::call(id, &doc, &op, value, std::time::Duration::from_secs(3))
    }).await.map_err(|e| McpError::internal_error(e.to_string(), None))?;
    out.map(|value| rmcp::Json(DocOut { value })).map_err(|e| McpError::invalid_params(e, None))
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct SetParamsInput {
    pub track: String,
    pub plugin: String,
    /// {"Param Name": value, …} — names exactly as plugin_params lists them
    /// (case-insensitive); values are clamped to each parameter's range.
    pub values: serde_json::Map<String, serde_json::Value>,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct DuckInput {
    /// The track to push down (its last SuperDuper Soothe does the work).
    pub track: String,
    /// The track it makes room for (as bus_list shows it); "" removes the duck.
    pub key: String,
    /// Deepest cut per band while the key is loud there, dB (default 6, max 24).
    pub depth_db: Option<f32>,
    /// How far above -50 dB the key must be in a band for the full depth (default 24).
    pub range_db: Option<f32>,
    /// Set Soothe's own resonance Amount too — pass 0 when this Soothe was
    /// added only for ducking; omit to leave it.
    pub amount_db: Option<f32>,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct PhaseInput {
    /// How long to listen while the song plays (default 10, max 60).
    pub seconds: Option<f32>,
    /// Same as mix_masking: track → plugin that stands for its output.
    pub prefer: Option<Prefer>,
    /// Same as mix_masking: track → fader dB.
    pub fader_db: Option<FaderDb>,
    /// Which master plugin to read (substring) — the first on the master is
    /// closest to the raw sum; default: the loudest master instance.
    pub master_plugin: Option<String>,
}

#[derive(Serialize, schemars::JsonSchema)]
pub struct PhaseBand {
    pub low_hz: f32,
    pub high_hz: f32,
    /// Master minus the sum of the tracks, broadband part removed. ≈ 0 = the
    /// parts add up; negative = they cancel here.
    pub deficit_db: f32,
    /// The loudest tracks in this band: the suspects.
    pub tracks: Vec<String>,
}

#[derive(Serialize, schemars::JsonSchema)]
pub struct PhaseOut {
    pub seconds: f32,
    pub master: String,
    /// Bands losing more than 2 dB to cancellation, worst first.
    pub cancelling: Vec<PhaseBand>,
    /// All 32 bands, low to high (dB, broadband removed).
    pub deficit_db: Vec<f32>,
    /// Bands where the master has much more than the tracks: sources that
    /// are not on the bus (no SuperDuper plugin), which hide cancellation.
    pub not_on_bus_hz: Vec<f32>,
}

#[derive(Clone)]
pub struct BusServer {
    tool_router: ToolRouter<Self>,
    prompt_router: rmcp::handler::server::router::prompt::PromptRouter<Self>,
}

#[rmcp::tool_router]
impl BusServer {
    pub fn new() -> Self { Self { tool_router: Self::tool_router(), prompt_router: Self::prompt_router() } }

    #[rmcp::tool(description = "Every running SuperDuper plugin instance: track, plugin, level, peak, detected key. Empty when no host is running our plugins.", annotations(read_only_hint = true, open_world_hint = false))]
    async fn bus_list(&self) -> Result<rmcp::Json<BusList>, McpError> {
        let rows = reader()?.read_all();
        let mut tracks: Vec<String> = rows.iter().map(|r| r.track.clone()).collect();
        tracks.sort();
        tracks.dedup();
        let instances = rows.into_iter().map(|r| Instance {
            bus: r.is_bus(), track: r.track, plugin: r.plugin, rms_db: r.rms_db, peak_db: r.peak_db, key: r.key.map(|k| NOTE_NAMES[k as usize].to_string()),
        }).collect();
        Ok(rmcp::Json(BusList { instances, tracks }))
    }

    #[rmcp::tool(description = "Current 32-band spectrum of one track (its loudest SuperDuper instance).", annotations(read_only_hint = true, open_world_hint = false))]
    async fn track_spectrum(&self, Parameters(TrackInput { track }): Parameters<TrackInput>) -> Result<rmcp::Json<TrackSpectrumOut>, McpError> {
        let all = tracks(&reader()?.read_all(), &Prefer::new());
        let q = track.to_lowercase();
        let t = all.iter().find(|t| t.name.to_lowercase() == q).or_else(|| all.iter().find(|t| t.name.to_lowercase().contains(&q)))
            .ok_or_else(|| McpError::invalid_params(format!("no track matching '{track}' on the bus"), None))?;
        Ok(rmcp::Json(TrackSpectrumOut { track: t.name.clone(), rms_db: t.rms_db, band_edges_hz: band_edges().to_vec(), bands_db: t.bands_db.to_vec() }))
    }

    #[rmcp::tool(description = "Listen to the bus for a few seconds of playback and report which track pairs mask each other in which band, most persistent first (clashes: both loud and within 6 dB), and which parts are buried 6-20 dB under a louder one (buried — a problem only for a part that must be heard there). Masters, group buses and returns excluded. Use it to decide EQ cuts.", annotations(read_only_hint = true, open_world_hint = false))]
    async fn mix_masking(&self, Parameters(MaskingInput { seconds, label, prefer, fader_db }): Parameters<MaskingInput>) -> Result<rmcp::Json<MaskingOut>, McpError> {
        let secs = seconds.unwrap_or(6.0).clamp(1.0, 60.0);
        let frames = listen(&reader()?, (secs * 10.0) as usize).await;
        let mut out = masking_report(&frames, prefer.as_ref().unwrap_or(&Prefer::new()), fader_db.as_ref())
            .map_err(|e| McpError::invalid_params(e, None))?;
        out.label = label;
        out.seconds = secs;
        if out.label.is_some() {
            use std::io::Write;
            let line = serde_json::to_string(&out).map_err(|e| McpError::internal_error(e.to_string(), None))?;
            std::fs::OpenOptions::new().create(true).append(true).open(mix_log_path())
                .and_then(|mut f| writeln!(f, "{line}"))
                .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        }
        Ok(rmcp::Json(out))
    }

    #[rmcp::tool(description = "Where parts cancel when summed (Sound Radix Pi's question): listens to the master and every track, and per band compares the master's energy with the sum of the tracks' — the broadband part (master limiter, fader offsets) removed. A band 2+ dB short is cancellation; its loudest tracks are the suspects (kick vs bass, layered kicks, two mics). Fix by flipping one suspect's polarity in the DAW (REAPER run_lua: SetMediaTrackInfo_Value(track, \"B_PHASE\", 1)) or nudging it a few ms, then run again and keep only what shrinks the deficit. Needs a SuperDuper plugin on the master and on every track that matters.", annotations(read_only_hint = true, open_world_hint = false))]
    async fn mix_phase(&self, Parameters(i): Parameters<PhaseInput>) -> Result<rmcp::Json<PhaseOut>, McpError> {
        let secs = i.seconds.unwrap_or(10.0).clamp(2.0, 60.0);
        let r = reader()?;
        let n = (secs * 10.0) as usize;
        let prefer = i.prefer.unwrap_or_default();
        let mut frames = Vec::with_capacity(n);
        for _ in 0..n {
            let rows = r.read_all();
            // Say so on the first frame without a master, not after the whole listen.
            if master_of(&rows, i.master_plugin.as_deref()).is_none() {
                return Err(McpError::invalid_params(NO_MASTER, None));
            }
            frames.push(rows);
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        let mut out = phase_report(&frames, &prefer, i.fader_db.as_ref(), i.master_plugin.as_deref())
            .map_err(|e| McpError::invalid_params(e, None))?;
        out.seconds = secs;
        Ok(rmcp::Json(out))
    }

    #[rmcp::tool(description = "Compare two labelled mix_masking runs (same section of the song): each clash's share before and after, and each track's level. Use after a change to see whether it helped; undo it in the DAW if it did not.", annotations(read_only_hint = true, open_world_hint = false))]
    async fn mix_compare(&self, Parameters(CompareInput { before, after }): Parameters<CompareInput>) -> Result<rmcp::Json<CompareOut>, McpError> {
        let (b, a) = (load_run(&before)?, load_run(&after)?);
        let mut clashes: Vec<ClashDelta> = Vec::new();
        for (c, is_after) in b.clashes.iter().map(|c| (c, false)).chain(a.clashes.iter().map(|c| (c, true))) {
            let i = clashes.iter().position(|d| d.a == c.a && d.b == c.b && d.low_hz == c.low_hz).unwrap_or_else(|| {
                clashes.push(ClashDelta { a: c.a.clone(), b: c.b.clone(), low_hz: c.low_hz, high_hz: c.high_hz, before: 0.0, after: 0.0 });
                clashes.len() - 1
            });
            if is_after { clashes[i].after = c.share } else { clashes[i].before = c.share }
        }
        clashes.sort_by(|x, y| (y.before - y.after).abs().total_cmp(&(x.before - x.after).abs()));
        let mut names: Vec<&str> = b.levels.iter().chain(&a.levels).map(|l| l.track.as_str()).collect();
        names.sort();
        names.dedup();
        let find = |run: &MaskingOut, n: &str| run.levels.iter().find(|l| l.track == n).map(|l| l.rms_db);
        let levels = names.into_iter().map(|n| LevelDelta { track: n.to_string(), before_db: find(&b, n), after_db: find(&a, n) }).collect();
        Ok(rmcp::Json(CompareOut { clashes, levels }))
    }

    #[rmcp::tool(description = "Which documents a plugin instance exposes beyond its parameters (Wave: \"wavetable\"), with the shape plugin_get returns and plugin_set takes.", annotations(read_only_hint = true, open_world_hint = false))]
    async fn plugin_docs(&self, Parameters(i): Parameters<DocInput>) -> Result<rmcp::Json<DocOut>, McpError> {
        mail(&i.track, &i.plugin, "", "list", serde_json::Value::Null).await
    }

    #[rmcp::tool(description = "Read a plugin document (e.g. Wave's wavetable as harmonic amplitudes per frame) from the running instance on a track.", annotations(read_only_hint = true, open_world_hint = false))]
    async fn plugin_get(&self, Parameters(i): Parameters<DocInput>) -> Result<rmcp::Json<DocOut>, McpError> {
        mail(&i.track, &i.plugin, &i.doc, "get", serde_json::Value::Null).await
    }

    #[rmcp::tool(description = "Replace a plugin document in the running instance (e.g. Wave wavetable {harmonics: [[1, 0.3, 0, 0.1]]} — fundamental, some 2nd, no 3rd). Read it first with plugin_get to keep the old one for undo; measure with mix_masking before and after.", annotations(read_only_hint = false, destructive_hint = false, idempotent_hint = true, open_world_hint = false))]
    async fn plugin_set(&self, Parameters(i): Parameters<DocInput>) -> Result<rmcp::Json<DocOut>, McpError> {
        mail(&i.track, &i.plugin, &i.doc, "set", i.value).await
    }

    #[rmcp::tool(description = "Every parameter of a running SuperDuper plugin (name, value, min, max, default, unit) — read before setting.", annotations(read_only_hint = true, open_world_hint = false))]
    async fn plugin_params(&self, Parameters(i): Parameters<DocInput>) -> Result<rmcp::Json<DocOut>, McpError> {
        mail(&i.track, &i.plugin, "params", "get", serde_json::Value::Null).await
    }

    #[rmcp::tool(description = "Set parameters of a running SuperDuper plugin by name, without the DAW: clamped to range, and the host sees the change (automation, project save). Read plugin_params first and keep the old values — that is the undo.", annotations(read_only_hint = false, destructive_hint = false, idempotent_hint = true, open_world_hint = false))]
    async fn plugin_set_params(&self, Parameters(i): Parameters<SetParamsInput>) -> Result<rmcp::Json<DocOut>, McpError> {
        mail(&i.track, &i.plugin, "params", "set", serde_json::Value::Object(i.values)).await
    }

    #[rmcp::tool(description = "Duck one track under another in the bands where they fight, only while the key plays (Trackspacer, no routing): sets the bus key and Key Depth/Range of the SuperDuper Soothe on `track`. Use for a part that must stay but yields to a more important one (guitar/pad under the vocal, synth under the lead); not for kick-bass pumping (the bus lags 50-100 ms — use the compressor side-chain). Soothe must be the last enabled plugin on that track: add it with REAPER MCP track_fx_add_by_name \"CLAP: SuperDuper Soothe\" and pass amount_db: 0 if it is only there to duck. Measure mix_masking (prefer {track: \"Soothe\"}) before and after and mix_compare.", annotations(read_only_hint = false, destructive_hint = false, idempotent_hint = true, open_world_hint = false))]
    async fn duck_under(&self, Parameters(i): Parameters<DuckInput>) -> Result<rmcp::Json<DocOut>, McpError> {
        let key = mail(&i.track, "Soothe", "key", "set", serde_json::json!({"track": i.key})).await?;
        let mut v = serde_json::Map::new();
        v.insert("Key Depth".into(), serde_json::json!(if i.key.is_empty() { 0.0 } else { i.depth_db.unwrap_or(6.0) }));
        v.insert("Key Range".into(), serde_json::json!(i.range_db.unwrap_or(24.0)));
        if let Some(a) = i.amount_db { v.insert("Amount".into(), serde_json::json!(a)); }
        let params = mail(&i.track, "Soothe", "params", "set", serde_json::Value::Object(v)).await?;
        Ok(rmcp::Json(DocOut { value: serde_json::json!({"key": key.0.value, "params": params.0.value}) }))
    }

    #[rmcp::tool(description = "The song key the SuperDuper Drum's Key=Auto follows (from ~/.superduper-dsp/song-key).", annotations(read_only_hint = true, open_world_hint = false))]
    async fn get_song_key(&self) -> Result<rmcp::Json<KeyOut>, McpError> {
        let p = song_key_path();
        let key = std::fs::read_to_string(&p).ok().map(|t| t.trim().to_string()).filter(|t| !t.is_empty());
        Ok(key_out(&p, key))
    }

    #[rmcp::tool(description = "Set the song key every SuperDuper plugin can follow (Drum Key=Auto retunes kick/808/toms to it). Accepts 'A', 'F#m', 'Bb major'; empty clears it.", annotations(read_only_hint = false, destructive_hint = false, idempotent_hint = true, open_world_hint = false))]
    async fn set_song_key(&self, Parameters(KeyInput { key }): Parameters<KeyInput>) -> Result<rmcp::Json<KeyOut>, McpError> {
        let p = song_key_path();
        let k = key.trim();
        if k.is_empty() {
            let _ = std::fs::remove_file(&p);
            return Ok(key_out(&p, None));
        }
        if parse_key_name(k).is_none() {
            return Err(McpError::invalid_params(format!("'{k}' is not a key — try 'A', 'F#m', 'Bb major'"), None));
        }
        if let Some(d) = p.parent() { let _ = std::fs::create_dir_all(d); }
        std::fs::write(&p, format!("{k}\n")).map_err(|e| McpError::internal_error(e.to_string(), None))?;
        Ok(key_out(&p, Some(k.to_string())))
    }
}

impl Default for BusServer { fn default() -> Self { Self::new() } }

/// The `sdsp://bus/snapshot` resource: every instance with its spectrum.
pub(crate) fn snapshot_json() -> Result<String, McpError> {
    let rows: Vec<_> = reader()?.read_all().into_iter().map(|s| serde_json::json!({
        "track": s.track, "plugin": s.plugin, "bus": s.is_bus(), "rms_db": s.rms_db, "peak_db": s.peak_db,
        "key": s.key.map(|k| NOTE_NAMES[k as usize]), "bands_db": s.bands_db.to_vec(),
    })).collect();
    serde_json::to_string_pretty(&serde_json::json!({"band_edges_hz": band_edges().to_vec(), "instances": rows}))
        .map_err(|e| McpError::internal_error(e.to_string(), None))
}

#[rmcp::tool_handler]
#[rmcp::prompt_handler(router = self.prompt_router)]
impl ServerHandler for BusServer {
    async fn list_resources(&self, _r: Option<rmcp::model::PaginatedRequestParams>, _c: rmcp::service::RequestContext<rmcp::RoleServer>) -> Result<rmcp::model::ListResourcesResult, McpError> {
        Ok(rmcp::model::ListResourcesResult::with_all_items(crate::guide::resources()))
    }

    async fn read_resource(&self, r: rmcp::model::ReadResourceRequestParams, _c: rmcp::service::RequestContext<rmcp::RoleServer>) -> Result<rmcp::model::ReadResourceResult, McpError> {
        crate::guide::read(&r.uri)
    }

    fn get_info(&self) -> rmcp::model::ServerInfo {
        let info = rmcp::model::Implementation::new("superduper-bus", env!("CARGO_PKG_VERSION")).with_title("SuperDuper bus");
        rmcp::model::ServerInfo::new(rmcp::model::ServerCapabilities::builder().enable_tools().enable_prompts().enable_resources().build())
            .with_protocol_version(rmcp::model::ProtocolVersion::LATEST)
            .with_server_info(info)
            .with_instructions("Live view of every SuperDuper plugin running in the DAW: bus_list, track_spectrum, mix_masking (who masks whom, while the song plays; label a run to keep it), mix_compare (two labelled runs: what a change did), mix_phase (bands where parts cancel in the sum), plugin_params/plugin_set_params (any SuperDuper plugin's parameters, no DAW needed), duck_under (Soothe keyed ducking), plugin_docs/get/set (state that is not a parameter, e.g. Wave's wavetable in harmonics; every plugin also has \"key\"), get/set_song_key. Changes and undo happen in the DAW (REAPER MCP); this server measures.")
    }
}

pub fn run(port: u16) -> std::io::Result<()> {
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    rt.block_on(async move {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
        eprintln!("sdsp-bus MCP on http://{}/mcp", listener.local_addr()?);
        let service = StreamableHttpService::new(
            || Ok(BusServer::new()),
            std::sync::Arc::new(LocalSessionManager::default()),
            StreamableHttpServerConfig::default().with_sse_keep_alive(None),
        );
        let router = axum::Router::new().nest_service("/mcp", service);
        axum::serve(listener, router).await
    })
}

pub fn run_stdio() -> std::io::Result<()> {
    use rmcp::ServiceExt;
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    rt.block_on(async {
        let svc = BusServer::new().serve(rmcp::transport::stdio()).await.map_err(std::io::Error::other)?;
        svc.waiting().await.map_err(std::io::Error::other)?;
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    /// rmcp checks every tool's output schema when the router is built and
    /// panics on a bad one — which killed the server at startup once
    /// (Json<Value> has no root "type"). Building the router is the test.
    #[test]
    fn every_tool_schema_is_accepted() {
        let tools = super::BusServer::new().tool_router.list_all();
        assert!(tools.len() >= 8, "{} tools", tools.len());
    }
}
