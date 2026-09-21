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
    /// This dog's own section of `dogs.toml` did not parse, or a value in
    /// it is outside what this dog accepts.
    ///
    /// A section that is WRONG, as against one that is merely empty; see
    /// [`Self::Unconfigured`] for why the two cannot share a variant.
    /// Nothing here clears on its own, because the only thing that would
    /// clear it is an operator editing the file.
    ///
    /// Carries the problem and not the section name, because the section
    /// is whatever name this dog was adopted under: `[discord]` only by
    /// default. Whoever prints this knows that name and says it:
    /// `crate::run::misconfigured_message` for this variant, which is the
    /// arm `crate::run::on_config_failure` exits on, and
    /// `crate::run::config_failed_message` for the arms it stays up for.
    Config(String),
    /// This dog's own section of `dogs.toml` has not been filled in yet:
    /// a key this dog cannot default is absent.
    ///
    /// Split out of [`Self::Config`] because the two ask opposite things
    /// of the run loop, and one flat variant covering both is what made
    /// the obvious fix wrong. A section that is wrong stays wrong until
    /// somebody edits it, so retrying is an infinite run of the same
    /// failure and `crate::run` exits on it. A section that is merely
    /// empty is what every freshly adopted dog has: `shep adopt` vets,
    /// registers, enables and starts in one command, so an absent `token`
    /// is the state of every first run before an operator has typed one.
    /// Exiting for that would spend the shepherd's restart budget in
    /// seconds and land this dog `Errored` before it could be configured
    /// at all, which is the opposite of the order shep's own docs ask an
    /// operator for.
    ///
    /// Carries the problem and not the section name, for the same reason
    /// [`Self::Config`] does.
    Unconfigured(String),
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
            Self::Config(message) | Self::Unconfigured(message) => write!(f, "{message}"),
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
            Self::Unexpected { .. } | Self::Config(_) | Self::Unconfigured(_) => None,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::assert_no_dashes;

    /// Every arm of this enum reaches a Discord followup, through
    /// `bot::interaction::report_failure`, and reaches a terminal through
    /// the run loop's own `eprintln!`. That makes all six person-facing
    /// strings, and this module had no test at all until now.
    ///
    /// The three wrapping arms are built from a real inner error rather
    /// than a stand-in, since what they interpolate is the other crate's
    /// wording as much as this one's: a dash arriving from `shep_client`
    /// or from serenity prints exactly the same replacement character in
    /// exactly the same place.
    #[test]
    fn no_arm_of_this_enum_carries_a_dash_when_it_is_printed() {
        let errors = [
            Error::Connect(shep_client::ConnectError::HandshakeClosed),
            Error::Request(shep_client::RequestError::Closed),
            Error::Unexpected {
                asked: "a Flock".to_owned(),
                got: "a Pong".to_owned(),
            },
            Error::Config("buffer_lines must be at least 1".to_owned()),
            Error::Unconfigured("token is required".to_owned()),
            Error::Discord(Box::new(serenity::Error::Other("refused"))),
        ];
        for err in &errors {
            assert_no_dashes(&err.to_string());
        }
    }
}
