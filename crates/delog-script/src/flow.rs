//! Executes `delog-flow` script nodes on the embedded-Python worker thread.
//!
//! `eval_flow_script` is a pure function call: it runs the node's code in a
//! fresh namespace (no `delog` object, no snapshot) and calls its `flow`
//! function. `EngineFlowHost` dispatches that call onto the `ScriptEngine`
//! worker and implements `delog_flow::script::ScriptNodeHost` so the data-flow
//! evaluator can run script nodes without depending on Python itself.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{RecvTimeoutError, Sender};
use std::time::Duration;

use numpy::{IntoPyArray, PyArray1};
use pyo3::exceptions::PyAttributeError;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyTuple};

use delog_flow::script::{ScriptInput, ScriptNodeHost, ScriptOutput, ScriptRequest};
use delog_flow::types::Value;

use crate::engine::{EngineCommand, ScriptCommand, format_pyerr, request_python_interrupt};

/// Sends `ScriptCommand::EvalFlowScript` to the `ScriptEngine` worker and
/// waits for the reply, requesting a Python interrupt once `cancel` is set.
pub struct EngineFlowHost {
    tx: Mutex<Sender<EngineCommand>>,
}

impl EngineFlowHost {
    pub(crate) fn new(tx: Sender<EngineCommand>) -> Self {
        Self { tx: Mutex::new(tx) }
    }
}

impl ScriptNodeHost for EngineFlowHost {
    fn eval(&self, request: ScriptRequest, cancel: &AtomicBool) -> Result<Vec<ScriptOutput>, String> {
        let (reply_tx, reply_rx) = std::sync::mpsc::sync_channel(1);
        {
            let tx = self.tx.lock().unwrap_or_else(|e| e.into_inner());
            tx.send(EngineCommand::Script(ScriptCommand::EvalFlowScript {
                request,
                reply: reply_tx,
            }))
            .map_err(|_| "script engine is not running".to_owned())?;
        }
        let mut interrupted = false;
        loop {
            match reply_rx.recv_timeout(Duration::from_millis(50)) {
                Ok(result) => return result,
                Err(RecvTimeoutError::Timeout) => {
                    if cancel.load(Ordering::Relaxed) && !interrupted {
                        request_python_interrupt();
                        interrupted = true;
                    }
                }
                Err(RecvTimeoutError::Disconnected) => {
                    return Err("script engine is not running".to_owned());
                }
            }
        }
    }
}

/// One input value exposed as a `FlowInputs` attribute: a `FlowSignal` for a
/// signal, or a plain `float` for a scalar.
#[pyclass(unsendable, name = "FlowSignal")]
struct FlowSignal {
    #[pyo3(get)]
    t: Py<PyArray1<i64>>,
    #[pyo3(get)]
    v: Py<PyArray1<f64>>,
    #[pyo3(get)]
    unit: Option<String>,
}

fn value_to_py(py: Python<'_>, value: &Value) -> PyResult<Py<PyAny>> {
    match value {
        Value::Signal(signal) => {
            let t = signal.t.as_ref().clone().into_pyarray(py).unbind();
            let v = signal.v.as_ref().clone().into_pyarray(py).unbind();
            let unit = signal.meta.unit.clone();
            Ok(Bound::new(py, FlowSignal { t, v, unit })?.into_any().unbind())
        }
        Value::Scalar(x) => Ok(x.into_pyobject(py)?.into_any().unbind()),
    }
}

/// `inputs.<port_name>` resolves to a `FlowSignal` or `float` via `__getattr__`.
#[pyclass(unsendable, name = "FlowInputs")]
struct FlowInputs {
    values: HashMap<String, Py<PyAny>>,
}

impl FlowInputs {
    fn build(py: Python<'_>, inputs: &[ScriptInput]) -> Result<Self, String> {
        let mut values = HashMap::with_capacity(inputs.len());
        for input in inputs {
            let obj = value_to_py(py, &input.value).map_err(|e| format_pyerr(py, &e))?;
            values.insert(input.name.clone(), obj);
        }
        Ok(Self { values })
    }
}

#[pymethods]
impl FlowInputs {
    fn __getattr__(&self, name: &str) -> PyResult<Py<PyAny>> {
        self.values
            .get(name)
            .map(|obj| Python::attach(|py| obj.clone_ref(py)))
            .ok_or_else(|| PyAttributeError::new_err(name.to_owned()))
    }
}

fn extract_f64_array(name: &str, value: &Bound<'_, PyAny>) -> Result<Vec<f64>, String> {
    let array: numpy::PyReadonlyArray1<f64> = value
        .extract()
        .map_err(|_| format!("flow output '{name}' values must be a 1-D float array"))?;
    array
        .as_slice()
        .map(<[f64]>::to_vec)
        .map_err(|_| format!("flow output '{name}' values array must be contiguous"))
}

fn extract_i64_array(name: &str, value: &Bound<'_, PyAny>) -> Result<Vec<i64>, String> {
    let array: numpy::PyReadonlyArray1<i64> = value
        .extract()
        .map_err(|_| format!("flow output '{name}' times must be a 1-D int64 array"))?;
    array
        .as_slice()
        .map(<[i64]>::to_vec)
        .map_err(|_| format!("flow output '{name}' times array must be contiguous"))
}

/// One dict value: bare `values`, `(times, values)`, or `(times, values, unit)`.
fn parse_flow_output_entry(name: &str, value: &Bound<'_, PyAny>) -> Result<ScriptOutput, String> {
    let Ok(tuple) = value.cast::<PyTuple>() else {
        let values = extract_f64_array(name, value)?;
        return Ok(ScriptOutput {
            times: None,
            values,
            unit: None,
        });
    };
    let (times, values, unit) = match tuple.len() {
        2 => {
            let times = extract_i64_array(
                name,
                &tuple
                    .get_item(0)
                    .map_err(|_| format!("flow output '{name}' tuple access failed"))?,
            )?;
            let values = extract_f64_array(
                name,
                &tuple
                    .get_item(1)
                    .map_err(|_| format!("flow output '{name}' tuple access failed"))?,
            )?;
            (times, values, None)
        }
        3 => {
            let times = extract_i64_array(
                name,
                &tuple
                    .get_item(0)
                    .map_err(|_| format!("flow output '{name}' tuple access failed"))?,
            )?;
            let values = extract_f64_array(
                name,
                &tuple
                    .get_item(1)
                    .map_err(|_| format!("flow output '{name}' tuple access failed"))?,
            )?;
            let unit: Option<String> = tuple
                .get_item(2)
                .map_err(|_| format!("flow output '{name}' tuple access failed"))?
                .extract()
                .map_err(|_| format!("flow output '{name}' unit must be a string or None"))?;
            (times, values, unit)
        }
        n => {
            return Err(format!(
                "flow output '{name}' tuple must be (times, values) or (times, values, unit), got a {n}-tuple"
            ));
        }
    };
    if times.len() != values.len() {
        return Err(format!(
            "flow output '{name}': times and values must have the same length"
        ));
    }
    if !times.windows(2).all(|pair| pair[0] <= pair[1]) {
        return Err(format!(
            "flow output '{name}': times must be sorted ascending"
        ));
    }
    Ok(ScriptOutput {
        times: Some(times),
        values,
        unit,
    })
}

/// `flow(inputs)` must return a dict whose keys are exactly the declared
/// outputs (order-insensitive).
pub(crate) fn parse_flow_result(
    py: Python<'_>,
    declared_outputs: &[String],
    ret: &Bound<'_, PyAny>,
) -> Result<Vec<ScriptOutput>, String> {
    let dict = ret
        .cast::<PyDict>()
        .map_err(|_| "flow(inputs) must return a dict of {output_name: values}".to_owned())?;

    let declared: HashSet<&str> = declared_outputs.iter().map(String::as_str).collect();
    let mut returned: HashSet<String> = HashSet::with_capacity(dict.len());
    for key in dict.keys() {
        let name: String = key
            .extract()
            .map_err(|_| "flow(inputs) return dict keys must be strings".to_owned())?;
        returned.insert(name);
    }
    let returned_refs: HashSet<&str> = returned.iter().map(String::as_str).collect();
    if returned_refs != declared {
        let mut missing: Vec<&str> = declared.difference(&returned_refs).copied().collect();
        missing.sort_unstable();
        let mut extra: Vec<&str> = returned_refs.difference(&declared).copied().collect();
        extra.sort_unstable();
        return Err(format!(
            "flow(inputs) return keys do not match declared outputs (missing: [{}], extra: [{}])",
            missing.join(", "),
            extra.join(", ")
        ));
    }

    declared_outputs
        .iter()
        .map(|name| {
            let value = dict
                .get_item(name)
                .map_err(|e| format_pyerr(py, &e))?
                .expect("key presence checked above");
            parse_flow_output_entry(name, &value)
        })
        .collect()
}

/// Runs `request.code` in a fresh namespace and calls its `flow(inputs)`.
/// Touches no snapshot state, `prev_sources`, markers, or params — a pure
/// function call.
pub(crate) fn eval_flow_script(request: &ScriptRequest) -> Result<Vec<ScriptOutput>, String> {
    Python::attach(|py| {
        let globals = PyDict::new(py);
        let code = std::ffi::CString::new(request.code.as_str())
            .map_err(|_| "script contains a NUL byte".to_owned())?;
        py.run(&code, Some(&globals), None)
            .map_err(|e| format_pyerr(py, &e))?;
        let flow = globals
            .get_item("flow")
            .ok()
            .flatten()
            .ok_or_else(|| "script must define flow(inputs)".to_owned())?;
        let inputs = FlowInputs::build(py, &request.inputs)?;
        let inputs = Bound::new(py, inputs).map_err(|e| format_pyerr(py, &e))?;
        let ret = flow.call1((inputs,)).map_err(|e| format_pyerr(py, &e))?;
        parse_flow_result(py, &request.outputs, &ret)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::Arc;

    use delog_core::metrics::MetricsRegistry;
    use delog_core::snapshot::DataStore;
    use delog_flow::graph::NodeId;
    use delog_flow::types::{Signal, SignalMeta, TimelineId};

    use crate::engine::{ENGINE_LOCK, ScriptEngine, ScriptEvent};

    fn dummy_sender() -> delog_core::ingest::IngestSender {
        delog_core::ingest::ingest_channel().0
    }

    fn spawn_test_engine() -> ScriptEngine {
        ScriptEngine::spawn(
            Arc::new(DataStore::new()),
            dummy_sender(),
            Arc::new(MetricsRegistry::new()),
            crate::params::shared_empty(),
        )
    }

    fn signal_input(name: &str, t: Vec<i64>, v: Vec<f64>, unit: Option<&str>) -> ScriptInput {
        ScriptInput {
            name: name.to_owned(),
            value: Value::Signal(Signal {
                t: Arc::new(t),
                v: Arc::new(v),
                meta: SignalMeta {
                    timeline: TimelineId::Node(NodeId(0)),
                    unit: unit.map(str::to_owned),
                },
            }),
        }
    }

    fn scalar_input(name: &str, x: f64) -> ScriptInput {
        ScriptInput {
            name: name.to_owned(),
            value: Value::Scalar(x),
        }
    }

    fn request(code: &str, inputs: Vec<ScriptInput>, outputs: &[&str]) -> ScriptRequest {
        ScriptRequest {
            node_label: "test-node".into(),
            code: code.to_owned(),
            inputs,
            outputs: outputs.iter().map(|s| (*s).to_owned()).collect(),
        }
    }

    #[test]
    fn flow_script_maps_inputs_and_returns_declared_outputs() {
        let _guard = ENGINE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let engine = spawn_test_engine();
        let host = engine.flow_host();
        let req = request(
            "def flow(inputs):\n    return {\"out\": inputs.a.v * 2.0}\n",
            vec![signal_input("a", vec![1, 2, 3], vec![1.0, 2.0, 3.0], None)],
            &["out"],
        );
        let result = host.eval(req, &AtomicBool::new(false)).expect("eval succeeds");
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].values, vec![2.0, 4.0, 6.0]);
        assert!(result[0].times.is_none());
        assert!(result[0].unit.is_none());
    }

    #[test]
    fn scalar_inputs_arrive_as_floats() {
        let _guard = ENGINE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let engine = spawn_test_engine();
        let host = engine.flow_host();
        let req = request(
            "import numpy as np\ndef flow(inputs):\n    return {\"out\": np.array([inputs.k * 3.0])}\n",
            vec![scalar_input("k", 4.0)],
            &["out"],
        );
        let result = host.eval(req, &AtomicBool::new(false)).expect("eval succeeds");
        assert_eq!(result[0].values, vec![12.0]);
    }

    #[test]
    fn tuple_forms_times_values_unit_are_parsed() {
        let _guard = ENGINE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let engine = spawn_test_engine();
        let host = engine.flow_host();
        let req = request(
            "import numpy as np\n\
             def flow(inputs):\n\
             \x20   t = np.array([10, 20, 30], dtype=np.int64)\n\
             \x20   v = np.array([1.0, 2.0, 3.0])\n\
             \x20   return {\"pair\": (t, v), \"triple\": (t, v, \"m/s\")}\n",
            vec![],
            &["pair", "triple"],
        );
        let result = host.eval(req, &AtomicBool::new(false)).expect("eval succeeds");
        assert_eq!(result[0].times, Some(vec![10, 20, 30]));
        assert_eq!(result[0].values, vec![1.0, 2.0, 3.0]);
        assert_eq!(result[0].unit, None);
        assert_eq!(result[1].times, Some(vec![10, 20, 30]));
        assert_eq!(result[1].unit.as_deref(), Some("m/s"));
    }

    #[test]
    fn python_exception_replies_err_with_traceback() {
        let _guard = ENGINE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let engine = spawn_test_engine();
        let host = engine.flow_host();
        let req = request(
            "def flow(inputs):\n    raise ValueError('boom')\n",
            vec![],
            &["out"],
        );
        let err = host.eval(req, &AtomicBool::new(false)).unwrap_err();
        assert!(err.contains("boom"), "{err}");
        assert!(err.contains("Traceback"), "{err}");
    }

    #[test]
    fn missing_flow_function_is_an_error() {
        let _guard = ENGINE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let engine = spawn_test_engine();
        let host = engine.flow_host();
        let req = request("x = 1\n", vec![], &["out"]);
        let err = host.eval(req, &AtomicBool::new(false)).unwrap_err();
        assert!(err.contains("flow(inputs)"), "{err}");
    }

    #[test]
    fn extra_or_missing_output_keys_are_errors() {
        let _guard = ENGINE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let engine = spawn_test_engine();
        let host = engine.flow_host();

        let extra = request(
            "def flow(inputs):\n    return {\"out\": [1.0], \"other\": [2.0]}\n",
            vec![],
            &["out"],
        );
        let err = host.eval(extra, &AtomicBool::new(false)).unwrap_err();
        assert!(err.contains("extra"), "{err}");

        let missing = request("def flow(inputs):\n    return {}\n", vec![], &["out"]);
        let err = host.eval(missing, &AtomicBool::new(false)).unwrap_err();
        assert!(err.contains("missing"), "{err}");
    }

    #[test]
    fn unsorted_times_are_rejected_engine_side() {
        let _guard = ENGINE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let engine = spawn_test_engine();
        let host = engine.flow_host();
        let req = request(
            "import numpy as np\n\
             def flow(inputs):\n\
             \x20   t = np.array([3, 1, 2], dtype=np.int64)\n\
             \x20   v = np.array([1.0, 2.0, 3.0])\n\
             \x20   return {\"out\": (t, v)}\n",
            vec![],
            &["out"],
        );
        let err = host.eval(req, &AtomicBool::new(false)).unwrap_err();
        assert!(err.contains("sorted"), "{err}");
    }

    #[test]
    fn print_output_reaches_the_console_events() {
        let _guard = ENGINE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let engine = spawn_test_engine();
        let host = engine.flow_host();
        let req = request(
            "import numpy as np\nprint('from-flow')\ndef flow(inputs):\n    return {\"out\": np.array([1.0])}\n",
            vec![],
            &["out"],
        );
        let result = host.eval(req, &AtomicBool::new(false));
        assert!(result.is_ok());
        let mut text = String::new();
        for event in engine.drain_events() {
            if let ScriptEvent::Output(s) = event {
                text.push_str(&s);
            }
        }
        assert!(text.contains("from-flow"), "captured: {text:?}");
    }

    #[test]
    #[ignore = "Py_AddPendingCall uses a global queue; when other tests run concurrently \
                the pending callback can be consumed by another thread's eval loop before the \
                while-True loop processes it, causing a hang. Passes when run in isolation \
                (cargo test ... cancel_interrupts_a_sleeping_flow_script). Same root cause as \
                engine::tests::interrupt_stops_a_long_loop_with_keyboardinterrupt."]
    fn cancel_interrupts_a_sleeping_flow_script() {
        let _guard = ENGINE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let engine = spawn_test_engine();
        let host = engine.flow_host();
        let req = request("while True:\n    pass\n", vec![], &["out"]);
        let cancel = AtomicBool::new(false);
        let result = std::thread::scope(|scope| {
            let worker = scope.spawn(|| host.eval(req, &cancel));
            std::thread::sleep(Duration::from_millis(200));
            cancel.store(true, Ordering::Relaxed);
            worker.join().expect("worker thread did not panic")
        });
        let err = result.unwrap_err();
        assert!(err.contains("KeyboardInterrupt"), "{err}");
    }
}
