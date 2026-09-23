use delog_api::params::{ParamKind, ParamSpec, ParamStore, ParamValue};

fn slider(max: f64) -> ParamSpec {
    ParamSpec {
        name: "gain".into(),
        label: "Gain".into(),
        kind: ParamKind::Slider {
            min: 0.0,
            max,
            step: None,
            integer: false,
        },
        default: ParamValue::Float(2.0),
        order: 0,
        generation: 0,
    }
}

#[test]
fn a_persisted_slider_value_is_clamped_when_the_range_tightens() {
    let mut store = ParamStore::default();
    store.declare("flight", 1, slider(10.0)).unwrap();
    store.set_value("flight", "gain", ParamValue::Float(9.0));
    assert_eq!(
        store.declare("flight", 2, slider(5.0)).unwrap(),
        ParamValue::Float(5.0)
    );
}
