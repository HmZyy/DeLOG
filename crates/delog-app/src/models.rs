//! Vehicle model registry. The built-in GLBs ship
//! embedded (`include_bytes!`); a procedural cone is the unconditional
//! fallback so a missing/broken asset can never blank the scene.

use delog_render::{MeshCpu, load_glb};

use crate::vehicle::ModelKind;

const QUAD_GLB: &[u8] = include_bytes!("../../../assets/models/QuadCopter.glb");
const FIXEDWING_GLB: &[u8] = include_bytes!("../../../assets/models/FixedWing.glb");
const DELTAWING_GLB: &[u8] = include_bytes!("../../../assets/models/DeltaWing.glb");

/// The procedural cone used for `ModelKind::Cone` and as the fallback when an
/// asset fails to decode.
pub fn cone_mesh() -> MeshCpu {
    MeshCpu::cone(4, 0.5, 1.4)
}

/// Decode the mesh for a model kind, falling back to the cone on any error.
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
    load_glb(bytes).unwrap_or_else(|_| cone_mesh())
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
}
