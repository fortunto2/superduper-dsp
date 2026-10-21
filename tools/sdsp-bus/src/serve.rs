//! `sdsp-bus serve` — the bus as an MCP server, for agents.
//!
//! Over stdio (`--stdio`, what the project's .mcp.json uses: the client
//! starts it, nothing to keep running) or streamable HTTP on
//! 127.0.0.1:7892/mcp. Not 7891: that is each plugin's own MCP server.
//! Read-only on the plugins, plus the one thing an agent writes today: the
//! song key Drum's Key = Auto follows.

use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    schemars,
    transport::streamable_http_server::{session::local::LocalSessionManager, tower::StreamableHttpServerConfig, StreamableHttpService},
    ErrorData as McpError, ServerHandler,
};
use serde::{Deserialize, Serialize};
use superduper_dsp_sdk::bus::{band_edges, Reader, Snapshot, BANDS};
use superduper_synth_core::drumlib::{parse_key_name, song_key_path};
use superduper_synth_core::melody::NOTE_NAMES;
use superduper_synth_core::mixmap::{loudest_per_track, MaskingTracker, TrackSpectrum};

/// Track name → plugin that stands for the track's output.
type Prefer = std::collections::HashMap<String, String>;

/// One spectrum per track. The bus has no chain order, so by default a track
/// is its loudest instance — wrong once a cut follows the instrument (the EQ
/// after it is quieter than it). `prefer` names the instance that is last in
/// the chain, which the agent reads from the DAW.
fn tracks(rows: &[Snapshot], prefer: &Prefer) -> Vec<TrackSpectrum> {
    let keep = |s: &&Snapshot| prefer.get(&s.track).is_none_or(|p| s.plugin.to_lowercase().contains(&p.to_lowercase()));
    loudest_per_track(rows.iter().filter(keep).map(|s| (s.track.as_str(), s.is_bus(), s.rms_db, &s.bands_db)))
}

fn key_out(p: &std::path::Path, key: Option<String>) -> rmcp::Json<KeyOut> {
    rmcp::Json(KeyOut { key, file: p.display().to_string() })
}

fn reader() -> Result<Reader, McpError> {
    Reader::open().ok_or_else(|| McpError::internal_error(format!("cannot open {}", superduper_dsp_sdk::bus::bus_path().display()), None))
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
    pub fader_db: Option<std::collections::HashMap<String, f32>>,
}

#[derive(Serialize, Deserialize, Clone, schemars::JsonSchema)]
pub struct ClashOut {
    pub a: String,
    pub b: String,
    pub low_hz: f32,
    pub high_hz: f32,
    /// Share of the listened time that band clashed (both > −50 dB, within 6 dB).
    pub share: f32,
    /// Which of the two is louder there on average, and by how much.
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
    let on_track = |s: &&Snapshot| s.track.to_lowercase() == t || s.track.to_lowercase().contains(&t);
    let hits: Vec<&Snapshot> = rows.iter().filter(on_track).filter(|s| s.plugin.to_lowercase().contains(&p)).collect();
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
    pub fader_db: Option<std::collections::HashMap<String, f32>>,
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
pub struct BusServer { tool_router: ToolRouter<Self> }

#[rmcp::tool_router]
impl BusServer {
    pub fn new() -> Self { Self { tool_router: Self::tool_router() } }

    #[rmcp::tool(description = "Every running SuperDuper plugin instance: track, plugin, level, peak, detected key. Empty when no host is running our plugins.")]
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

    #[rmcp::tool(description = "Current 32-band spectrum of one track (its loudest SuperDuper instance).")]
    async fn track_spectrum(&self, Parameters(TrackInput { track }): Parameters<TrackInput>) -> Result<rmcp::Json<TrackSpectrumOut>, McpError> {
        let all = tracks(&reader()?.read_all(), &Prefer::new());
        let q = track.to_lowercase();
        let t = all.iter().find(|t| t.name.to_lowercase() == q).or_else(|| all.iter().find(|t| t.name.to_lowercase().contains(&q)))
            .ok_or_else(|| McpError::invalid_params(format!("no track matching '{track}' on the bus"), None))?;
        Ok(rmcp::Json(TrackSpectrumOut { track: t.name.clone(), rms_db: t.rms_db, band_edges_hz: band_edges().to_vec(), bands_db: t.bands_db.to_vec() }))
    }

    #[rmcp::tool(description = "Listen to the bus for a few seconds of playback and report which track pairs mask each other in which band, most persistent first (clashes: both loud and within 6 dB), and which parts are buried 6-20 dB under a louder one (buried — a problem only for a part that must be heard there). Masters, group buses and returns excluded. Use it to decide EQ cuts.")]
    async fn mix_masking(&self, Parameters(MaskingInput { seconds, label, prefer, fader_db }): Parameters<MaskingInput>) -> Result<rmcp::Json<MaskingOut>, McpError> {
        let secs = seconds.unwrap_or(6.0).clamp(1.0, 60.0);
        let r = reader()?;
        let frames = (secs * 10.0) as usize;
        let mut tracker = MaskingTracker::mean();
        let mut sums: std::collections::HashMap<String, [f64; BANDS]> = Default::default();
        let mut power: std::collections::HashMap<String, f64> = Default::default();
        let mut last = Vec::new();
        for _ in 0..frames {
            let mut tracks = tracks(&r.read_all(), prefer.as_ref().unwrap_or(&Prefer::new()));
            if let Some(f) = &fader_db {
                for t in &mut tracks {
                    if let Some(g) = f.get(&t.name) {
                        t.rms_db += g;
                        for b in t.bands_db.iter_mut() { *b += g; }
                    }
                }
            }
            for t in &tracks {
                *power.entry(t.name.clone()).or_default() += 10f64.powf(t.rms_db as f64 / 10.0);
                let s = sums.entry(t.name.clone()).or_insert([0.0; BANDS]);
                for (i, v) in t.bands_db.iter().enumerate() { s[i] += 10f64.powf(*v as f64 / 10.0); }
            }
            tracker.update(&tracks);
            last = tracks;
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        let edges = band_edges();
        let avg = |n: &str, b: usize| sums.get(n).map_or(-120.0, |s| 10.0 * (s[b] / frames as f64 + 1e-12).log10()) as f32;
        let clashes = tracker.top(8).into_iter().filter(|c| c.share > 0.05).map(|c| {
            let (la, lb) = (avg(&c.a, c.band), avg(&c.b, c.band));
            let (louder, by) = if la >= lb { (c.a.clone(), la - lb) } else { (c.b.clone(), lb - la) };
            ClashOut { low_hz: edges[c.band], high_hz: edges[c.band + 1], share: c.share, louder, by_db: by, a: c.a, b: c.b }
        }).collect();
        let names = last.into_iter().filter(|t| !t.bus).map(|t| t.name).collect();
        let mut levels: Vec<TrackLevel> = power.into_iter()
            .map(|(track, p)| TrackLevel { track, rms_db: (10.0 * (p / frames as f64 + 1e-12).log10()) as f32 })
            .collect();
        levels.sort_by(|a, b| a.track.cmp(&b.track));
        let buried = tracker.top_buried(8, None).into_iter().filter(|b| b.share > 0.2).map(|b| BuriedOut {
            low_hz: edges[b.band], high_hz: edges[b.band + 1], share: b.share, quiet: b.quiet, under: b.loud,
        }).collect();
        let out = MaskingOut { label, seconds: secs, tracks: names, clashes, buried, levels };
        if out.label.is_some() {
            use std::io::Write;
            let line = serde_json::to_string(&out).map_err(|e| McpError::internal_error(e.to_string(), None))?;
            std::fs::OpenOptions::new().create(true).append(true).open(mix_log_path())
                .and_then(|mut f| writeln!(f, "{line}"))
                .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        }
        Ok(rmcp::Json(out))
    }

    #[rmcp::tool(description = "Where parts cancel when summed (Sound Radix Pi's question): listens to the master and every track, and per band compares the master's energy with the sum of the tracks' — the broadband part (master limiter, fader offsets) removed. A band 2+ dB short is cancellation; its loudest tracks are the suspects (kick vs bass, layered kicks, two mics). Fix by flipping one suspect's polarity in the DAW (REAPER run_lua: SetMediaTrackInfo_Value(track, \"B_PHASE\", 1)) or nudging it a few ms, then run again and keep only what shrinks the deficit. Needs a SuperDuper plugin on the master and on every track that matters.")]
    async fn mix_phase(&self, Parameters(i): Parameters<PhaseInput>) -> Result<rmcp::Json<PhaseOut>, McpError> {
        use superduper_dsp_sdk::bus::TRACK_MASTER;
        let secs = i.seconds.unwrap_or(10.0).clamp(2.0, 60.0);
        let r = reader()?;
        let frames = (secs * 10.0) as usize;
        let prefer = i.prefer.unwrap_or_default();
        let (mut tp, mut mp) = ([0f64; BANDS], [0f64; BANDS]);
        let mut master_name = String::new();
        let mut contrib: std::collections::HashMap<String, [f64; BANDS]> = Default::default();
        for _ in 0..frames {
            let rows = r.read_all();
            let is_master = |s: &Snapshot| s.track_flags & TRACK_MASTER != 0 || s.track.eq_ignore_ascii_case("master");
            let m = rows.iter().filter(|s| is_master(s))
                .filter(|s| i.master_plugin.as_ref().is_none_or(|p| s.plugin.to_lowercase().contains(&p.to_lowercase())))
                .max_by(|a, b| a.rms_db.total_cmp(&b.rms_db));
            let Some(m) = m else {
                return Err(McpError::invalid_params("no SuperDuper plugin on the master: put one there (Spectrum or EQ, first in the chain)", None));
            };
            master_name = m.plugin.clone();
            for (b, v) in m.bands_db.iter().enumerate() { mp[b] += 10f64.powf(*v as f64 / 10.0); }
            for t in tracks(&rows, &prefer).into_iter().filter(|t| !t.bus) {
                let g = i.fader_db.as_ref().and_then(|f| f.get(&t.name)).copied().unwrap_or(0.0);
                let c = contrib.entry(t.name.clone()).or_insert([0.0; BANDS]);
                for (b, v) in t.bands_db.iter().enumerate() {
                    let p = 10f64.powf((*v + g) as f64 / 10.0);
                    tp[b] += p;
                    c[b] += p;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        let d = superduper_synth_core::mixmap::sum_deficit_db(&tp, &mp);
        let edges = band_edges();
        let mut cancelling: Vec<PhaseBand> = (0..BANDS).filter(|&b| d[b] < -2.0 && tp[b] > 1e-9 * frames as f64).map(|b| {
            let mut who: Vec<(&String, f64)> = contrib.iter().map(|(n, c)| (n, c[b])).collect();
            who.sort_by(|x, y| y.1.total_cmp(&x.1));
            PhaseBand { low_hz: edges[b], high_hz: edges[b + 1], deficit_db: d[b], tracks: who.into_iter().take(3).filter(|w| w.1 > 0.0).map(|w| w.0.clone()).collect() }
        }).collect();
        cancelling.sort_by(|a, b| a.deficit_db.total_cmp(&b.deficit_db));
        let not_on_bus_hz = (0..BANDS).filter(|&b| d[b] > 6.0).map(|b| (edges[b] * edges[b + 1]).sqrt()).collect();
        Ok(rmcp::Json(PhaseOut { seconds: secs, master: master_name, cancelling, deficit_db: d.to_vec(), not_on_bus_hz }))
    }

    #[rmcp::tool(description = "Compare two labelled mix_masking runs (same section of the song): each clash's share before and after, and each track's level. Use after a change to see whether it helped; undo it in the DAW if it did not.")]
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

    #[rmcp::tool(description = "Which documents a plugin instance exposes beyond its parameters (Wave: \"wavetable\"), with the shape plugin_get returns and plugin_set takes.")]
    async fn plugin_docs(&self, Parameters(i): Parameters<DocInput>) -> Result<rmcp::Json<DocOut>, McpError> {
        mail(&i.track, &i.plugin, "", "list", serde_json::Value::Null).await
    }

    #[rmcp::tool(description = "Read a plugin document (e.g. Wave's wavetable as harmonic amplitudes per frame) from the running instance on a track.")]
    async fn plugin_get(&self, Parameters(i): Parameters<DocInput>) -> Result<rmcp::Json<DocOut>, McpError> {
        mail(&i.track, &i.plugin, &i.doc, "get", serde_json::Value::Null).await
    }

    #[rmcp::tool(description = "Replace a plugin document in the running instance (e.g. Wave wavetable {harmonics: [[1, 0.3, 0, 0.1]]} — fundamental, some 2nd, no 3rd). Read it first with plugin_get to keep the old one for undo; measure with mix_masking before and after.")]
    async fn plugin_set(&self, Parameters(i): Parameters<DocInput>) -> Result<rmcp::Json<DocOut>, McpError> {
        mail(&i.track, &i.plugin, &i.doc, "set", i.value).await
    }

    #[rmcp::tool(description = "Every parameter of a running SuperDuper plugin (name, value, min, max, default, unit) — read before setting.")]
    async fn plugin_params(&self, Parameters(i): Parameters<DocInput>) -> Result<rmcp::Json<DocOut>, McpError> {
        mail(&i.track, &i.plugin, "params", "get", serde_json::Value::Null).await
    }

    #[rmcp::tool(description = "Set parameters of a running SuperDuper plugin by name, without the DAW: clamped to range, and the host sees the change (automation, project save). Read plugin_params first and keep the old values — that is the undo.")]
    async fn plugin_set_params(&self, Parameters(i): Parameters<SetParamsInput>) -> Result<rmcp::Json<DocOut>, McpError> {
        mail(&i.track, &i.plugin, "params", "set", serde_json::Value::Object(i.values)).await
    }

    #[rmcp::tool(description = "Duck one track under another in the bands where they fight, only while the key plays (Trackspacer, no routing): sets the bus key and Key Depth/Range of the SuperDuper Soothe on `track`. Use for a part that must stay but yields to a more important one (guitar/pad under the vocal, synth under the lead); not for kick-bass pumping (the bus lags 50-100 ms — use the compressor side-chain). Soothe must be the last enabled plugin on that track: add it with REAPER MCP track_fx_add_by_name \"CLAP: SuperDuper Soothe\" and pass amount_db: 0 if it is only there to duck. Measure mix_masking (prefer {track: \"Soothe\"}) before and after and mix_compare.")]
    async fn duck_under(&self, Parameters(i): Parameters<DuckInput>) -> Result<rmcp::Json<DocOut>, McpError> {
        let key = mail(&i.track, "Soothe", "key", "set", serde_json::json!({"track": i.key})).await?;
        let mut v = serde_json::Map::new();
        v.insert("Key Depth".into(), serde_json::json!(if i.key.is_empty() { 0.0 } else { i.depth_db.unwrap_or(6.0) }));
        v.insert("Key Range".into(), serde_json::json!(i.range_db.unwrap_or(24.0)));
        if let Some(a) = i.amount_db { v.insert("Amount".into(), serde_json::json!(a)); }
        let params = mail(&i.track, "Soothe", "params", "set", serde_json::Value::Object(v)).await?;
        Ok(rmcp::Json(DocOut { value: serde_json::json!({"key": key.0.value, "params": params.0.value}) }))
    }

    #[rmcp::tool(description = "The song key the SuperDuper Drum's Key=Auto follows (from ~/.superduper-dsp/song-key).")]
    async fn get_song_key(&self) -> Result<rmcp::Json<KeyOut>, McpError> {
        let p = song_key_path();
        let key = std::fs::read_to_string(&p).ok().map(|t| t.trim().to_string()).filter(|t| !t.is_empty());
        Ok(key_out(&p, key))
    }

    #[rmcp::tool(description = "Set the song key every SuperDuper plugin can follow (Drum Key=Auto retunes kick/808/toms to it). Accepts 'A', 'F#m', 'Bb major'; empty clears it.")]
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

#[rmcp::tool_handler]
impl ServerHandler for BusServer {
    fn get_info(&self) -> rmcp::model::ServerInfo {
        let info = rmcp::model::Implementation::new("superduper-bus", env!("CARGO_PKG_VERSION")).with_title("SuperDuper bus");
        rmcp::model::ServerInfo::new(rmcp::model::ServerCapabilities::builder().enable_tools().build())
            .with_protocol_version(rmcp::model::ProtocolVersion::V_2025_03_26)
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
