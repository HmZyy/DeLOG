use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

pub type SharedParams = Arc<Mutex<ParamStore>>;

pub fn shared_empty() -> SharedParams {
    Arc::new(Mutex::new(ParamStore::default()))
}

#[derive(Clone, Debug, PartialEq)]
pub enum ParamKind {
    Slider { min: f64, max: f64, step: Option<f64>, integer: bool },
    Checkbox,
    Combo { options: Vec<String> },
    Text,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "lowercase")]
pub enum ParamValue {
    Float(f64),
    Bool(bool),
    Text(String),
}

impl ParamValue {
    pub fn compatible_with(&self, kind: &ParamKind) -> bool {
        matches!(
            (self, kind),
            (ParamValue::Float(_), ParamKind::Slider { .. })
                | (ParamValue::Bool(_), ParamKind::Checkbox)
                | (ParamValue::Text(_), ParamKind::Combo { .. })
                | (ParamValue::Text(_), ParamKind::Text)
        )
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ParamSpec {
    pub name: String,
    pub label: String,
    pub kind: ParamKind,
    pub default: ParamValue,
    pub order: u32,
    pub generation: u64,
}

#[derive(Clone, Debug, Default)]
pub struct ScriptParams {
    pub specs: Vec<ParamSpec>,
    pub values: HashMap<String, ParamValue>,
    pub last_generation: Option<u64>,
    pub has_snapshot: bool,
    pub has_live: bool,
}

#[derive(Clone, Debug, Default)]
pub struct ParamStore {
    pub scripts: HashMap<String, ScriptParams>,
}

impl ParamStore {
    pub fn declare(
        &mut self,
        script: &str,
        generation: u64,
        mut spec: ParamSpec,
    ) -> Result<ParamValue, String> {
        if spec.name.is_empty() {
            return Err("param name must not be empty".into());
        }
        let sp = self.scripts.entry(script.to_string()).or_default();
        // First declaration of a new run clears the prior generation's specs
        // (implicit pruning) while keeping persisted/current values.
        if sp.last_generation != Some(generation) {
            sp.last_generation = Some(generation);
            sp.specs.clear();
        }
        if sp.specs.iter().any(|s| s.name == spec.name) {
            return Err(format!("param '{}' declared twice in one run", spec.name));
        }
        // Keep an existing value if present and compatible with this kind.
        let keep = sp.values.get(&spec.name).and_then(|v| {
            if !v.compatible_with(&spec.kind) {
                return None;
            }
            match (&spec.kind, v) {
                // A persisted / prior-run value may fall outside a range that
                // was since tightened; clamp it so an out-of-range value never
                // reaches the script's computation.
                (ParamKind::Slider { min, max, .. }, ParamValue::Float(f)) => {
                    Some(ParamValue::Float(f.clamp(*min, *max)))
                }
                // A combo value must still be one of the current options.
                (ParamKind::Combo { options }, ParamValue::Text(s)) => {
                    options.contains(s).then(|| v.clone())
                }
                _ => Some(v.clone()),
            }
        });
        let value = keep.unwrap_or_else(|| spec.default.clone());
        sp.values.insert(spec.name.clone(), value.clone());
        spec.order = sp.specs.len() as u32;
        spec.generation = generation;
        sp.specs.push(spec);
        Ok(value)
    }

    pub fn value(&self, script: &str, name: &str) -> Option<ParamValue> {
        self.scripts.get(script)?.values.get(name).cloned()
    }

    pub fn spec(&self, script: &str, name: &str) -> Option<ParamSpec> {
        self.scripts
            .get(script)?
            .specs
            .iter()
            .find(|s| s.name == name)
            .cloned()
    }

    pub fn set_value(&mut self, script: &str, name: &str, value: ParamValue) {
        self.scripts
            .entry(script.to_string())
            .or_default()
            .values
            .insert(name.to_string(), value);
    }

    pub fn reset_value(&mut self, script: &str, name: &str) -> Option<ParamValue> {
        let sp = self.scripts.get_mut(script)?;
        let default = sp.specs.iter().find(|s| s.name == name)?.default.clone();
        sp.values.insert(name.to_string(), default.clone());
        Some(default)
    }

    pub fn finalize(
        &mut self,
        script: &str,
        generation: u64,
        has_snapshot: bool,
        has_live: bool,
    ) {
        let sp = self.scripts.entry(script.to_string()).or_default();
        // A run that declared no params clears the prior generation's specs.
        if sp.last_generation != Some(generation) {
            sp.last_generation = Some(generation);
            sp.specs.clear();
        }
        sp.has_snapshot = has_snapshot;
        sp.has_live = has_live;
    }
}

thread_local! {
    static CURRENT_SCRIPT: std::cell::RefCell<Option<String>> =
        const { std::cell::RefCell::new(None) };
}

pub fn set_current_script(name: Option<String>) {
    CURRENT_SCRIPT.with(|c| *c.borrow_mut() = name);
}

pub fn current_script() -> Option<String> {
    CURRENT_SCRIPT.with(|c| c.borrow().clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slider(name: &str, default: f64, min: f64, max: f64) -> ParamSpec {
        ParamSpec {
            name: name.into(),
            label: name.into(),
            kind: ParamKind::Slider { min, max, step: None, integer: false },
            default: ParamValue::Float(default),
            order: 0,
            generation: 0,
        }
    }

    #[test]
    fn declare_seeds_default_then_returns_current() {
        let mut s = ParamStore::default();
        let v = s.declare("foo", 1, slider("gain", 2.0, 0.0, 10.0)).unwrap();
        assert_eq!(v, ParamValue::Float(2.0));
        assert_eq!(s.value("foo", "gain"), Some(ParamValue::Float(2.0)));
        assert_eq!(s.scripts["foo"].specs[0].order, 0);
    }

    #[test]
    fn declare_keeps_existing_value_across_runs() {
        let mut s = ParamStore::default();
        s.declare("foo", 1, slider("gain", 2.0, 0.0, 10.0)).unwrap();
        s.set_value("foo", "gain", ParamValue::Float(7.5));
        let v = s.declare("foo", 2, slider("gain", 2.0, 0.0, 10.0)).unwrap();
        assert_eq!(v, ParamValue::Float(7.5)); // edit preserved, not reset to default
    }

    #[test]
    fn declare_clamps_kept_value_into_a_tightened_slider_range() {
        let mut s = ParamStore::default();
        s.declare("foo", 1, slider("gain", 2.0, 0.0, 10.0)).unwrap();
        s.set_value("foo", "gain", ParamValue::Float(9.0));
        // Re-declared with a tighter max: the kept 9.0 must be clamped to 5.0.
        let v = s.declare("foo", 2, slider("gain", 2.0, 0.0, 5.0)).unwrap();
        assert_eq!(v, ParamValue::Float(5.0));
        assert_eq!(s.value("foo", "gain"), Some(ParamValue::Float(5.0)));
    }

    #[test]
    fn declare_falls_back_to_default_on_incompatible_kind() {
        let mut s = ParamStore::default();
        s.set_value("foo", "x", ParamValue::Text("hello".into())); // stale value, wrong kind
        let v = s.declare("foo", 1, slider("x", 3.0, 0.0, 10.0)).unwrap();
        assert_eq!(v, ParamValue::Float(3.0));
    }

    #[test]
    fn combo_falls_back_when_value_not_in_options() {
        let mut s = ParamStore::default();
        s.set_value("foo", "m", ParamValue::Text("old".into()));
        let spec = ParamSpec {
            name: "m".into(),
            label: "m".into(),
            kind: ParamKind::Combo { options: vec!["a".into(), "b".into()] },
            default: ParamValue::Text("a".into()),
            order: 0,
            generation: 0,
        };
        let v = s.declare("foo", 1, spec).unwrap();
        assert_eq!(v, ParamValue::Text("a".into()));
    }

    #[test]
    fn redeclare_prunes_params_no_longer_declared() {
        let mut s = ParamStore::default();
        s.declare("foo", 1, slider("a", 1.0, 0.0, 2.0)).unwrap();
        s.declare("foo", 1, slider("b", 1.0, 0.0, 2.0)).unwrap();
        assert_eq!(s.scripts["foo"].specs.len(), 2);
        // Next run declares only "a": "b"'s spec is pruned.
        s.declare("foo", 2, slider("a", 1.0, 0.0, 2.0)).unwrap();
        s.finalize("foo", 2, true, false);
        let names: Vec<_> = s.scripts["foo"].specs.iter().map(|x| x.name.clone()).collect();
        assert_eq!(names, vec!["a".to_string()]);
    }

    #[test]
    fn finalize_clears_specs_when_run_declares_nothing() {
        let mut s = ParamStore::default();
        s.declare("foo", 1, slider("a", 1.0, 0.0, 2.0)).unwrap();
        s.finalize("foo", 2, false, true); // run 2 declared nothing
        assert!(s.scripts["foo"].specs.is_empty());
        assert!(s.scripts["foo"].has_live);
    }

    #[test]
    fn duplicate_name_in_one_run_is_error() {
        let mut s = ParamStore::default();
        s.declare("foo", 1, slider("a", 1.0, 0.0, 2.0)).unwrap();
        assert!(s.declare("foo", 1, slider("a", 1.0, 0.0, 2.0)).is_err());
    }

    #[test]
    fn empty_name_is_error() {
        let mut s = ParamStore::default();
        assert!(s.declare("foo", 1, slider("", 1.0, 0.0, 2.0)).is_err());
    }

    #[test]
    fn reset_value_restores_default() {
        let mut s = ParamStore::default();
        s.declare("foo", 1, slider("g", 2.0, 0.0, 10.0)).unwrap();
        s.set_value("foo", "g", ParamValue::Float(9.0));
        assert_eq!(s.reset_value("foo", "g"), Some(ParamValue::Float(2.0)));
        assert_eq!(s.value("foo", "g"), Some(ParamValue::Float(2.0)));
    }

    #[test]
    fn current_script_thread_local_roundtrips() {
        assert_eq!(current_script(), None);
        set_current_script(Some("s".into()));
        assert_eq!(current_script(), Some("s".into()));
        set_current_script(None);
        assert_eq!(current_script(), None);
    }
}
