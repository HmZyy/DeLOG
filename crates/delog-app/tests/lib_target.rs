#[test]
fn crate_is_importable_as_a_library() {
    let icon = delog_app::app_icon();
    assert_eq!(icon.width, 256);
    assert_eq!(icon.height, 256);
    assert_eq!(icon.rgba.len(), 256 * 256 * 4);
}
