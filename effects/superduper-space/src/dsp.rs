//! Where a track sits in the room, as speaker gains.
//!
//! Vector-base amplitude panning (Pulkki 1997) on the bed ring: the source's
//! azimuth picks the two speakers either side of it and the gains are the
//! inverse of that pair's base, equal-power normalised, so a source exactly
//! on a speaker is only that speaker and a source between two is a constant
//! loudness. 7.1.4 adds a height ring the elevation blends in. `Spread`
//! pulls the point source towards "a little of every speaker"; `Distance`
//! takes level and air (a one-pole low-pass) away. No layout here is a
//! Dolby Atmos bed — these are the channel orders REAPER, Logic and the
//! ITU tables agree on (L R C LFE Ls Rs Lb Rb Tfl Tfr Tbl Tbr).

/// Output channels the plugin declares: enough for 7.1.4. Smaller layouts
/// leave the rest silent.
pub const MAX_OUT: usize = 12;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layout { Stereo, Quad, Surround51, Surround71, Surround714 }

pub const LAYOUT_NAMES: [&str; 5] = ["Stereo", "Quad", "5.1", "7.1", "7.1.4"];

impl Layout {
    pub fn from_param(v: f32) -> Self {
        match v.round().max(0.0) as u32 {
            0 => Layout::Stereo,
            1 => Layout::Quad,
            2 => Layout::Surround51,
            3 => Layout::Surround71,
            _ => Layout::Surround714,
        }
    }

    /// Channels this layout drives (the port is always `MAX_OUT` wide).
    pub fn channels(self) -> usize {
        match self { Layout::Stereo => 2, Layout::Quad => 4, Layout::Surround51 => 6, Layout::Surround71 => 8, Layout::Surround714 => 12 }
    }

    /// Bed speakers: (channel, azimuth degrees; 0 = front, +right).
    pub fn bed(self) -> &'static [(usize, f32)] {
        match self {
            Layout::Stereo => &[(0, -30.0), (1, 30.0)],
            Layout::Quad => &[(0, -45.0), (1, 45.0), (2, -135.0), (3, 135.0)],
            Layout::Surround51 => &[(0, -30.0), (1, 30.0), (2, 0.0), (4, -110.0), (5, 110.0)],
            Layout::Surround71 | Layout::Surround714 => &[(0, -30.0), (1, 30.0), (2, 0.0), (4, -90.0), (5, 90.0), (6, -150.0), (7, 150.0)],
        }
    }

    /// Height speakers, 7.1.4 only.
    pub fn height(self) -> &'static [(usize, f32)] {
        match self {
            Layout::Surround714 => &[(8, -45.0), (9, 45.0), (10, -135.0), (11, 135.0)],
            _ => &[],
        }
    }

    pub fn lfe(self) -> Option<usize> {
        match self { Layout::Surround51 | Layout::Surround71 | Layout::Surround714 => Some(3), _ => None }
    }

    /// Every speaker with its name, for the map and the `position` document.
    pub fn speakers(self) -> Vec<(usize, &'static str, f32, bool)> {
        const NAMES: [&str; MAX_OUT] = ["L", "R", "C", "LFE", "Ls", "Rs", "Lb", "Rb", "Tfl", "Tfr", "Tbl", "Tbr"];
        let mut v: Vec<(usize, &'static str, f32, bool)> = self.bed().iter().map(|&(c, a)| (c, NAMES[c], a, false)).collect();
        v.extend(self.height().iter().map(|&(c, a)| (c, NAMES[c], a, true)));
        if let Some(c) = self.lfe() { v.push((c, NAMES[c], 0.0, false)); }
        v.sort_by_key(|s| s.0);
        v
    }
}

/// Azimuth (degrees, 0 = front, clockwise) of a map position; `(0, 0)` is
/// the listener and reads as front.
pub fn azimuth_deg(x: f32, y: f32) -> f32 {
    if x.abs() < 1e-6 && y.abs() < 1e-6 { 0.0 } else { x.atan2(y).to_degrees() }
}

/// VBAP gains over one ring of speakers, written into `out` by channel.
/// A ring of one speaker gets everything; two or more pick the enclosing
/// pair. Equal-power: the written gains' squares sum to 1.
fn ring_gains(ring: &[(usize, f32)], az_deg: f32, out: &mut [f32; MAX_OUT]) {
    if ring.is_empty() { return; }
    if ring.len() == 1 { out[ring[0].0] = 1.0; return; }
    // Everything in 0..360 so the ring is one ordered circle.
    let mut order: Vec<(usize, f32)> = ring.iter().map(|&(c, a)| (c, wrap_deg(a))).collect();
    order.sort_by(|a, b| a.1.total_cmp(&b.1));
    let az = wrap_deg(az_deg);
    let n = order.len();
    let (a, b) = if az < order[0].1 || az >= order[n - 1].1 {
        (order[n - 1], order[0])
    } else {
        let i = (0..n - 1).find(|&i| order[i].1 <= az && az < order[i + 1].1).unwrap_or(0);
        (order[i], order[i + 1])
    };
    let (ax, ay) = unit(a.1);
    let (bx, by) = unit(b.1);
    let (px, py) = unit(az);
    // solve [ax bx; ay by] [ga gb] = [px py]
    let det = ax * by - bx * ay;
    let (ga, gb) = if det.abs() < 1e-6 {
        (0.5f32.sqrt(), 0.5f32.sqrt())
    } else {
        ((px * by - bx * py) / det, (ax * py - px * ay) / det)
    };
    let (ga, gb) = (ga.max(0.0), gb.max(0.0));
    let norm = (ga * ga + gb * gb).sqrt().max(1e-9);
    out[a.0] += ga / norm;
    out[b.0] += gb / norm;
}

fn unit(deg: f32) -> (f32, f32) {
    let r = deg.to_radians();
    (r.sin(), r.cos())
}

fn wrap_deg(d: f32) -> f32 {
    let mut d = d % 360.0;
    if d < 0.0 { d += 360.0; }
    d
}

/// One source's gains on every output channel (LFE excluded; the plugin
/// adds its send separately). `spread` 0 = VBAP point, 1 = every speaker
/// alike; `z` 0..1 lifts the source into the height ring where there is one.
pub fn gains(layout: Layout, az_deg: f32, z: f32, spread: f32) -> [f32; MAX_OUT] {
    let mut out = [0.0f32; MAX_OUT];
    let height = layout.height();
    let z = if height.is_empty() { 0.0 } else { z.clamp(0.0, 1.0) };
    let (w_bed, w_top) = (((1.0 - z) * core::f32::consts::FRAC_PI_2).sin().max(0.0), (z * core::f32::consts::FRAC_PI_2).sin().max(0.0));
    let mut bed = [0.0f32; MAX_OUT];
    ring_gains(layout.bed(), az_deg, &mut bed);
    for c in 0..MAX_OUT { out[c] += bed[c] * w_bed; }
    if !height.is_empty() && w_top > 0.0 {
        let mut top = [0.0f32; MAX_OUT];
        ring_gains(height, az_deg, &mut top);
        for c in 0..MAX_OUT { out[c] += top[c] * w_top; }
    }
    let spread = spread.clamp(0.0, 1.0);
    if spread > 0.0 {
        let active: Vec<usize> = layout.bed().iter().chain(height.iter()).map(|s| s.0).collect();
        let even = 1.0 / (active.len() as f32).sqrt();
        for &c in &active { out[c] = (1.0 - spread) * out[c] + spread * even; }
        let norm = out.iter().map(|g| g * g).sum::<f32>().sqrt().max(1e-9);
        for g in out.iter_mut() { *g /= norm; }
    }
    out
}

/// Level and air for a source `d` (0 = at the listener, 1 = far):
/// −12 dB and a 4 kHz one-pole at the far end, nothing at the near end.
pub fn distance_cues(d: f32) -> (f32, f32) {
    let d = d.clamp(0.0, 1.0);
    let gain = 1.0 / (1.0 + 3.0 * d);
    let cutoff_hz = 20_000.0 * (4_000.0f32 / 20_000.0).powf(d);
    (gain, cutoff_hz)
}

/// Equal-power stereo fold of a layout's channels, for monitoring on two
/// speakers: each channel pans by its azimuth (height folded onto the bed).
pub fn fold_weights(layout: Layout) -> [(f32, f32); MAX_OUT] {
    let mut w = [(0.0f32, 0.0f32); MAX_OUT];
    for &(c, az) in layout.bed().iter().chain(layout.height().iter()) {
        let t = ((az.clamp(-90.0, 90.0) / 90.0) + 1.0) * 0.25 * core::f32::consts::PI; // 0..π/2
        w[c] = (t.cos(), t.sin());
    }
    if let Some(c) = layout.lfe() { w[c] = (0.5f32.sqrt(), 0.5f32.sqrt()); }
    w
}

#[cfg(test)]
mod tests {
    use super::*;

    fn energy(g: &[f32; MAX_OUT]) -> f32 { g.iter().map(|x| x * x).sum::<f32>() }

    #[test]
    fn a_source_on_a_speaker_is_only_that_speaker() {
        for layout in [Layout::Stereo, Layout::Quad, Layout::Surround51, Layout::Surround71, Layout::Surround714] {
            for &(c, az) in layout.bed() {
                let g = gains(layout, az, 0.0, 0.0);
                assert!((g[c] - 1.0).abs() < 1e-4, "{layout:?} {az}: {g:?}");
                assert!((energy(&g) - 1.0).abs() < 1e-4);
            }
        }
    }

    #[test]
    fn between_two_speakers_is_equal_power_and_only_those_two() {
        let g = gains(Layout::Stereo, 0.0, 0.0, 0.0);
        assert!((g[0] - g[1]).abs() < 1e-5 && (energy(&g) - 1.0).abs() < 1e-5, "{g:?}");
        let g = gains(Layout::Surround71, -60.0, 0.0, 0.0); // between L (−30) and Ls (−90)
        assert!(g[0] > 0.0 && g[4] > 0.0 && g[1] == 0.0 && g[2] == 0.0 && g[6] == 0.0, "{g:?}");
        for az in (-180..180).step_by(7) {
            let g = gains(Layout::Surround714, az as f32, 0.0, 0.0);
            assert!((energy(&g) - 1.0).abs() < 1e-4, "{az}: {g:?}");
            assert!(g[3] == 0.0, "no LFE from the panner");
        }
    }

    #[test]
    fn height_moves_energy_to_the_top_ring_only_where_there_is_one() {
        let up = gains(Layout::Surround714, 45.0, 1.0, 0.0);
        assert!((up[9] - 1.0).abs() < 1e-4, "Tfr alone: {up:?}");
        assert!(up[..8].iter().all(|g| *g == 0.0));
        let half = gains(Layout::Surround714, 45.0, 0.5, 0.0);
        assert!((energy(&half) - 1.0).abs() < 1e-4 && half[9] > 0.0 && half[1] > 0.0);
        let flat = gains(Layout::Surround71, 45.0, 1.0, 0.0);
        assert!(flat[8..].iter().all(|g| *g == 0.0) && (energy(&flat) - 1.0).abs() < 1e-4);
    }

    #[test]
    fn spread_one_is_every_speaker_alike() {
        let g = gains(Layout::Surround51, -30.0, 0.0, 1.0);
        let active: Vec<f32> = Layout::Surround51.bed().iter().map(|s| g[s.0]).collect();
        assert!(active.iter().all(|v| (v - active[0]).abs() < 1e-5), "{g:?}");
        assert!((energy(&g) - 1.0).abs() < 1e-4);
    }

    #[test]
    fn distance_takes_level_and_air() {
        let (g0, f0) = distance_cues(0.0);
        let (g1, f1) = distance_cues(1.0);
        assert!((g0 - 1.0).abs() < 1e-6 && f0 >= 19_999.0);
        assert!((20.0 * g1.log10() + 12.04).abs() < 0.1, "{g1}");
        assert!((f1 - 4000.0).abs() < 1.0);
    }

    #[test]
    fn azimuth_reads_the_map() {
        assert_eq!(azimuth_deg(0.0, 1.0), 0.0);
        assert!((azimuth_deg(1.0, 0.0) - 90.0).abs() < 1e-4);
        assert!((azimuth_deg(-1.0, 0.0) + 90.0).abs() < 1e-4);
        assert!((azimuth_deg(0.0, -1.0).abs() - 180.0).abs() < 1e-4);
    }
}
