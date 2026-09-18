//! The bot half of this dog: slash commands, the embed they answer with,
//! and the gateway connection that dispatches both.
//!
//! [`embed`] renders one sheep as a [`serenity`] embed with its action
//! buttons; [`command`] is the contract every slash command implements and
//! the state they all read; [`commands`] is where the concrete commands
//! live; [`interaction`] wires an incoming gateway interaction to one of
//! them; and [`run`] brings the gateway itself up. [`crate::stream`] is a
//! separate connection to Discord, over REST alone, for the log-streaming
//! half of this dog; nothing in this module touches it, and nothing in
//! [`crate::stream`] touches a gateway.

pub mod command;
pub mod commands;
pub mod embed;
pub mod interaction;

use std::sync::Arc;

use serenity::all::{Client, GatewayIntents, GuildId};

use crate::{
    config::Config,
    stop::{self, Interrupted, Stop},
};

/// How long a failed gateway connection attempt waits before the next one.
///
/// The same order as `main`'s own [`crate::RECHECK_INTERVAL`] for the
/// streaming side, and for the same reason: short enough that an operator
/// watching this dog's own output is not left wondering for long, long
/// enough not to hammer Discord's own gateway on a token or network
/// problem that will not resolve itself between one attempt and the next.
const GATEWAY_RETRY_INTERVAL: core::time::Duration = core::time::Duration::from_secs(30);

/// Bring the gateway up, and keep it up, until `stop` resolves.
///
/// A failed or dropped gateway connection is printed and retried on the
/// next interval, on the same principle `main`'s own run loop already
/// holds for the streaming side: nothing here is fatal except `stop`
/// resolving, because the shepherd restarting underneath this dog, or
/// Discord's own gateway dropping a shard, is ordinary rather than
/// exceptional, and this half of the dog exiting for it would take the
/// streaming half down with the whole process for no reason of its own.
///
/// `state` and `config` are cloned into a fresh [`interaction::Handler`]
/// on every attempt: every field either carries is an `Arc`, so a
/// reconnect after a dropped shard shares the same shepherd session,
/// resolved config, and name cache the last attempt used rather than
/// losing any of it.
///
/// Never returns an error itself: a failed attempt is printed and retried
/// rather than propagated, on the reasoning above. [`run_once`] is the one
/// place a single attempt's own failure surfaces, for this function to
/// print.
pub async fn run(config: Arc<Config>, state: command::State, mut stop: Stop) {
    loop {
        // Cloned before the `select!` rather than inside it: `stop.wait()`
        // below borrows `stop` mutably for as long as the `select!` body
        // runs, and `run_once` needs its own clone to hand to the
        // attempt's own shutdown watcher, so that clone has to exist
        // before the mutable borrow starts rather than alongside it.
        let attempt_stop = stop.clone();
        tokio::select! {
            biased;
            () = stop.wait() => return,
            outcome = run_once(&config, state.clone(), attempt_stop) => {
                if let Err(err) = outcome {
                    eprintln!("shep-discord: the gateway connection ended: {err}");
                }
            }
        }
        if stop::wait(GATEWAY_RETRY_INTERVAL, &mut stop).await == Interrupted::Yes {
            return;
        }
    }
}

/// One gateway connection attempt: build a client, start it, and shut it
/// down cleanly the moment `stop` resolves.
///
/// The shutdown watcher runs as its own task rather than inside a
/// `tokio::select!` around `client.start()`: `start()` does not return
/// until the shard manager itself is told to stop, so racing it against
/// `stop.wait()` in one `select!` would drop the still-running client
/// future the moment `stop` resolved, leaving the shard's own connection
/// open with nothing left polling it. Asking the shard manager to shut
/// down instead lets `start()` return on its own once it actually has.
async fn run_once(
    config: &Config,
    state: command::State,
    mut stop: Stop,
) -> Result<(), serenity::Error> {
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

    let shard_manager = Arc::clone(&client.shard_manager);
    let shutdown = tokio::spawn(async move {
        stop.wait().await;
        shard_manager.shutdown_all().await;
    });

    let result = client.start().await;
    shutdown.abort();
    result
}
