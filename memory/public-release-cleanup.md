---
name: public-release-cleanup
description: PLAN.md and internal dev docs were removed; codebase prepped for public open-source release
metadata:
  type: project
---

On 2026-06-21 the repo was cleaned for public open-source release (branch `custom_parsers`):

- Deleted `PLAN.md`, `BUGS.md`, `AGENTS.md`, `CLAUDE.md`, and all of `docs/superpowers/`. Kept `docs/scripting.md` and `assets/README.md`.
- Stripped all internal references from comments across the workspace: `PLAN.md` mentions, `§N.N` section refs, checklist IDs (e.g. `CORE-07`, `PLT-02`, `ANA-10`), and `ZC-N` zero-copy invariant tags — keeping the explanatory prose, dropping only the tokens. Also cleaned `.wgsl` shaders, `Cargo.toml`s, `rust-toolchain.toml`.

**Why:** prepping for public release; the internal planning/checklist artifacts don't belong in a public repo.

**How to apply:** PLAN.md / the §22 checklist / the ZC-N citation convention no longer exist in this tree — don't look for them or try to update a checklist. Earlier memories that call PLAN.md "the single source of truth" are stale. The cleanup left code logic untouched (comment/doc-only edits); clippy, fmt, and all tests pass.
