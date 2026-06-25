//! `CoreError` — the typed error every fallible core path returns (RR21-FR3).
//!
//! The JNI boundary maps a `CoreError` to a stable status code + message the shell
//! surfaces (Error Handling table); the core never panics across JNI. Status codes are
//! part of the JNI contract, so keep their numeric values stable.

use std::fmt;

/// A status code crossing the JNI boundary. `0` is reserved for success; every error
/// variant maps to a distinct non-zero code via [`CoreError::status_code`].
pub type StatusCode = i32;

/// Success status (no error).
pub const STATUS_OK: StatusCode = 0;

/// The typed error surface of `reader-core`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoreError {
    /// A null/zero handle or out-of-range argument crossed the boundary (RR21-FR2).
    InvalidArgument(String),
    /// The file format is unsupported or unrecognized.
    UnsupportedFormat(String),
    /// The document is corrupt or truncated.
    CorruptDocument(String),
    /// The document is DRM-protected; no decrypt is attempted (RR7-FR5).
    DrmProtected,
    /// A requested page index was out of range.
    PageOutOfRange {
        /// The requested index.
        requested: usize,
        /// The number of pages available.
        available: usize,
    },
    /// The destination pixel buffer did not match the expected geometry/stride.
    BufferMismatch(String),
    /// The PDF backend (pdfium) reported a failure.
    RenderBackend(String),
    /// The pdfium library could not be bound (host: no libpdfium present).
    BackendUnavailable(String),
    /// A persistence/storage operation failed (SQLite) (RR12, ADR Decision 4).
    Persistence(String),
    /// A panic was caught at the boundary and converted (RR21-FR3).
    InternalPanic(String),
}

impl CoreError {
    /// The stable status code for this error (non-zero). Kept in sync with the Kotlin side.
    #[must_use]
    pub fn status_code(&self) -> StatusCode {
        match self {
            CoreError::InvalidArgument(_) => 1,
            CoreError::UnsupportedFormat(_) => 2,
            CoreError::CorruptDocument(_) => 3,
            CoreError::DrmProtected => 4,
            CoreError::PageOutOfRange { .. } => 5,
            CoreError::BufferMismatch(_) => 6,
            CoreError::RenderBackend(_) => 7,
            CoreError::BackendUnavailable(_) => 8,
            CoreError::InternalPanic(_) => 9,
            CoreError::Persistence(_) => 10,
        }
    }
}

impl fmt::Display for CoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CoreError::InvalidArgument(m) => write!(f, "invalid argument: {m}"),
            CoreError::UnsupportedFormat(m) => write!(f, "unsupported file type: {m}"),
            CoreError::CorruptDocument(m) => write!(f, "this file appears damaged: {m}"),
            CoreError::DrmProtected => write!(f, "this file is protected and can't be opened"),
            CoreError::PageOutOfRange {
                requested,
                available,
            } => write!(f, "page {requested} out of range (have {available})"),
            CoreError::BufferMismatch(m) => write!(f, "pixel buffer mismatch: {m}"),
            CoreError::RenderBackend(m) => write!(f, "render failed: {m}"),
            CoreError::BackendUnavailable(m) => write!(f, "render backend unavailable: {m}"),
            CoreError::Persistence(m) => write!(f, "storage error: {m}"),
            CoreError::InternalPanic(m) => write!(f, "internal error: {m}"),
        }
    }
}

impl std::error::Error for CoreError {}

/// Map an ink-domain error onto the core surface at the persistence boundary (RR21-FR3): a
/// malformed `.inkbin` is a corrupt document; everything else is a bad argument.
impl From<inkread_ink::InkError> for CoreError {
    fn from(e: inkread_ink::InkError) -> Self {
        match e {
            inkread_ink::InkError::BadEncoding(m) => {
                CoreError::CorruptDocument(format!("ink: {m}"))
            }
            other => CoreError::InvalidArgument(other.to_string()),
        }
    }
}

/// The core's result alias.
pub type CoreResult<T> = Result<T, CoreError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_codes_are_distinct_and_nonzero() {
        let errs = [
            CoreError::InvalidArgument(String::new()),
            CoreError::UnsupportedFormat(String::new()),
            CoreError::CorruptDocument(String::new()),
            CoreError::DrmProtected,
            CoreError::PageOutOfRange {
                requested: 0,
                available: 0,
            },
            CoreError::BufferMismatch(String::new()),
            CoreError::RenderBackend(String::new()),
            CoreError::BackendUnavailable(String::new()),
            CoreError::Persistence(String::new()),
            CoreError::InternalPanic(String::new()),
        ];
        let mut codes: Vec<StatusCode> = errs.iter().map(CoreError::status_code).collect();
        assert!(codes.iter().all(|&c| c != STATUS_OK));
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes.len(), errs.len(), "status codes must be distinct");
    }

    #[test]
    fn display_is_user_facing() {
        assert_eq!(
            CoreError::DrmProtected.to_string(),
            "this file is protected and can't be opened"
        );
    }

    // The status code AND the message prefix of every variant are part of the JNI contract (the
    // Kotlin side switches on the code and surfaces the message). Pin both exactly so a future
    // refactor that renumbers a variant or rewords a message breaks the build, not the device.
    #[test]
    fn status_codes_and_messages_are_pinned() {
        let cases: [(CoreError, StatusCode, &str); 10] = [
            (
                CoreError::InvalidArgument("x".into()),
                1,
                "invalid argument: x",
            ),
            (
                CoreError::UnsupportedFormat("x".into()),
                2,
                "unsupported file type: x",
            ),
            (
                CoreError::CorruptDocument("x".into()),
                3,
                "this file appears damaged: x",
            ),
            (
                CoreError::DrmProtected,
                4,
                "this file is protected and can't be opened",
            ),
            (
                CoreError::PageOutOfRange {
                    requested: 7,
                    available: 3,
                },
                5,
                "page 7 out of range (have 3)",
            ),
            (
                CoreError::BufferMismatch("x".into()),
                6,
                "pixel buffer mismatch: x",
            ),
            (CoreError::RenderBackend("x".into()), 7, "render failed: x"),
            (
                CoreError::BackendUnavailable("x".into()),
                8,
                "render backend unavailable: x",
            ),
            (CoreError::InternalPanic("x".into()), 9, "internal error: x"),
            (CoreError::Persistence("x".into()), 10, "storage error: x"),
        ];
        for (err, code, msg) in &cases {
            assert_eq!(err.status_code(), *code, "code for {err:?}");
            assert_eq!(&err.to_string(), msg, "message for {err:?}");
        }
    }
}
