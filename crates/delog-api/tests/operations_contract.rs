use delog_api::operations::OperationMode;

#[test]
fn operation_mode_defaults_and_preserves_unknown_mode_errors() {
    assert_eq!(OperationMode::parse(None).unwrap(), OperationMode::Both);
    assert_eq!(
        OperationMode::parse(Some("stream"))
            .unwrap_err()
            .to_string(),
        "mode must be 'snapshot', 'live', or 'both', got 'stream'"
    );
}
