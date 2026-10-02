//! Point-to-plane ICP (what dimos's loop closure runs through Open3D): target normals from a PCA of each point's
//! 30 nearest neighbours within 0.3 m, Gauss-Newton on the linearized point-to-plane error with small-angle updates.
//! Returns the source→target correction and the inlier RMSE² (m²), which the pose graph uses as a variance.
use crate::tf::Iso;
use kiddo::{ImmutableKdTree, SquaredEuclidean};
use nalgebra::{Matrix3, Matrix6, SymmetricEigen, Translation3, UnitQuaternion, Vector3, Vector6};
use std::num::NonZero;

pub struct IcpResult {
    pub transform: Iso,
    /// mean squared distance of inlier correspondences (m²); infinity on failure
    pub rmse2: f64,
    /// inlier share of the source points
    pub fitness: f64,
}

pub struct Target {
    tree: ImmutableKdTree<f64, 3>,
    points: Vec<[f64; 3]>,
    normals: Vec<Option<Vector3<f64>>>,
}

impl Target {
    pub fn new(points: Vec<[f64; 3]>) -> Target {
        let tree = ImmutableKdTree::new_from_slice(&points);
        let normals = points
            .iter()
            .map(|p| {
                let near = tree.nearest_n_within::<SquaredEuclidean>(p, 0.3 * 0.3, NonZero::new(30).unwrap(), true);
                if near.len() < 5 {
                    return None;
                }
                let mut mean = Vector3::zeros();
                for n in &near {
                    mean += Vector3::from(points[n.item as usize]);
                }
                mean /= near.len() as f64;
                let mut covariance = Matrix3::zeros();
                for n in &near {
                    let d = Vector3::from(points[n.item as usize]) - mean;
                    covariance += d * d.transpose();
                }
                let eigen = SymmetricEigen::new(covariance);
                let (index, _) = eigen.eigenvalues.iter().enumerate().min_by(|a, b| a.1.total_cmp(b.1))?;
                Some(eigen.eigenvectors.column(index).into_owned().normalize())
            })
            .collect();
        Target { tree, points, normals }
    }

    pub fn len(&self) -> usize {
        self.points.len()
    }

    pub fn is_empty(&self) -> bool {
        self.points.is_empty()
    }
}

pub fn icp(source: &[[f64; 3]], target: &Target, max_iterations: usize, max_distance: f64, min_inliers: usize) -> IcpResult {
    let failed = IcpResult { transform: Iso::identity(), rmse2: f64::INFINITY, fitness: 0.0 };
    if source.len() < min_inliers || target.len() < min_inliers {
        return failed;
    }
    let mut transform = Iso::identity();
    let max2 = max_distance * max_distance;
    for _ in 0..max_iterations {
        let mut h = Matrix6::<f64>::zeros();
        let mut g = Vector6::<f64>::zeros();
        let mut used = 0usize;
        for p in source {
            let moved = transform * nalgebra::Point3::new(p[0], p[1], p[2]);
            let near = target.tree.nearest_one::<SquaredEuclidean>(&[moved.x, moved.y, moved.z]);
            if near.distance > max2 {
                continue;
            }
            let Some(normal) = target.normals[near.item as usize] else { continue };
            let q = Vector3::from(target.points[near.item as usize]);
            let r = normal.dot(&(moved.coords - q));
            let cross = moved.coords.cross(&normal);
            let j = Vector6::new(cross.x, cross.y, cross.z, normal.x, normal.y, normal.z);
            h += j * j.transpose();
            g += j * r;
            used += 1;
        }
        if used < min_inliers {
            return failed;
        }
        let Some(step) = h.cholesky().map(|c| c.solve(&(-g))) else { return failed };
        let rotation = UnitQuaternion::from_scaled_axis(Vector3::new(step[0], step[1], step[2]));
        let update = Iso::from_parts(Translation3::new(step[3], step[4], step[5]), rotation);
        transform = update * transform;
        if step.norm() < 1e-6 {
            break;
        }
    }
    let mut sum = 0.0;
    let mut inliers = 0usize;
    for p in source {
        let moved = transform * nalgebra::Point3::new(p[0], p[1], p[2]);
        let near = target.tree.nearest_one::<SquaredEuclidean>(&[moved.x, moved.y, moved.z]);
        if near.distance <= max2 {
            sum += near.distance;
            inliers += 1;
        }
    }
    if inliers < min_inliers {
        return failed;
    }
    IcpResult { transform, rmse2: sum / inliers as f64, fitness: inliers as f64 / source.len() as f64 }
}

/// One point per occupied `resolution` cell (the cell's mean), like Open3D's voxel_down_sample.
pub fn voxel_downsample(points: impl IntoIterator<Item = [f64; 3]>, resolution: f64) -> Vec<[f64; 3]> {
    let mut cells: ahash::AHashMap<(i64, i64, i64), ([f64; 3], u32)> = ahash::AHashMap::new();
    for p in points {
        let key = ((p[0] / resolution).floor() as i64, (p[1] / resolution).floor() as i64, (p[2] / resolution).floor() as i64);
        let entry = cells.entry(key).or_insert(([0.0; 3], 0));
        for axis in 0..3 {
            entry.0[axis] += p[axis];
        }
        entry.1 += 1;
    }
    cells.into_values().map(|(sum, n)| [sum[0] / n as f64, sum[1] / n as f64, sum[2] / n as f64]).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// a box-shaped room (floor + 3 walls) sampled on a 5 cm grid
    pub fn room() -> Vec<[f64; 3]> {
        let mut points = Vec::new();
        for i in 0..80 {
            for j in 0..60 {
                let (a, b) = (i as f64 * 0.05, j as f64 * 0.05);
                points.push([a, b, 0.0]);
                if j < 40 {
                    points.push([a, 0.0, b]);
                    points.push([0.0, a.min(3.0), b]);
                    points.push([4.0, a.min(3.0), b]);
                }
            }
        }
        voxel_downsample(points, 0.05)
    }

    #[test]
    fn recovers_a_small_offset() {
        let target = Target::new(room());
        let truth = Iso::from_parts(Translation3::new(0.15, -0.1, 0.05), UnitQuaternion::from_euler_angles(0.0, 0.0, 0.05));
        let source: Vec<[f64; 3]> = room()
            .iter()
            .step_by(3)
            .map(|p| {
                let moved = truth.inverse() * nalgebra::Point3::new(p[0], p[1], p[2]);
                [moved.x, moved.y, moved.z]
            })
            .collect();
        let result = icp(&source, &target, 50, 1.0, 10);
        let error = result.transform * truth.inverse();
        assert!(error.translation.vector.norm() < 0.01, "{:?}", error.translation);
        assert!(error.rotation.angle() < 0.005);
        assert!(result.rmse2 < 1e-3 && result.fitness > 0.9);
    }
}
