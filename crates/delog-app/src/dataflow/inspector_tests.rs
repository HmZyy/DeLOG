use super::*;
use delog_flow::graph::{ConversionKind, FieldSelector, Graph, Node, NodeId, OutputSpec};
use egui::accesskit::{Node as AccessibleNode, Role};
use std::collections::HashSet;

fn context() -> egui::Context {
    let ctx = egui::Context::default();
    ctx.enable_accesskit();
    crate::ui::theme::ThemeChoice::CatppuccinMocha.apply(&ctx);
    ctx
}

fn editor(kind: NodeKind) -> DataFlowEditor {
    let mut graph = Graph::new("inspector");
    graph.insert_node(Node {
        id: NodeId(1),
        pos: [0.0, 0.0],
        kind,
    });
    let mut editor = DataFlowEditor::new(1, graph, None, Default::default());
    editor.controller.selection = HashSet::from([NodeId(1)]);
    editor
}

fn render(
    ctx: &egui::Context,
    editor: &mut DataFlowEditor,
    width: f32,
    events: Vec<egui::Event>,
) -> Vec<AccessibleNode> {
    let output = ctx.run_ui(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(width, 1200.0),
            )),
            events,
            ..Default::default()
        },
        |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    egui::Frame::new().inner_margin(12).show(ui, |ui| {
                        editor.inspector(ui, &Arc::new(StoreSnapshot::empty()), &mut Vec::new());
                    });
                });
        },
    );
    output
        .platform_output
        .accesskit_update
        .unwrap()
        .nodes
        .into_iter()
        .map(|(_, node)| node)
        .collect()
}

fn output_kind() -> NodeKind {
    NodeKind::Output(OutputSpec {
        topic: "DERIVED_TELEMETRY".into(),
        fields: vec![
            OutputFieldSpec {
                name: "altitude".into(),
                unit: Some("m".into()),
            },
            OutputFieldSpec {
                name: "speed".into(),
                unit: Some("m/s".into()),
            },
        ],
    })
}

fn assert_field_controls_fit(kind: NodeKind, fields: &[&str], remove_count: usize) {
    let ctx = context();
    let mut editor = editor(kind);
    for width in [360.0, 240.0, 180.0, 300.0, 180.0] {
        for _ in 0..3 {
            let nodes = render(&ctx, &mut editor, width, vec![]);
            for value in fields {
                let input = nodes
                    .iter()
                    .find(|node| node.role() == Role::TextInput && node.value() == Some(*value))
                    .unwrap_or_else(|| panic!("missing input {value} at width {width}"));
                let bounds = input.bounds().unwrap();
                assert!(
                    bounds.x0 >= 0.0 && bounds.x1 <= f64::from(width) && bounds.width() >= 24.0,
                    "input {value} is outside the {width}px inspector: {bounds:?}"
                );
                assert!(
                    bounds.height() >= 24.0,
                    "input {value} is squashed to {bounds:?}"
                );
            }
            let buttons: Vec<_> = nodes
                .iter()
                .filter(|node| {
                    node.role() == Role::Button
                        && node
                            .label()
                            .is_some_and(|label| label.starts_with("Remove"))
                })
                .collect();
            assert_eq!(buttons.len(), remove_count);
            for button in buttons {
                let bounds = button.bounds().unwrap();
                assert!(bounds.x0 >= 0.0 && bounds.x1 <= f64::from(width));
            }
            assert!(!editor.has_unsaved_changes());
        }
    }
}

#[test]
fn output_fields_and_remove_buttons_fit_a_narrow_inspector() {
    assert_field_controls_fit(
        output_kind(),
        &["DERIVED_TELEMETRY", "altitude", "m", "speed", "m/s"],
        2,
    );
}

#[cfg(feature = "scripting")]
#[test]
fn script_fields_and_remove_buttons_fit_a_narrow_inspector() {
    assert_field_controls_fit(
        NodeKind::Script(delog_flow::script::ScriptSpec {
            name: "Flight calculations".into(),
            inputs: vec![ScriptInputSpec {
                name: "signal".into(),
            }],
            outputs: vec![ScriptOutputSpec {
                name: "result".into(),
                unit: Some("deg".into()),
            }],
            code: "def flow(inputs):\n    return {\"result\": inputs.signal.v}\n".into(),
        }),
        &["Flight calculations", "signal", "result", "deg"],
        2,
    );
}

fn click(ctx: &egui::Context, editor: &mut DataFlowEditor, node: &AccessibleNode) {
    let bounds = node.bounds().unwrap();
    let pos = egui::pos2(
        ((bounds.x0 + bounds.x1) / 2.0) as f32,
        ((bounds.y0 + bounds.y1) / 2.0) as f32,
    );
    for pressed in [true, false] {
        render(
            ctx,
            editor,
            240.0,
            vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
    }
}

#[test]
fn removing_an_output_field_and_undoing_restores_its_connection() {
    let ctx = context();
    let mut editor = editor(output_kind());
    editor.controller.graph.insert_node(Node {
        id: NodeId(2),
        pos: [0.0, 0.0],
        kind: NodeKind::Add,
    });
    editor
        .controller
        .graph
        .connect(NodeId(2), 0, NodeId(1), 0)
        .unwrap();
    let original = editor.controller.graph.clone();
    render(&ctx, &mut editor, 240.0, vec![]);
    let nodes = render(&ctx, &mut editor, 240.0, vec![]);
    let button = nodes
        .iter()
        .find(|node| node.label() == Some("Remove field altitude"))
        .unwrap();
    click(&ctx, &mut editor, button);
    let NodeKind::Output(spec) = &editor.controller.graph.node(NodeId(1)).unwrap().kind else {
        panic!("expected an output node");
    };
    assert_eq!(spec.fields.len(), 1);
    assert_eq!(spec.fields[0].name, "speed");
    assert!(editor.controller.graph.edges.is_empty());
    assert!(editor.has_unsaved_changes());
    editor.controller.undo();
    assert_eq!(editor.controller.graph, original);
    assert!(!editor.controller.can_undo());
}

#[test]
fn editing_a_unit_in_the_narrow_table_updates_only_that_field() {
    let ctx = context();
    let mut editor = editor(output_kind());
    render(&ctx, &mut editor, 240.0, vec![]);
    let nodes = render(&ctx, &mut editor, 240.0, vec![]);
    let input = nodes
        .iter()
        .find(|node| node.role() == Role::TextInput && node.value() == Some("m"))
        .unwrap();
    click(&ctx, &mut editor, input);
    render(
        &ctx,
        &mut editor,
        240.0,
        vec![
            egui::Event::Key {
                key: egui::Key::A,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers {
                    ctrl: true,
                    command: true,
                    ..Default::default()
                },
            },
            egui::Event::Text("ft".into()),
        ],
    );
    let NodeKind::Output(spec) = &editor.controller.graph.node(NodeId(1)).unwrap().kind else {
        panic!("expected an output node");
    };
    assert_eq!(spec.fields[0].unit.as_deref(), Some("ft"));
    assert_eq!(spec.fields[1].unit.as_deref(), Some("m/s"));
    assert!(editor.has_unsaved_changes());
}

#[test]
fn property_controls_stay_inside_the_inspector_for_every_node_kind() {
    let mut kinds = vec![
        NodeKind::DataField(FieldSelector {
            source: None,
            topic: "LONG_TELEMETRY_TOPIC_NAME".into(),
            instance: Some(0),
            field: "long_telemetry_field_name".into(),
        }),
        NodeKind::Add,
        NodeKind::Subtract,
        NodeKind::Multiply,
        NodeKind::Divide,
        NodeKind::Constant { value: 42.0 },
        NodeKind::ScaleOffset {
            multiplier: 100000000000.0,
            offset: -0.123456,
        },
        NodeKind::Align {
            mode: AlignMode::Nearest,
        },
        NodeKind::Unknown(serde_json::json!({"type": "future_node"})),
    ];
    kinds.extend(
        ConversionKind::ALL
            .into_iter()
            .map(|kind| NodeKind::Convert { kind }),
    );
    kinds.extend(
        delog_flow::filter::FilterKind::ALL
            .into_iter()
            .map(|kind| NodeKind::Filter(delog_flow::filter::FilterSpec::new(kind))),
    );
    for kind in kinds {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let mut editor = editor(kind.clone());
        for width in [240.0, 180.0, 360.0] {
            let nodes = render(&ctx, &mut editor, width, vec![]);
            assert!(nodes.iter().any(|node| node.value() == Some("Type")));
            for node in nodes.iter().filter(|node| {
                matches!(node.role(), Role::Button | Role::TextInput | Role::ComboBox)
            }) {
                let bounds = node.bounds().unwrap();
                assert!(
                    bounds.x0 >= 0.0 && bounds.x1 <= f64::from(width),
                    "{kind:?} control is outside the {width}px inspector: {bounds:?}"
                );
            }
            assert!(!editor.has_unsaved_changes());
        }
    }
}

#[test]
fn an_invalid_filter_range_reports_the_error_under_a_diagnostics_heading() {
    let ctx = context();
    let mut spec = delog_flow::filter::FilterSpec::new(delog_flow::filter::FilterKind::Between);
    spec.value = 2.0;
    let mut editor = editor(NodeKind::Filter(spec));
    render(&ctx, &mut editor, 320.0, vec![]);
    let nodes = render(&ctx, &mut editor, 320.0, vec![]);
    assert!(nodes.iter().any(|node| node.value() == Some("DIAGNOSTICS")));
    let message = nodes
        .iter()
        .find(|node| {
            node.value()
                .is_some_and(|value| value.starts_with("Filter minimum must be less"))
        })
        .expect("the validation message is shown");
    let bounds = message.bounds().unwrap();
    assert!(bounds.x0 >= 0.0 && bounds.x1 <= 320.0, "{bounds:?}");
}

fn painted(
    ctx: &egui::Context,
    editor: &mut DataFlowEditor,
    width: f32,
) -> (Vec<egui::Rect>, Vec<(f32, f32, f32)>) {
    let output = ctx.run_ui(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(width, 1200.0),
            )),
            ..Default::default()
        },
        |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    egui::Frame::new().inner_margin(12).show(ui, |ui| {
                        editor.inspector(ui, &Arc::new(StoreSnapshot::empty()), &mut Vec::new());
                    });
                });
        },
    );
    let mut cards = Vec::new();
    let mut dividers = Vec::new();
    for shape in output.shapes {
        match shape.shape {
            egui::epaint::Shape::Rect(rect) if rect.stroke.width > 0.0 => cards.push(rect.rect),
            egui::epaint::Shape::LineSegment { points, .. } => {
                dividers.push((points[0].y, points[0].x, points[1].x));
            }
            _ => {}
        }
    }
    (cards, dividers)
}

#[test]
fn tables_are_framed_cards_with_row_dividers_instead_of_zebra_stripes() {
    let ctx = context();
    let mut editor = editor(output_kind());
    painted(&ctx, &mut editor, 291.0);
    let (cards, dividers) = painted(&ctx, &mut editor, 291.0);

    assert_eq!(
        cards.len(),
        2,
        "one outlined card per section, not one band per row"
    );
    for card in &cards {
        assert_eq!((card.left(), card.right()), (12.0, 279.0), "{card:?}");
    }

    assert_eq!(
        dividers.len(),
        4,
        "one between each pair of rows, plus the Fields header and add-field rules: {dividers:?}"
    );
    let spans: Vec<_> = dividers.iter().map(|(_, x0, x1)| (*x0, *x1)).collect();
    assert!(
        spans.windows(2).all(|pair| pair[0] == pair[1]),
        "dividers must share one inset: {spans:?}"
    );
    for (y, x0, x1) in &dividers {
        let card = cards
            .iter()
            .find(|card| card.y_range().contains(*y))
            .unwrap_or_else(|| panic!("divider at {y} is outside every card"));
        assert!(*x0 > card.left() && *x1 < card.right(), "{x0}..{x1}");
        assert!(*y > card.top() && *y < card.bottom(), "{y} in {card:?}");
    }
}
