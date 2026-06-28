//! A procedural cone is the unconditional fallback so a missing/broken asset
//! can never blank the scene.

use delog_render::{MeshCpu, load_glb};

use crate::vehicle::ModelKind;

const QUAD_GLB: &[u8] = include_bytes!("../../../assets/models/QuadCopter.glb");
const FIXEDWING_GLB: &[u8] = include_bytes!("../../../assets/models/FixedWing.glb");
const DELTAWING_GLB: &[u8] = include_bytes!("../../../assets/models/DeltaWing.glb");

pub fn cone_mesh() -> MeshCpu {
    MeshCpu::cone(4, 0.5, 1.4)
}

pub fn mesh_for(kind: &ModelKind) -> MeshCpu {
    let bytes: &[u8] = match kind {
        ModelKind::Quad => QUAD_GLB,
        ModelKind::FixedWing => FIXEDWING_GLB,
        ModelKind::DeltaWing => DELTAWING_GLB,
        ModelKind::Cone => return cone_mesh(),
        ModelKind::CustomGlb(path) => {
            return std::fs::read(path)
                .ok()
                .and_then(|b| load_glb(&b).ok())
                .unwrap_or_else(cone_mesh);
        }
    };
    let mesh = load_glb(bytes).unwrap_or_else(|_| cone_mesh());
    match kind {
        ModelKind::FixedWing | ModelKind::DeltaWing => center_mesh_axis(mesh, 1),
        _ => mesh,
    }
}

fn center_mesh_axis(mut mesh: MeshCpu, axis: usize) -> MeshCpu {
    let mut min = f32::INFINITY;
    let mut max = f32::NEG_INFINITY;
    for vertex in &mesh.vertices {
        min = min.min(vertex.pos[axis]);
        max = max.max(vertex.pos[axis]);
    }
    let center = (min + max) * 0.5;
    if center.is_finite() {
        for vertex in &mut mesh.vertices {
            vertex.pos[axis] -= center;
        }
    }
    mesh
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_models_decode_to_non_empty_meshes() {
        for kind in [ModelKind::Quad, ModelKind::FixedWing, ModelKind::DeltaWing] {
            let mesh = mesh_for(&kind);
            assert!(
                !mesh.vertices.is_empty() && !mesh.indices.is_empty(),
                "{} should decode to geometry",
                kind.label()
            );
            assert!(
                mesh.indices
                    .iter()
                    .all(|&i| (i as usize) < mesh.vertices.len()),
                "{} has out-of-range indices",
                kind.label()
            );
        }
    }

    #[test]
    fn cone_kind_and_bad_custom_path_fall_back_to_the_cone() {
        let cone = mesh_for(&ModelKind::Cone);
        assert!(!cone.vertices.is_empty());
        let missing = mesh_for(&ModelKind::CustomGlb("/no/such/file.glb".into()));
        assert_eq!(missing.vertices.len(), cone.vertices.len());
    }

    fn body_z_bounds_center(kind: ModelKind) -> f32 {
        let mesh = mesh_for(&kind);
        let rot = kind.orientation_offset();
        let mut min_z = f32::INFINITY;
        let mut max_z = f32::NEG_INFINITY;
        for vertex in &mesh.vertices {
            let p = rot * glam::Vec3::from_array(vertex.pos);
            min_z = min_z.min(p.z);
            max_z = max_z.max(p.z);
        }
        (min_z + max_z) * 0.5
    }

    #[test]
    fn embedded_wing_models_have_no_body_z_offset_after_orientation_fix() {
        for kind in [ModelKind::FixedWing, ModelKind::DeltaWing] {
            let z = body_z_bounds_center(kind.clone());
            assert!(
                z.abs() < 1e-5,
                "{} body-Z bounds should be centered on the vehicle origin, got {z}",
                kind.label()
            );
        }
    }
}
