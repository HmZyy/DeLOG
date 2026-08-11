use super::*;

fn view() -> PaneView {
    PaneView {
        rect: egui::Rect::from_min_max(egui::pos2(100.0, 50.0), egui::pos2(600.0, 350.0)),
        x_range: (0.0, 10.0),
        y_range: (-5.0, 15.0),
    }
}

fn unit_transform() -> PlotTransform {
    PlotTransform::new(
        PaneView {
            rect: egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(100.0, 100.0)),
            x_range: (0.0, 100.0),
            y_range: (0.0, 100.0),
        },
        0,
    )
}

fn annot(id: u64, geom: Geometry) -> Annotation {
    Annotation {
        id,
        geom,
        label: String::new(),
        style: default_style(id),
    }
}

fn filled(id: u64, geom: Geometry) -> Annotation {
    let mut a = annot(id, geom);
    a.style.fill_opacity = 0.5;
    a
}

fn box_geom() -> Geometry {
    Geometry::Rect {
        a: DataPos {
            t_us: 20_000_000,
            y: 20.0,
        },
        b: DataPos {
            t_us: 80_000_000,
            y: 80.0,
        },
    }
}

fn layer_with_box() -> (AnnotationLayer, u64) {
    let mut layer = AnnotationLayer::default();
    let id = layer.add(
        Kind::Rect,
        DataPos {
            t_us: 50_000_000,
            y: 50.0,
        },
        60_000_000,
        60.0,
    );
    layer.get_mut(id).expect("exists").geom = box_geom();
    (layer, id)
}

mod draw;
mod edit;
mod geometry;
mod hit;
mod interact;
mod place;
