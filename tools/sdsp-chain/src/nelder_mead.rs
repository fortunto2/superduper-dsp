//! Bounded Nelder-Mead on the unit cube, a line-for-line port of scipy's `_minimize_neldermead`
//! (rho 1, chi 2, psi 0.5, sigma 0.5, non-adaptive) as `mixopt.py` calls it: bounds clip every vertex,
//! the initial simplex is given, stop on xatol/fatol or when the objective says the budget is spent.

pub struct Settings {
    pub xatol: f64,
    pub fatol: f64,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            xatol: 1e-3,
            fatol: 1e-5,
        }
    }
}

const RHO: f64 = 1.0;
const CHI: f64 = 2.0;
const PSI: f64 = 0.5;
const SIGMA: f64 = 0.5;

fn clip(v: &mut [f64]) {
    for x in v.iter_mut() {
        *x = x.clamp(0.0, 1.0);
    }
}

/// Order the simplex by objective value (stable, so ties keep their order).
fn sort_simplex(sim: &mut Vec<Vec<f64>>, fsim: &mut Vec<f64>) {
    let mut idx: Vec<usize> = (0..fsim.len()).collect();
    idx.sort_by(|&a, &b| {
        fsim[a]
            .partial_cmp(&fsim[b])
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    *sim = idx.iter().map(|&i| sim[i].clone()).collect();
    *fsim = idx.iter().map(|&i| fsim[i]).collect();
}

fn combine(xbar: &[f64], last: &[f64], a: f64, b: f64) -> Vec<f64> {
    let mut v: Vec<f64> = xbar.iter().zip(last).map(|(x, l)| a * x + b * l).collect();
    clip(&mut v);
    v
}

fn converged(sim: &[Vec<f64>], fsim: &[f64], s: &Settings) -> bool {
    let dx = sim[1..]
        .iter()
        .flat_map(|v| v.iter().zip(&sim[0]).map(|(a, b)| (a - b).abs()))
        .fold(0.0, f64::max);
    let df = fsim[1..]
        .iter()
        .map(|f| (fsim[0] - f).abs())
        .fold(0.0, f64::max);
    dx <= s.xatol && df <= s.fatol
}

/// One iteration: reflect, then expand / contract / shrink. `f` may stop the run by returning Err.
fn step<E>(
    sim: &mut [Vec<f64>],
    fsim: &mut [f64],
    f: &mut impl FnMut(&[f64]) -> Result<f64, E>,
) -> Result<(), E> {
    let n = sim.len() - 1;
    let xbar: Vec<f64> = (0..n)
        .map(|j| sim[..n].iter().map(|v| v[j]).sum::<f64>() / n as f64)
        .collect();
    let last = sim[n].clone();
    let xr = combine(&xbar, &last, 1.0 + RHO, -RHO);
    let fxr = f(&xr)?;
    let mut shrink = false;
    if fxr < fsim[0] {
        let xe = combine(&xbar, &last, 1.0 + RHO * CHI, -RHO * CHI);
        let fxe = f(&xe)?;
        if fxe < fxr {
            sim[n] = xe;
            fsim[n] = fxe;
        } else {
            sim[n] = xr;
            fsim[n] = fxr;
        }
    } else if fxr < fsim[n - 1] {
        sim[n] = xr;
        fsim[n] = fxr;
    } else {
        let (xc, fxc, ok) = if fxr < fsim[n] {
            let xc = combine(&xbar, &last, 1.0 + PSI * RHO, -PSI * RHO);
            let fxc = f(&xc)?;
            (xc, fxc, fxc <= fxr)
        } else {
            let xcc = combine(&xbar, &last, 1.0 - PSI, PSI);
            let fxcc = f(&xcc)?;
            (xcc, fxcc, fxcc < fsim[n])
        };
        if ok {
            sim[n] = xc;
            fsim[n] = fxc;
        } else {
            shrink = true;
        }
    }
    if shrink {
        for j in 1..=n {
            let mut v: Vec<f64> = sim[0]
                .iter()
                .zip(&sim[j])
                .map(|(a, b)| a + SIGMA * (b - a))
                .collect();
            clip(&mut v);
            fsim[j] = f(&v)?;
            sim[j] = v;
        }
    }
    Ok(())
}

/// Minimise `f` from the given initial simplex (n+1 vertices in [0,1]^n). Returns Err as soon as `f` does
/// (the evaluation budget), otherwise the best vertex when the simplex has collapsed.
pub fn minimize<E>(
    mut simplex: Vec<Vec<f64>>,
    s: &Settings,
    mut f: impl FnMut(&[f64]) -> Result<f64, E>,
) -> Result<Vec<f64>, E> {
    simplex.iter_mut().for_each(|v| clip(v));
    let mut fsim = Vec::with_capacity(simplex.len());
    for v in &simplex {
        fsim.push(f(v)?);
    }
    sort_simplex(&mut simplex, &mut fsim);
    while !converged(&simplex, &fsim, s) {
        step(&mut simplex, &mut fsim, &mut f)?;
        sort_simplex(&mut simplex, &mut fsim);
    }
    Ok(simplex[0].clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn simplex(u0: &[f64], step: f64) -> Vec<Vec<f64>> {
        let mut s = vec![u0.to_vec()];
        for i in 0..u0.len() {
            let mut e = u0.to_vec();
            e[i] = if e[i] + step <= 1.0 {
                e[i] + step
            } else {
                e[i] - step
            };
            s.push(e);
        }
        s
    }

    #[test]
    fn finds_the_minimum_of_a_bowl_inside_the_cube() {
        let mut calls = 0;
        let best = minimize::<()>(simplex(&[0.9, 0.1], 0.25), &Settings::default(), |u| {
            calls += 1;
            Ok((u[0] - 0.3).powi(2) + 2.0 * (u[1] - 0.7).powi(2))
        })
        .unwrap();
        assert!(
            (best[0] - 0.3).abs() < 0.01 && (best[1] - 0.7).abs() < 0.01,
            "{best:?}"
        );
        assert!(calls < 200, "{calls}");
    }

    #[test]
    fn a_minimum_outside_the_cube_ends_on_the_bound() {
        let best = minimize::<()>(simplex(&[0.5], 0.25), &Settings::default(), |u| {
            Ok((u[0] - 2.0).powi(2))
        })
        .unwrap();
        assert!((best[0] - 1.0).abs() < 1e-3, "{best:?}");
    }

    #[test]
    fn the_budget_error_stops_the_run() {
        let mut n = 0;
        let r = minimize(simplex(&[0.5], 0.25), &Settings::default(), |u| {
            n += 1;
            if n > 4 {
                Err("budget")
            } else {
                Ok(u[0])
            }
        });
        assert_eq!(r, Err("budget"));
    }

    /// scipy.optimize.minimize(method="Nelder-Mead", bounds=[(0,1)]*2, initial_simplex, xatol 1e-3, fatol 1e-5)
    /// on the same bowl from (0.9, 0.1): x = [0.29985941, 0.699708] after 49 evaluations.
    #[test]
    fn follows_scipy_step_for_step_on_the_bowl() {
        let mut calls = 0;
        let best = minimize::<()>(simplex(&[0.9, 0.1], 0.25), &Settings::default(), |u| {
            calls += 1;
            Ok((u[0] - 0.3).powi(2) + 2.0 * (u[1] - 0.7).powi(2))
        })
        .unwrap();
        assert_eq!(calls, 49);
        assert!(
            (best[0] - 0.29985941).abs() < 1e-7 && (best[1] - 0.699708).abs() < 1e-7,
            "{best:?}"
        );
    }

    /// Sphere in 3-D with the minimum (0.2, 0.5, 0.8) inside the cube.
    /// scipy from (0.6, 0.6, 0.1): x = [0.20029152, 0.50040904, 0.79984993] after 72 evaluations.
    #[test]
    fn sphere_matches_scipy() {
        let mut calls = 0;
        let best = minimize::<()>(simplex(&[0.6, 0.6, 0.1], 0.25), &Settings::default(), |u| {
            calls += 1;
            Ok(u.iter()
                .zip([0.2, 0.5, 0.8])
                .map(|(a, b)| (a - b).powi(2))
                .sum())
        })
        .unwrap();
        assert_eq!(calls, 72);
        for (g, w) in best.iter().zip([0.20029152, 0.50040904, 0.79984993]) {
            assert!((g - w).abs() < 1e-7, "{best:?}");
        }
    }

    /// Rosenbrock, x = 2u - 0.5 so its minimum (1, 1) sits at u = (0.75, 0.75).
    /// scipy from (0.3, 0.3): x = [0.74986456, 0.74970207] after 86 evaluations.
    #[test]
    fn rosenbrock_matches_scipy_and_finds_its_valley_floor() {
        let mut calls = 0;
        let best = minimize::<()>(simplex(&[0.3, 0.3], 0.25), &Settings::default(), |u| {
            calls += 1;
            let (x, y) = (2.0 * u[0] - 0.5, 2.0 * u[1] - 0.5);
            Ok(100.0 * (y - x * x).powi(2) + (1.0 - x).powi(2))
        })
        .unwrap();
        assert_eq!(calls, 86);
        assert!(
            (best[0] - 0.74986456).abs() < 1e-7 && (best[1] - 0.74970207).abs() < 1e-7,
            "{best:?}"
        );
        assert!((best[0] - 0.75).abs() < 1e-3 && (best[1] - 0.75).abs() < 1e-3);
    }
}
