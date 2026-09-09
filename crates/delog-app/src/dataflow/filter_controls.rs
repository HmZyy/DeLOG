use delog_flow::filter::FilterSpec;

/// Shared by the node body and inspector so comparison edits behave identically.
pub(super) fn filter_controls(ui: &mut egui::Ui, spec: &mut FilterSpec) -> bool {
    let mut changed = false;
    if spec.filter.supports_inclusive() {
        changed |= ui.checkbox(&mut spec.inclusive, "Inclusive").changed();
    }
    if spec.filter.is_range() {
        changed |= ui
            .add(egui::DragValue::new(&mut spec.value).prefix("Min "))
            .changed();
        changed |= ui
            .add(egui::DragValue::new(&mut spec.upper).prefix("Max "))
            .changed();
    } else {
        // Read the symbol after rendering the checkbox to reflect a click this frame.
        let prefix = format!("x {} ", spec.symbol());
        changed |= ui
            .add(egui::DragValue::new(&mut spec.value).prefix(prefix))
            .changed();
    }
    if let Err(message) = spec.validate() {
        ui.colored_label(ui.visuals().error_fg_color, message);
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    use delog_flow::filter::FilterKind;

    fn frame(
        ctx: &egui::Context,
        spec: &mut FilterSpec,
        events: Vec<egui::Event>,
    ) -> (bool, egui::Pos2, Vec<String>) {
        let mut changed = false;
        let mut first_control = egui::Pos2::ZERO;
        let output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(400.0, 300.0),
                )),
                events,
                ..Default::default()
            },
            |ui| {
                first_control = ui.next_widget_position() + egui::vec2(6.0, 8.0);
                changed = filter_controls(ui, spec);
            },
        );
        let labels = output
            .shapes
            .into_iter()
            .filter_map(|shape| match shape.shape {
                egui::epaint::Shape::Text(text) => Some(text.galley.text().to_owned()),
                _ => None,
            })
            .collect();
        (changed, first_control, labels)
    }

    #[test]
    fn clicking_inclusive_updates_symbol_and_keeps_threshold_visible_in_same_frame() {
        for kind in [FilterKind::LessThan, FilterKind::GreaterThan] {
            let ctx = egui::Context::default();
            let mut spec = FilterSpec::new(kind);
            let (_, pos, _) = frame(&ctx, &mut spec, vec![]);
            for inclusive in [true, false] {
                frame(
                    &ctx,
                    &mut spec,
                    vec![
                        egui::Event::PointerMoved(pos),
                        egui::Event::PointerButton {
                            pos,
                            button: egui::PointerButton::Primary,
                            pressed: true,
                            modifiers: egui::Modifiers::NONE,
                        },
                    ],
                );
                let (changed, _, labels) = frame(
                    &ctx,
                    &mut spec,
                    vec![egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed: false,
                        modifiers: egui::Modifiers::NONE,
                    }],
                );
                assert!(changed);
                assert_eq!(spec.inclusive, inclusive);
                assert!(
                    labels
                        .iter()
                        .any(|text| text.contains(&format!("x {}", spec.symbol()))),
                    "{labels:?}"
                );
                assert_eq!(spec.value, 0.0);
            }
        }
    }

    #[test]
    fn only_comparisons_show_inclusive_and_invalid_ranges_show_diagnostics() {
        for kind in FilterKind::ALL {
            let ctx = egui::Context::default();
            let mut spec = FilterSpec::new(kind);
            frame(&ctx, &mut spec, vec![]);
            let (changed, _, labels) = frame(&ctx, &mut spec, vec![]);
            assert!(!changed);
            assert_eq!(
                labels.iter().any(|text| text == "Inclusive"),
                kind.supports_inclusive()
            );
            if kind.is_range() {
                assert!(labels.iter().any(|text| text.contains("Min")));
                assert!(labels.iter().any(|text| text.contains("Max")));
                spec.value = 2.0;
                let (_, _, labels) = frame(&ctx, &mut spec, vec![]);
                assert!(
                    labels
                        .iter()
                        .any(|text| text.contains("Filter minimum must"))
                );
            }
        }
    }
}
