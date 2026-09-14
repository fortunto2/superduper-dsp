//! The parallel step must be the serial step, spike for spike. Noise is counter-based, so a
//! brain above the parallel threshold produces the same train on one thread and on all of
//! them — otherwise a receipt from a phone could never be checked against one from a Mac.

use connectome_core::{Graph, Lif, LifParams};

fn train(threads: usize) -> Vec<u32> {
    rayon::ThreadPoolBuilder::new().num_threads(threads).build().unwrap().install(|| {
        // 30k cells: above PARALLEL_MIN_NEURONS, so the parallel paths are the ones running.
        let params = LifParams { noise: 3.5, weight_scale: 0.15, ..LifParams::default() };
        let mut sim = Lif::new(Graph::synthetic(30_000, 12, 7), params, 7);
        for i in (0..30_000).step_by(10) {
            sim.set_stimulus(i, 6.0);
        }
        sim.run(60)
    })
}

#[test]
fn one_thread_and_many_threads_produce_the_same_train() {
    let one = train(1);
    let many = train(8);
    assert!(one.iter().sum::<u32>() > 1000, "baseline must be alive: {}", one.iter().sum::<u32>());
    assert_eq!(one, many);
}
