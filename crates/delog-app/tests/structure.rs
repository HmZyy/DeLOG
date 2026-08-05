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

fn leading_identifier(text: &str) -> String {
    let trimmed = text.trim_start();
    let end = trimmed
        .find(|c: char| !c.is_alphanumeric() && c != '_')
        .unwrap_or(trimmed.len());
    trimmed[..end].to_owned()
}

fn matching_brace(text: &str) -> Option<usize> {
    let mut depth = 0usize;
    for (offset, ch) in text.char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(offset);
                }
            }
            _ => {}
        }
    }
    None
}

fn inside_string_literal(source: &str, index: usize) -> bool {
    let line_start = source[..index].rfind('\n').map_or(0, |n| n + 1);
    let mut quotes = 0usize;
    let mut escaped = false;
    for ch in source[line_start..index].chars() {
        if escaped {
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
        } else if ch == '"' {
            quotes += 1;
        }
    }
    quotes % 2 == 1
}

fn imported_layers(source: &str) -> Vec<String> {
    let mut found = Vec::new();
    for (index, _) in source.match_indices("crate::") {
        if inside_string_literal(source, index) {
            continue;
        }
        let rest = &source[index + "crate::".len()..];
        if rest.starts_with('{') {
            let Some(close) = matching_brace(rest) else {
                continue;
            };
            for entry in rest[1..close].split(',') {
                let segment = leading_identifier(entry);
                if !segment.is_empty() {
                    found.push(segment);
                }
            }
            continue;
        }
        let segment = leading_identifier(rest);
        if !segment.is_empty() {
            found.push(segment);
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
    let mut root_relative = Vec::new();
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
        if source.contains("super::super") {
            root_relative.push(relative.display().to_string());
        }
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

    root_relative.sort();
    assert!(
        root_relative.is_empty(),
        "`super::super` reaches the crate root and hides the layer it lands in; use a `crate::` path instead:\n{}",
        root_relative.join("\n")
    );

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
    "delog-cache/src/trace.rs",
    "delog-parsers/src/ardupilot.rs",
    "delog-parsers/src/ulog.rs",
    "delog-script/src/api.rs",
    "delog-script/src/engine.rs",
    "delog-script/src/operations/live.rs",
];

enum Scan {
    Code,
    LineComment,
    BlockComment(usize),
    Str,
    RawStr(usize),
}

fn code_lines(source: &str) -> Vec<String> {
    let chars: Vec<char> = source.chars().collect();
    let mut out = vec![String::new(); source.lines().count()];
    let mut mode = Scan::Code;
    let mut line = 0usize;
    let mut index = 0usize;
    let mut skip = 0usize;
    while index < chars.len() {
        let ch = chars[index];
        if skip > 0 {
            skip -= 1;
            if ch == '\n' {
                line += 1;
            }
            index += 1;
            continue;
        }
        if ch == '\n' {
            line += 1;
            if matches!(mode, Scan::LineComment) {
                mode = Scan::Code;
            }
            index += 1;
            continue;
        }
        let next = chars.get(index + 1).copied();
        match mode {
            Scan::LineComment => index += 1,
            Scan::Code => {
                if ch == '/' && next == Some('/') {
                    mode = Scan::LineComment;
                    index += 1;
                } else if ch == '/' && next == Some('*') {
                    mode = Scan::BlockComment(1);
                    skip = 1;
                    index += 1;
                } else if ch == '"' {
                    mode = Scan::Str;
                    index += 1;
                } else if ch == 'r' && matches!(next, Some('"') | Some('#')) {
                    let mut scan = index + 1;
                    let mut hashes = 0usize;
                    while chars.get(scan) == Some(&'#') {
                        hashes += 1;
                        scan += 1;
                    }
                    if chars.get(scan) == Some(&'"') {
                        mode = Scan::RawStr(hashes);
                        index = scan + 1;
                    } else {
                        push_code(&mut out, line, ch);
                        index += 1;
                    }
                } else if ch == '\'' && next == Some('\\') {
                    let mut scan = index + 2;
                    while scan < chars.len() && chars[scan] != '\'' && chars[scan] != '\n' {
                        scan += 1;
                    }
                    index = if chars.get(scan) == Some(&'\'') {
                        scan + 1
                    } else {
                        scan
                    };
                } else if ch == '\'' && chars.get(index + 2) == Some(&'\'') {
                    index += 3;
                } else {
                    push_code(&mut out, line, ch);
                    index += 1;
                }
            }
            Scan::Str => {
                if ch == '\\' {
                    skip = 1;
                } else if ch == '"' {
                    mode = Scan::Code;
                }
                index += 1;
            }
            Scan::RawStr(hashes) => {
                let closes = ch == '"'
                    && (1..=hashes).all(|offset| chars.get(index + offset) == Some(&'#'));
                if closes {
                    mode = Scan::Code;
                    index += 1 + hashes;
                } else {
                    index += 1;
                }
            }
            Scan::BlockComment(depth) => {
                if ch == '/' && next == Some('*') {
                    mode = Scan::BlockComment(depth + 1);
                    skip = 1;
                } else if ch == '*' && next == Some('/') {
                    mode = if depth == 1 {
                        Scan::Code
                    } else {
                        Scan::BlockComment(depth - 1)
                    };
                    skip = 1;
                }
                index += 1;
            }
        }
    }
    out
}

fn push_code(out: &mut [String], line: usize, ch: char) {
    if let Some(text) = out.get_mut(line) {
        text.push(ch);
    }
}

fn is_test_attribute(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed == "#[cfg(test)]" || trimmed.starts_with("#[cfg(all(test")
}

fn end_of_test_item(code: &[String], attribute: usize) -> usize {
    let mut start = attribute + 1;
    while start < code.len() && code[start].trim_start().starts_with('#') {
        start += 1;
    }
    if start >= code.len() {
        return attribute + 1;
    }
    let mut depth = 0i32;
    let mut opened = false;
    let mut index = start;
    while index < code.len() {
        let text = code[index].as_str();
        depth += text.matches('{').count() as i32;
        depth -= text.matches('}').count() as i32;
        if text.contains('{') {
            opened = true;
        }
        if opened && depth <= 0 {
            return index + 1;
        }
        let trimmed = text.trim_end();
        if !opened && (trimmed.ends_with(';') || (index == start && trimmed.ends_with(','))) {
            return index + 1;
        }
        index += 1;
    }
    attribute + 1
}

fn implementation_lines(source: &str) -> usize {
    let lines: Vec<&str> = source.lines().collect();
    let code = code_lines(source);
    let mut count = 0;
    let mut index = 0;
    while index < lines.len() {
        if is_test_attribute(lines[index]) {
            index = end_of_test_item(&code, index);
            continue;
        }
        count += 1;
        index += 1;
    }
    count
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
