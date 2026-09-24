#![cfg(feature = "python")]

use std::sync::{Arc, Mutex};

use delog_api::control::{
    ControlHost, ControlRequest, ControlResponse, LayoutFieldIssue, LayoutRequest, LoadReport,
};

#[derive(Default)]
struct LayoutHost {
    seen: Mutex<Vec<ControlRequest>>,
}

impl LayoutHost {
    fn taken(&self) -> Vec<ControlRequest> {
        std::mem::take(&mut *self.seen.lock().unwrap())
    }
}

impl ControlHost for LayoutHost {
    fn call(&self, request: ControlRequest) -> delog_api::Result<ControlResponse> {
        self.seen.lock().unwrap().push(request.clone());
        match request {
            ControlRequest::Layouts(LayoutRequest::List) => Ok(ControlResponse::Names(vec![
                "analysis".into(),
                "cruise".into(),
            ])),
            ControlRequest::Layouts(LayoutRequest::Current) => Ok(ControlResponse::Layout(
                r#"{"delog_layout":2,"workspace":{}}"#.into(),
            )),
            ControlRequest::Layouts(
                LayoutRequest::Load { .. }
                | LayoutRequest::ImportFile { .. }
                | LayoutRequest::Apply { .. },
            ) => Ok(ControlResponse::LoadReport(LoadReport {
                ambiguous: vec![LayoutFieldIssue {
                    field: "ATT.roll".into(),
                    candidates: vec!["flight-a".into(), "flight-b".into()],
                }],
                unresolved: vec!["GPS.alt".into()],
                warnings: vec!["annotation skipped".into()],
            })),
            ControlRequest::Layouts(_) => Ok(ControlResponse::Unit),
            other => Err(delog_api::Error::execution(format!(
                "unexpected request: {other:?}"
            ))),
        }
    }
}

#[test]
fn layout_library_round_trips_names_documents_and_reports() {
    let host = Arc::new(LayoutHost::default());
    delog_script::control::testing::eval_with_host(
        host.clone(),
        r#"
assert delog.layouts.list() == ["analysis", "cruise"]
doc = delog.layouts.current()
assert isinstance(doc, dict)
assert doc["delog_layout"] == 2
delog.layouts.save(" analysis ")
loaded = delog.layouts.load("analysis")
assert loaded.ambiguous == [{"field": "ATT.roll", "candidates": ["flight-a", "flight-b"]}]
assert loaded.unresolved == ["GPS.alt"]
assert loaded.warnings == ["annotation skipped"]
delog.layouts.delete("analysis")
delog.layouts.rename("analysis", "cruise")
delog.layouts.duplicate("cruise", "copy-1")
imported = delog.layouts.import_file("/tmp/in.json")
delog.layouts.export_file("cruise", "/tmp/out.json")
delog.layouts.clear()
applied = delog.layouts.apply(doc)
assert applied.unresolved == ["GPS.alt"]
"#,
    )
    .unwrap();

    assert_eq!(
        host.taken(),
        vec![
            ControlRequest::Layouts(LayoutRequest::List),
            ControlRequest::Layouts(LayoutRequest::Current),
            ControlRequest::Layouts(LayoutRequest::Save {
                name: "analysis".into(),
            }),
            ControlRequest::Layouts(LayoutRequest::Load {
                name: "analysis".into(),
            }),
            ControlRequest::Layouts(LayoutRequest::Delete {
                name: "analysis".into(),
            }),
            ControlRequest::Layouts(LayoutRequest::Rename {
                from: "analysis".into(),
                to: "cruise".into(),
            }),
            ControlRequest::Layouts(LayoutRequest::Duplicate {
                from: "cruise".into(),
                to: "copy-1".into(),
            }),
            ControlRequest::Layouts(LayoutRequest::ImportFile {
                path: "/tmp/in.json".into(),
            }),
            ControlRequest::Layouts(LayoutRequest::ExportFile {
                name: "cruise".into(),
                path: "/tmp/out.json".into(),
            }),
            ControlRequest::Layouts(LayoutRequest::Clear),
            ControlRequest::Layouts(LayoutRequest::Apply {
                json: r#"{"delog_layout":2,"workspace":{}}"#.into(),
            }),
        ]
    );
}

#[test]
fn invalid_layout_arguments_fail_before_contacting_the_host() {
    for statement in [
        "delog.layouts.save('')",
        "delog.layouts.save('unsafe name')",
        "delog.layouts.rename('ok', '../bad')",
        "delog.layouts.import_file('   ')",
        "delog.layouts.export_file('ok', '')",
        "delog.layouts.apply({'x': float('nan')})",
        "d = {}; d['d'] = d; delog.layouts.apply(d)",
    ] {
        let host = Arc::new(LayoutHost::default());
        let error =
            delog_script::control::testing::eval_with_host(host.clone(), statement).unwrap_err();
        assert!(error.contains("ValueError"), "{statement}: {error}");
        assert!(host.taken().is_empty(), "host called for {statement}");
    }
}

#[test]
fn layout_operations_are_rejected_inside_batches() {
    for statement in [
        "delog.layouts.list()",
        "delog.layouts.save('ok')",
        "delog.layouts.load('ok')",
        "delog.layouts.delete('ok')",
        "delog.layouts.rename('ok', 'new')",
        "delog.layouts.duplicate('ok', 'copy')",
        "delog.layouts.import_file('/tmp/in.json')",
        "delog.layouts.export_file('ok', '/tmp/out.json')",
        "delog.layouts.clear()",
        "delog.layouts.current()",
        "delog.layouts.apply({})",
    ] {
        let host = Arc::new(LayoutHost::default());
        let source = format!("with delog.batch():\n    {statement}");
        let error = delog_script::control::testing::eval_with_host_and_staged_batches(
            host.clone(),
            &source,
        )
        .unwrap_err();
        assert!(
            error.contains("ValueError") && error.contains("batch"),
            "{error}"
        );
        assert!(host.taken().is_empty());
    }
}

#[test]
fn layouts_need_an_active_app_host() {
    let error =
        delog_script::control::testing::eval_without_host("delog.layouts.list()").unwrap_err();
    assert!(
        error.contains("RuntimeError") && error.contains("not available"),
        "{error}"
    );
}
