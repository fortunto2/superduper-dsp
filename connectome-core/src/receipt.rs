//! What a run actually computed, printed rather than assumed.
//!
//! The rule this follows is the one in `rules/harness-sensors.md`: "ok" alone is forbidden,
//! and zero scope is never a pass. A simulation that emitted no spikes and a simulation
//! that never ran are indistinguishable unless the run says which it was.

use crate::lif::Lif;

#[derive(Debug, Clone, PartialEq)]
pub struct Receipt {
    pub neurons: usize,
    pub edges: usize,
    pub steps: u64,
    pub spikes: u64,
    pub steps_per_second: f64,
    pub seed: u64,
    /// Mean firing rate in Hz, assuming the params' `dt` is in milliseconds.
    pub mean_rate_hz: f64,
}

impl Receipt {
    pub fn of(sim: &Lif, seed: u64, elapsed_secs: f64, dt_ms: f32) -> Self {
        let neurons = sim.graph().neurons();
        let steps = sim.steps();
        let spikes = sim.total_spikes();
        let sim_secs = steps as f64 * dt_ms as f64 / 1000.0;
        Self {
            neurons,
            edges: sim.graph().edges(),
            steps,
            spikes,
            steps_per_second: if elapsed_secs > 0.0 { steps as f64 / elapsed_secs } else { 0.0 },
            seed,
            mean_rate_hz: if sim_secs > 0.0 && neurons > 0 {
                spikes as f64 / sim_secs / neurons as f64
            } else {
                0.0
            },
        }
    }

    /// True when this run is worth drawing a conclusion from. A receipt with no spikes is
    /// reported, never silently treated as a quiet brain.
    pub fn is_conclusive(&self) -> bool {
        self.steps > 0 && self.spikes > 0
    }
}

impl std::fmt::Display for Receipt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} neurons · {} edges · {} steps · {} spikes · {:.1} Hz mean · {:.0} steps/s · seed {}{}",
            self.neurons,
            self.edges,
            self.steps,
            self.spikes,
            self.mean_rate_hz,
            self.steps_per_second,
            self.seed,
            if self.is_conclusive() { "" } else { "  ⚠ NO SPIKES — this run concluded nothing" }
        )
    }
}
