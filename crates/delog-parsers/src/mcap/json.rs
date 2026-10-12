//! JSON-Schema channel definitions and streaming payload extraction.
//!
//! Each MCAP channel pairs a JSON Schema with JSON payloads. Only top-level
//! scalar properties become DeLOG fields; nested objects/arrays and base64
//! blobs are skipped. Extraction streams the payload through `serde` into a
//! preallocated scratch slice, so payloads are never materialized as
//! `serde_json::Value` - which keeps large base64 blobs from allocating.

use std::collections::HashMap;

use serde::de::{DeserializeSeed, Deserializer, IgnoredAny, MapAccess, Visitor};
use serde_json::Value;

/// A scalar JSON value captured for one field.
#[derive(Debug, Clone, PartialEq)]
pub enum Scalar {
    Missing,
    Int(i64),
    Float(f64),
    Bool(bool),
    Str(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldDtype {
    Int,
    Float,
    Bool,
    Str,
}

#[derive(Debug, Clone)]
pub struct FieldSpec {
    pub key: String,
    pub dtype: FieldDtype,
}

/// Properties dropped as clock/descriptor metadata rather than signals; MCAP
/// producers repeat them on every channel and they would bury the real signals.
const DROPPED_FIELDS: &[&str] = &["rtc", "timestamp", "unit", "exponent"];

pub struct SchemaPlan {
    pub fields: Vec<FieldSpec>,
    /// True when the schema declares a base64 blob, i.e. this is a media
    /// channel rather than a scalar-signal channel.
    pub is_media: bool,
}

/// Derive the field list for a channel from its JSON Schema.
pub fn plan_schema(data: &[u8]) -> Result<SchemaPlan, String> {
    let root: Value =
        serde_json::from_slice(data).map_err(|error| format!("schema is not JSON: {error}"))?;
    let mut fields = Vec::new();
    let mut is_media = false;
    if let Some(properties) = root.get("properties").and_then(Value::as_object) {
        for (name, spec) in properties {
            if spec.get("contentEncoding").and_then(Value::as_str) == Some("base64") {
                is_media = true;
                continue;
            }
            if DROPPED_FIELDS.contains(&name.as_str()) {
                continue;
            }
            let dtype = match spec.get("type").and_then(Value::as_str) {
                Some("integer") => FieldDtype::Int,
                Some("number") => FieldDtype::Float,
                Some("boolean") => FieldDtype::Bool,
                Some("string") => FieldDtype::Str,
                // Arrays, nested objects, unions, and untyped properties.
                _ => continue,
            };
            fields.push(FieldSpec {
                key: name.clone(),
                dtype,
            });
        }
    }
    Ok(SchemaPlan { fields, is_media })
}

/// Extract the wanted fields from one JSON payload into `out`, which is sized to
/// the field list. Returns `Err` when the payload is not a JSON object. Slots
/// with no matching property are left [`Scalar::Missing`].
pub fn read_fields(
    data: &[u8],
    wanted: &HashMap<String, usize>,
    out: &mut [Scalar],
) -> Result<(), String> {
    for slot in out.iter_mut() {
        *slot = Scalar::Missing;
    }
    let mut deserializer = serde_json::Deserializer::from_slice(data);
    MessageSeed { wanted, out }
        .deserialize(&mut deserializer)
        .map_err(|error| error.to_string())
}

struct MessageSeed<'a> {
    wanted: &'a HashMap<String, usize>,
    out: &'a mut [Scalar],
}

impl<'de> DeserializeSeed<'de> for MessageSeed<'_> {
    type Value = ();

    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<(), D::Error> {
        deserializer.deserialize_map(MessageVisitor {
            wanted: self.wanted,
            out: self.out,
        })
    }
}

struct MessageVisitor<'a> {
    wanted: &'a HashMap<String, usize>,
    out: &'a mut [Scalar],
}

impl<'de> Visitor<'de> for MessageVisitor<'_> {
    type Value = ();

    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("a JSON object of channel fields")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        let MessageVisitor { wanted, out } = self;
        while let Some(key) = map.next_key::<&str>()? {
            if let Some(&index) = wanted.get(key) {
                out[index] = map.next_value_seed(ScalarSeed)?;
            } else {
                // Skips the value without allocating it (base64 blobs, nested
                // objects, unknown properties).
                map.next_value::<IgnoredAny>()?;
            }
        }
        Ok(())
    }
}

struct ScalarSeed;

impl<'de> DeserializeSeed<'de> for ScalarSeed {
    type Value = Scalar;

    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<Scalar, D::Error> {
        deserializer.deserialize_any(ScalarVisitor)
    }
}

struct ScalarVisitor;

impl<'de> Visitor<'de> for ScalarVisitor {
    type Value = Scalar;

    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("a JSON scalar")
    }

    fn visit_i64<E>(self, value: i64) -> Result<Scalar, E> {
        Ok(Scalar::Int(value))
    }

    fn visit_u64<E>(self, value: u64) -> Result<Scalar, E> {
        Ok(match i64::try_from(value) {
            Ok(value) => Scalar::Int(value),
            Err(_) => Scalar::Float(value as f64),
        })
    }

    fn visit_f64<E>(self, value: f64) -> Result<Scalar, E> {
        Ok(Scalar::Float(value))
    }

    fn visit_bool<E>(self, value: bool) -> Result<Scalar, E> {
        Ok(Scalar::Bool(value))
    }

    fn visit_str<E>(self, value: &str) -> Result<Scalar, E> {
        Ok(Scalar::Str(value.to_owned()))
    }

    fn visit_string<E>(self, value: String) -> Result<Scalar, E> {
        Ok(Scalar::Str(value))
    }

    fn visit_unit<E>(self) -> Result<Scalar, E> {
        Ok(Scalar::Missing)
    }

    fn visit_none<E>(self) -> Result<Scalar, E> {
        Ok(Scalar::Missing)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wanted(specs: &[FieldSpec]) -> HashMap<String, usize> {
        specs
            .iter()
            .enumerate()
            .map(|(index, spec)| (spec.key.clone(), index))
            .collect()
    }

    #[test]
    fn plan_keeps_scalar_signals_and_drops_metadata() {
        let schema = br#"{"properties":{
            "rtc":{"type":"number"},
            "value":{"type":"integer"},
            "unit":{"type":"string"},
        }}"#;
        let plan = plan_schema(schema).unwrap();
        let keys: Vec<&str> = plan.fields.iter().map(|f| f.key.as_str()).collect();
        assert_eq!(keys, ["value"]);
        assert!(!plan.is_media);
    }

    #[test]
    fn plan_flags_base64_channels_as_media() {
        let schema = br#"{"properties":{
            "data":{"type":"string","contentEncoding":"base64"},
            "format":{"type":"string"}
        }}"#;
        let plan = plan_schema(schema).unwrap();
        assert!(plan.is_media);
        assert_eq!(plan.fields.len(), 1);
        assert_eq!(plan.fields[0].key, "format");
    }

    #[test]
    fn read_fields_captures_wanted_scalars_and_skips_others() {
        let specs = vec![
            FieldSpec {
                key: "value".into(),
                dtype: FieldDtype::Int,
            },
            FieldSpec {
                key: "name".into(),
                dtype: FieldDtype::Str,
            },
            FieldSpec {
                key: "missing".into(),
                dtype: FieldDtype::Float,
            },
        ];
        let map = wanted(&specs);
        let mut out = vec![Scalar::Missing; specs.len()];

        read_fields(
            br#"{"value":7,"name":"hi","blob":"AAAA","big":1e30}"#,
            &map,
            &mut out,
        )
        .unwrap();

        assert_eq!(out[0], Scalar::Int(7));
        assert_eq!(out[1], Scalar::Str("hi".into()));
        assert_eq!(out[2], Scalar::Missing);
    }

    #[test]
    fn read_fields_rejects_non_objects() {
        let map = HashMap::new();
        let mut out: Vec<Scalar> = Vec::new();
        assert!(read_fields(b"[1,2,3]", &map, &mut out).is_err());
    }

    #[test]
    fn unsigned_values_beyond_i64_become_floats() {
        let specs = vec![FieldSpec {
            key: "big".into(),
            dtype: FieldDtype::Int,
        }];
        let map = wanted(&specs);
        let mut out = vec![Scalar::Missing; 1];
        read_fields(br#"{"big":18446744073709551615}"#, &map, &mut out).unwrap();
        assert!(matches!(out[0], Scalar::Float(_)));
    }
}
