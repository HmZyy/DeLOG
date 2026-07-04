//! Persistence for script param values: a single JSON file mapping
//! `{ script_name: { param_name: value } }`. Only values persist; specs
//! always come from live declarations.

use std::collections::HashMap;
use std::path::Path;

use delog_script::params::{ParamStore, ParamValue};

pub type Loaded = HashMap<String, HashMap<String, ParamValue>>;

/// Load persisted values. Missing or unparsable file -> empty (never errors,
/// so a corrupt file can't wedge startup).
pub fn load(path: &Path) -> Loaded {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
        Err(_) => Loaded::default(),
    }
}

/// Seed a store's `values` from loaded data (before any script runs, so the
/// first declaration keeps these instead of the script default).
pub fn apply_loaded(store: &mut ParamStore, loaded: Loaded) {
    for (script, values) in loaded {
        let sp = store.scripts.entry(script).or_default();
        for (name, value) in values {
            sp.values.insert(name, value);
        }
    }
}

/// Serialize every script's current values to `path`.
pub fn save(path: &Path, store: &ParamStore) -> std::io::Result<()> {
    let mut out: Loaded = HashMap::new();
    for (script, sp) in &store.scripts {
        if !sp.values.is_empty() {
            out.insert(script.clone(), sp.values.clone());
        }
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_vec_pretty(&out).expect("param values serialize");
    std::fs::write(path, json)
}

/// A committed edit re-runs only a snapshot-producing, named (library) script.
pub fn should_rerun(has_snapshot: bool, script_is_named: bool) -> bool {
    has_snapshot && script_is_named
}

#[cfg(test)]
mod tests {
    use super::*;
    use delog_script::params::{ParamKind, ParamSpec};

    fn tmp(name: &str) -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("delog_params_test_{}_{}.json", name, std::process::id()));
        p
    }

    #[test]
    fn save_then_load_roundtrips_values() {
        let mut store = ParamStore::default();
        store.set_value("foo", "gain", ParamValue::Float(3.5));
        store.set_value("foo", "smooth", ParamValue::Bool(true));
        store.set_value("bar", "label", ParamValue::Text("speed".into()));
        let path = tmp("roundtrip");
        save(&path, &store).unwrap();

        let loaded = load(&path);
        assert_eq!(loaded["foo"]["gain"], ParamValue::Float(3.5));
        assert_eq!(loaded["foo"]["smooth"], ParamValue::Bool(true));
        assert_eq!(loaded["bar"]["label"], ParamValue::Text("speed".into()));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn load_missing_file_is_empty() {
        let loaded = load(std::path::Path::new("/no/such/delog/params.json"));
        assert!(loaded.is_empty());
    }

    #[test]
    fn apply_loaded_then_declare_keeps_persisted_value() {
        let mut store = ParamStore::default();
        let mut loaded = Loaded::default();
        loaded.entry("foo".into()).or_default().insert("gain".into(), ParamValue::Float(6.0));
        apply_loaded(&mut store, loaded);
        // Declaration seeds default 1.0 but must keep the persisted 6.0.
        let spec = ParamSpec {
            name: "gain".into(), label: "gain".into(),
            kind: ParamKind::Slider { min: 0.0, max: 10.0, step: None, integer: false },
            default: ParamValue::Float(1.0), order: 0, generation: 0,
        };
        let v = store.declare("foo", 1, spec).unwrap();
        assert_eq!(v, ParamValue::Float(6.0));
    }

    #[test]
    fn should_rerun_only_for_named_snapshot_scripts() {
        assert!(should_rerun(true, true));
        assert!(!should_rerun(true, false)); // scratch / unsaved
        assert!(!should_rerun(false, true)); // pure live transform
    }
}
