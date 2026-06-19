# Centered Non-Collapsible Popups Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make every floating `egui::Window` non-collapsible and centered on its first spawn without preventing later dragging or remembered positioning.

**Architecture:** Keep the policy explicit on every existing window builder with `collapsible(false)`, a viewport-center `default_pos`, and a center pivot. Add a source-level regression test that counts window builders and each required policy call across all popup-owning modules, ensuring future windows cannot silently omit the policy.

**Tech Stack:** Rust 2024, egui/eframe, Cargo tests

---

### Task 1: Add the popup-policy regression test

**Files:**
- Create: `crates/delog-app/tests/popup_policy.rs`

- [x] **Step 1: Write the failing policy test**

```rust
const POPUP_SOURCES: &[&str] = &[
    include_str!("../src/app.rs"),
    include_str!("../src/browser.rs"),
    include_str!("../src/generate_markers.rs"),
    include_str!("../src/live.rs"),
    include_str!("../src/scripts.rs"),
    include_str!("../src/settings.rs"),
    include_str!("../src/vehicle_dialog.rs"),
    include_str!("../src/workspace.rs"),
];

fn occurrence_count(needle: &str) -> usize {
    POPUP_SOURCES
        .iter()
        .map(|source| source.matches(needle).count())
        .sum()
}

#[test]
fn every_popup_is_non_collapsible_and_centered_by_default() {
    let popup_count = occurrence_count("egui::Window::new(");

    assert_eq!(occurrence_count(".collapsible(false)"), popup_count);
    assert_eq!(
        occurrence_count(".default_pos(ctx.content_rect().center())")
            + occurrence_count(".default_pos(ui.ctx().content_rect().center())"),
        popup_count
    );
    assert_eq!(
        occurrence_count(".pivot(egui::Align2::CENTER_CENTER)"),
        popup_count
    );
}
```

- [x] **Step 2: Run the test and confirm the RED state**

Run: `cargo test -p delog-app --test popup_policy`

Expected: the test fails because only a subset of the 14 existing windows has `.collapsible(false)` and none has both centering calls.

### Task 2: Apply the policy to all popup builders

**Files:**
- Modify: `crates/delog-app/src/app.rs`
- Modify: `crates/delog-app/src/browser.rs`
- Modify: `crates/delog-app/src/generate_markers.rs`
- Modify: `crates/delog-app/src/live.rs`
- Modify: `crates/delog-app/src/scripts.rs`
- Modify: `crates/delog-app/src/settings.rs`
- Modify: `crates/delog-app/src/vehicle_dialog.rs`
- Modify: `crates/delog-app/src/workspace.rs`

- [x] **Step 1: Add the three builder settings to every window**

Immediately after each window's `.open(...)` or `.id(...)` configuration, ensure the builder contains:

```rust
.collapsible(false)
.default_pos(ctx.content_rect().center())
.pivot(egui::Align2::CENTER_CENTER)
```

For builders whose function receives `ui` instead of `ctx`, use the equivalent:

```rust
.collapsible(false)
.default_pos(ui.ctx().content_rect().center())
.pivot(egui::Align2::CENTER_CENTER)
```

Do not change window IDs, open state, sizes, resizability, or contents.

- [x] **Step 2: Run the focused test and confirm the GREEN state**

Run: `cargo test -p delog-app --test popup_policy`

Expected: one test passes.

- [x] **Step 3: Format and inspect all popup builders**

Run: `cargo fmt --all`

Run: `rg -n -C 8 "egui::Window::new" crates/delog-app/src --glob '*.rs'`

Expected: all 14 builders visibly contain the three policy calls.

### Task 3: Verify and complete UIX-14

**Files:**
- Modify: `PLAN.md`

- [x] **Step 1: Run application and workspace verification**

Run: `cargo test -p delog-app`

Expected: all `delog-app` tests pass.

Run: `cargo check --workspace`

Expected: workspace compilation succeeds.

Run: `cargo clippy --workspace -- -D warnings`

Expected: clippy succeeds without warnings.

- [x] **Step 2: Mark the checklist item complete**

Change the UIX-14 entry to:

```markdown
- [x] **UIX-14** — All floating `egui::Window` popups are non-collapsible and default to the center of the viewport on first spawn while remaining freely draggable; enforced by `popup_policy` regression coverage
```

- [x] **Step 3: Commit the implementation**

```bash
git add PLAN.md crates/delog-app/src crates/delog-app/tests/popup_policy.rs docs/superpowers/plans/2026-06-19-centered-non-collapsible-popups.md
git commit -m "Center all popup windows by default"
```
