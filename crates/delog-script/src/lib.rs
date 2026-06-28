//! With the `python` feature off, only the feature-independent script library
//! (file persistence) is compiled; the interpreter engine lives behind `python`.

pub mod library;

#[cfg(feature = "python")]
pub mod parser_library;

#[cfg(feature = "python")]
pub mod custom_parser;

#[cfg(feature = "python")]
pub mod api;

#[cfg(feature = "python")]
pub mod emit;

#[cfg(feature = "python")]
pub mod engine;

#[cfg(feature = "python")]
pub mod live;

#[cfg(feature = "python")]
pub use engine::{ParserEvent, ScriptCommand, ScriptEngine, ScriptEvent};
