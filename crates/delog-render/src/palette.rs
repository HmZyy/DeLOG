//! Trace color palette, `include!`d from `assets/palette.rs` (keep that file
//! `include!`-safe — no leading `//!`).

include!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../assets/palette.rs"
));
