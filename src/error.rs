//! Errors and problems.
//!
//! An [`Error`] stops an operation (a document that does not parse, a run
//! that fails). A [`Problem`] is one finding of a check that keeps going
//! (`Document::lint`, `Description::check`): the caller gets them all.

use std::fmt;

/// A failure with a message meant for the person reading the scenario.
///
/// Returned by every fallible public function of the crate; looks like
/// `Error("/workflows/0/steps/1: step s: channelPath requires action (send|receive)")`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error(pub String);

/// `std::result::Result` with [`Error`].
pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    /// Joins several problems into one error, one per line.
    ///
    /// The text is one `<path>: <message>` per line.
    ///
    /// # Examples
    ///
    /// ```
    /// use arazzo_jrpc::{Error, Problem};
    ///
    /// let e = Error::from_problems(&[Problem::new("/workflows/0", "no steps"), Problem::new("", "no info")]);
    /// assert_eq!(e.to_string(), "/workflows/0: no steps\nno info");
    /// ```
    pub fn from_problems(problems: &[Problem]) -> Self {
        Error(problems.iter().map(Problem::to_string).collect::<Vec<_>>().join("\n"))
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

impl From<String> for Error {
    fn from(s: String) -> Self {
        Error(s)
    }
}

impl From<&str> for Error {
    fn from(s: &str) -> Self {
        Error(s.to_string())
    }
}

/// One finding, located by a JSON Pointer into the document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    pub path: String,
    pub message: String,
}

impl Problem {
    /// Builds a finding at a JSON Pointer path (`""` for the whole document).
    ///
    /// # Examples
    ///
    /// ```
    /// use arazzo_jrpc::Problem;
    ///
    /// assert_eq!(Problem::new("/info", "no title").to_string(), "/info: no title");
    /// assert_eq!(Problem::new("", "not an object").to_string(), "not an object");
    /// ```
    pub fn new(path: impl Into<String>, message: impl Into<String>) -> Self {
        Problem {
            path: path.into(),
            message: message.into(),
        }
    }
}

/// `"<path>: <message>"`, or just the message when the path is empty.
impl fmt::Display for Problem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.path.is_empty() {
            f.write_str(&self.message)
        } else {
            write!(f, "{}: {}", self.path, self.message)
        }
    }
}
