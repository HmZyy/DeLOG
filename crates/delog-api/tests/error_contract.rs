use delog_api::{Error, ErrorKind};

#[test]
fn typed_errors_preserve_the_exact_message() {
    let error = Error::invalid_input("marker label must not be empty");
    assert_eq!(error.kind(), ErrorKind::InvalidInput);
    assert_eq!(error.to_string(), "marker label must not be empty");
}

#[test]
fn constructors_preserve_categories_and_owned_messages() {
    let cases = [
        (Error::invalid_input("invalid"), ErrorKind::InvalidInput),
        (Error::not_found("missing"), ErrorKind::NotFound),
        (Error::ambiguous("ambiguous"), ErrorKind::Ambiguous),
        (Error::unavailable("unavailable"), ErrorKind::Unavailable),
        (Error::protocol("protocol"), ErrorKind::Protocol),
        (Error::execution("execution"), ErrorKind::Execution),
    ];

    for (error, kind) in cases {
        assert_eq!(error.kind(), kind);
        assert_eq!(error.clone().into_message(), error.to_string());
    }
}
