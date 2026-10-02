//! A floor's 2D plan from the 3D map: per `resolution` cell, occupied when something stands there between
//! `obstacle_min` and `obstacle_max` above the floor (walls, furniture, not the floor or the ceiling), free when the
//! floor itself was seen there, unknown otherwise. The same -1 / 0 / 100 cells as nav_msgs/OccupancyGrid.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FloorPlan {
    /// the floor's height (map frame)
    pub z: f32,
    pub resolution: f32,
    /// world x, y of cell (0, 0)'s corner
    pub origin: [f32; 2],
    pub width: u32,
    pub height: u32,
    /// row-major from the origin: -1 unknown, 0 free, 100 occupied
    pub cells: Vec<i8>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanOptions {
    pub resolution: f32,
    pub obstacle_min: f32,
    pub obstacle_max: f32,
    /// how far below / above the floor height a point still counts as floor
    pub floor_band: f32,
}

impl Default for PlanOptions {
    fn default() -> Self {
        PlanOptions { resolution: 0.05, obstacle_min: 0.15, obstacle_max: 1.8, floor_band: 0.12 }
    }
}

/// `next_floor` caps the obstacle band (a lower storey doesn't see the upper storey's floor as an obstacle).
pub fn rasterize(points: &[[f32; 3]], z: f32, next_floor: Option<f32>, options: &PlanOptions) -> FloorPlan {
    let top = next_floor.map_or(z + options.obstacle_max, |next| (z + options.obstacle_max).min(next - 0.1));
    let relevant: Vec<&[f32; 3]> = points.iter().filter(|p| p[2] >= z - options.floor_band && p[2] <= top).collect();
    let r = options.resolution;
    if relevant.is_empty() {
        return FloorPlan { z, resolution: r, origin: [0.0, 0.0], width: 0, height: 0, cells: Vec::new() };
    }
    let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for p in &relevant {
        x0 = x0.min(p[0]);
        y0 = y0.min(p[1]);
        x1 = x1.max(p[0]);
        y1 = y1.max(p[1]);
    }
    let origin = [(x0 / r).floor() * r - r, (y0 / r).floor() * r - r];
    let width = ((x1 - origin[0]) / r).ceil() as u32 + 2;
    let height = ((y1 - origin[1]) / r).ceil() as u32 + 2;
    let mut cells = vec![-1i8; (width * height) as usize];
    let cell = |p: &[f32; 3]| -> usize {
        let cx = ((p[0] - origin[0]) / r) as u32;
        let cy = ((p[1] - origin[1]) / r) as u32;
        (cy.min(height - 1) * width + cx.min(width - 1)) as usize
    };
    for p in &relevant {
        if (p[2] - z).abs() <= options.floor_band && cells[cell(p)] == -1 {
            cells[cell(p)] = 0;
        }
    }
    for p in &relevant {
        let above = p[2] - z;
        if above >= options.obstacle_min && p[2] <= top {
            cells[cell(p)] = 100;
        }
    }
    FloorPlan { z, resolution: r, origin, width, height, cells }
}

impl FloorPlan {
    /// An 8-bit grey PNG (top row = +y): free white, occupied black, unknown mid-grey.
    pub fn png(&self) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut out, self.width.max(1), self.height.max(1));
            encoder.set_color(png::ColorType::Grayscale);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().expect("png header");
            let mut pixels = vec![128u8; (self.width.max(1) * self.height.max(1)) as usize];
            for row in 0..self.height {
                for column in 0..self.width {
                    let value = self.cells[(row * self.width + column) as usize];
                    let flipped = (self.height - 1 - row) * self.width + column;
                    pixels[flipped as usize] = match value {
                        0 => 255,
                        100 => 20,
                        _ => 128,
                    };
                }
            }
            writer.write_image_data(&pixels).expect("png data");
        }
        out
    }

    pub fn counts(&self) -> (usize, usize, usize) {
        let free = self.cells.iter().filter(|c| **c == 0).count();
        let occupied = self.cells.iter().filter(|c| **c == 100).count();
        (free, occupied, self.cells.len() - free - occupied)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn walls_occupied_floor_free() {
        let mut points = Vec::new();
        for i in 0..40 {
            for j in 0..40 {
                points.push([i as f32 * 0.05, j as f32 * 0.05, 0.0]);
            }
            for k in 0..30 {
                points.push([i as f32 * 0.05, 0.0, 0.2 + k as f32 * 0.05]); // a wall along y = 0
            }
            points.push([i as f32 * 0.05, 1.0, 2.6]); // the ceiling: not an obstacle
        }
        let plan = rasterize(&points, 0.0, None, &PlanOptions::default());
        let (free, occupied, _) = plan.counts();
        assert!((30..=45).contains(&occupied), "{occupied}");
        assert!(free > 1000, "{free}");
        assert_eq!(&plan.png()[1..4], b"PNG");
    }
}
