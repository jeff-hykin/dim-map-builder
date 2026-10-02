//! Loop closure, ported from dimos's `dimos/mapping/loop_closure/pgo.py` (same defaults and decisions): keyframes
//! every 0.5 m or 10°; for each new keyframe, the nearest older keyframe (optimized position, within 2 m, at least
//! 20 s apart) is a loop candidate; the keyframe's scan is ICP'd against that keyframe's ±10-keyframe submap
//! (0.2 m voxels), and accepted when the inlier RMSE² ≤ 0.3. The pose graph (odometry between-factors with dimos's
//! go2-tuned variances, loop factors with variance from ICP) is solved with Levenberg–Marquardt; dimos uses gtsam's
//! ISAM2, here the whole graph is re-solved when a loop lands (odometry-only keyframes don't move earlier ones).
use crate::icp::{icp, voxel_downsample, Target};
use crate::tf::{interpolate, Iso};
use kiddo::{ImmutableKdTree, SquaredEuclidean};
use nalgebra::{DVector, Matrix6, Translation3, UnitQuaternion, Vector3, Vector6};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PgoConfig {
    pub key_pose_delta_trans: f64,
    pub key_pose_delta_deg: f64,
    pub loop_search_radius: f64,
    pub loop_time_thresh: f64,
    pub loop_score_thresh: f64,
    pub loop_submap_half_range: usize,
    pub min_icp_inliers: usize,
    pub min_keyframes_for_loop_search: usize,
    pub submap_resolution: f64,
    pub min_loop_detect_duration: f64,
    pub max_icp_iterations: usize,
    pub max_icp_correspondence_dist: f64,
    pub odom_rot_var: f64,
    pub odom_trans_var_xy: f64,
    pub odom_trans_var_z: f64,
    pub loop_rot_var: f64,
}

impl Default for PgoConfig {
    fn default() -> Self {
        PgoConfig {
            key_pose_delta_trans: 0.5,
            key_pose_delta_deg: 10.0,
            loop_search_radius: 2.0,
            loop_time_thresh: 20.0,
            loop_score_thresh: 0.3,
            loop_submap_half_range: 10,
            min_icp_inliers: 10,
            min_keyframes_for_loop_search: 10,
            submap_resolution: 0.2,
            min_loop_detect_duration: 5.0,
            max_icp_iterations: 50,
            max_icp_correspondence_dist: 1.0,
            odom_rot_var: 1e-6,
            odom_trans_var_xy: 1e-4,
            odom_trans_var_z: 1e-6,
            loop_rot_var: 0.05,
        }
    }
}

#[derive(Clone)]
pub struct Keyframe {
    pub ts: f64,
    /// sensor pose in the raw (odometry) world
    pub local: Iso,
    /// sensor pose in the drift-corrected world
    pub optimized: Iso,
    /// the scan in the sensor frame, 0.2 m voxels
    pub body: Vec<[f64; 3]>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Loop {
    pub source: usize,
    pub target: usize,
    pub score: f64,
    #[serde(skip)]
    pub offset: Iso,
}

struct Factor {
    i: usize,
    j: usize,
    measured: Iso,
    /// 1/variance for [rx, ry, rz, x, y, z]
    information: Vector6<f64>,
}

pub struct Pgo {
    pub config: PgoConfig,
    pub keyframes: Vec<Keyframe>,
    pub loops: Vec<Loop>,
    factors: Vec<Factor>,
    world_correction: Iso,
    last_loop_ts: Option<f64>,
}

fn residual(xi: &Iso, xj: &Iso, measured: &Iso) -> Vector6<f64> {
    let error = measured.inverse() * (xi.inverse() * xj);
    let r = error.rotation.scaled_axis();
    let t = error.translation.vector;
    Vector6::new(r.x, r.y, r.z, t.x, t.y, t.z)
}

fn retract(x: &Iso, delta: &[f64]) -> Iso {
    let rotation = UnitQuaternion::from_scaled_axis(Vector3::new(delta[0], delta[1], delta[2]));
    x * Iso::from_parts(Translation3::new(delta[3], delta[4], delta[5]), rotation)
}

impl Pgo {
    pub fn new(config: PgoConfig) -> Pgo {
        Pgo { config, keyframes: Vec::new(), loops: Vec::new(), factors: Vec::new(), world_correction: Iso::identity(), last_loop_ts: None }
    }

    fn is_keyframe(&self, local: &Iso) -> bool {
        let Some(last) = self.keyframes.last() else { return true };
        let delta = last.local.inverse() * local;
        delta.translation.vector.norm() > self.config.key_pose_delta_trans || delta.rotation.angle().to_degrees() > self.config.key_pose_delta_deg
    }

    /// One scan: its sensor pose in the odometry world and its points in the sensor frame.
    /// Returns true when a loop closed (the graph was re-solved).
    pub fn process(&mut self, local: Iso, ts: f64, sensor_points: &[[f32; 3]]) -> bool {
        if sensor_points.is_empty() || !self.is_keyframe(&local) {
            return false;
        }
        let body = voxel_downsample(sensor_points.iter().map(|p| [p[0] as f64, p[1] as f64, p[2] as f64]), self.config.submap_resolution);
        let index = self.keyframes.len();
        let optimized = self.world_correction * local;
        if index > 0 {
            let last = &self.keyframes[index - 1];
            let c = &self.config;
            self.factors.push(Factor {
                i: index - 1,
                j: index,
                measured: last.local.inverse() * local,
                information: Vector6::new(
                    1.0 / c.odom_rot_var,
                    1.0 / c.odom_rot_var,
                    1.0 / c.odom_rot_var,
                    1.0 / c.odom_trans_var_xy,
                    1.0 / c.odom_trans_var_xy,
                    1.0 / c.odom_trans_var_z,
                ),
            });
        }
        self.keyframes.push(Keyframe { ts, local, optimized, body });
        if let Some(found) = self.search_for_loop() {
            let trans_information = 1.0 / found.score.max(1e-4);
            let rot_information = 1.0 / self.config.loop_rot_var;
            self.factors.push(Factor {
                i: found.target,
                j: found.source,
                measured: found.offset,
                information: Vector6::new(rot_information, rot_information, rot_information, trans_information, trans_information, trans_information),
            });
            self.loops.push(found);
            self.optimize(20);
            let last = self.keyframes.last().unwrap();
            self.world_correction = last.optimized * last.local.inverse();
            return true;
        }
        false
    }

    fn submap(&self, index: usize, half_range: usize) -> Vec<[f64; 3]> {
        let lo = index.saturating_sub(half_range);
        let hi = (index + half_range).min(self.keyframes.len() - 1);
        let points = (lo..=hi).flat_map(|k| {
            let keyframe = &self.keyframes[k];
            keyframe.body.iter().map(move |p| {
                let w = keyframe.optimized * nalgebra::Point3::new(p[0], p[1], p[2]);
                [w.x, w.y, w.z]
            })
        });
        voxel_downsample(points, self.config.submap_resolution)
    }

    fn search_for_loop(&mut self) -> Option<Loop> {
        let c = &self.config;
        if self.keyframes.len() < c.min_keyframes_for_loop_search {
            return None;
        }
        let current = self.keyframes.len() - 1;
        let now = self.keyframes[current].ts;
        if self.last_loop_ts.is_some_and(|last| now - last < c.min_loop_detect_duration) {
            return None;
        }
        let positions: Vec<[f64; 3]> = self.keyframes[..current].iter().map(|k| k.optimized.translation.vector.into()).collect();
        let tree = ImmutableKdTree::new_from_slice(&positions);
        let here: [f64; 3] = self.keyframes[current].optimized.translation.vector.into();
        let mut candidates: Vec<(f64, usize)> = tree
            .within_unsorted::<SquaredEuclidean>(&here, c.loop_search_radius * c.loop_search_radius)
            .into_iter()
            .map(|n| (n.distance, n.item as usize))
            .filter(|(_, k)| (now - self.keyframes[*k].ts).abs() > c.loop_time_thresh)
            .collect();
        candidates.sort_by(|a, b| a.0.total_cmp(&b.0));
        let (_, target_index) = *candidates.first()?;
        let target = Target::new(self.submap(target_index, c.loop_submap_half_range));
        let source = self.submap(current, 0);
        let result = icp(&source, &target, c.max_icp_iterations, c.max_icp_correspondence_dist, c.min_icp_inliers);
        if !(result.rmse2 <= c.loop_score_thresh) {
            return None;
        }
        // the ICP correction moves the current keyframe's optimized pose onto the old submap
        let refined = result.transform * self.keyframes[current].optimized;
        let offset = self.keyframes[target_index].optimized.inverse() * refined;
        self.last_loop_ts = Some(now);
        Some(Loop { source: current, target: target_index, score: result.rmse2, offset })
    }

    fn cost(&self, poses: &[Iso]) -> f64 {
        self.factors
            .iter()
            .map(|f| {
                let r = residual(&poses[f.i], &poses[f.j], &f.measured);
                r.component_mul(&r).dot(&f.information)
            })
            .sum()
    }

    /// Levenberg–Marquardt over every keyframe but the first (held fixed, dimos's tight prior). The normal equations
    /// are block-sparse; they're solved with conjugate gradients, block-Jacobi preconditioned.
    pub fn optimize(&mut self, iterations: usize) {
        let n = self.keyframes.len();
        if n < 2 {
            return;
        }
        let mut poses: Vec<Iso> = self.keyframes.iter().map(|k| k.optimized).collect();
        let mut lambda = 1e-4;
        let mut cost = self.cost(&poses);
        for _ in 0..iterations {
            // blocks: diagonal (n of them) and off-diagonal (one per factor)
            let mut diagonal = vec![Matrix6::<f64>::zeros(); n];
            let mut off: Vec<(usize, usize, Matrix6<f64>)> = Vec::with_capacity(self.factors.len());
            let mut gradient = DVector::<f64>::zeros(6 * n);
            for factor in &self.factors {
                let r = residual(&poses[factor.i], &poses[factor.j], &factor.measured);
                let h = 1e-6;
                let mut ji = Matrix6::<f64>::zeros();
                let mut jj = Matrix6::<f64>::zeros();
                for k in 0..6 {
                    let mut delta = [0.0; 6];
                    delta[k] = h;
                    let plus_i = residual(&retract(&poses[factor.i], &delta), &poses[factor.j], &factor.measured);
                    let plus_j = residual(&poses[factor.i], &retract(&poses[factor.j], &delta), &factor.measured);
                    ji.set_column(k, &((plus_i - r) / h));
                    jj.set_column(k, &((plus_j - r) / h));
                }
                let w = Matrix6::from_diagonal(&factor.information);
                diagonal[factor.i] += ji.transpose() * w * ji;
                diagonal[factor.j] += jj.transpose() * w * jj;
                off.push((factor.i, factor.j, ji.transpose() * w * jj));
                let gi = ji.transpose() * w * r;
                let gj = jj.transpose() * w * r;
                for k in 0..6 {
                    gradient[6 * factor.i + k] += gi[k];
                    gradient[6 * factor.j + k] += gj[k];
                }
            }
            // pose 0 is fixed
            for k in 0..6 {
                gradient[k] = 0.0;
            }
            let damped: Vec<Matrix6<f64>> = diagonal
                .iter()
                .map(|block| {
                    let mut b = *block;
                    for k in 0..6 {
                        b[(k, k)] += lambda * b[(k, k)].max(1e-9);
                    }
                    b
                })
                .collect();
            let multiply = |x: &DVector<f64>| -> DVector<f64> {
                let mut y = DVector::<f64>::zeros(6 * n);
                for (index, block) in damped.iter().enumerate().skip(1) {
                    let v = block * x.fixed_rows::<6>(6 * index);
                    y.fixed_rows_mut::<6>(6 * index).copy_from(&v);
                }
                for (i, j, block) in &off {
                    if *i == 0 || *j == 0 {
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
                    let v = inverse * x.fixed_rows::<6>(6 * index);
                    y.fixed_rows_mut::<6>(6 * index).copy_from(&v);
                }
                y
            };
            // solve A step = -gradient
            let b = -&gradient;
            let mut step = DVector::<f64>::zeros(6 * n);
            let mut residual_vector = b.clone();
            let mut z = precondition(&residual_vector);
            let mut direction = z.clone();
            let mut rz = residual_vector.dot(&z);
            let b_norm = b.norm().max(1e-30);
            for _ in 0..(6 * n).min(2000) {
                let ad = multiply(&direction);
                let alpha = rz / direction.dot(&ad).max(1e-300);
                step.axpy(alpha, &direction, 1.0);
                residual_vector.axpy(-alpha, &ad, 1.0);
                if residual_vector.norm() / b_norm < 1e-9 {
                    break;
                }
                z = precondition(&residual_vector);
                let rz_next = residual_vector.dot(&z);
                direction = &z + (rz_next / rz) * &direction;
                rz = rz_next;
            }
            let candidate: Vec<Iso> = poses
                .iter()
                .enumerate()
                .map(|(index, pose)| if index == 0 { *pose } else { retract(pose, step.fixed_rows::<6>(6 * index).as_slice()) })
                .collect();
            let candidate_cost = self.cost(&candidate);
            if candidate_cost < cost {
                let improvement = (cost - candidate_cost) / cost.max(1e-30);
                poses = candidate;
                cost = candidate_cost;
                lambda = (lambda * 0.3).max(1e-9);
                if improvement < 1e-8 {
                    break;
                }
            } else {
                lambda *= 10.0;
                if lambda > 1e8 {
                    break;
                }
            }
        }
        for (keyframe, pose) in self.keyframes.iter_mut().zip(poses) {
            keyframe.optimized = pose;
        }
    }

    /// The drift correction (corrected world ← raw world) at `ts`, interpolated between keyframes.
    pub fn correction_at(&self, ts: f64) -> Iso {
        let keyframes = &self.keyframes;
        if keyframes.is_empty() {
            return Iso::identity();
        }
        let correction = |k: &Keyframe| k.optimized * k.local.inverse();
        let index = keyframes.partition_point(|k| k.ts < ts);
        if index == 0 {
            return correction(&keyframes[0]);
        }
        if index >= keyframes.len() {
            return correction(&keyframes[keyframes.len() - 1]);
        }
        let (a, b) = (&keyframes[index - 1], &keyframes[index]);
        let alpha = if b.ts > a.ts { (ts - a.ts) / (b.ts - a.ts) } else { 0.0 };
        interpolate(&correction(a), &correction(b), alpha)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Drive a square loop twice past a structured scene with odometry that drifts in yaw; PGO should close the
    /// loop and pull the second pass back onto the first.
    #[test]
    fn closes_a_drifting_loop() {
        let scene: Vec<[f64; 3]> = {
            let mut points = Vec::new();
            for i in 0..120 {
                for j in 0..30 {
                    let (a, b) = (i as f64 * 0.1 - 2.0, j as f64 * 0.1);
                    points.extend([[a, -2.0, b], [a, 10.0, b], [-2.0, a, b], [10.0, a, b], [a, 4.0 + (i % 7) as f64 * 0.05, b * 0.5]]);
                }
            }
            points
        };
        let mut pgo = Pgo::new(PgoConfig::default());
        let corners = [[0.0, 0.0], [8.0, 0.0], [8.0, 8.0], [0.0, 8.0], [0.0, 0.0]];
        let mut ts = 0.0;
        let mut drift_yaw = 0.0;
        let mut second_lap = Vec::new();
        for lap in 0..2 {
            for leg in 0..4 {
                let (a, b) = (corners[leg], corners[leg + 1]);
                for step in 0..20 {
                    let s = step as f64 / 20.0;
                    let truth = Iso::from_parts(Translation3::new(a[0] + (b[0] - a[0]) * s, a[1] + (b[1] - a[1]) * s, 0.5), UnitQuaternion::identity());
                    drift_yaw += 0.0015;
                    let drift = Iso::from_parts(Translation3::identity(), UnitQuaternion::from_euler_angles(0.0, 0.0, drift_yaw));
                    let local = drift * truth;
                    let body: Vec<[f32; 3]> = scene
                        .iter()
                        .filter(|p| (p[0] - truth.translation.x).hypot(p[1] - truth.translation.y) < 6.0)
                        .map(|p| {
                            let q = truth.inverse() * nalgebra::Point3::new(p[0], p[1], p[2]);
                            [q.x as f32, q.y as f32, q.z as f32]
                        })
                        .collect();
                    ts += 1.0;
                    pgo.process(local, ts, &body);
                    if lap == 1 {
                        second_lap.push((ts, local, truth));
                    }
                }
            }
        }
        assert!(!pgo.loops.is_empty(), "no loop closed");
        let mean = |error: &dyn Fn(&(f64, Iso, Iso)) -> f64| second_lap.iter().map(error).sum::<f64>() / second_lap.len() as f64;
        let raw = mean(&|(_, local, truth)| (local.translation.vector - truth.translation.vector).norm());
        let corrected = mean(&|(ts, local, truth)| ((pgo.correction_at(*ts) * local).translation.vector - truth.translation.vector).norm());
        assert!(corrected < raw * 0.5, "second lap mean error: raw {raw} corrected {corrected} ({} loops)", pgo.loops.len());
    }
}
