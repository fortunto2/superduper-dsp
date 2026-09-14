//! Tune synaptic gain on a real export: `real_sweep edges.csv threshold`. Real weights are
//! synapse counts (1..hundreds), so the synthetic gain saturates the brain; this finds the
//! gain that puts idle activity in the app's walk band (~5% of cells a step) and reports
//! steps/s at that activity, which is what decides whether a phone runs it in real time.
use connectome_core::{Graph, Lif, LifParams};
use std::{fs::File, io::BufReader, time::Instant};

fn main() {
    let path = std::env::args().nth(1).expect("edges.csv");
    let threshold: u32 = std::env::args().nth(2).and_then(|s| s.parse().ok()).unwrap_or(5);
    let graph = Graph::from_edge_list(BufReader::new(File::open(&path).unwrap()), threshold).unwrap();
    let n = graph.neurons();
    println!("{n} neurons · {} edges · threshold {threshold}", graph.edges());
    println!("scale    noise  activity  steps/s");
    // `real_sweep edges.csv threshold [scale]`: with a scale, walk the noise ladder at that
    // gain instead — the arousal dial the app turns.
    let table: Vec<(f32, f32)> = match std::env::args().nth(3).and_then(|s| s.parse::<f32>().ok()) {
        Some(scale) => [0.0f32, 0.5, 1.0, 1.5, 2.0, 2.5, 3.5, 5.0, 8.0].iter().map(|&n| (scale, n)).collect(),
        None => vec![(0.15, 3.5), (0.05, 3.5), (0.02, 3.5), (0.01, 3.5), (0.005, 3.5), (0.01, 2.0), (0.01, 5.0)],
    };
    for &(scale, noise) in &table {
        let params = LifParams { noise, weight_scale: scale, ..LifParams::default() };
        let mut sim = Lif::new(graph.clone(), params, 1);
        sim.run(300);
        let t0 = Instant::now();
        let fired: u64 = sim.run(300).iter().map(|&x| x as u64).sum();
        let secs = t0.elapsed().as_secs_f64();
        println!("{scale:<7}  {noise:<5}  {:.4}    {:.0}", fired as f64 / (300.0 * n as f64), 300.0 / secs);
    }
}
