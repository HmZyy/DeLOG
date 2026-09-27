use std::error::Error;
use std::io;

use delog_parsers::ParseError;

#[test]
fn parser_io_error_keeps_conversion_message_and_source() {
    let error = ParseError::from(io::Error::new(io::ErrorKind::NotFound, "missing"));
    assert_eq!(error.to_string(), "io error: missing");
    assert_eq!(
        error.source().map(ToString::to_string),
        Some("missing".into())
    );
}

#[test]
fn parser_non_io_errors_keep_messages_without_sources() {
    let cases = [
        (
            ParseError::Setup {
                detail: "config".into(),
            },
            "parser setup failed: config",
        ),
        (ParseError::SetupCancelled, "parser setup cancelled"),
        (
            ParseError::UnsupportedFormat { detail: "X".into() },
            "unsupported format: X",
        ),
        (ParseError::Cancelled, "parse cancelled"),
        (
            ParseError::Framing {
                byte_offset: 42,
                detail: "bad header".into(),
            },
            "framing corruption at byte 42: bad header",
        ),
    ];
    for (error, expected) in cases {
        assert_eq!(error.to_string(), expected);
        assert!(error.source().is_none());
    }
}
