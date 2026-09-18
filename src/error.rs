//! Everything that can go wrong, in one enum.

use core::fmt;

use shep_client::{ConnectError, RequestError};

// Nothing builds or matches an `Error` yet: the socket connection that
// raises `Connect`/`Request`, and the parser that raises `Config`, both
// land in later commits. Declared now because it is this task's own
// interface, and every later module that touches the shepherd depends on
// it existing already rather than growing its own ad hoc error type.
#[allow(
    dead_code,
    reason = "constructed once the socket connection and the config parser land"
)]
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// The first connection, or a reconnect the supervisor gave up on.
    Connect(ConnectError),
    /// A request the shepherd refused or could not answer.
    Request(RequestError),
    /// The shepherd answered, with something else. Names both sides,
    /// because "unexpected response" alone sends the reader to the wrong
    /// end of the wire.
    Unexpected { asked: String, got: String },
    /// `dogs.toml`'s `[discord]` section did not parse, or a value in it
    /// is outside what this dog accepts.
    Config(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Connect(err) => write!(f, "cannot reach the shepherd: {err}"),
            Self::Request(err) => write!(f, "the shepherd refused a request: {err}"),
            Self::Unexpected { asked, got } => {
                write!(f, "asked the shepherd for {asked} and got {got}")
            }
            Self::Config(message) => write!(f, "[discord] in dogs.toml: {message}"),
        }
    }
}

impl core::error::Error for Error {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Connect(err) => Some(err),
            Self::Request(err) => Some(err),
            Self::Unexpected { .. } | Self::Config(_) => None,
        }
    }
}

impl From<ConnectError> for Error {
    fn from(err: ConnectError) -> Self {
        Self::Connect(err)
    }
}

impl From<RequestError> for Error {
    fn from(err: RequestError) -> Self {
        Self::Request(err)
    }
}
