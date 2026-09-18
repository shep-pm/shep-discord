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

use serenity::all::{ChannelId, CreateEmbed, CreateMessage, Http};

use crate::stream::{Sink, SinkError, pack::Chunk};

/// A [`Sink`] backed by Discord's HTTP API.
///
/// Holds an [`Http`] rather than a full [`serenity::Client`]: sending a
/// message needs a bot token and a route, and both live on [`Http`] alone,
/// so this type never opens the gateway connection a `Client` would.
pub struct DiscordSink {
    http: Http,
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
}
