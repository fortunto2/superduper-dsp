//! `sdsp-bus serve` — the bus as an MCP server, for agents.
//!
//! Streamable HTTP on 127.0.0.1:7892/mcp (the `superduper-bus` entry in the
//! project's .mcp.json). Not 7891: that is each plugin's own MCP server.
//! Read-only on the plugins, plus the one thing an agent writes today: the
//! song key Drum's Key = Auto follows.

use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    schemars,
    transport::streamable_http_server::{session::local::LocalSessionManager, tower::StreamableHttpServerConfig, StreamableHttpService},
    ErrorData as McpError, ServerHandler,
};
use serde::{Deserialize, Serialize};
use superduper_dsp_sdk::bus::{band_edges, Reader};
use superduper_synth_core::drumlib::{parse_key_name, song_key_path};
use superduper_synth_core::mixmap::{is_bus_name, loudest_per_track, MaskingTracker};

const KEYS: [&str; 12] = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];

fn reader() -> Result<Reader, McpError> {
    Reader::open().ok_or_else(|| McpError::internal_error(format!("cannot open {}", superduper_dsp_sdk::bus::bus_path().display()), None))
}

#[derive(Serialize, schemars::JsonSchema)]
pub struct Instance {
    /// Host track name; "Master" for the master; empty if the host gave none.
    pub track: String,
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
}

#[derive(Serialize, schemars::JsonSchema)]
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

#[derive(Serialize, schemars::JsonSchema)]
pub struct MaskingOut { pub seconds: f32, pub tracks: Vec<String>, pub clashes: Vec<ClashOut> }

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
            track: r.track, plugin: r.plugin, rms_db: r.rms_db, peak_db: r.peak_db, key: r.key.map(|k| KEYS[k as usize].to_string()),
        }).collect();
        Ok(rmcp::Json(BusList { instances, tracks }))
    }

    #[rmcp::tool(description = "Current 32-band spectrum of one track (its loudest SuperDuper instance).")]
    async fn track_spectrum(&self, Parameters(TrackInput { track }): Parameters<TrackInput>) -> Result<rmcp::Json<TrackSpectrumOut>, McpError> {
        let rows = reader()?.read_all();
        let all = loudest_per_track(rows.iter().map(|s| (s.track.as_str(), s.rms_db, &s.bands_db)));
        let q = track.to_lowercase();
        let t = all.iter().find(|t| t.name.to_lowercase() == q).or_else(|| all.iter().find(|t| t.name.to_lowercase().contains(&q)))
            .ok_or_else(|| McpError::invalid_params(format!("no track matching '{track}' on the bus"), None))?;
        Ok(rmcp::Json(TrackSpectrumOut { track: t.name.clone(), rms_db: t.rms_db, band_edges_hz: band_edges().to_vec(), bands_db: t.bands_db.to_vec() }))
    }

    #[rmcp::tool(description = "Listen to the bus for a few seconds of playback and report which track pairs mask each other in which band, most persistent first. Masters/unnamed buses excluded. Use it to decide EQ cuts.")]
    async fn mix_masking(&self, Parameters(MaskingInput { seconds }): Parameters<MaskingInput>) -> Result<rmcp::Json<MaskingOut>, McpError> {
        let secs = seconds.unwrap_or(6.0).clamp(1.0, 60.0);
        let r = reader()?;
        let frames = (secs * 10.0) as usize;
        let mut tracker = MaskingTracker::mean();
        let mut sums: std::collections::HashMap<String, [f64; 32]> = Default::default();
        let mut names = Vec::new();
        for _ in 0..frames {
            let rows = r.read_all();
            let tracks = loudest_per_track(rows.iter().map(|s| (s.track.as_str(), s.rms_db, &s.bands_db)));
            for t in &tracks {
                let s = sums.entry(t.name.clone()).or_insert([0.0; 32]);
                for (i, v) in t.bands_db.iter().enumerate() { s[i] += 10f64.powf(*v as f64 / 10.0); }
            }
            tracker.update(&tracks);
            names = tracks.iter().map(|t| t.name.clone()).filter(|n| !is_bus_name(n)).collect();
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        let edges = band_edges();
        let avg = |n: &str, b: usize| sums.get(n).map_or(-120.0, |s| 10.0 * (s[b] / frames as f64 + 1e-12).log10()) as f32;
        let clashes = tracker.top(8).into_iter().filter(|c| c.share > 0.05).map(|c| {
            let (la, lb) = (avg(&c.a, c.band), avg(&c.b, c.band));
            let (louder, by) = if la >= lb { (c.a.clone(), la - lb) } else { (c.b.clone(), lb - la) };
            ClashOut { low_hz: edges[c.band], high_hz: edges[c.band + 1], share: c.share, louder, by_db: by, a: c.a, b: c.b }
        }).collect();
        Ok(rmcp::Json(MaskingOut { seconds: secs, tracks: names, clashes }))
    }

    #[rmcp::tool(description = "The song key the SuperDuper Drum's Key=Auto follows (from ~/.superduper-dsp/song-key).")]
    async fn get_song_key(&self) -> Result<rmcp::Json<KeyOut>, McpError> {
        let p = song_key_path();
        let key = std::fs::read_to_string(&p).ok().map(|t| t.trim().to_string()).filter(|t| !t.is_empty());
        Ok(rmcp::Json(KeyOut { key, file: p.display().to_string() }))
    }

    #[rmcp::tool(description = "Set the song key every SuperDuper plugin can follow (Drum Key=Auto retunes kick/808/toms to it). Accepts 'A', 'F#m', 'Bb major'; empty clears it.")]
    async fn set_song_key(&self, Parameters(KeyInput { key }): Parameters<KeyInput>) -> Result<rmcp::Json<KeyOut>, McpError> {
        let p = song_key_path();
        let k = key.trim();
        if k.is_empty() {
            let _ = std::fs::remove_file(&p);
            return Ok(rmcp::Json(KeyOut { key: None, file: p.display().to_string() }));
        }
        if parse_key_name(k).is_none() {
            return Err(McpError::invalid_params(format!("'{k}' is not a key — try 'A', 'F#m', 'Bb major'"), None));
        }
        if let Some(d) = p.parent() { let _ = std::fs::create_dir_all(d); }
        std::fs::write(&p, format!("{k}\n")).map_err(|e| McpError::internal_error(e.to_string(), None))?;
        Ok(rmcp::Json(KeyOut { key: Some(k.to_string()), file: p.display().to_string() }))
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
            .with_instructions("Live view of every SuperDuper plugin running in the DAW: bus_list, track_spectrum, mix_masking (who masks whom, while the song plays), get/set_song_key.")
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
