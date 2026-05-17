//! sdsp-runner — minimal standalone CLAP host.
//!
//! Goal: collapse the dev loop from
//!   edit → cargo build → bundle → restart REAPER → re-insert FX → listen
//! to
//!   edit → cargo build → bundle → `sdsp-runner …` → listen
//!
//! Usage:
//!   cargo run -p sdsp-runner --release -- \
//!       ~/Library/Audio/Plug-Ins/CLAP/SuperDuperReverb.clap test.wav
//!
//! Loads the .clap bundle, instantiates the first plugin in its factory,
//! reads `input.wav` (any sample format hound supports), processes it
//! through the plugin block-by-block, and streams the result to the
//! system default audio output via cpal.
//!
//! Limitations (v1):
//!   * No GUI yet (the plugin's UI extension is ignored).
//!   * No MIDI input (synth plugins still run, just silent).
//!   * No parameter automation — plugin starts at its defaults.
//!   * Mono/stereo WAV only.
//!   * Plays the file once then exits.

mod audio;
mod host;

use anyhow::{Context, Result};
use clap::Parser;
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Cli {
    /// Path to the .clap bundle (directory) or its inner dylib.
    plugin: PathBuf,

    /// WAV file to play through the plugin. If omitted, the plugin gets
    /// silence as input (useful for testing synths once MIDI is wired).
    input: Option<PathBuf>,

    /// Override CPAL block size in frames (default = CPAL's choice).
    #[arg(long)]
    block: Option<u32>,

    /// Loop the input WAV until Ctrl-C (default: play once and exit).
    #[arg(short, long)]
    looped: bool,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    eprintln!("sdsp-runner: loading {}", cli.plugin.display());

    let plugin = host::load_plugin(&cli.plugin)
        .with_context(|| format!("failed to load CLAP plugin at {}", cli.plugin.display()))?;
    eprintln!(
        "sdsp-runner: loaded '{}' (id={})",
        plugin.descriptor_name, plugin.descriptor_id
    );

    let wav = if let Some(path) = cli.input.as_ref() {
        let w = audio::load_wav(path)
            .with_context(|| format!("failed to read WAV at {}", path.display()))?;
        eprintln!(
            "sdsp-runner: loaded WAV {} ({} frames, {} ch, {} Hz)",
            path.display(),
            w.frames(),
            w.channels,
            w.sample_rate
        );
        w
    } else {
        eprintln!("sdsp-runner: no WAV given — plugin will receive silence");
        audio::WavContent::silence(48_000, 2, 2 * 48_000)
    };

    audio::run(plugin, wav, cli.block, cli.looped)
}
