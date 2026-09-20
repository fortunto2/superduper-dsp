//! C ABI over `connectome-core`. Shaped after `mobile/sdsp-ios`, which already ships.
//!
//! Every entry point is null-tolerant: a Swift caller that lost its pointer gets a zero or a
//! no-op rather than a crash inside a render loop.

use connectome_core::{Graph, Lif, LifParams};
use std::ffi::CStr;
use std::os::raw::{c_char, c_float, c_int, c_uint};

/// Defaults for a synthetic brain, from `connectome-core/examples/noise_sweep.rs`: about 5% of
/// cells firing a step, the app's "walk" band, with a touch peaking well above it.
pub const SYNTHETIC_NOISE: f32 = 3.5;
pub const SYNTHETIC_WEIGHT_SCALE: f32 = 0.15;

/// Gain for a real export, whose weights are synapse counts (1..hundreds) rather than the
/// synthetic 1..10: measured on FlyWire FAFB v783 with `examples/real_sweep.rs`, 0.02 at
/// noise 3.5 puts 4.9% of cells firing a step — the walk band — where 0.15 saturates at 18%.
pub const REAL_WEIGHT_SCALE: f32 = 0.02;

pub struct FlyBrain {
    sim: Lif,
    synthetic: bool,
    /// Cell positions in µm, index order; empty for a synthetic brain.
    /// Arc so a colony shares one copy — "shared" means anatomy too, and the
    /// non-empty ⇔ real-brain invariant lives in `FlyBrain::new` alone.
    positions: std::sync::Arc<Vec<[f32; 3]>>,
}

impl FlyBrain {
    fn new(sim: Lif, synthetic: bool, positions: std::sync::Arc<Vec<[f32; 3]>>) -> *mut FlyBrain {
        Box::into_raw(Box::new(FlyBrain { sim, synthetic, positions }))
    }
}

macro_rules! borrow {
    ($p:expr, $default:expr) => {
        match unsafe { $p.as_ref() } {
            Some(b) => b,
            None => return $default,
        }
    };
}

#[no_mangle]
pub extern "C" fn fly_is_synthetic(brain: *const FlyBrain) -> c_int {
    borrow!(brain, 1).synthetic as c_int
}

#[no_mangle]
pub extern "C" fn fly_neurons(brain: *const FlyBrain) -> c_uint {
    borrow!(brain, 0).sim.graph().neurons() as c_uint
}

#[no_mangle]
pub extern "C" fn fly_edges(brain: *const FlyBrain) -> c_uint {
    borrow!(brain, 0).sim.graph().edges() as c_uint
}

#[no_mangle]
pub extern "C" fn fly_step(brain: *mut FlyBrain) -> c_uint {
    match unsafe { brain.as_mut() } {
        Some(b) => b.sim.step() as c_uint,
        None => 0,
    }
}

#[no_mangle]
pub extern "C" fn fly_create_synthetic(neurons: c_uint, fan_out: c_uint, seed: u64) -> *mut FlyBrain {
    let n = neurons.max(1) as usize;
    let graph = Graph::synthetic(n, fan_out.max(1) as usize, seed);
    // Alive through background noise, not a tonic stimulus: a stimulus is sensory and gets
    // cleared when a touch ends, which used to leave the brain silent for good. The gain is
    // lowered so noise gives a graded band (sleep … walk) instead of an on/off switch.
    let params = LifParams { noise: SYNTHETIC_NOISE, weight_scale: SYNTHETIC_WEIGHT_SCALE, ..LifParams::default() };
    let sim = Lif::new(graph, params, seed);
    FlyBrain::new(sim, true, std::sync::Arc::new(Vec::new()))
}

/// Returns null when the file is missing, unparseable, or thresholds down to nothing. The
/// caller must check: an empty graph would step happily and report a brain that never fires.
#[no_mangle]
pub extern "C" fn fly_create_from_csv(path: *const c_char, threshold: c_uint, seed: u64) -> *mut FlyBrain {
    if path.is_null() {
        return std::ptr::null_mut();
    }
    let Ok(path) = (unsafe { CStr::from_ptr(path) }).to_str() else {
        return std::ptr::null_mut();
    };
    let Ok(file) = std::fs::File::open(path) else {
        return std::ptr::null_mut();
    };
    let Ok(graph) = Graph::from_edge_list(std::io::BufReader::new(file), threshold) else {
        return std::ptr::null_mut();
    };
    let params = LifParams { noise: SYNTHETIC_NOISE, weight_scale: REAL_WEIGHT_SCALE, ..LifParams::default() };
    let sim = Lif::new(graph, params, seed);
    FlyBrain::new(sim, false, std::sync::Arc::new(Vec::new()))
}

/// The app's packed export (`brain.fcb`): graph plus cell positions. Null on any failure,
/// same contract as the CSV loader.
#[no_mangle]
pub extern "C" fn fly_create_from_fcb(path: *const c_char, threshold: c_uint, seed: u64) -> *mut FlyBrain {
    if path.is_null() {
        return std::ptr::null_mut();
    }
    let Ok(path) = (unsafe { CStr::from_ptr(path) }).to_str() else {
        return std::ptr::null_mut();
    };
    let Ok(bytes) = std::fs::read(path) else {
        return std::ptr::null_mut();
    };
    let Ok((graph, positions)) = Graph::from_fcb(&bytes, threshold) else {
        return std::ptr::null_mut();
    };
    let params = LifParams { noise: SYNTHETIC_NOISE, weight_scale: REAL_WEIGHT_SCALE, ..LifParams::default() };
    let sim = Lif::new(graph, params, seed);
    FlyBrain::new(sim, false, std::sync::Arc::new(positions))
}

/// Cell positions in µm as x,y,z triples, index order; `capacity` counts floats. Returns
/// floats written — 0 for a synthetic brain, which has no anatomy to draw.
#[no_mangle]
pub extern "C" fn fly_copy_positions(brain: *const FlyBrain, out: *mut c_float, capacity: c_uint) -> c_uint {
    let b = borrow!(brain, 0);
    if out.is_null() {
        return 0;
    }
    let flat: &[f32] = bytemuck_free_flatten(&b.positions);
    let n = flat.len().min(capacity as usize);
    unsafe { std::ptr::copy_nonoverlapping(flat.as_ptr(), out, n) };
    n as c_uint
}

fn bytemuck_free_flatten(p: &[[f32; 3]]) -> &[f32] {
    // [f32; 3] is three contiguous f32s with no padding; this is the safe-in-practice cast
    // that `bytemuck::cast_slice` would do, written out to avoid a dependency.
    unsafe { std::slice::from_raw_parts(p.as_ptr() as *const f32, p.len() * 3) }
}

/// Another brain over the same wiring: a second fly costs its state (~2.8 MB on the full
/// connectome); the anatomy positions are Arc-shared, so a second fly costs
/// no copy of them either, and none of the 31 MB graph. The positions MUST come along: fly_copy_positions() returning 0 is the
/// documented "synthetic brain" signal, and a shared fly over a real .fcb is not one.
/// Null if `from` is null.
#[no_mangle]
pub extern "C" fn fly_create_shared(from: *const FlyBrain, seed: u64) -> *mut FlyBrain {
    let Some(b) = (unsafe { from.as_ref() }) else { return std::ptr::null_mut() };
    let sim = Lif::new(b.sim.shared_graph(), *b.sim.params(), seed);
    FlyBrain::new(sim, b.synthetic, std::sync::Arc::clone(&b.positions))
}

#[no_mangle]
pub extern "C" fn fly_destroy(brain: *mut FlyBrain) {
    if !brain.is_null() {
        drop(unsafe { Box::from_raw(brain) });
    }
}


#[no_mangle]
pub extern "C" fn fly_total_spikes(brain: *const FlyBrain) -> u64 {
    borrow!(brain, 0).sim.total_spikes()
}

#[no_mangle]
pub extern "C" fn fly_steps(brain: *const FlyBrain) -> u64 {
    borrow!(brain, 0).sim.steps()
}

#[no_mangle]
pub extern "C" fn fly_reset(brain: *mut FlyBrain, seed: u64) {
    if let Some(b) = unsafe { brain.as_mut() } {
        b.sim.reset(seed);
    }
}

#[no_mangle]
pub extern "C" fn fly_set_stimulus(brain: *mut FlyBrain, neuron: c_uint, current: c_float) {
    if let Some(b) = unsafe { brain.as_mut() } {
        b.sim.set_stimulus(neuron as usize, current);
    }
}

/// Background drive — the arousal dial. See `Lif::set_noise` for the measured bands.
#[no_mangle]
pub extern "C" fn fly_set_noise(brain: *mut FlyBrain, noise: c_float) {
    if let Some(b) = unsafe { brain.as_mut() } {
        b.sim.set_noise(noise);
    }
}

#[no_mangle]
pub extern "C" fn fly_noise(brain: *const FlyBrain) -> c_float {
    borrow!(brain, 0.0).sim.noise()
}

/// Set current on many cells at once. The eye drives 10 600 photoreceptors every frame, and
/// crossing the language boundary once per cell is most of that frame.
#[no_mangle]
pub extern "C" fn fly_set_stimulus_many(brain: *mut FlyBrain, neurons: *const c_uint,
                                        currents: *const c_float, count: c_uint) {
    let Some(b) = (unsafe { brain.as_mut() }) else { return };
    if neurons.is_null() || currents.is_null() {
        return;
    }
    let n = count as usize;
    let (ids, vals) = unsafe {
        (std::slice::from_raw_parts(neurons, n), std::slice::from_raw_parts(currents, n))
    };
    for (&i, &v) in ids.iter().zip(vals) {
        b.sim.set_stimulus(i as usize, v);
    }
}

#[no_mangle]
pub extern "C" fn fly_clear_stimulus(brain: *mut FlyBrain) {
    if let Some(b) = unsafe { brain.as_mut() } {
        b.sim.clear_stimulus();
    }
}

#[no_mangle]
pub extern "C" fn fly_copy_spikes(brain: *const FlyBrain, out: *mut c_uint, capacity: c_uint) -> c_uint {
    let b = borrow!(brain, 0);
    if out.is_null() {
        return 0;
    }
    let spikes = b.sim.spikes();
    let cap = capacity as usize;
    if spikes.len() <= cap {
        unsafe { std::ptr::copy_nonoverlapping(spikes.as_ptr(), out, spikes.len()) };
        return spikes.len() as c_uint;
    }
    // Over capacity: a stride across the whole buffer, not its prefix. Spikes are pushed in
    // index order, so a prefix would light only the low-index rows of a raster and leave the
    // rest permanently dark while the activity bar says otherwise.
    let stride = spikes.len().div_ceil(cap);
    let mut n = 0usize;
    for (k, &idx) in spikes.iter().step_by(stride).enumerate().take(cap) {
        unsafe { *out.add(k) = idx };
        n = k + 1;
    }
    n as c_uint
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spikes_over_capacity_are_sampled_across_the_range_not_truncated() {
        let brain = fly_create_synthetic(139_255, 12, 1);
        for _ in 0..200 {
            fly_step(brain);
        }
        let mut out = vec![0u32; 4096];
        let n = fly_copy_spikes(brain, out.as_mut_ptr(), 4096) as usize;
        assert!(n > 1000, "full tier should fire thousands a step, got {n}");
        let max = *out[..n].iter().max().unwrap();
        assert!(max > 100_000, "sampled spikes must reach the high indices, max {max}");
        fly_destroy(brain);
    }

    #[test]
    fn a_packed_export_loads_with_positions_and_a_threshold() {
        // 3 cells, 2 edges (7 and 12 synapses), positions 1..9, written the way the importer does.
        let mut b: Vec<u8> = b"FCB1".to_vec();
        b.extend(3u32.to_le_bytes());
        b.extend(2u32.to_le_bytes());
        for id in [100u64, 200, 300] {
            b.extend(id.to_le_bytes());
        }
        for s in [0u32, 1, 2, 2] {
            b.extend(s.to_le_bytes());
        }
        for t in [1u32, 2] {
            b.extend(t.to_le_bytes());
        }
        for w in [7u16, 12] {
            b.extend(w.to_le_bytes());
        }
        for f in 1..=9 {
            b.extend((f as f32).to_le_bytes());
        }
        let dir = std::env::temp_dir().join(format!("fcb-{}", std::process::id()));
        std::fs::write(&dir, &b).unwrap();
        let c = std::ffi::CString::new(dir.to_str().unwrap()).unwrap();

        let brain = fly_create_from_fcb(c.as_ptr(), 10, 1);
        assert!(!brain.is_null());
        assert_eq!(fly_neurons(brain), 3, "every cell is kept so positions stay aligned");
        assert_eq!(fly_edges(brain), 1, "the 7-synapse edge is under the threshold");
        assert_eq!(fly_is_synthetic(brain), 0);
        let mut ids = [0u64; 3];
        assert_eq!(fly_copy_ids(brain, ids.as_mut_ptr(), 3), 3);
        assert_eq!(ids, [100, 200, 300]);
        let mut pos = [0f32; 9];
        assert_eq!(fly_copy_positions(brain, pos.as_mut_ptr(), 9), 9);
        assert_eq!(pos[3..6], [4.0, 5.0, 6.0]);
        fly_destroy(brain);
        std::fs::remove_file(&dir).ok();
    }

    #[test]
    fn firing_rates_follow_the_activity_and_a_touched_cell_runs_hot() {
        let brain = fly_create_synthetic(700, 12, 1);
        for _ in 0..500 {
            fly_step(brain);
        }
        let mut rates = vec![0f32; 700];
        assert_eq!(fly_copy_rates(brain, rates.as_mut_ptr(), 700), 700);
        let mean = rates.iter().sum::<f32>() / 700.0;
        assert!(mean > 0.02 && mean < 0.1, "mean rate should sit near the activity: {mean}");
        fly_set_stimulus(brain, 5, 8.0);
        for _ in 0..300 {
            fly_step(brain);
        }
        fly_copy_rates(brain, rates.as_mut_ptr(), 700);
        assert!(rates[5] > 2.0 * mean, "a driven cell must read hotter than the mean: {} vs {mean}", rates[5]);
        fly_destroy(brain);
    }

    #[test]
    fn a_bulk_stimulus_reaches_every_cell_it_names() {
        let brain = fly_create_synthetic(700, 12, 1);
        let ids: Vec<c_uint> = (0..50).map(|i| i * 3).collect();
        let vals = vec![9.0f32; ids.len()];
        fly_set_stimulus_many(brain, ids.as_ptr(), vals.as_ptr(), ids.len() as c_uint);
        for _ in 0..400 {
            fly_step(brain);
        }
        let mut rates = vec![0f32; 700];
        fly_copy_rates(brain, rates.as_mut_ptr(), 700);
        let driven: f32 = ids.iter().map(|&i| rates[i as usize]).sum::<f32>() / ids.len() as f32;
        let rest: f32 = rates.iter().sum::<f32>() / 700.0;
        assert!(driven > 2.0 * rest, "bulk-driven cells should run hot: {driven} vs {rest}");
        fly_destroy(brain);
    }

    #[test]
    fn a_shared_brain_has_the_same_wiring_and_its_own_life() {
        let a = fly_create_synthetic(700, 12, 1);
        let b = fly_create_shared(a, 99);
        assert!(!b.is_null());
        assert_eq!(fly_neurons(b), fly_neurons(a));
        assert_eq!(fly_edges(b), fly_edges(a));
        for _ in 0..500 {
            fly_step(a);
            fly_step(b);
        }
        // Same wiring, different seed: alive, and living differently.
        assert!(fly_total_spikes(a) > 1000 && fly_total_spikes(b) > 1000);
        assert_ne!(fly_total_spikes(a), fly_total_spikes(b));
        // And one fly's senses are its own.
        fly_set_stimulus(a, 5, 9.0);
        for _ in 0..300 {
            fly_step(a);
            fly_step(b);
        }
        let (mut ra, mut rb) = (vec![0f32; 700], vec![0f32; 700]);
        fly_copy_rates(a, ra.as_mut_ptr(), 700);
        fly_copy_rates(b, rb.as_mut_ptr(), 700);
        assert!(ra[5] > 2.0 * rb[5], "stimulus leaked between flies: {} vs {}", ra[5], rb[5]);
        fly_destroy(a);
        fly_destroy(b);
    }

    #[test]
    fn clearing_the_stimulus_leaves_the_brain_alive() {
        let brain = fly_create_synthetic(700, 12, 1);
        for _ in 0..2000 {
            fly_step(brain);
        }
        for i in (0..700).step_by(10) {
            fly_set_stimulus(brain, i, 8.0);
        }
        for _ in 0..250 {
            fly_step(brain);
        }
        fly_clear_stimulus(brain);
        let fired: u32 = (0..500).map(|_| fly_step(brain)).sum();
        assert!(fired > 5000, "brain went quiet after the touch: {fired} spikes in 500 steps");
        fly_destroy(brain);
    }
}

/// Neuron ids by index: for a loaded export these are the source's root ids, so the app can
/// look up each cell's position; for a synthetic brain they are 0..n. Same copy-out contract.
#[no_mangle]
pub extern "C" fn fly_copy_ids(brain: *const FlyBrain, out: *mut u64, capacity: c_uint) -> c_uint {
    let b = borrow!(brain, 0);
    if out.is_null() {
        return 0;
    }
    let ids = &b.sim.graph().ids;
    let n = ids.len().min(capacity as usize);
    unsafe { std::ptr::copy_nonoverlapping(ids.as_ptr(), out, n) };
    n as c_uint
}

/// Running firing rate per cell, 0..1, index order — the heat map. Same copy-out contract.
#[no_mangle]
pub extern "C" fn fly_copy_rates(brain: *const FlyBrain, out: *mut c_float, capacity: c_uint) -> c_uint {
    let b = borrow!(brain, 0);
    if out.is_null() {
        return 0;
    }
    let r = b.sim.rates();
    let n = r.len().min(capacity as usize);
    unsafe { std::ptr::copy_nonoverlapping(r.as_ptr(), out, n) };
    n as c_uint
}

#[no_mangle]
pub extern "C" fn fly_copy_voltages(brain: *const FlyBrain, out: *mut c_float, capacity: c_uint) -> c_uint {
    let b = borrow!(brain, 0);
    if out.is_null() {
        return 0;
    }
    let v = b.sim.voltages();
    let n = v.len().min(capacity as usize);
    unsafe { std::ptr::copy_nonoverlapping(v.as_ptr(), out, n) };
    n as c_uint
}
