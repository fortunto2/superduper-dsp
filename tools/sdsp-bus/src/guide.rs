//! What the gateway offers besides tools: prompts that run the sdsp-mix method
//! as one command (a slash command in Claude Code, a menu entry in Claude
//! Desktop and Cursor), and resources an agent can attach — the method itself,
//! how to verify a change, and the live bus as JSON.
//!
//! A prompt is the method written down, not new logic: every step names the
//! tool it uses, measures before and after, and keeps the old values as undo.

use rmcp::{
    handler::server::wrapper::Parameters,
    model::{PromptMessage, PromptMessageRole, RawResource, ReadResourceResult, Resource, ResourceContents, AnnotateAble},
    schemars, ErrorData as McpError,
};
use serde::Deserialize;

use crate::serve::BusServer;

const METHOD: &str = include_str!("../../../skills/sdsp-mix/references/mixing-method.md");
const VERIFY: &str = include_str!("../../../skills/sdsp-mix/references/verify.md");

/// Shared rules every mixing prompt ends with.
const RULES: &str = "Rules: measure with the song playing (the bus hears only what plays). Read plugin_params before any plugin_set_params and keep the old values: that is the undo. Change one thing, measure again, keep it only if the numbers moved the right way, otherwise put the old values back. Say what you changed and the before/after numbers.";

#[derive(Deserialize, schemars::JsonSchema)]
pub struct SectionArgs {
    /// Name for this measurement run, e.g. "chorus-before". Optional.
    pub label: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct VocalArgs {
    /// The vocal track that must be heard (as the DAW names it).
    pub vocal: String,
    /// Tracks allowed to yield, comma-separated. Empty = decide from the measurement.
    pub others: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct DuckArgs {
    /// Track that should get out of the way (guitar, pad, synth).
    pub track: String,
    /// Track it yields to (usually the lead vocal).
    pub key: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct MasterArgs {
    /// Integrated loudness target in LUFS, e.g. -14 for streaming, -9 for club.
    /// A string: MCP prompt arguments always arrive as strings.
    pub lufs: Option<String>,
}

fn user(text: String) -> Vec<PromptMessage> {
    vec![PromptMessage::new_text(PromptMessageRole::User, text)]
}

#[rmcp::prompt_router(vis = "pub(crate)")]
impl BusServer {
    #[rmcp::prompt(name = "mix-check", description = "Measure the mix while it plays: levels, who masks whom, where parts cancel. Report only — changes nothing.")]
    async fn mix_check(&self, Parameters(a): Parameters<SectionArgs>) -> Vec<PromptMessage> {
        let label = a.label.map(|l| format!(" with label \"{l}\"")).unwrap_or_default();
        user(format!(
            "Check my mix with the superduper-bus tools. The song is playing.\n\
             1. bus_list: which tracks carry SuperDuper plugins, and which have none (those are invisible to the bus — say so).\n\
             2. mix_masking for 8 seconds{label}: list the clashes that hold most of the time and the buried parts that should be heard.\n\
             3. If a plugin sits on the master, mix_phase for 10 seconds: bands losing 2+ dB to cancellation and their suspects.\n\
             4. Summarise in plain words, worst problem first, and for each one the move that would fix it (EQ cut on which track and band, duck_under, polarity flip, level). Change nothing."
        ))
    }

    #[rmcp::prompt(name = "make-room-for-vocal", description = "Find what buries the vocal and carve space for it: masking measurement, then the smallest change that frees it, verified.")]
    async fn make_room_for_vocal(&self, Parameters(a): Parameters<VocalArgs>) -> Vec<PromptMessage> {
        let others = a.others.filter(|o| !o.trim().is_empty())
            .map(|o| format!(" Only these tracks may yield: {o}."))
            .unwrap_or_default();
        user(format!(
            "Make room for the vocal \"{v}\" using the superduper-bus tools.{others}\n\
             1. mix_masking 8 s, label \"vocal-before\": which tracks clash with \"{v}\" and in which bands (presence 1-5 kHz matters most, body 150-400 Hz next).\n\
             2. For each clash, smallest fix first: a part that must stay but can yield only while the vocal sings → duck_under(track, key=\"{v}\"); a part that is in the way all the time → a static EQ cut on that track in that band (plugin_params, then plugin_set_params on its SuperDuper EQ); never boost the vocal into the clash.\n\
             3. mix_masking 8 s, label \"vocal-after\", then mix_compare vocal-before vs vocal-after.\n\
             {RULES}",
            v = a.vocal
        ))
    }

    #[rmcp::prompt(name = "duck-under", description = "Make one track yield to another only while the key plays (Trackspacer-style, no routing), and prove it helped.")]
    async fn duck(&self, Parameters(a): Parameters<DuckArgs>) -> Vec<PromptMessage> {
        user(format!(
            "Duck \"{t}\" under \"{k}\" with superduper-bus.\n\
             1. bus_list: \"{t}\" needs a SuperDuper Soothe as its last enabled plugin. If it has none, tell me to add \"CLAP: SuperDuper Soothe\" there (or add it through the REAPER MCP if that server is connected) and stop until it is on the bus.\n\
             2. mix_masking 8 s, label \"duck-before\", prefer {{\"{t}\": \"Soothe\"}}.\n\
             3. duck_under(track=\"{t}\", key=\"{k}\", amount_db=0 if Soothe is only there to duck).\n\
             4. mix_masking 8 s, label \"duck-after\" (same prefer), mix_compare. Keep it only if the {t}/{k} clash shrank; otherwise duck_under with key \"\" to switch it off.\n\
             {RULES}",
            t = a.track, k = a.key
        ))
    }

    #[rmcp::prompt(name = "fix-mud", description = "Find the low-mid build-up (150-500 Hz): which tracks pile up there, and thin the ones that do not need it.")]
    async fn fix_mud(&self, Parameters(a): Parameters<SectionArgs>) -> Vec<PromptMessage> {
        let label = a.label.unwrap_or_else(|| "mud".into());
        user(format!(
            "Find and fix mud with superduper-bus. Mud is masking, not a frequency: several parts loud in the same low-mid band.\n\
             1. mix_masking 8 s, label \"{label}-before\": clashes between 150 and 500 Hz, and every track's level.\n\
             2. Before any EQ, check register and timing: two parts playing the same octave at the same time is an arrangement problem — say so instead of cutting.\n\
             3. Otherwise pick the part that needs the band least (pads, guitars, keys before bass and kick) and cut 2-4 dB there on its SuperDuper EQ, or high-pass it just below its lowest useful note.\n\
             4. mix_masking 8 s, label \"{label}-after\", mix_compare.\n\
             {RULES}"
        ))
    }

    #[rmcp::prompt(name = "master-to-target", description = "Bring the master to a loudness target without flattening it: read the master chain, set it by name, check level and crest.")]
    async fn master_to_target(&self, Parameters(a): Parameters<MasterArgs>) -> Vec<PromptMessage> {
        let lufs = a.lufs.and_then(|l| l.trim().trim_end_matches("LUFS").trim().parse::<f32>().ok()).unwrap_or(-14.0);
        user(format!(
            "Master to {lufs} LUFS integrated with superduper-bus, keeping the dynamics.\n\
             1. bus_list: the plugins on the master track (expect Compressor and Limiter, ideally Spectrum for LUFS). If there is no Limiter, tell me to add \"CLAP: SuperDuper Limiter\" last on the master and stop.\n\
             2. plugin_params on the master's Limiter and Compressor; note the values.\n\
             3. Glue first, gently: Compressor ratio ≤ 2, gain reduction 1-3 dB on the loudest part. Then raise the Limiter input toward the target with the ceiling at -1 dBTP.\n\
             4. Loudness is not on the bus: ask me to play the loudest section and read the SuperDuper Spectrum meter (or render and run tools/mixcheck.py), and stop pushing when crest falls under 8 dB — a louder master that lost its punch is worse.\n\
             {RULES}"
        ))
    }
}

pub(crate) fn resources() -> Vec<Resource> {
    vec![
        RawResource::new("sdsp://method/mixing", "mixing-method")
            .with_title("How to mix with SuperDuper")
            .with_description("The method behind every prompt: masking before EQ, register and timing first, measure-change-measure.")
            .with_mime_type("text/markdown")
            .no_annotation(),
        RawResource::new("sdsp://method/verify", "verify")
            .with_title("How to prove a mix change helped")
            .with_description("Two instruments must agree: the offline render and the live bus.")
            .with_mime_type("text/markdown")
            .no_annotation(),
        RawResource::new("sdsp://bus/snapshot", "bus-snapshot")
            .with_title("Live bus")
            .with_description("Every running SuperDuper instance right now: track, plugin, level, peak, 32-band spectrum.")
            .with_mime_type("application/json")
            .no_annotation(),
    ]
}

pub(crate) fn read(uri: &str) -> Result<ReadResourceResult, McpError> {
    let text = match uri {
        "sdsp://method/mixing" => METHOD.to_string(),
        "sdsp://method/verify" => VERIFY.to_string(),
        "sdsp://bus/snapshot" => crate::serve::snapshot_json()?,
        _ => return Err(McpError::resource_not_found(format!("no resource {uri}"), None)),
    };
    Ok(ReadResourceResult::new(vec![ResourceContents::text(text, uri)]))
}
