//! Does driving one side's sensory cells actually steer the descending neurons?
//!
//!     cargo run --release -p connectome-core --example descending -- brain.fcb populations.json
//!
//! The app wants to read the fly's turn off the left/right descending populations, the cells
//! that really carry commands from brain to nerve cord. That only works if asymmetric input
//! produces asymmetric descending output *through the wiring*. This measures it instead of
//! assuming it: baseline, then one eye, then one side's bristles.
use connectome_core::{Graph, Lif, LifParams};
use std::collections::HashMap;

/// The populations file is a flat `{"name": [int, ...]}`; parse it without a JSON crate,
/// because connectome-core compiles for iOS and stays dependency-light on purpose.
fn populations(text: &str) -> HashMap<String, Vec<usize>> {
    let mut out = HashMap::new();
    let mut rest = text;
    while let Some(q) = rest.find('"') {
        let after = &rest[q + 1..];
        let Some(e) = after.find('"') else { break };
        let key = after[..e].to_string();
        let tail = &after[e + 1..];
        let Some(lb) = tail.find('[') else { break };
        let Some(rb) = tail[lb..].find(']') else { break };
        let list = &tail[lb + 1..lb + rb];
        out.insert(key, list.split(',').filter_map(|s| s.trim().parse().ok()).collect());
        rest = &tail[lb + rb..];
    }
    out
}

fn mean_rate(sim: &Lif, cells: &[usize]) -> f64 {
    if cells.is_empty() { return 0.0; }
    cells.iter().map(|&i| sim.rates()[i] as f64).sum::<f64>() / cells.len() as f64
}

fn main() {
    let mut a = std::env::args().skip(1);
    let fcb = std::fs::read(a.next().expect("brain.fcb")).unwrap();
    let pops = populations(&std::fs::read_to_string(a.next().expect("populations.json")).unwrap());
    let (graph, _) = Graph::from_fcb(&fcb, 5).unwrap();
    println!("{} neurons · {} edges", graph.neurons(), graph.edges());
    for k in ["visual.left", "visual.right", "mechano.left", "mechano.right", "descending.left", "descending.right"] {
        println!("  {k:<18}{:>7}", pops[k].len());
    }

    // Two questions the design rests on: how hard must a cue be to reach the descending
    // neurons, and does a *patch* of the eye (something entering the visual field) carry
    // where uniform illumination of the whole eye does not — as it would in a real fly,
    // which steers on motion and contrast rather than on overall brightness.
    let dl = pops["descending.left"].clone();
    let dr = pops["descending.right"].clone();
    println!();
    println!("{:<30}{:>9}{:>9}{:>12}{:>10}", "stimulus", "DN L", "DN R", "asym shift", "drive");
    for noise in [1.0f32, 2.0] {
        println!("-- background noise {noise}");
        let params = LifParams { noise, weight_scale: 0.02, ..LifParams::default() };
        let mut sim = Lif::new(graph.clone(), params, 1);
        let rate = |sim: &Lif| (mean_rate(sim, &dl), mean_rate(sim, &dr));
        sim.run(3000);
        let (bl, br) = rate(&sim);
        let base = (br - bl) / (br + bl).max(1e-9);
        let base_drive = (bl + br) / 2.0;
        println!("{:<30}{bl:>9.4}{br:>9.4}{:>12.3}{base_drive:>10.4}", "baseline", 0.0);

        let quarter = |v: &Vec<usize>| v.iter().copied().take(v.len() / 4).collect::<Vec<_>>();
        let cases: Vec<(String, Vec<usize>, f32)> = vec![
            ("visual.right whole @ 6".into(), pops["visual.right"].clone(), 6.0),
            ("visual.right whole @ 20".into(), pops["visual.right"].clone(), 20.0),
            ("visual.right whole @ 60".into(), pops["visual.right"].clone(), 60.0),
            ("visual.right quarter @ 60".into(), quarter(&pops["visual.right"]), 60.0),
            ("mechano.right @ 8".into(), pops["mechano.right"].clone(), 8.0),
            ("mechano.right @ 30".into(), pops["mechano.right"].clone(), 30.0),
            ("olfactory @ 20".into(), pops["olfactory"].clone(), 20.0),
        ];
        for (name, cells, drive) in cases {
            sim.clear_stimulus();
            sim.run(1500);
            for &i in &cells { sim.set_stimulus(i, drive); }
            sim.run(1500);
            let (l, r) = rate(&sim);
            let asym = (r - l) / (r + l).max(1e-9);
            println!("{name:<30}{l:>9.4}{r:>9.4}{:>12.3}{:>10.4}", asym - base, (l + r) / 2.0);
        }
    }
}
