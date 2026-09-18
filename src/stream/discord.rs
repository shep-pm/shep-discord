//! The one [`Sink`] that actually reaches Discord.
//!
//! Log streaming needs Discord's REST API and nothing else: no gateway, no
//! shard, no event loop. `serenity::http::Http::new` (`http/client.rs:208`
//! in the vendored 0.12.5 source, itself calling `HttpBuilder::new` at
//! `:67`) builds a client that opens no gateway connection, and `impl
//! CacheHttp for Http` (`http/mod.rs:118`, alongside the blanket `impl<T>
//! CacheHttp for &T` just above it) is what makes `&Http` acceptable
//! wherever [`serenity::model::id::ChannelId::send_message`] asks for
//! `impl CacheHttp`. Nothing here builds a [`serenity::Client`] or opens a
//! gateway; that is Task 11's job, once slash commands need one.

use core::fmt;

use serenity::all::{ChannelId, CreateEmbed, CreateMessage, Http};

use crate::stream::{Sink, SinkError, pack::Chunk};

/// A marker whose `Debug` always prints the same placeholder, never the
/// value it stands in for.
struct Redacted;

impl fmt::Debug for Redacted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<redacted>")
    }
}

/// A [`Sink`] backed by Discord's HTTP API.
///
/// Holds an [`Http`] rather than a full [`serenity::Client`]: sending a
/// message needs a bot token and a route, and both live on [`Http`] alone,
/// so this type never opens the gateway connection a `Client` would.
///
/// `Debug` is hand-written rather than derived, the same reason
/// [`crate::config::Config`]'s is. `Http` holds the bot token this dog was
/// built with behind `secrecy::SecretString`, whose own `Debug` already
/// redacts it (nothing in this crate proves that; it is serenity's
/// contract, not this crate's), so nothing leaks today. But that safety
/// sits in a dependency this crate does not test, and a derived `Debug`
/// here would put whatever `Http`'s own `Debug` prints into any log line or
/// panic message that renders a `DiscordSink` in an error chain. Pinned by
/// `the_token_never_reaches_a_debug_line`, because a later
/// `#[derive(Debug)]` here would be a silent regression.
pub struct DiscordSink {
    http: Http,
}

impl fmt::Debug for DiscordSink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DiscordSink")
            .field("http", &Redacted)
            .finish()
    }
}

impl DiscordSink {
    /// Build a sink authenticated as the bot `token` names.
    ///
    /// `Http::new` never makes a network call of its own; the token is
    /// only checked against Discord the first time [`Sink::send`] uses it.
    #[must_use]
    pub fn new(token: &str) -> Self {
        Self {
            http: Http::new(token),
        }
    }
}

impl Sink for DiscordSink {
    /// Send one message per call, one embed per [`Chunk`], `title` and
    /// `description` copied straight across: [`crate::stream::pack::chunks`]
    /// and [`crate::stream::pack::into_messages`] already fit both to
    /// Discord's own limits, so nothing here re-derives that budget.
    ///
    /// Every failure `send_message` can return, a rejected shape or
    /// anything else, collapses to [`SinkError::BadRequest`]: see
    /// [`SinkError`]'s own doc for why a rate limit never reaches this far,
    /// and why there is only the one variant left to map onto.
    async fn send(&self, channel: u64, chunks: Vec<Chunk>) -> Result<(), SinkError> {
        let mut message = CreateMessage::new();
        for chunk in chunks {
            message = message.add_embed(
                CreateEmbed::new()
                    .title(chunk.title)
                    .description(chunk.description),
            );
        }
        ChannelId::new(channel)
            .send_message(&self.http, message)
            .await
            .map(|_message| ())
            .map_err(|_err| SinkError::BadRequest)
    }
}

#[cfg(test)]
mod tests {
    use super::DiscordSink;

    /// `send` itself needs a socket, so this crate's tests cannot exercise
    /// it; this is the one thing about `DiscordSink` a test can still
    /// prove, per this module's own doc: `Http::new` makes no network call
    /// of its own, so building a sink cannot fail or block before the
    /// first `send`.
    #[test]
    fn a_sink_builds_without_reaching_the_network() {
        let _sink = DiscordSink::new("fake-token");
    }

    /// A derived `Debug` on a type holding a bot token, even indirectly
    /// through `Http`, puts that token into every log line, panic message
    /// and error chain that prints it. An exact string, not a `contains`:
    /// a redaction that stops covering a newly added field still passes a
    /// `contains` check.
    #[test]
    fn the_token_never_reaches_a_debug_line() {
        let sink = DiscordSink::new("MTIzNDU2Nzg5.GaBcDe.ThisIsNotARealToken");
        let rendered = format!("{sink:?}");
        assert_eq!(rendered, "DiscordSink { http: <redacted> }");
        assert!(!rendered.contains("ThisIsNotARealToken"), "{rendered}");
    }
}
