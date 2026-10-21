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

fn tracks(rows: &[Snapshot]) -> Vec<TrackSpectrum> {
    loudest_per_track(rows.iter().map(|s| (s.track.as_str(), s.is_bus(), s.rms_db, &s.bands_db)))
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

#[derive(Serialize, Deserialize, schemars::JsonSchema)]
pub struct TrackLevel { pub track: String, pub rms_db: f32 }

#[derive(Serialize, Deserialize, schemars::JsonSchema)]
pub struct MaskingOut {
    pub label: Option<String>,
    pub seconds: f32,
    pub tracks: Vec<String>,
    pub clashes: Vec<ClashOut>,
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
        let all = tracks(&reader()?.read_all());
        let q = track.to_lowercase();
        let t = all.iter().find(|t| t.name.to_lowercase() == q).or_else(|| all.iter().find(|t| t.name.to_lowercase().contains(&q)))
            .ok_or_else(|| McpError::invalid_params(format!("no track matching '{track}' on the bus"), None))?;
        Ok(rmcp::Json(TrackSpectrumOut { track: t.name.clone(), rms_db: t.rms_db, band_edges_hz: band_edges().to_vec(), bands_db: t.bands_db.to_vec() }))
    }

    #[rmcp::tool(description = "Listen to the bus for a few seconds of playback and report which track pairs mask each other in which band, most persistent first. Masters, group buses and returns excluded. Use it to decide EQ cuts.")]
    async fn mix_masking(&self, Parameters(MaskingInput { seconds, label }): Parameters<MaskingInput>) -> Result<rmcp::Json<MaskingOut>, McpError> {
        let secs = seconds.unwrap_or(6.0).clamp(1.0, 60.0);
        let r = reader()?;
        let frames = (secs * 10.0) as usize;
        let mut tracker = MaskingTracker::mean();
        let mut sums: std::collections::HashMap<String, [f64; BANDS]> = Default::default();
        let mut power: std::collections::HashMap<String, f64> = Default::default();
        let mut last = Vec::new();
        for _ in 0..frames {
            let tracks = tracks(&r.read_all());
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
        let out = MaskingOut { label, seconds: secs, tracks: names, clashes, levels };
        if out.label.is_some() {
            use std::io::Write;
            let line = serde_json::to_string(&out).map_err(|e| McpError::internal_error(e.to_string(), None))?;
            std::fs::OpenOptions::new().create(true).append(true).open(mix_log_path())
                .and_then(|mut f| writeln!(f, "{line}"))
                .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        }
        Ok(rmcp::Json(out))
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
            .with_instructions("Live view of every SuperDuper plugin running in the DAW: bus_list, track_spectrum, mix_masking (who masks whom, while the song plays; label a run to keep it), mix_compare (two labelled runs: what a change did), get/set_song_key. Changes and undo happen in the DAW (REAPER MCP); this server measures.")
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
