//! A general pose graph: pose variables (keyframes, and landmarks such as AprilTags), between factors with a full 6x6
//! information matrix (tangent order [rx, ry, rz, x, y, z], gtsam's), optionally Huber-robust, solved by
//! Levenberg–Marquardt with variable 0 held fixed (the gravity anchor). The normal equations are block-sparse and
//! solved by block-Jacobi preconditioned conjugate gradients, as in `pgo.rs`; Jacobians are numeric.
use crate::tf::Iso;
use nalgebra::{DVector, Matrix6, Translation3, UnitQuaternion, Vector3, Vector6};

pub struct Between {
    pub i: usize,
    pub j: usize,
    /// xi⁻¹ · xj as measured
    pub measured: Iso,
    pub information: Matrix6<f64>,
    /// Huber threshold on the whitened error (gtsam's mEstimator::Huber k), None = plain least squares
    pub huber: Option<f64>,
}

#[derive(Default)]
pub struct Graph {
    pub values: Vec<Iso>,
    pub factors: Vec<Between>,
}

/// The error of a between factor: log(measured⁻¹ · xi⁻¹ · xj) as [rotation vector, translation].
pub fn residual(xi: &Iso, xj: &Iso, measured: &Iso) -> Vector6<f64> {
    let error = measured.inverse() * (xi.inverse() * xj);
    let r = error.rotation.scaled_axis();
    let t = error.translation.vector;
    Vector6::new(r.x, r.y, r.z, t.x, t.y, t.z)
}

fn retract(x: &Iso, delta: &[f64]) -> Iso {
    let rotation = UnitQuaternion::from_scaled_axis(Vector3::new(delta[0], delta[1], delta[2]));
    x * Iso::from_parts(Translation3::new(delta[3], delta[4], delta[5]), rotation)
}

/// An information matrix from per-axis variances.
pub fn diagonal(variances: [f64; 6]) -> Matrix6<f64> {
    Matrix6::from_diagonal(&Vector6::from_iterator(variances.iter().map(|v| 1.0 / v.max(1e-300))))
}

impl Graph {
    pub fn add(&mut self, value: Iso) -> usize {
        self.values.push(value);
        self.values.len() - 1
    }

    pub fn between(&mut self, i: usize, j: usize, measured: Iso, information: Matrix6<f64>, huber: Option<f64>) {
        self.factors.push(Between { i, j, measured, information, huber });
    }

    /// The robust weight of a factor at its current error: 1, or k / ‖e‖ past the Huber threshold.
    fn weight(factor: &Between, r: &Vector6<f64>) -> f64 {
        match factor.huber {
            Some(k) => {
                let whitened = (r.transpose() * factor.information * r)[(0, 0)].max(0.0).sqrt();
                if whitened > k {
                    k / whitened
                } else {
                    1.0
                }
            }
            None => 1.0,
        }
    }

    /// Σ ρ(e) with Huber's ρ (½e² inside k, k·|e| − ½k² outside), times 2 so plain factors give eᵀΩe.
    pub fn cost(&self, poses: &[Iso]) -> f64 {
        self.factors
            .iter()
            .map(|factor| {
                let r = residual(&poses[factor.i], &poses[factor.j], &factor.measured);
                let squared = (r.transpose() * factor.information * r)[(0, 0)].max(0.0);
                match factor.huber {
                    Some(k) if squared.sqrt() > k => 2.0 * (k * squared.sqrt() - 0.5 * k * k),
                    _ => squared,
                }
            })
            .sum()
    }

    /// Levenberg–Marquardt, `iterations` at most; returns the final cost. Cancellation is checked between iterations.
    pub fn optimize(&mut self, iterations: usize, cancelled: &dyn Fn() -> bool) -> f64 {
        let n = self.values.len();
        if n < 2 || self.factors.is_empty() {
            return self.cost(&self.values);
        }
        let mut poses = self.values.clone();
        let mut lambda = 1e-5;
        let mut cost = self.cost(&poses);
        for _ in 0..iterations {
            if cancelled() {
                break;
            }
            let mut diagonal_blocks = vec![Matrix6::<f64>::zeros(); n];
            let mut off: Vec<(usize, usize, Matrix6<f64>)> = Vec::with_capacity(self.factors.len());
            let mut gradient = DVector::<f64>::zeros(6 * n);
            for factor in &self.factors {
                let (xi, xj) = (&poses[factor.i], &poses[factor.j]);
                let r = residual(xi, xj, &factor.measured);
                let h = 1e-6;
                let mut ji = Matrix6::<f64>::zeros();
                let mut jj = Matrix6::<f64>::zeros();
                for k in 0..6 {
                    let mut delta = [0.0; 6];
                    delta[k] = h;
                    ji.set_column(k, &((residual(&retract(xi, &delta), xj, &factor.measured) - r) / h));
                    jj.set_column(k, &((residual(xi, &retract(xj, &delta), &factor.measured) - r) / h));
                }
                let w = factor.information * Self::weight(factor, &r);
                diagonal_blocks[factor.i] += ji.transpose() * w * ji;
                diagonal_blocks[factor.j] += jj.transpose() * w * jj;
                off.push((factor.i, factor.j, ji.transpose() * w * jj));
                let (gi, gj) = (ji.transpose() * w * r, jj.transpose() * w * r);
                for k in 0..6 {
                    gradient[6 * factor.i + k] += gi[k];
                    gradient[6 * factor.j + k] += gj[k];
                }
            }
            for k in 0..6 {
                gradient[k] = 0.0;
            }
            let damped: Vec<Matrix6<f64>> = diagonal_blocks
                .iter()
                .map(|block| {
                    let mut b = *block;
                    for k in 0..6 {
                        b[(k, k)] += lambda * b[(k, k)].max(1e-6);
                    }
                    b
                })
                .collect();
            let multiply = |x: &DVector<f64>| -> DVector<f64> {
                let mut y = DVector::<f64>::zeros(6 * n);
                for (index, block) in damped.iter().enumerate().skip(1) {
                    y.fixed_rows_mut::<6>(6 * index).copy_from(&(block * x.fixed_rows::<6>(6 * index)));
                }
                for (i, j, block) in &off {
                    if *i == 0 || *j == 0 || i == j {
                        continue;
                    }
                    let yi = block * x.fixed_rows::<6>(6 * j);
                    let yj = block.transpose() * x.fixed_rows::<6>(6 * i);
                    let mut si = y.fixed_rows_mut::<6>(6 * i);
                    si += yi;
                    let mut sj = y.fixed_rows_mut::<6>(6 * j);
                    sj += yj;
                }
                y
            };
            let inverses: Vec<Matrix6<f64>> = damped.iter().map(|b| b.try_inverse().unwrap_or(Matrix6::identity())).collect();
            let precondition = |x: &DVector<f64>| -> DVector<f64> {
                let mut y = DVector::<f64>::zeros(6 * n);
                for (index, inverse) in inverses.iter().enumerate().skip(1) {
                    y.fixed_rows_mut::<6>(6 * index).copy_from(&(inverse * x.fixed_rows::<6>(6 * index)));
                }
                y
            };
            let b = -&gradient;
            let mut step = DVector::<f64>::zeros(6 * n);
            let mut remaining = b.clone();
            let mut z = precondition(&remaining);
            let mut direction = z.clone();
            let mut rz = remaining.dot(&z);
            let b_norm = b.norm().max(1e-30);
            for _ in 0..(6 * n).min(4000) {
                let ad = multiply(&direction);
                let alpha = rz / direction.dot(&ad).max(1e-300);
                step.axpy(alpha, &direction, 1.0);
                remaining.axpy(-alpha, &ad, 1.0);
                if remaining.norm() / b_norm < 1e-10 {
                    break;
                }
                z = precondition(&remaining);
                let rz_next = remaining.dot(&z);
                direction = &z + (rz_next / rz) * &direction;
                rz = rz_next;
            }
            let candidate: Vec<Iso> = poses.iter().enumerate().map(|(index, pose)| if index == 0 { *pose } else { retract(pose, step.fixed_rows::<6>(6 * index).as_slice()) }).collect();
            let candidate_cost = self.cost(&candidate);
            if candidate_cost < cost {
                let improvement = (cost - candidate_cost) / cost.max(1e-30);
                poses = candidate;
                cost = candidate_cost;
                lambda = (lambda * 0.3).max(1e-12);
                if improvement < 1e-9 {
                    break;
                }
            } else {
                lambda *= 10.0;
                if lambda > 1e10 {
                    break;
                }
            }
        }
        self.values = poses;
        cost
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A square walked with yaw drift; one landmark seen at the start and again at the end pulls the end back.
    #[test]
    fn a_landmark_seen_twice_closes_the_loop() {
        let mut truth = Vec::new();
        for leg in 0..4 {
            for step in 0..10 {
                let yaw = leg as f64 * std::f64::consts::FRAC_PI_2;
                let along = step as f64;
                let corner = [[0.0, 0.0], [10.0, 0.0], [10.0, 10.0], [0.0, 10.0]][leg];
                let direction = [yaw.cos(), yaw.sin()];
                truth.push(Iso::from_parts(Translation3::new(corner[0] + direction[0] * along, corner[1] + direction[1] * along, 0.0), UnitQuaternion::from_euler_angles(0.0, 0.0, yaw)));
            }
        }
        truth.push(truth[0]);
        // odometry with a yaw bias
        let bias = Iso::from_parts(Translation3::identity(), UnitQuaternion::from_euler_angles(0.0, 0.0, 0.01));
        let mut odometry = vec![truth[0]];
        for k in 1..truth.len() {
            let last = *odometry.last().unwrap();
            odometry.push(last * (truth[k - 1].inverse() * truth[k]) * bias);
        }
        let mut graph = Graph::default();
        for pose in &odometry {
            graph.add(*pose);
        }
        let odometry_information = diagonal([1e-8, 1e-8, 1e-4, 1e-4, 1e-4, 1e-6]);
        for k in 1..odometry.len() {
            graph.between(k - 1, k, odometry[k - 1].inverse() * odometry[k], odometry_information, None);
        }
        let tag_truth = Iso::from_parts(Translation3::new(1.0, -1.0, 0.5), UnitQuaternion::identity());
        let landmark = graph.add(odometry[0] * truth[0].inverse() * tag_truth);
        let last = odometry.len() - 1;
        for k in [0, 1, last] {
            graph.between(k, landmark, truth[k].inverse() * tag_truth, diagonal([0.01, 0.01, 0.01, 0.0025, 0.0025, 0.0025]), None);
        }
        let before = (odometry[last].translation.vector - truth[last].translation.vector).norm();
        graph.optimize(100, &|| false);
        let after = (graph.values[last].translation.vector - truth[last].translation.vector).norm();
        assert!(before > 1.0 && after < before / 10.0, "end error {before} -> {after}");
    }

    #[test]
    fn huber_limits_a_bad_factor() {
        let mut graph = Graph::default();
        graph.add(Iso::identity());
        graph.add(Iso::translation(1.0, 0.0, 0.0));
        let information = diagonal([1e-4; 6]);
        graph.between(0, 1, Iso::translation(1.0, 0.0, 0.0), information, None);
        graph.between(0, 1, Iso::translation(1.0, 0.0, 0.0), information, None);
        graph.between(0, 1, Iso::translation(6.0, 0.0, 0.0), information, Some(1.345));
        graph.optimize(50, &|| false);
        assert!((graph.values[1].translation.x - 1.0).abs() < 0.05, "{}", graph.values[1].translation.x);
    }
}
