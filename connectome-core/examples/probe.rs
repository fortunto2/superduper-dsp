//! `cargo run -p connectome-core --example probe [-- path.csv threshold]`
//!
//! Prints the receipt for one run. With no file it uses a synthetic graph and says so,
//! because a number produced from random wiring must never be quoted as a fly.

use connectome_core::{Graph, Lif, LifParams, Receipt};
use std::{fs::File, io::BufReader, time::Instant};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let path = args.next();
    let threshold: u32 = args.next().and_then(|s| s.parse().ok()).unwrap_or(5);
    let seed = 1;

    let (graph, source) = match &path {
        Some(p) => (
            Graph::from_edge_list(BufReader::new(File::open(p)?), threshold)?,
            format!("{p} (threshold {threshold})"),
        ),
        None => (Graph::synthetic(2700, 20, 42), "SYNTHETIC — random wiring, not a fly".into()),
    };

    // Same drive as the app (mobile/fly-ios): background noise, no tonic stimulus.
    let params = LifParams { noise: 3.5, weight_scale: 0.15, ..LifParams::default() };
    let mut sim = Lif::new(graph, params, seed);

    let t0 = Instant::now();
    sim.run(1000);
    let receipt = Receipt::of(&sim, seed, t0.elapsed().as_secs_f64(), params.dt);

    println!("source:  {source}");
    println!("receipt: {receipt}");
    if !receipt.is_conclusive() {
        std::process::exit(2); // nothing was measured; do not let a script read this as success
    }
    Ok(())
}
