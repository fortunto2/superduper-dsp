//! Does the spike train depend on the connectome, or on the defaults?
//!
//! Every project in this space can render a spike raster. Almost none show the raster would
//! look different had the wiring been different — which is the only thing that makes it a
//! simulation of *this* graph rather than of an LIF model's resting behaviour.
//!
//! Three mutants, each a defect a reviewer would block: destroy a bundle of synapses,
//! remove cells, perturb the seed. Each must change the measured train.

use connectome_core::{Graph, Lif, LifParams, Receipt};

const STEPS: usize = 400;

/// A graph that actually fires: stimulus on a tenth of the cells, weights strong enough to
/// propagate. The baseline has to be non-empty before any mutant means anything — two
/// silent runs match perfectly and prove nothing, which is the vacuous-comparison trap.
fn driven(seed: u64) -> Lif {
    let graph = Graph::synthetic(600, 12, 42);
    let mut sim = Lif::new(graph, LifParams::default(), seed);
    for i in (0..600).step_by(10) {
        sim.set_stimulus(i, 3.0);
    }
    sim
}

fn train(sim: &mut Lif) -> Vec<u32> {
    sim.run(STEPS)
}

fn differs(a: &[u32], b: &[u32]) -> usize {
    a.iter().zip(b).filter(|(x, y)| x != y).count()
}

#[test]
fn the_baseline_is_alive_before_anything_is_concluded_from_it() {
    let mut sim = driven(1);
    let t = train(&mut sim);
    let total: u64 = t.iter().map(|&x| x as u64).sum();
    assert!(total > 0, "baseline emitted no spikes — every comparison below would be vacuous");
    let r = Receipt::of(&sim, 1, 0.01, LifParams::default().dt);
    assert!(r.is_conclusive(), "receipt says the run concluded nothing: {r}");
    assert_eq!(r.steps, STEPS as u64);
}

#[test]
fn killing_a_synapse_bundle_changes_the_train() {
    let mut control = driven(1);
    let before = train(&mut control);

    let mut lesioned = driven(1);
    let edges = lesioned.graph().edges();
    lesioned.lesion_synapses(0, edges / 3);
    let after = train(&mut lesioned);

    assert!(
        differs(&before, &after) > 0,
        "a third of the synapses were zeroed and the spike train did not move: the model is \
         playing its defaults, not this connectome"
    );
}

#[test]
fn removing_cells_changes_the_train() {
    let mut control = driven(1);
    let before = train(&mut control);

    // 5% fewer neurons, same wiring rule and seed: a different graph must integrate differently.
    let smaller = Graph::synthetic(570, 12, 42);
    let mut sim = Lif::new(smaller, LifParams::default(), 1);
    for i in (0..570).step_by(10) {
        sim.set_stimulus(i, 3.0);
    }
    let after = train(&mut sim);

    assert!(differs(&before, &after) > 0, "deleting 5% of the cells changed nothing");
}

#[test]
fn a_run_is_reproducible_from_its_seed() {
    // The other half of the contract: a mutation test is only readable if an unmutated
    // repeat is identical. Otherwise every difference is just noise.
    let a = train(&mut driven(7));
    let b = train(&mut driven(7));
    assert_eq!(a, b, "same seed, same graph, different train — nothing here is measurable");
}

#[test]
fn noise_is_the_one_thing_the_seed_controls() {
    let mut params = LifParams { noise: 1.5, ..Default::default() };
    params.dt = 1.0;
    let graph = Graph::synthetic(600, 12, 42);
    let mut a = Lif::new(graph.clone(), params, 1);
    let mut b = Lif::new(graph, params, 2);
    for i in (0..600).step_by(10) {
        a.set_stimulus(i, 3.0);
        b.set_stimulus(i, 3.0);
    }
    assert!(differs(&train(&mut a), &train(&mut b)) > 0, "seed had no effect with noise on");
}
