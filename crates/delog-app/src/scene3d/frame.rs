use glam::{DMat3, DMat4, DVec3};

use super::geo;

#[derive(Debug, Default)]
pub(crate) struct SceneFrame {
    pub reference: Option<[f64; 3]>,
}

impl SceneFrame {
    pub fn update(&mut self, references: &[Option<[f64; 3]>]) {
        self.reference = self.reference.filter(valid_reference);
        if self.reference.is_none() || !references.contains(&self.reference) {
            self.reference = references.iter().flatten().copied().find(valid_reference);
        }
    }

    pub fn transform(&self, reference: Option<[f64; 3]>) -> DMat4 {
        let (Some(source), Some(target)) = (
            reference.filter(valid_reference),
            self.reference.filter(valid_reference),
        ) else {
            return DMat4::IDENTITY;
        };
        if source == target {
            return DMat4::IDENTITY;
        }
        let basis = render_to_ecef(target).transpose();
        let rotation = basis * render_to_ecef(source);
        let translation = basis
            * (geo::geodetic_to_ecef(source[0], source[1], source[2])
                - geo::geodetic_to_ecef(target[0], target[1], target[2]));
        DMat4::from_cols(
            rotation.x_axis.extend(0.0),
            rotation.y_axis.extend(0.0),
            rotation.z_axis.extend(0.0),
            translation.extend(1.0),
        )
    }
}

fn valid_reference(reference: &[f64; 3]) -> bool {
    reference.iter().all(|value| value.is_finite())
}

fn render_to_ecef(reference: [f64; 3]) -> DMat3 {
    let (sl, cl) = reference[0].sin_cos();
    let (so, co) = reference[1].sin_cos();
    DMat3::from_cols(
        DVec3::new(-so, co, 0.0),
        DVec3::new(cl * co, cl * so, sl),
        DVec3::new(sl * co, sl * so, -cl),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_frame_retains_its_origin_when_references_are_reordered() {
        let first = Some([0.5, 0.2, 100.0]);
        let second = Some([0.6, 0.3, 200.0]);
        let mut frame = SceneFrame::default();
        frame.update(&[None, first, second]);
        assert_eq!(frame.reference, first);
        frame.update(&[second, first]);
        assert_eq!(frame.reference, first);
        frame.update(&[second]);
        assert_eq!(frame.reference, second);
        frame.update(&[]);
        assert_eq!(frame.reference, None);
    }

    #[test]
    fn shared_frame_translates_different_altitude_origins() {
        let frame = SceneFrame {
            reference: Some([0.0, 0.0, 100.0]),
        };
        let transform = frame.transform(Some([0.0, 0.0, 150.0]));
        let pos = transform.transform_point3(DVec3::new(10.0, -20.0, 30.0));
        assert!((pos - DVec3::new(10.0, 30.0, 30.0)).length() < 1e-8);
    }

    #[test]
    fn shared_frame_rotates_local_axes_between_origins() {
        let frame = SceneFrame {
            reference: Some([0.0, 0.0, 0.0]),
        };
        let transform = frame.transform(Some([0.0, std::f64::consts::FRAC_PI_2, 0.0]));
        assert!((transform.transform_vector3(DVec3::Y) - DVec3::X).length() < 1e-12);
        assert!((transform.transform_vector3(DVec3::X) + DVec3::Y).length() < 1e-12);
        assert!((transform.transform_vector3(DVec3::Z) - DVec3::Z).length() < 1e-12);
        assert!((transform.determinant() - 1.0).abs() < 1e-12);
    }

    #[test]
    fn shared_frame_preserves_unreferenced_local_coordinates() {
        let frame = SceneFrame {
            reference: Some([0.5, 0.2, 100.0]),
        };
        assert_eq!(frame.transform(None), DMat4::IDENTITY);
        assert_eq!(frame.transform(frame.reference), DMat4::IDENTITY);
        assert_eq!(
            SceneFrame::default().transform(frame.reference),
            DMat4::IDENTITY
        );
    }

    #[test]
    fn shared_frame_skips_nonfinite_origins() {
        let valid = Some([0.5, 0.2, 100.0]);
        for invalid in [
            Some([f64::NAN, 0.2, 100.0]),
            Some([0.5, f64::INFINITY, 100.0]),
            Some([0.5, 0.2, f64::NEG_INFINITY]),
        ] {
            let mut frame = SceneFrame { reference: invalid };
            frame.update(&[invalid, valid]);
            assert_eq!(frame.reference, valid);
            assert_eq!(frame.transform(invalid), DMat4::IDENTITY);
            frame.update(&[invalid]);
            assert_eq!(frame.reference, None);
        }
    }
}
