//! Tuning table for the synthetic brain: background `noise` and synaptic `weight_scale`
//! against the app's readout bands (sleep ≤0.005, groom >0.01, walk ≥0.04, turn/startle ≥0.18).
//! "idle" is mean single-step activity over steps 4000..5000; "peak" the max single step
//! in the 40 steps after a touch (8.0 into 40 neurons) applied at step 5000.
use connectome_core::{Graph, Lif, LifParams};

fn run(noise: f32, weight_scale: f32) -> (f64, f64, f64) {
    let n = 700;
    let graph = Graph::synthetic(n, 12, 1);
    let mut sim = Lif::new(graph, LifParams { noise, weight_scale, ..LifParams::default() }, 1);
    let (mut sum, mut idle_peak) = (0usize, 0usize);
    for step in 0..5000 {
        let f = sim.step();
        if step >= 4000 { sum += f; idle_peak = idle_peak.max(f); }
    }
    for i in (0..120).step_by(3) { sim.set_stimulus(i, 8.0); }
    let mut peak = 0usize;
    for _ in 0..40 { peak = peak.max(sim.step()); }
    let d = n as f64;
    (sum as f64 / (1000.0 * d), idle_peak as f64 / d, peak as f64 / d)
}

fn main() {
    println!("noise  scale   idle  idlepeak  touchpeak");
    let mut table = Vec::new();
    for scale in [0.15f32] {
        for noise in [1.0f32, 1.5, 2.0, 2.5, 3.0, 3.5, 4.0, 5.0, 6.0, 8.0] { table.push((noise, scale)); }
    }
    for &(noise, scale) in &table {
        let (idle, ip, tp) = run(noise, scale);
        println!("{noise:>5}  {scale:>5}  {idle:.3}  {ip:.3}     {tp:.3}");
    }
}
