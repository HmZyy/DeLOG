use delog_api::{Error, ErrorKind, MutationCompletion};

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

#[test]
fn typed_categories_and_completion_are_public_and_stable() {
    let cases = [
        (Error::stale_handle("plot is gone"), ErrorKind::StaleHandle),
        (Error::forbidden("manual plot"), ErrorKind::Forbidden),
        (Error::conflict("name exists"), ErrorKind::Conflict),
        (Error::internal("invariant"), ErrorKind::Internal),
    ];
    for (error, kind) in cases {
        assert_eq!(error.kind(), kind);
    }

    let error = Error::unavailable("timed out")
        .with_completion(MutationCompletion::Unknown)
        .with_context("control request 2");
    assert_eq!(error.kind(), ErrorKind::Unavailable);
    assert_eq!(error.completion(), Some(MutationCompletion::Unknown));
    assert_eq!(error.to_string(), "control request 2: timed out");
}
