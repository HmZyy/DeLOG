use super::editor::DataFlowEditor;
use crate::ui::logging::LogLevel;
use delog_core::align::AlignMode;
use delog_core::snapshot::StoreSnapshot;
use delog_flow::command::GraphCommand;
#[cfg(feature = "scripting")]
use delog_flow::graph::NodeId;
use delog_flow::graph::{FieldSelector, NodeKind, OutputFieldSpec};
#[cfg(feature = "scripting")]
use delog_flow::script::{ScriptInputSpec, ScriptOutputSpec};
#[cfg(feature = "scripting")]
use egui_code_editor::{CodeEditor, ColorTheme, Syntax};
use std::sync::Arc;

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
        let Some(node) = self.controller.graph.node(id) else {
            return;
        };
        ui.strong(node.kind.label());
        for diagnostic in self.controller.diagnostics_for(id) {
            ui.colored_label(ui.visuals().error_fg_color, diagnostic);
        }
        ui.separator();
        let mut source_choice = None;
        if let NodeKind::DataField(selector) = &node.kind {
            let candidates = delog_flow::resolve::candidate_source_labels(snapshot, selector);
            if candidates.len() > 1 {
                let mut chosen = selector.source.clone();
                egui::ComboBox::from_label("Source")
                    .selected_text(chosen.as_deref().unwrap_or("(choose)"))
                    .show_ui(ui, |ui| {
                        for label in &candidates {
                            ui.selectable_value(&mut chosen, Some(label.clone()), label);
                        }
                    });
                if chosen != selector.source {
                    source_choice = Some(chosen);
                }
            }
        }
        if let Some(choice) = source_choice {
            self.controller.set_field_source(id, choice);
        }

        let Some(node) = self.controller.graph.node(id) else {
            return;
        };
        let mut edited = node.kind.clone();
        let mut structural_edit = None;
        match &mut edited {
            NodeKind::DataField(selector) => show_selector(ui, selector),
            NodeKind::Constant { value } => {
                ui.horizontal(|ui| {
                    ui.label("Value");
                    ui.add(egui::DragValue::new(value));
                });
            }
            NodeKind::ScaleOffset { multiplier, offset } => {
                ui.horizontal(|ui| {
                    ui.label("Multiplier");
                    ui.add(egui::DragValue::new(multiplier));
                });
                ui.horizontal(|ui| {
                    ui.label("Offset");
                    ui.add(egui::DragValue::new(offset));
                });
            }
            NodeKind::Filter(spec) => {
                egui::ComboBox::from_id_salt(("dataflow-filter-condition", id.0))
                    .selected_text(spec.filter.label())
                    .show_ui(ui, |ui| {
                        for option in delog_flow::filter::FilterKind::ALL {
                            ui.selectable_value(&mut spec.filter, option, option.label());
                        }
                    });
                super::filter_controls::filter_controls(ui, spec);
                ui.label("Matching values are kept; rejected values become gaps.");
                if spec.filter.is_range() {
                    ui.label("Between includes both bounds. Outside Range keeps values strictly outside them.");
                }
            }
            NodeKind::Convert { kind } => {
                egui::ComboBox::from_label("Conversion")
                    .selected_text(kind.label())
                    .show_ui(ui, |ui| {
                        for option in delog_flow::graph::ConversionKind::ALL {
                            ui.selectable_value(kind, option, option.label());
                        }
                    });
            }
            NodeKind::Align { mode } => {
                egui::ComboBox::from_id_salt(("dataflow-align-mode", id.0))
                    .selected_text(mode.as_str())
                    .show_ui(ui, |ui| {
                        ui.selectable_value(mode, AlignMode::Prev, "prev");
                        ui.selectable_value(mode, AlignMode::Nearest, "nearest");
                        ui.selectable_value(mode, AlignMode::Linear, "linear");
                    });
            }
            NodeKind::Output(spec) => {
                ui.horizontal(|ui| {
                    ui.label("Topic");
                    ui.text_edit_singleline(&mut spec.topic);
                });
                let mut remove = None;
                for (index, field) in spec.fields.iter_mut().enumerate() {
                    ui.horizontal(|ui| {
                        ui.text_edit_singleline(&mut field.name);
                        let mut unit = field.unit.clone().unwrap_or_default();
                        if ui
                            .add(egui::TextEdit::singleline(&mut unit).hint_text("unit"))
                            .changed()
                        {
                            field.unit = (!unit.is_empty()).then_some(unit);
                        }
                        if ui.button("Remove").clicked() {
                            remove = Some(index);
                        }
                    });
                }
                if let Some(index) = remove {
                    structural_edit = Some(GraphCommand::RemoveOutputField { id, index });
                }
                if ui.button("Add field").clicked() {
                    structural_edit = Some(GraphCommand::InsertOutputField {
                        id,
                        index: spec.fields.len(),
                        field: OutputFieldSpec {
                            name: format!("field_{}", spec.fields.len() + 1),
                            unit: None,
                        },
                        connection: None,
                    });
                }
            }
            #[cfg(feature = "scripting")]
            NodeKind::Script(spec) => {
                ui.horizontal(|ui| {
                    ui.label("Name");
                    ui.text_edit_singleline(&mut spec.name);
                });

                ui.separator();
                ui.strong("Inputs");
                let mut remove_input = None;
                for (index, input) in spec.inputs.iter_mut().enumerate() {
                    ui.horizontal(|ui| {
                        ui.text_edit_singleline(&mut input.name);
                        if ui.button("Remove").clicked() {
                            remove_input = Some(index);
                        }
                    });
                }
                if let Some(index) = remove_input {
                    structural_edit = Some(GraphCommand::RemoveScriptInput { id, index });
                }
                if ui.button("Add input").clicked() {
                    structural_edit = Some(GraphCommand::InsertScriptInput {
                        id,
                        index: spec.inputs.len(),
                        input: ScriptInputSpec {
                            name: format!("in_{}", spec.inputs.len() + 1),
                        },
                        connection: None,
                    });
                }

                ui.separator();
                ui.strong("Outputs");
                let mut remove_output = None;
                for (index, output) in spec.outputs.iter_mut().enumerate() {
                    ui.horizontal(|ui| {
                        ui.text_edit_singleline(&mut output.name);
                        let mut unit = output.unit.clone().unwrap_or_default();
                        if ui
                            .add(egui::TextEdit::singleline(&mut unit).hint_text("unit"))
                            .changed()
                        {
                            output.unit = (!unit.is_empty()).then_some(unit);
                        }
                        if ui.button("Remove").clicked() {
                            remove_output = Some(index);
                        }
                    });
                }
                if let Some(index) = remove_output {
                    structural_edit = Some(GraphCommand::RemoveScriptOutput { id, index });
                }
                if ui.button("Add output").clicked() {
                    structural_edit = Some(GraphCommand::InsertScriptOutput {
                        id,
                        index: spec.outputs.len(),
                        output: ScriptOutputSpec {
                            name: format!("out_{}", spec.outputs.len() + 1),
                            unit: None,
                        },
                    });
                }

                ui.separator();
                ui.strong("Code");
                let buffer = script_editor_buffer(&mut self.script_editor, id, &spec.code);
                CodeEditor::default()
                    .id_source(format!("dataflow-script-code-{}-{}", self.id, id.0))
                    .with_rows(14)
                    .with_theme(ColorTheme::GITHUB_DARK)
                    .show(ui, buffer, &Syntax::python());

                if ui.button("Apply").clicked() {
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

                ui.separator();
                ui.strong("Preview");
                for (index, output) in spec.outputs.iter().enumerate() {
                    if let Some(preview) = self.controller.preview_for(id, index) {
                        ui.label(output.name.as_str());
                        egui::Grid::new(("dataflow-script-preview", id.0, index))
                            .num_columns(2)
                            .show(ui, |ui| {
                                stat_row(ui, "Count", preview.count);
                                stat_row(ui, "NaN", preview.nan_count);
                                stat_row(ui, "Min", preview.min);
                                stat_row(ui, "Max", preview.max);
                                stat_row(ui, "Mean", preview.mean);
                                stat_row(ui, "Stddev", preview.stddev);
                                stat_row(ui, "Start (us)", preview.t0_us);
                                stat_row(ui, "End (us)", preview.t1_us);
                            });
                    }
                }
            }
            NodeKind::Add
            | NodeKind::Subtract
            | NodeKind::Multiply
            | NodeKind::Divide
            | NodeKind::Unknown(_) => {}
        }
        #[cfg(feature = "scripting")]
        let has_own_preview_section = matches!(node.kind, NodeKind::Script(_));
        #[cfg(not(feature = "scripting"))]
        let has_own_preview_section = false;
        if let Some(command) = structural_edit {
            self.apply(command, logs);
        } else if edited != node.kind {
            self.apply(GraphCommand::SetKind { id, kind: edited }, logs);
        }

        if !has_own_preview_section && let Some(preview) = self.controller.preview_for(id, 0) {
            ui.separator();
            ui.strong("Preview");
            egui::Grid::new(("dataflow-preview", id.0))
                .num_columns(2)
                .show(ui, |ui| {
                    stat_row(ui, "Count", preview.count);
                    stat_row(ui, "NaN", preview.nan_count);
                    stat_row(ui, "Min", preview.min);
                    stat_row(ui, "Max", preview.max);
                    stat_row(ui, "Mean", preview.mean);
                    stat_row(ui, "Stddev", preview.stddev);
                    stat_row(ui, "Start (us)", preview.t0_us);
                    stat_row(ui, "End (us)", preview.t1_us);
                });
        }
    }
}

fn show_selector(ui: &mut egui::Ui, selector: &FieldSelector) {
    ui.label(format!("Topic: {}", selector.topic));
    if let Some(instance) = selector.instance {
        ui.label(format!("Instance: {instance}"));
    }
    ui.label(format!("Field: {}", selector.field));
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

fn stat_row(ui: &mut egui::Ui, label: &str, value: impl std::fmt::Display) {
    ui.label(label);
    ui.label(value.to_string());
    ui.end_row();
}
