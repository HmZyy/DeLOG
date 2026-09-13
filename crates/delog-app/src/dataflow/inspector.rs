use super::editor::DataFlowEditor;
#[cfg(feature = "scripting")]
use super::inspector_tables::wide_button;
use super::inspector_tables::{
    PortEdit, combo, errors, number, ports, preview, properties, section, text_edit, text_value,
};
use crate::ui::logging::LogLevel;
use delog_core::align::AlignMode;
use delog_core::snapshot::StoreSnapshot;
use delog_flow::command::GraphCommand;
use delog_flow::graph::{NodeId, NodeKind, OutputFieldSpec};
#[cfg(feature = "scripting")]
use delog_flow::script::{ScriptInputSpec, ScriptOutputSpec};
#[cfg(feature = "scripting")]
use egui_code_editor::{CodeEditor, ColorTheme, Syntax};
use std::sync::Arc;

const FILTER_HINT: &str = "Matching values are kept; rejected values become gaps.";
const RANGE_HINT: &str = "Between includes both bounds. Outside Range excludes both bounds.";

#[cfg(feature = "scripting")]
pub(super) struct ScriptEditorState {
    pub(super) node: NodeId,
    pub(super) baseline: String,
    pub(super) buffer: String,
}

impl DataFlowEditor {
    pub(super) fn inspector(
        &mut self,
        ui: &mut egui::Ui,
        snapshot: &Arc<StoreSnapshot>,
        logs: &mut Vec<(LogLevel, String)>,
    ) {
        let Some(id) = self.controller.sole_selection() else {
            let count = self.controller.selection.len();
            if count > 1 {
                ui.weak(format!("{count} nodes selected"));
            } else {
                ui.weak("Select a node to inspect it");
            }
            return;
        };
        ui.push_id(("node-inspector", self.id, id.0), |ui| {
            self.inspect_node(ui, id, snapshot, logs);
        });
    }

    fn inspect_node(
        &mut self,
        ui: &mut egui::Ui,
        id: NodeId,
        snapshot: &Arc<StoreSnapshot>,
        logs: &mut Vec<(LogLevel, String)>,
    ) {
        let Some(node) = self.controller.graph.node(id) else {
            return;
        };
        let original = node.kind.clone();
        let mut edited = original.clone();
        let mut source_choice = None;
        let mut structural_edit = None;

        section(ui, "Properties");
        properties(ui, "node-properties", |rows| {
            rows.property("Type", |ui| text_value(ui, type_label(&original)));
            match &mut edited {
                NodeKind::DataField(selector) => {
                    rows.property("Source", |ui| {
                        let candidates =
                            delog_flow::resolve::candidate_source_labels(snapshot, selector);
                        if candidates.len() > 1 {
                            let mut chosen = selector.source.clone();
                            let selected =
                                chosen.clone().unwrap_or_else(|| "Choose source".to_owned());
                            combo(ui, "source", &selected, |ui| {
                                for label in candidates {
                                    ui.selectable_value(&mut chosen, Some(label.clone()), label);
                                }
                            });
                            if chosen != selector.source {
                                source_choice = Some(chosen);
                            }
                        } else {
                            text_value(
                                ui,
                                selector
                                    .source
                                    .as_deref()
                                    .or_else(|| candidates.first().map(String::as_str))
                                    .unwrap_or("Automatic"),
                            );
                        }
                    });
                    rows.property("Topic", |ui| text_value(ui, &selector.topic));
                    if let Some(instance) = selector.instance {
                        rows.property("Instance", |ui| text_value(ui, instance.to_string()));
                    }
                    rows.property("Field", |ui| text_value(ui, &selector.field));
                }
                NodeKind::Constant { value } => {
                    rows.property("Value", |ui| number(ui, value));
                }
                NodeKind::ScaleOffset { multiplier, offset } => {
                    rows.property("Multiplier", |ui| number(ui, multiplier));
                    rows.property("Offset", |ui| number(ui, offset));
                }
                NodeKind::Filter(spec) => {
                    rows.hinted("Condition", FILTER_HINT, |ui| {
                        combo(ui, "filter-condition", spec.filter.label(), |ui| {
                            for option in delog_flow::filter::FilterKind::ALL {
                                ui.selectable_value(&mut spec.filter, option, option.label());
                            }
                        });
                    });
                    if spec.filter.supports_inclusive() {
                        rows.hinted(
                            "Inclusive",
                            "Keep values that sit exactly on the bound.",
                            |ui| {
                                ui.checkbox(&mut spec.inclusive, "");
                            },
                        );
                    }
                    if spec.filter.is_range() {
                        rows.hinted("Minimum", RANGE_HINT, |ui| number(ui, &mut spec.value));
                        rows.hinted("Maximum", RANGE_HINT, |ui| number(ui, &mut spec.upper));
                    } else {
                        rows.hinted("Threshold", FILTER_HINT, |ui| number(ui, &mut spec.value));
                    }
                }
                NodeKind::Convert { kind } => {
                    rows.property("Conversion", |ui| {
                        combo(ui, "conversion", kind.label(), |ui| {
                            for option in delog_flow::graph::ConversionKind::ALL {
                                ui.selectable_value(kind, option, option.label());
                            }
                        });
                    });
                }
                NodeKind::Align { mode } => {
                    rows.hinted(
                        "Mode",
                        "How samples are resampled onto the reference timeline.",
                        |ui| {
                            combo(ui, "align-mode", mode.as_str(), |ui| {
                                ui.selectable_value(mode, AlignMode::Prev, "prev");
                                ui.selectable_value(mode, AlignMode::Nearest, "nearest");
                                ui.selectable_value(mode, AlignMode::Linear, "linear");
                            });
                        },
                    );
                }
                NodeKind::Output(spec) => {
                    rows.property("Topic", |ui| {
                        text_edit(ui, &mut spec.topic, "Topic");
                    });
                }
                #[cfg(feature = "scripting")]
                NodeKind::Script(spec) => {
                    rows.property("Name", |ui| {
                        text_edit(ui, &mut spec.name, "Name");
                    });
                }
                NodeKind::Add
                | NodeKind::Subtract
                | NodeKind::Multiply
                | NodeKind::Divide
                | NodeKind::Unknown(_) => {}
            }
        });

        match &mut edited {
            NodeKind::Output(spec) => {
                section(ui, "Fields");
                structural_edit = match ports(
                    ui,
                    "output-fields",
                    "field",
                    true,
                    "Add field",
                    spec.fields
                        .iter_mut()
                        .map(|field| (&mut field.name, Some(&mut field.unit))),
                ) {
                    Some(PortEdit::Remove(index)) => {
                        Some(GraphCommand::RemoveOutputField { id, index })
                    }
                    Some(PortEdit::Add) => Some(GraphCommand::InsertOutputField {
                        id,
                        index: spec.fields.len(),
                        field: OutputFieldSpec {
                            name: format!("field_{}", spec.fields.len() + 1),
                            unit: None,
                        },
                        connection: None,
                    }),
                    None => None,
                };
            }
            #[cfg(feature = "scripting")]
            NodeKind::Script(spec) => {
                section(ui, "Inputs");
                structural_edit = match ports(
                    ui,
                    "script-inputs",
                    "input",
                    false,
                    "Add input",
                    spec.inputs.iter_mut().map(|input| (&mut input.name, None)),
                ) {
                    Some(PortEdit::Remove(index)) => {
                        Some(GraphCommand::RemoveScriptInput { id, index })
                    }
                    Some(PortEdit::Add) => Some(GraphCommand::InsertScriptInput {
                        id,
                        index: spec.inputs.len(),
                        input: ScriptInputSpec {
                            name: format!("in_{}", spec.inputs.len() + 1),
                        },
                        connection: None,
                    }),
                    None => None,
                };

                section(ui, "Outputs");
                structural_edit = match ports(
                    ui,
                    "script-outputs",
                    "output",
                    true,
                    "Add output",
                    spec.outputs
                        .iter_mut()
                        .map(|output| (&mut output.name, Some(&mut output.unit))),
                ) {
                    Some(PortEdit::Remove(index)) => {
                        Some(GraphCommand::RemoveScriptOutput { id, index })
                    }
                    Some(PortEdit::Add) => Some(GraphCommand::InsertScriptOutput {
                        id,
                        index: spec.outputs.len(),
                        output: ScriptOutputSpec {
                            name: format!("out_{}", spec.outputs.len() + 1),
                            unit: None,
                        },
                    }),
                    None => None,
                }
                .or(structural_edit);

                section(ui, "Code");
                let buffer = script_editor_buffer(&mut self.script_editor, id, &spec.code);
                CodeEditor::default()
                    .id_source(format!("dataflow-script-code-{}-{}", self.id, id.0))
                    .with_rows(10)
                    .desired_width(ui.available_width())
                    .with_theme(ColorTheme::GITHUB_DARK)
                    .show(ui, buffer, &Syntax::python());
                ui.add_space(6.0);
                if wide_button(ui, "Apply code") {
                    let mut applied = spec.clone();
                    applied.code = self
                        .script_editor
                        .as_ref()
                        .map_or_else(|| spec.code.clone(), |state| state.buffer.clone());
                    structural_edit = Some(GraphCommand::SetKind {
                        id,
                        kind: NodeKind::Script(applied),
                    });
                }
            }
            _ => {}
        }

        if let Some(choice) = source_choice {
            self.controller.set_field_source(id, choice);
        }
        let invalid = match &edited {
            NodeKind::Filter(spec) => spec.validate().err(),
            _ => None,
        };
        if let Some(command) = structural_edit {
            self.apply(command, logs);
        } else if edited != original {
            self.apply(GraphCommand::SetKind { id, kind: edited }, logs);
        }

        let diagnostics = self.controller.diagnostics_for(id);
        if invalid.is_some() || !diagnostics.is_empty() {
            section(ui, "Diagnostics");
            errors(ui, invalid.as_deref().into_iter().chain(diagnostics));
        }

        #[cfg(feature = "scripting")]
        if let NodeKind::Script(spec) = &original {
            for (index, output) in spec.outputs.iter().enumerate() {
                if let Some(stats) = self.controller.preview_for(id, index) {
                    section(ui, &format!("Preview: {}", output.name));
                    preview(ui, ("script-preview", index), stats);
                }
            }
            return;
        }
        if let Some(stats) = self.controller.preview_for(id, 0) {
            section(ui, "Preview");
            preview(ui, "node-preview", stats);
        }
    }
}

fn type_label(kind: &NodeKind) -> String {
    match kind {
        NodeKind::DataField(_) => "Data field".into(),
        NodeKind::Output(_) => "Derived output".into(),
        #[cfg(feature = "scripting")]
        NodeKind::Script(_) => "Python script".into(),
        _ => kind.label(),
    }
}

#[cfg(feature = "scripting")]
fn script_editor_buffer<'a>(
    state: &'a mut Option<ScriptEditorState>,
    id: NodeId,
    code: &str,
) -> &'a mut String {
    let needs_reset = match state {
        Some(existing) => existing.node != id || existing.baseline != code,
        None => true,
    };
    if needs_reset {
        *state = Some(ScriptEditorState {
            node: id,
            baseline: code.to_owned(),
            buffer: code.to_owned(),
        });
    }
    &mut state.as_mut().expect("just reset if absent").buffer
}

#[cfg(test)]
#[path = "inspector_tests.rs"]
mod tests;
