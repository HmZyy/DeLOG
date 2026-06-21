//! The `LogParser` trait, format sniffing, and the detection registry.

use std::error::Error;
use std::fmt;
use std::io::{self, Read, Seek};
use std::sync::Arc;

use delog_core::ingest::{IngestSink, ParseSummary};
use delog_core::parse_ctl::ParseCtl;

/// Bytes of file head handed to [`LogParser::sniff`].
pub const SNIFF_HEAD_LEN: usize = 4096;

/// Minimum top score for confident auto-detection; below this the UI raises the
/// manual-override picker.
pub const SNIFF_CONFIDENCE: u8 = 60;

/// A parser's confidence that it can read a given file head.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sniff {
    /// `0..=100`; 0 means "definitely not mine".
    pub score: u8,
    pub reason: &'static str,
}

impl Sniff {
    pub const fn new(score: u8, reason: &'static str) -> Self {
        Self { score, reason }
    }

    /// A definitive non-match.
    pub const fn no() -> Self {
        Self {
            score: 0,
            reason: "no match",
        }
    }
}

/// Combined object-safe bound for parse input: a trait object cannot name two
/// non-auto traits (`Read` + `Seek`) directly, so we alias them here.
pub trait ReadSeek: Read + Seek + Send {}
impl<T: Read + Seek + Send> ReadSeek for T {}

/// A log-format parser. Implementors are stateless and shared behind an
/// `Arc` in the [`ParserRegistry`].
pub trait LogParser: Send + Sync {
    /// Stable, machine-usable name (also the manual-override key).
    fn name(&self) -> &'static str;

    /// Score this file head. Reads at most [`SNIFF_HEAD_LEN`] bytes.
    fn sniff(&self, head: &[u8]) -> Sniff;

    /// Parse `src` into `sink`, honouring `ctl` for progress and cancellation.
    /// Malformed *records* are skipped with a diagnostic; only
    /// unrecoverable framing corruption returns `Err`.
    fn parse(
        &self,
        src: Box<dyn ReadSeek>,
        sink: &mut dyn IngestSink,
        ctl: &ParseCtl,
    ) -> Result<ParseSummary, ParseError>;
}

/// Failure modes of a parse. Only framing/IO failures abort; record
/// corruption is a diagnostic, not an error.
#[derive(Debug)]
pub enum ParseError {
    Io(io::Error),
    /// The bytes are not in the format this parser handles.
    UnsupportedFormat {
        detail: String,
    },
    /// `ctl` requested cancellation; partial data already submitted is kept.
    Cancelled,
    /// Unrecoverable framing corruption at a byte offset.
    Framing {
        byte_offset: u64,
        detail: String,
    },
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(err) => write!(f, "io error: {err}"),
            Self::UnsupportedFormat { detail } => write!(f, "unsupported format: {detail}"),
            Self::Cancelled => write!(f, "parse cancelled"),
            Self::Framing {
                byte_offset,
                detail,
            } => {
                write!(f, "framing corruption at byte {byte_offset}: {detail}")
            }
        }
    }
}

impl Error for ParseError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(err) => Some(err),
            _ => None,
        }
    }
}

impl From<io::Error> for ParseError {
    fn from(err: io::Error) -> Self {
        Self::Io(err)
    }
}

/// One scored parser, returned to the manual-override picker on ambiguity.
#[derive(Clone)]
pub struct Candidate {
    pub parser: Arc<dyn LogParser>,
    pub sniff: Sniff,
}

impl fmt::Debug for Candidate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Candidate")
            .field("parser", &self.parser.name())
            .field("sniff", &self.sniff)
            .finish()
    }
}

/// Outcome of running every parser's `sniff` over a file head.
#[derive(Clone)]
pub enum Detection {
    /// A confident, unambiguous winner — open it directly.
    Auto(Arc<dyn LogParser>),
    /// A tie at the top, or a top score below [`SNIFF_CONFIDENCE`]: the UI must
    /// raise the manual-override picker. Candidates are sorted best-first.
    Ambiguous(Vec<Candidate>),
    /// Nothing recognised the head at all.
    Unknown,
}

impl fmt::Debug for Detection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Auto(p) => f.debug_tuple("Auto").field(&p.name()).finish(),
            Self::Ambiguous(c) => f.debug_tuple("Ambiguous").field(c).finish(),
            Self::Unknown => f.write_str("Unknown"),
        }
    }
}

/// The set of registered parsers and the auto-detection entry point.
#[derive(Default)]
pub struct ParserRegistry {
    parsers: Vec<Arc<dyn LogParser>>,
}

impl ParserRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, parser: Arc<dyn LogParser>) {
        self.parsers.push(parser);
    }

    pub fn parsers(&self) -> &[Arc<dyn LogParser>] {
        &self.parsers
    }

    /// Resolve a manual-override choice by parser name.
    pub fn by_name(&self, name: &str) -> Option<Arc<dyn LogParser>> {
        self.parsers
            .iter()
            .find(|p| p.name() == name)
            .map(Arc::clone)
    }

    /// Run every parser's `sniff` over `head` and decide:
    /// the unique top scorer ≥ [`SNIFF_CONFIDENCE`] auto-opens; a tie at the top
    /// or a low top score is [`Detection::Ambiguous`]; nothing scoring is
    /// [`Detection::Unknown`].
    pub fn detect(&self, head: &[u8]) -> Detection {
        let mut candidates: Vec<Candidate> = self
            .parsers
            .iter()
            .map(|parser| Candidate {
                parser: Arc::clone(parser),
                sniff: parser.sniff(head),
            })
            .filter(|c| c.sniff.score > 0)
            .collect();
        candidates.sort_by_key(|c| std::cmp::Reverse(c.sniff.score));

        match candidates.as_slice() {
            [] => Detection::Unknown,
            [top, ..] if top.sniff.score < SNIFF_CONFIDENCE => Detection::Ambiguous(candidates),
            [top, second, ..] if top.sniff.score == second.sniff.score => {
                Detection::Ambiguous(candidates)
            }
            [top, ..] => Detection::Auto(Arc::clone(&top.parser)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Stub {
        name: &'static str,
        score: u8,
    }
    impl LogParser for Stub {
        fn name(&self) -> &'static str {
            self.name
        }
        fn sniff(&self, _head: &[u8]) -> Sniff {
            Sniff::new(self.score, "stub")
        }
        fn parse(
            &self,
            _src: Box<dyn ReadSeek>,
            _sink: &mut dyn IngestSink,
            _ctl: &ParseCtl,
        ) -> Result<ParseSummary, ParseError> {
            Ok(ParseSummary::default())
        }
    }

    fn registry(parsers: &[(&'static str, u8)]) -> ParserRegistry {
        let mut reg = ParserRegistry::new();
        for &(name, score) in parsers {
            reg.register(Arc::new(Stub { name, score }));
        }
        reg
    }

    #[test]
    fn unique_confident_winner_auto_detects() {
        let reg = registry(&[("bin", 90), ("ulog", 10), ("tlog", 0)]);
        match reg.detect(b"head") {
            Detection::Auto(p) => assert_eq!(p.name(), "bin"),
            other => panic!("expected auto, got {other:?}"),
        }
    }

    #[test]
    fn a_tie_at_the_top_is_ambiguous() {
        let reg = registry(&[("bin", 80), ("ulog", 80)]);
        match reg.detect(b"head") {
            Detection::Ambiguous(c) => {
                assert_eq!(c.len(), 2);
                assert_eq!(c[0].sniff.score, 80);
            }
            other => panic!("expected ambiguous, got {other:?}"),
        }
    }

    #[test]
    fn a_low_top_score_is_ambiguous() {
        let reg = registry(&[("bin", 40), ("ulog", 20)]);
        match reg.detect(b"head") {
            Detection::Ambiguous(c) => {
                // Sorted best-first, only the scoring parsers.
                assert_eq!(c[0].parser.name(), "bin");
                assert_eq!(c.len(), 2);
            }
            other => panic!("expected ambiguous, got {other:?}"),
        }
    }

    #[test]
    fn nothing_scoring_is_unknown() {
        let reg = registry(&[("bin", 0), ("ulog", 0)]);
        assert!(matches!(reg.detect(b"head"), Detection::Unknown));
    }

    #[test]
    fn by_name_resolves_a_manual_override() {
        let reg = registry(&[("bin", 0), ("ulog", 0)]);
        assert_eq!(reg.by_name("ulog").unwrap().name(), "ulog");
        assert!(reg.by_name("missing").is_none());
    }
}
