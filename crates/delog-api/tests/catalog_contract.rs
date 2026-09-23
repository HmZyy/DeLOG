use delog_api::color::{format_hex_color, parse_hex_color};
use delog_api::markers::PendingMarker;
use delog_api::timestamps::{AlignmentMode, TimestampMode, align_values};

#[test]
fn hexadecimal_colors_preserve_rgb_and_default_alpha() {
    assert_eq!(
        parse_hex_color("#FF8000").unwrap(),
        [1.0, 128.0 / 255.0, 0.0, 1.0]
    );
    assert_eq!(
        parse_hex_color("#FF800040").unwrap(),
        [1.0, 128.0 / 255.0, 0.0, 64.0 / 255.0]
    );
}

#[test]
fn malformed_colors_preserve_the_validation_message() {
    assert_eq!(
        parse_hex_color("orange").unwrap_err().to_string(),
        "marker color must be #RRGGBB or #RRGGBBAA"
    );
}

#[test]
fn timestamp_mode_defaults_to_effective_time() {
    assert_eq!(TimestampMode::default(), TimestampMode::Effective);
}

#[test]
fn color_formatting_and_pending_markers_share_the_native_parser() {
    let marker =
        PendingMarker::new(42, "launch".into(), Some("#11223344"), Some("note".into())).unwrap();
    assert_eq!(
        marker.color,
        Some([17.0 / 255.0, 34.0 / 255.0, 51.0 / 255.0, 68.0 / 255.0])
    );
    assert_eq!(marker.note, "note");
    assert_eq!(format_hex_color(marker.color.unwrap()), "#11223344");
    assert_eq!(
        PendingMarker::new(0, String::new(), None, None)
            .unwrap_err()
            .to_string(),
        "marker label must not be empty"
    );
}

#[test]
fn alignment_modes_parse_and_delegate_to_the_core_algorithm() {
    assert_eq!(AlignmentMode::parse("prev").unwrap(), AlignmentMode::Prev);
    assert_eq!(
        AlignmentMode::parse("cubic").unwrap_err().to_string(),
        "align mode must be 'prev', 'nearest', or 'linear'"
    );
    assert_eq!(
        align_values(&[10, 20], &[1.0, 3.0], &[15], AlignmentMode::Linear),
        vec![2.0]
    );
}
