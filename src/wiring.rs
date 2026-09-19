//! The Discord clients this process holds, built once and shared.
//!
//! Its own module rather than a corner of [`crate::run`] because it
//! changes for its own reason: which Discord clients exist and what each
//! one is for. None of that is read while reasoning about the two names
//! this dog answers to or about the reconnect loop, and neither of those
//! is read while adding a client here.
//!
//! This is also the one place in the crate that touches both halves of
//! the dog at once, the log sink from [`crate::stream`] and the monitor
//! board from [`crate::bot`], which is what makes it worth naming.

use std::sync::Arc;

use crate::{
    bot::{channel, monitor::Monitor, monitor::watch},
    config::Config,
    stream::discord::DiscordSink,
};
use serenity::all::Http;

/// The Discord clients this process keeps for as long as it runs.
///
/// Every one of these is a `serenity::all::Http` underneath, and serenity
/// keeps its rate-limit buckets on the `Http` that made the request
/// rather than in one place (`http/ratelimiting.rs:84` in the vendored
/// 0.12.5 source: `routes` and `global` are fields of `Ratelimiter`, and
/// `Ratelimiter::new` starts both empty). A client built per cycle
/// therefore begins every cycle knowing nothing about limits this dog has
/// already been told, and several of them writing one channel at the same
/// time each have to take their own 429 to learn a bucket the others have
/// already found. Building them once is what stops that.
///
/// It also inherits the gateway's own posture, argued at
/// [`crate::run::run`]: a
/// `token` or `monitor_channel` changed in `dogs.toml` is picked up on a
/// restart rather than on the next reread, because these are built from
/// the first config that resolves and nothing rebuilds them afterwards.
pub struct Wiring {
    /// Where log lines go. One per process rather than one per streaming
    /// cycle, so a shepherd handover does not also mean relearning the
    /// log channel's limits.
    pub sink: DiscordSink<Http>,
    /// The one board the monitor channel is written with, or `None` when
    /// `dogs.toml` names no `monitor_channel`. Shared with the refresh
    /// task, with `wired` below, and with `/monitor update` through
    /// [`crate::bot::command::State`].
    pub board: Option<Arc<channel::Live>>,
    /// The monitor and that same board, in the shape
    /// [`crate::stream::run`] hands a `process.*` event to. `Some`
    /// exactly when `board` is.
    pub wired: Option<Arc<watch::Wired>>,
}

impl Wiring {
    /// Build the clients from the first config that resolved.
    #[must_use]
    pub fn new(config: &Config, monitor: &Arc<Monitor>) -> Self {
        let board = config
            .monitor_channel
            .map(|channel| Arc::new(channel::Live::new(&config.token, channel)));
        let wired = board.as_ref().map(|board| {
            Arc::new(watch::Wired {
                monitor: Arc::clone(monitor),
                board: Arc::clone(board),
            })
        });
        Self {
            sink: DiscordSink::new(&config.token),
            board,
            wired,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The board and the bus-event side of the monitor must be the SAME
    /// board, not two built from one channel id. `Arc::ptr_eq` rather
    /// than comparing channel ids: two boards on one channel is exactly
    /// the shape this module exists to prevent, and it would compare
    /// equal on every field.
    #[test]
    fn the_monitor_channel_is_written_through_one_board() {
        let config = Config::from_toml("token = \"t\"\nguild_id = 1\nmonitor_channel = 7\n")
            .expect("parsed");
        let wiring = Wiring::new(&config, &Arc::new(Monitor::new()));

        let board = wiring.board.as_ref().expect("a channel was named");
        let wired = wiring.wired.as_ref().expect("a channel was named");
        assert!(Arc::ptr_eq(board, &wired.board));
        assert_eq!(
            board.channel(),
            serenity::all::ChannelId::new(7),
            "the board is built for the channel dogs.toml names"
        );
    }

    /// No `monitor_channel` is nowhere to draw, so there is no board and
    /// nothing for a `process.*` event to reach. The two are `None`
    /// together or the run loop would hand `/monitor` a board the bus
    /// side does not have.
    #[test]
    fn a_config_naming_no_channel_builds_no_board_and_no_wiring_for_one() {
        let config = Config::from_toml("token = \"t\"\nguild_id = 1\n").expect("parsed");
        let wiring = Wiring::new(&config, &Arc::new(Monitor::new()));

        assert!(wiring.board.is_none());
        assert!(wiring.wired.is_none());
    }
}
