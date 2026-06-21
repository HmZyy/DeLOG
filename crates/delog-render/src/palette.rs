//! Trace color palette, the single source for plots, legend dots
//! and 3D paths. The constants live in `assets/palette.rs` and are `include!`d
//! here so `delog-app` can reach them through `delog_render::palette` without a
//! crate of its own — see that file's header for why the form is sRGB `u8`.

include!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../assets/palette.rs"
));
