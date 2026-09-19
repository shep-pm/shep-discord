//! The bot half of this dog: slash commands, the embed they answer with,
//! and the gateway connection that dispatches both.
//!
//! [`embed`] renders one sheep as a [`serenity`] embed with its action
//! buttons; [`command`] is the contract every slash command implements and
//! the state they all read; [`commands`] is where the concrete commands
//! live; [`interaction`] wires an incoming gateway interaction to one of
//! them; and [`run`] brings the gateway itself up. [`channel`] and
//! [`monitor`] are the live monitor: one message per sheep in a channel of
//! its own, edited in place, with [`channel::Board`] as the seam the
//! network sits behind. [`monitor`] is three files, the engine plus
//! [`monitor::refresh`] for the task that drives it and
//! [`monitor::watch`] for what a bus event asks of it. [`crate::stream`] is a
//! separate connection to Discord, over REST alone, for the log-streaming
//! half of this dog; nothing in this module touches it, and nothing in
//! [`crate::stream`] touches a gateway.

pub mod channel;
pub mod command;
pub mod commands;
pub mod embed;
pub mod interaction;
pub mod monitor;

use std::sync::Arc;

use serenity::all::{Client, GatewayIntents, GuildId};

use crate::{
    config::Config,
    stop::{self, Interrupted, Stop},
};

/// How long a failed gateway connection attempt waits before the next one.
///
/// The same order as the run loop's own [`crate::run::RECHECK_INTERVAL`]
/// for the
/// streaming side, and for the same reason: short enough that an operator
/// watching this dog's own output is not left wondering for long, long
/// enough not to hammer Discord's own gateway on a token or network
/// problem that will not resolve itself between one attempt and the next.
const GATEWAY_RETRY_INTERVAL: core::time::Duration = core::time::Duration::from_secs(30);

/// Bring the gateway up, and keep it up, until `stop` resolves.
///
/// A failed or dropped gateway connection is printed and retried on the
/// next interval, on the same principle [`crate::run::run`] already holds
/// for the streaming side: nothing here is fatal except `stop`
/// resolving, because the shepherd restarting underneath this dog, or
/// Discord's own gateway dropping a shard, is ordinary rather than
/// exceptional, and this half of the dog exiting for it would take the
/// streaming half down with the whole process for no reason of its own.
///
/// `state` and `config` are cloned into a fresh [`interaction::Handler`]
/// on every attempt: every field either carries is an `Arc`, so a
/// reconnect after a dropped shard shares the same shepherd session,
/// resolved config, and monitor the last attempt used rather than losing
/// any of it.
///
/// Never returns an error itself: a failed attempt is printed and retried
/// rather than propagated, on the reasoning above. [`run_once`] is the one
/// place a single attempt's own failure surfaces, for this function to
/// print.
/// The stderr line printed when one gateway attempt ended.
///
/// A function rather than an inline `eprintln!`, the same rule
/// `main` and the run loop follow: a person-facing string a test cannot
/// reach is a string the dash sweep cannot check.
fn gateway_attempt_ended_message(err: &serenity::Error) -> String {
    format!("shep-discord: the gateway connection ended: {err}")
}

pub async fn run(config: Arc<Config>, state: command::State, mut stop: Stop) {
    loop {
        tokio::select! {
            biased;
            () = stop.wait() => return,
            outcome = run_once(&config, state.clone()) => {
                if let Err(err) = outcome {
                    eprintln!("{}", gateway_attempt_ended_message(&err));
                }
            }
        }
        if stop::wait(GATEWAY_RETRY_INTERVAL, &mut stop).await == Interrupted::Yes {
            return;
        }
    }
}

/// One gateway connection attempt: build a client and run it until it
/// returns on its own, or until `run`'s own outer `select!` above drops
/// this whole future because `stop` resolved first.
///
/// There used to be a second mechanism here: a task spawned to wait on a
/// clone of `stop` and call `shard_manager.shutdown_all()`, closing the
/// websocket properly before this attempt ended, with `shutdown.abort()`
/// cleaning that task up once `client.start()` returned. It never ran.
/// `run`'s `select!` is `biased` with `stop.wait()` listed first, so the
/// instant `stop` resolves that branch wins immediately and this whole
/// function's future, the shutdown task included, is dropped rather than
/// polled again; `shutdown.abort()` only ran after `client.start()`
/// returned on its own, which a stop never let happen. Measured: ctrl-c
/// returns in about 0.02 seconds, far too fast for a websocket close
/// handshake to have taken place, so the graceful path was dead code that
/// looked alive.
///
/// The connection is simply dropped instead. Discord treats a dropped
/// gateway connection the same as one closed properly, and the process is
/// exiting either way, so nothing is lost by not closing it in words. A
/// real graceful close would mean giving up the race above that makes
/// ctrl-c prompt: `run_once` would have to keep running until its own
/// shutdown finished, rather than being torn down the moment `stop.wait()`
/// wins the outer `select!`, which is a different shape of loop than
/// `run`'s.
async fn run_once(config: &Config, state: command::State) -> Result<(), serenity::Error> {
    // `GUILDS` alone, no `GUILD_MESSAGES`: a slash command interaction
    // arrives over the gateway regardless of intent, since it is Discord
    // asking this bot to act rather than a message this bot would have to
    // read. `GUILDS` itself is what keeps this dog's own view of which
    // guild it is in and that guild's channels up to date, which
    // `command::register`'s own `GuildId` needs to mean something.
    // `GUILD_MESSAGES` would ask an operator to grant a permission this
    // bot spends nowhere: it reads no message content and answers no
    // prefix command, only slash commands and the buttons its own embeds
    // draw.
    let guild_id = GuildId::new(config.guild_id);
    let handler = interaction::Handler::new(state, guild_id);
    let mut client = Client::builder(&config.token, GatewayIntents::GUILDS)
        .event_handler(handler)
        .await?;
    client.start().await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The one string this module prints for a person. It was an inline
    /// `eprintln!` until now, which is why no sweep reached it: a literal
    /// inside a `select!` arm cannot be read by a test, and this module
    /// had no test module at all as a result.
    #[test]
    fn nothing_printed_for_a_person_carries_a_dash() {
        crate::test_support::assert_no_dashes(&gateway_attempt_ended_message(
            &serenity::Error::Other("refused"),
        ));
    }
}
