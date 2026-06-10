//! Application identity, license metadata and the `Help ▸ About` window
//! (PLAN.md §19.2, checklist ARC-07).
//!
//! ARC-07 establishes only the metadata and the About entry; the full
//! File/Tools/Layout/Help menu set is UIX-03's job (M10). The identity strings
//! come from the crate's Cargo metadata at compile time, so they track the
//! workspace `[package]`/`[workspace.package]` tables without duplication.

/// DeLOG's display name.
pub const NAME: &str = "DeLOG";
/// Semantic version (`CARGO_PKG_VERSION`, inherited from the workspace).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
/// One-line product description (`CARGO_PKG_DESCRIPTION`).
pub const DESCRIPTION: &str = env!("CARGO_PKG_DESCRIPTION");
/// Source repository URL (`CARGO_PKG_REPOSITORY`).
pub const REPOSITORY: &str = env!("CARGO_PKG_REPOSITORY");
/// SPDX identifier of DeLOG's own license (`CARGO_PKG_LICENSE`).
pub const LICENSE: &str = env!("CARGO_PKG_LICENSE");

/// One third-party component surfaced in the About window's attribution list.
pub struct Attribution {
    pub name: &'static str,
    pub license: &'static str,
}

/// The foundational open-source stack DeLOG is built on.
///
/// Hand-curated and intentionally not exhaustive — the authoritative set is the
/// workspace `Cargo.toml`. A generated attribution file (e.g. `cargo-about`) can
/// replace this when the full `Help ▸ licenses` view lands under UIX-03.
pub const THIRD_PARTY: &[Attribution] = &[
    Attribution {
        name: "egui / eframe",
        license: "MIT OR Apache-2.0",
    },
    Attribution {
        name: "wgpu",
        license: "MIT OR Apache-2.0",
    },
    Attribution {
        name: "Apache Arrow",
        license: "Apache-2.0",
    },
    Attribution {
        name: "rust-mavlink",
        license: "MIT OR Apache-2.0",
    },
    Attribution {
        name: "serialport",
        license: "MPL-2.0",
    },
    Attribution {
        name: "gltf",
        license: "MIT OR Apache-2.0",
    },
    Attribution {
        name: "rayon / crossbeam / arc-swap",
        license: "MIT OR Apache-2.0",
    },
    Attribution {
        name: "serde / serde_json",
        license: "MIT OR Apache-2.0",
    },
    Attribution {
        name: "tracing",
        license: "MIT",
    },
];

/// Render the About window when `open` is set; the window's close control
/// clears the flag. Painted above the workspace as a standalone egui window.
pub fn window(ctx: &egui::Context, open: &mut bool) {
    egui::Window::new("About DeLOG")
        .open(open)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .show(ctx, |ui| {
            ui.heading(format!("{NAME} {VERSION}"));
            ui.label(DESCRIPTION);
            ui.hyperlink(REPOSITORY);
            ui.add_space(4.0);
            ui.label(format!("Licensed under {LICENSE}."));

            ui.add_space(8.0);
            ui.separator();
            ui.add_space(4.0);
            ui.label("Built with open-source components:");
            egui::Grid::new("about_third_party")
                .num_columns(2)
                .spacing([16.0, 2.0])
                .show(ui, |ui| {
                    for c in THIRD_PARTY {
                        ui.label(c.name);
                        ui.weak(c.license);
                        ui.end_row();
                    }
                });
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_metadata_is_populated() {
        assert_eq!(NAME, "DeLOG");
        // Version comes from Cargo and must be a real semver-ish string.
        assert!(!VERSION.is_empty());
        assert!(VERSION.contains('.'), "version `{VERSION}` is not dotted");
        assert!(!DESCRIPTION.is_empty());
        assert!(REPOSITORY.starts_with("https://"), "repo `{REPOSITORY}`");
        assert_eq!(LICENSE, "MIT");
    }

    #[test]
    fn third_party_attributions_are_well_formed() {
        assert!(!THIRD_PARTY.is_empty());
        for a in THIRD_PARTY {
            assert!(!a.name.is_empty(), "empty attribution name");
            assert!(!a.license.is_empty(), "{} has no license", a.name);
        }
    }
}
