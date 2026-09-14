//! Does a touch read as a touch through a 16-step frame mean? For each tier size and
//! arousal level: idle frame-mean activity (max over 60 frames) vs the max frame mean in
//! the 16 frames of a touch that injects 8.0 into `fraction` of the cells.
use connectome_core::{Graph, Lif, LifParams};

fn frame_means(sim: &mut Lif, frames: usize, n: usize) -> Vec<f64> {
    (0..frames).map(|_| sim.run(16).iter().map(|&x| x as f64).sum::<f64>() / (16.0 * n as f64)).collect()
}

fn main() {
    println!("neurons  noise  frac   idle(max)  touched(max)  touched(mean)");
    for &n in &[700usize, 6000, 139_255] {
        for &noise in &[3.5f32, 1.0] {
            for &frac in &[0.05f64, 0.10] {
                let params = LifParams { noise, weight_scale: 0.15, ..LifParams::default() };
                let mut sim = Lif::new(Graph::synthetic(n, 12, 1), params, 1);
                sim.run(2000);
                let idle = frame_means(&mut sim, 60, n).into_iter().fold(0.0, f64::max);
                let stride = (1.0 / frac) as usize;
                for i in (0..n).step_by(stride) { sim.set_stimulus(i, 8.0); }
                let t = frame_means(&mut sim, 16, n);
                sim.clear_stimulus();
                let tmax = t.iter().cloned().fold(0.0, f64::max);
                let tmean = t.iter().sum::<f64>() / t.len() as f64;
                println!("{n:>7}  {noise:>5}  {frac:.2}   {idle:.3}      {tmax:.3}         {tmean:.3}");
            }
        }
    }
}
