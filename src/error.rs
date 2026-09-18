//! Everything that can go wrong, in one enum.

use core::fmt;

use shep_client::{ConnectError, RequestError};

// Declared whole now because it is this crate's own interface, and every
// later module that touches the shepherd depends on it existing already
// rather than growing its own ad hoc error type.
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// The first connection, or a reconnect the supervisor gave up on.
    ///
    /// Live already, through `impl From<ConnectError> for Error`, even
    /// though nothing in `main` calls it yet: dead-code analysis counts a
    /// variant constructed inside a trait impl as constructed, whether or
    /// not the impl itself is ever invoked.
    Connect(ConnectError),
    /// A request the shepherd refused or could not answer.
    ///
    /// Live for the same reason as `Connect`, through
    /// `impl From<RequestError> for Error`.
    Request(RequestError),
    /// The shepherd answered, with something else. Names both sides,
    /// because "unexpected response" alone sends the reader to the wrong
    /// end of the wire.
    ///
    /// Constructed in `shepherd::unexpected`, the one place this crate
    /// reads a `Response`.
    Unexpected { asked: String, got: String },
    /// `dogs.toml`'s `[discord]` section did not parse, or a value in it
    /// is outside what this dog accepts.
    Config(String),
    /// Discord itself refused or could not answer a request a command
    /// made once the gateway came up: a broken token, a permission the
    /// guild never granted the bot, or the request otherwise failing.
    ///
    /// Nothing needed this before a `Command` existed with a reason to
    /// call Discord back: `Connect` and `Request` above are the shepherd
    /// side of this dog, and this is the same wrapping for the other
    /// side, through `impl From<serenity::Error> for Error`.
    ///
    /// Boxed rather than inline: `serenity::Error` is over 100 bytes on
    /// its own, and an unboxed variant that size would make every `Result<
    /// _, Error>` in this crate pay for the biggest variant on every
    /// return, `Ok` included, whether or not it ever carries a Discord
    /// error.
    Discord(Box<serenity::Error>),
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
            Self::Discord(err) => write!(f, "Discord refused a request: {err}"),
        }
    }
}

impl core::error::Error for Error {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Connect(err) => Some(err),
            Self::Request(err) => Some(err),
            Self::Discord(err) => Some(err),
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

impl From<serenity::Error> for Error {
    fn from(err: serenity::Error) -> Self {
        Self::Discord(Box::new(err))
    }
}
