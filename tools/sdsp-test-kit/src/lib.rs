//! Shared test harness: an in-process CLAP host, plus snapshot-based quality
//! measurement.
//!
//! Two problems this solves.
//!
//! **The host boilerplate was copied per plugin.** Roughly 120 lines of
//! clack-host setup — host handlers, port buffers, the block loop — living in
//! every `clap_e2e.rs`. Nobody writes that a 30th time, which is why 21 of the
//! 30 plugins have no end-to-end test at all.
//!
//! **Nothing catches gradual damage.** The existing `quality_audit.rs` files
//! assert against fixed thresholds ("THD between −60 and −10 dB"), which catches
//! a plugin that broke outright and says nothing about one that got 3 dB darker.
//! Every audible fault on this project was of the second kind: a kit rendering
//! with no hi-hats, a loudness meter reading 3 dB light, a master 6 dB short of
//! its reference in the presence band. All of them measurable, none measured.
//!
//! So: measure a fixed set of probes, write them to a snapshot file, and fail
//! when they move. Updating is deliberate:
//!
//! ```text
//! cargo test -p superduper-saturator --test quality          # check
//! SDSP_UPDATE_SNAPSHOTS=1 cargo test -p superduper-saturator --test quality
//! ```
//!
//! A snapshot diff in review then reads as "this change made the saturator
//! 1.2 dB louder and added 4 dB of THD" — a decision, not an accident.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub mod alloc;
pub mod host;
pub mod params;
pub mod probes;

pub use host::{render_effect, render_instrument, PluginUnderTest};

/// A set of named measurements compared against a stored snapshot.
pub struct Suite {
    name: String,
    path: PathBuf,
    measured: BTreeMap<String, f64>,
    /// Absolute tolerance per key, in the unit of that measurement.
    tolerance: BTreeMap<String, f64>,
    default_tolerance: f64,
}

impl Suite {
    /// `path` is usually `concat!(env!("CARGO_MANIFEST_DIR"), "/tests/quality.snap")`.
    pub fn new(name: impl Into<String>, path: impl AsRef<Path>) -> Self {
        Self {
            name: name.into(),
            path: path.as_ref().to_path_buf(),
            measured: BTreeMap::new(),
            tolerance: BTreeMap::new(),
            default_tolerance: 0.5,
        }
    }

    /// Plugins with noise sources or free-running LFOs never reproduce exactly;
    /// give those a wider band rather than seeding every generator.
    pub fn default_tolerance(mut self, db: f64) -> Self {
        self.default_tolerance = db;
        self
    }

    pub fn tolerate(mut self, key: &str, amount: f64) -> Self {
        self.tolerance.insert(key.to_string(), amount);
        self
    }

    pub fn record(&mut self, key: impl Into<String>, value: f64) {
        self.measured.insert(key.into(), value);
    }

    /// Compare against the snapshot — or write it, with SDSP_UPDATE_SNAPSHOTS=1.
    pub fn finish(self) {
        let updating = std::env::var("SDSP_UPDATE_SNAPSHOTS").is_ok();
        let existing = std::fs::read_to_string(&self.path).ok();

        if updating || existing.is_none() {
            let body = self
                .measured
                .iter()
                .map(|(k, v)| format!("{k} = {v:.3}"))
                .collect::<Vec<_>>()
                .join("\n");
            let header = format!(
                "# Quality snapshot for {}. Regenerate deliberately:\n\
                 #   SDSP_UPDATE_SNAPSHOTS=1 cargo test -p superduper-{} --test quality\n\
                 # A change here is a change in how the plugin sounds.\n",
                self.name, self.name
            );
            if let Some(parent) = self.path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            std::fs::write(&self.path, format!("{header}{body}\n")).expect("write snapshot");
            if existing.is_none() && !updating {
                eprintln!(
                    "note: {} had no snapshot; wrote one with {} probes. \
                     Review it — it is now the definition of 'correct'.",
                    self.name,
                    self.measured.len()
                );
            }
            return;
        }

        let stored: BTreeMap<String, f64> = existing
            .unwrap()
            .lines()
            .filter(|l| !l.trim_start().starts_with('#') && l.contains('='))
            .filter_map(|l| {
                let (k, v) = l.split_once('=')?;
                Some((k.trim().to_string(), v.trim().parse().ok()?))
            })
            .collect();

        let mut drifted = Vec::new();
        for (key, now) in &self.measured {
            let Some(then) = stored.get(key) else {
                drifted.push(format!("  {key}: new probe, measured {now:.2} (not in snapshot)"));
                continue;
            };
            let tol = self.tolerance.get(key).copied().unwrap_or(self.default_tolerance);
            let delta = now - then;
            if delta.abs() > tol {
                drifted.push(format!(
                    "  {key}: {then:.2} → {now:.2}  ({delta:+.2}, tolerance ±{tol:.2})"
                ));
            }
        }
        for key in stored.keys() {
            if !self.measured.contains_key(key) {
                drifted.push(format!("  {key}: probe disappeared (was {:.2})", stored[key]));
            }
        }

        assert!(
            drifted.is_empty(),
            "{} sounds different than its snapshot:\n{}\n\n\
             If the change is intended, re-record with:\n  \
             SDSP_UPDATE_SNAPSHOTS=1 cargo test -p superduper-{} --test quality",
            self.name,
            drifted.join("\n"),
            self.name,
        );
    }
}
