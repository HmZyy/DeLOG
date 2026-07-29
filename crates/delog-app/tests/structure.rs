use std::collections::HashMap;
use std::path::{Path, PathBuf};

const LAYER_RANKS: &[(&str, u32)] = &[
    ("ui", 0),
    ("scene3d", 0),
    ("map", 1),
    ("config", 2),
    ("plotting", 3),
    ("export", 4),
    ("ingest", 4),
    ("sync", 4),
    ("session", 5),
    ("scripting", 5),
    ("dataflow", 5),
    ("shell", 6),
];

fn src_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("readable directory") {
        let path = entry.expect("readable entry").path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

fn imported_layers(source: &str) -> Vec<String> {
    let mut found = Vec::new();
    for (index, _) in source.match_indices("crate::") {
        let rest = &source[index + "crate::".len()..];
        let end = rest
            .find(|c: char| !c.is_alphanumeric() && c != '_')
            .unwrap_or(rest.len());
        if end > 0 {
            found.push(rest[..end].to_owned());
        }
    }
    found
}

#[test]
fn layer_imports_point_downward() {
    let ranks: HashMap<&str, u32> = LAYER_RANKS.iter().copied().collect();
    let src = src_dir();
    let mut files = Vec::new();
    rust_files(&src, &mut files);

    assert!(
        files.len() > 50,
        "the source walk found only {} files, which means it is not actually scanning the crate",
        files.len()
    );

    let mut violations = Vec::new();
    for file in files {
        let relative = file.strip_prefix(&src).expect("path under src");
        let Some(layer) = relative
            .components()
            .next()
            .and_then(|c| c.as_os_str().to_str())
            .filter(|name| ranks.contains_key(*name))
        else {
            continue;
        };
        let own_rank = ranks[layer];
        let source = std::fs::read_to_string(&file).expect("readable source");
        for imported in imported_layers(&source) {
            let Some(&other_rank) = ranks.get(imported.as_str()) else {
                continue;
            };
            if imported != layer && other_rank >= own_rank {
                violations.push(format!(
                    "{} ({}) imports crate::{} ({})",
                    relative.display(),
                    own_rank,
                    imported,
                    other_rank
                ));
            }
        }
    }

    assert!(
        violations.is_empty(),
        "layer imports must point downward:\n{}",
        violations.join("\n")
    );
}

#[test]
fn every_layer_directory_has_a_rank() {
    let ranks: HashMap<&str, u32> = LAYER_RANKS.iter().copied().collect();
    let src = src_dir();
    let mut unranked = Vec::new();
    for entry in std::fs::read_dir(&src).expect("readable src") {
        let path = entry.expect("readable entry").path();
        if path.is_dir() {
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .expect("utf8 directory name")
                .to_owned();
            if !ranks.contains_key(name.as_str()) {
                unranked.push(name);
            }
        }
    }
    assert!(
        unranked.is_empty(),
        "new top-level directories must be added to LAYER_RANKS: {unranked:?}"
    );
}

const BUDGET: usize = 800;

const ALLOWED_OVER_BUDGET: &[&str] = &[
    "delog-script/src/engine.rs",
    "delog-script/src/api.rs",
    "delog-script/src/operations/live.rs",
    "delog-parsers/src/ardupilot.rs",
    "delog-parsers/src/ulog.rs",
    "delog-cache/src/trace.rs",
    "delog-app/src/config/layout/doc.rs",
    "delog-app/src/config/settings.rs",
    "delog-app/src/dataflow/controller/mod.rs",
    "delog-app/src/dataflow/window.rs",
    "delog-app/src/export/data_export/mod.rs",
    "delog-app/src/map/worker/mod.rs",
    "delog-app/src/plotting/browser.rs",
    "delog-app/src/plotting/gpu/mod.rs",
    "delog-app/src/scripting/scripts.rs",
    "delog-app/src/session/vehicle_dialog.rs",
    "delog-app/src/shell/app/mod.rs",
    "delog-app/src/shell/workspace/mod.rs",
    "delog-app/src/sync/sync_window/mod.rs",
];

fn implementation_lines(source: &str) -> usize {
    let lines: Vec<&str> = source.lines().collect();
    for (index, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();
        let declares_test_module = trimmed.starts_with("mod tests")
            || trimmed.starts_with("pub mod tests")
            || trimmed.starts_with("pub(crate) mod tests");
        if declares_test_module && index > 0 && lines[index - 1].trim() == "#[cfg(test)]" {
            return index - 1;
        }
    }
    lines.len()
}

#[test]
fn implementation_files_stay_under_budget() {
    let crates_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates directory")
        .to_path_buf();

    let mut files = Vec::new();
    for entry in std::fs::read_dir(&crates_dir).expect("readable crates dir") {
        let crate_src = entry.expect("readable entry").path().join("src");
        if crate_src.is_dir() {
            rust_files(&crate_src, &mut files);
        }
    }

    let mut over = Vec::new();
    for file in files {
        let relative = file
            .strip_prefix(&crates_dir)
            .expect("path under crates")
            .to_string_lossy()
            .replace('\\', "/");
        if relative.ends_with("/tests.rs") || ALLOWED_OVER_BUDGET.contains(&relative.as_str()) {
            continue;
        }
        let source = std::fs::read_to_string(&file).expect("readable source");
        let lines = implementation_lines(&source);
        if lines > BUDGET {
            over.push(format!("{relative}: {lines} lines"));
        }
    }

    over.sort();
    assert!(
        over.is_empty(),
        "files over the {BUDGET}-line implementation budget:\n{}",
        over.join("\n")
    );
}
