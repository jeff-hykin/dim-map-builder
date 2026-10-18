//! The Map Editor's map math, all native: the ray-traced voxel map (dimos's crate), a recorded tf tree, loop
//! closure (a port of dimos's PGO), the cleanup selections, floors and floor plans, and the staged build pipeline.
pub mod apriltag;
pub mod aruco;
mod aruco_dictionaries;
pub mod build;
pub mod clock;
pub mod correct;
pub mod edit;
pub mod floor;
pub mod floorplan;
pub mod frames;
pub mod global;
pub mod graph;
pub mod icp;
pub mod pgo;
pub mod pipeline;
pub mod rrd;
pub mod tags;
pub mod tf;
pub mod voxels;
pub mod wall;
