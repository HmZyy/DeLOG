use delog_core::align::AlignMode;
use delog_flow::graph::{ConversionKind, NodeKind, OutputFieldSpec, OutputSpec};

use crate::ui::fuzzy::fuzzy_match_score;

pub const ADD_DATA_INDEX: usize = usize::MAX;

pub struct NodeTemplate {
    pub name: &'static str,
    pub category: &'static str,
    pub aliases: &'static [&'static str],
    pub make: fn() -> NodeKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MenuEntry {
    pub index: usize,
    pub score: u32,
}

pub fn templates() -> &'static [NodeTemplate] {
    static TEMPLATES: &[NodeTemplate] = &[
        NodeTemplate {
            name: "Constant",
            category: "Data",
            aliases: &["const", "number", "value"],
            make: || NodeKind::Constant { value: 0.0 },
        },
        NodeTemplate {
            name: "Add",
            category: "Math",
            aliases: &["+", "plus", "sum"],
            make: || NodeKind::Add,
        },
        NodeTemplate {
            name: "Subtract",
            category: "Math",
            aliases: &["-", "minus"],
            make: || NodeKind::Subtract,
        },
        NodeTemplate {
            name: "Multiply",
            category: "Math",
            aliases: &["*", "x", "times"],
            make: || NodeKind::Multiply,
        },
        NodeTemplate {
            name: "Divide",
            category: "Math",
            aliases: &["/", "over"],
            make: || NodeKind::Divide,
        },
        NodeTemplate {
            name: "Scale / Offset",
            category: "Math",
            aliases: &["scale", "offset", "gain"],
            make: || NodeKind::ScaleOffset {
                multiplier: 1.0,
                offset: 0.0,
            },
        },
        NodeTemplate {
            name: "Radians to Degrees",
            category: "Convert",
            aliases: &["rad", "deg", "radians", "degrees", "convert"],
            make: || NodeKind::Convert {
                kind: ConversionKind::RadToDeg,
            },
        },
        NodeTemplate {
            name: "Degrees to Radians",
            category: "Convert",
            aliases: &["deg", "rad", "degrees", "radians", "convert"],
            make: || NodeKind::Convert {
                kind: ConversionKind::DegToRad,
            },
        },
        NodeTemplate {
            name: "Heading 0..360",
            category: "Convert",
            aliases: &["heading", "wrap", "normalize", "angle", "yaw"],
            make: || NodeKind::Convert {
                kind: ConversionKind::Heading0To360,
            },
        },
        NodeTemplate {
            name: "Heading -180..180",
            category: "Convert",
            aliases: &["heading", "wrap", "normalize", "angle", "yaw"],
            make: || NodeKind::Convert {
                kind: ConversionKind::HeadingPm180,
            },
        },
        NodeTemplate {
            name: "m/s to km/h",
            category: "Convert",
            aliases: &["speed", "mps", "kmh", "kph", "velocity"],
            make: || NodeKind::Convert {
                kind: ConversionKind::MsToKmh,
            },
        },
        NodeTemplate {
            name: "km/h to m/s",
            category: "Convert",
            aliases: &["speed", "kmh", "kph", "mps", "velocity"],
            make: || NodeKind::Convert {
                kind: ConversionKind::KmhToMs,
            },
        },
        NodeTemplate {
            name: "Meters to Feet",
            category: "Convert",
            aliases: &["meters", "feet", "ft", "altitude", "distance"],
            make: || NodeKind::Convert {
                kind: ConversionKind::MToFt,
            },
        },
        NodeTemplate {
            name: "Feet to Meters",
            category: "Convert",
            aliases: &["feet", "meters", "ft", "altitude", "distance"],
            make: || NodeKind::Convert {
                kind: ConversionKind::FtToM,
            },
        },
        NodeTemplate {
            name: "Align to Timeline",
            category: "Time",
            aliases: &["align", "resample-to", "interp"],
            make: || NodeKind::Align {
                mode: AlignMode::Prev,
            },
        },
        NodeTemplate {
            name: "Derived Topic Output",
            category: "Output",
            aliases: &["output", "publish", "emit"],
            make: || {
                NodeKind::Output(OutputSpec {
                    topic: "derived".to_owned(),
                    fields: vec![OutputFieldSpec {
                        name: "out".to_owned(),
                        unit: None,
                    }],
                })
            },
        },
        #[cfg(feature = "scripting")]
        NodeTemplate {
            name: "Python Script",
            category: "Script",
            aliases: &["python", "py", "script", "code"],
            make: || {
                NodeKind::Script(delog_flow::script::ScriptSpec {
                    name: "Script".to_owned(),
                    inputs: vec![delog_flow::script::ScriptInputSpec {
                        name: "a".to_owned(),
                    }],
                    outputs: vec![delog_flow::script::ScriptOutputSpec {
                        name: "out".to_owned(),
                        unit: None,
                    }],
                    code: "def flow(inputs):\n    return {\"out\": inputs.a.v}\n".to_owned(),
                })
            },
        },
    ];
    TEMPLATES
}

pub fn search_templates(query: &str) -> Vec<MenuEntry> {
    if query.trim().is_empty() {
        return templates()
            .iter()
            .enumerate()
            .map(|(index, _)| MenuEntry { index, score: 0 })
            .collect();
    }
    let mut entries = templates()
        .iter()
        .enumerate()
        .filter_map(|(index, template)| {
            let mut score = fuzzy_match_score(query, template.name);
            for alias in template.aliases {
                let alias_score = if query.trim().eq_ignore_ascii_case(alias) {
                    Some(0)
                } else {
                    fuzzy_match_score(query, alias)
                };
                score = match (score, alias_score) {
                    (Some(a), Some(b)) => Some(a.min(b)),
                    (None, hit) | (hit, None) => hit,
                };
            }
            score.map(|score| MenuEntry { index, score })
        })
        .collect::<Vec<_>>();
    entries.sort_by_key(|entry| (entry.score, entry.index));
    entries
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_conversion_has_a_searchable_template() {
        let names: Vec<_> = templates().iter().map(|template| template.name).collect();
        for kind in ConversionKind::ALL {
            assert!(
                names.contains(&kind.label()),
                "missing template for {kind:?}"
            );
        }
    }

    #[test]
    fn symbol_aliases_rank_their_operation_first() {
        for (symbol, expected) in [
            ("+", "Add"),
            ("-", "Subtract"),
            ("*", "Multiply"),
            ("/", "Divide"),
        ] {
            let hits = search_templates(symbol);
            assert_eq!(templates()[hits[0].index].name, expected, "for {symbol}");
        }
    }

    #[test]
    fn fuzzy_matches_rank_substrings_before_subsequences() {
        let hits = search_templates("ali");
        assert_eq!(templates()[hits[0].index].name, "Align to Timeline");
    }

    #[test]
    fn empty_query_lists_everything_grouped_by_category() {
        let hits = search_templates("");
        assert_eq!(hits.len(), templates().len());
    }

    #[test]
    #[cfg(feature = "scripting")]
    fn python_alias_ranks_the_script_template_first() {
        let hits = search_templates("py");
        assert_eq!(templates()[hits[0].index].name, "Python Script");
    }
}
