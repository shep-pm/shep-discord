//! `/monitor`: turn the live monitor on and off while the dog is running.
//!
//! Two subcommands and no options. `monitor_interval` in `dogs.toml` is
//! what decides whether the monitor runs from boot; this command is the
//! runtime override, and it says so in its own reply, because a setting an
//! operator expects to persist and does not is worse than one that never
//! claimed to.
//!
//! Every sentence this command answers with is a function below rather
//! than an inline literal, the same shape `/system` uses: it is what lets
//! the dash check reach the text, and what lets a test read the reply
//! without a `Context` to answer through.

use core::{future::Future, time::Duration};
use std::{pin::Pin, sync::Arc};

use serenity::all::{
    CommandInteraction, CommandOptionType, Context, CreateCommand, CreateCommandOption,
    CreateInteractionResponseFollowup, Permissions,
};
use shep_client::shep_core::values::UpDuration;

use crate::{
    bot::{
        channel,
        command::{Command, State},
        monitor::{self, Refresh},
    },
    config::MIN_MONITOR_INTERVAL_MS,
    error::Error,
    limits,
};

/// `/monitor`.
pub struct MonitorCommand;

/// The refresh interval a `/monitor start` uses: whatever `dogs.toml`
/// names, or the floor when it names nothing.
///
/// An operator who never set `monitor_interval` still gets a working
/// monitor from the command, rather than being told to edit a config file
/// and restart the dog to use the command they just ran. The floor is the
/// same one [`crate::config`] clamps a written interval up to, so a
/// monitor started this way can never refresh faster than one started from
/// `dogs.toml`.
fn interval_for(configured: Option<UpDuration>) -> UpDuration {
    configured.unwrap_or(UpDuration::from_millis(MIN_MONITOR_INTERVAL_MS))
}

/// What a started monitor answers with.
///
/// Names the interval, and names the fact that this is not written
/// anywhere: an operator who runs this, sees a monitor appear and assumes
/// it survives a restart finds out on the next deploy, which is the worst
/// possible time.
///
/// Fitted to [`limits::MESSAGE_CONTENT_LIMIT`] where it is built rather
/// than trusted to be short: `interval` is rendered from an
/// operator-supplied `dogs.toml` value, and a reply Discord refuses for
/// length tells the operator nothing at all about the monitor that did, in
/// fact, start.
fn started_reply(interval: &str) -> String {
    limits::fit(
        &format!(
            "The monitor is on, refreshing every {interval}. This is a runtime override and it \
             does not survive a restart: set monitor_interval in dogs.toml to have it start on \
             its own."
        ),
        limits::MESSAGE_CONTENT_LIMIT,
    )
}

/// What a second `/monitor start` answers with.
fn already_running_message() -> &'static str {
    "The monitor is already running."
}

/// What a stopped monitor answers with.
///
/// Says what is left in the channel, because the messages stay: they are
/// what the next start adopts rather than posting a second set beside
/// them.
fn stopped_message() -> &'static str {
    "The monitor is off. The messages already in the channel stay as they are, and the next \
     start picks them up again rather than posting a second set."
}

/// What `/monitor stop` answers with when nothing was running.
fn not_running_message() -> &'static str {
    "The monitor is not running."
}

/// What `/monitor start` answers with when `dogs.toml` names no channel.
fn no_channel_message() -> &'static str {
    "No monitor_channel is set in the [discord] section of dogs.toml, so there is nowhere to \
     draw the monitor."
}

/// One subcommand that takes no options at all. Both of them do.
fn bare_subcommand(name: &str, description: &str) -> CreateCommandOption {
    CreateCommandOption::new(CommandOptionType::SubCommand, name, description)
}

impl MonitorCommand {
    /// Send `content` as an ephemeral followup.
    async fn answer(
        &self,
        ctx: &Context,
        interaction: &CommandInteraction,
        content: &str,
    ) -> Result<(), Error> {
        interaction
            .create_followup(
                ctx,
                CreateInteractionResponseFollowup::new()
                    .content(content)
                    .ephemeral(true),
            )
            .await?;
        Ok(())
    }

    /// Start the refresh task, and say what happened.
    ///
    /// Builds its own [`channel::Live`] from the token and channel in the
    /// resolved config: a board is an [`serenity::all::Http`] and a channel
    /// id, neither of which reaches the network until the first request,
    /// so there is nothing to hold open between starts.
    fn start(&self, state: &State) -> String {
        let Some(channel) = state.config.monitor_channel else {
            return no_channel_message().to_owned();
        };
        let interval = interval_for(state.config.monitor_interval);
        let refresh = Refresh {
            board: channel::Live::new(&state.config.token, channel),
            live: Arc::clone(&state.live),
            names: Arc::clone(&state.names),
            ignore_dogs: state.config.ignore_dogs,
            interval: Duration::from_millis(interval.as_millis()),
        };
        if monitor::start(&state.monitor, refresh) {
            started_reply(&interval.to_string())
        } else {
            already_running_message().to_owned()
        }
    }
}

impl Command for MonitorCommand {
    fn data(&self) -> CreateCommand {
        CreateCommand::new(self.name())
            .description("Turn the live monitor channel on or off.")
            .default_member_permissions(Permissions::ADMINISTRATOR)
            .add_option(bare_subcommand(
                "start",
                "Start refreshing one message per sheep until this dog restarts.",
            ))
            .add_option(bare_subcommand("stop", "Stop refreshing the monitor."))
    }

    fn name(&self) -> &'static str {
        "monitor"
    }

    fn run<'a>(
        &'a self,
        ctx: &'a Context,
        interaction: &'a CommandInteraction,
        state: &'a State,
    ) -> Pin<Box<dyn Future<Output = Result<(), Error>> + Send + 'a>> {
        Box::pin(async move {
            let options = interaction.data.options();
            // Discord only ever sends one of the two subcommands `data`
            // declares; anything else here is a bug in this file's own
            // registration, not a shape an operator can reach.
            let Some(sub) = options.first() else {
                return Ok(());
            };

            let reply = match sub.name {
                "start" => self.start(state),
                "stop" => {
                    if state.monitor.stop() {
                        stopped_message().to_owned()
                    } else {
                        not_running_message().to_owned()
                    }
                }
                _ => return Ok(()),
            };
            self.answer(ctx, interaction, &reply).await
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::test_support::{assert_no_dashes, assert_no_dashes_deep};

    use super::*;

    #[test]
    fn the_command_is_admin_gated_and_named_for_what_it_registers() {
        let json = serde_json::to_value(MonitorCommand.data()).expect("json");
        assert_eq!(json["name"], MonitorCommand.name());
        assert_eq!(
            json["default_member_permissions"],
            Permissions::ADMINISTRATOR.bits().to_string()
        );
    }

    #[test]
    fn the_command_declares_exactly_start_and_stop() {
        let json = serde_json::to_value(MonitorCommand.data()).expect("json");
        let names: Vec<&str> = json["options"]
            .as_array()
            .expect("options")
            .iter()
            .map(|option| option["name"].as_str().expect("name"))
            .collect();
        assert_eq!(names, vec!["start", "stop"]);
    }

    /// An unset `monitor_interval` still starts a working monitor, at the
    /// same floor a written one is clamped up to. Reads the constant
    /// rather than restating the number.
    #[test]
    fn an_unconfigured_interval_falls_back_to_the_floor() {
        assert_eq!(
            interval_for(None),
            UpDuration::from_millis(MIN_MONITOR_INTERVAL_MS)
        );
        let written = UpDuration::from_millis(MIN_MONITOR_INTERVAL_MS * 4);
        assert_eq!(interval_for(Some(written)), written);
    }

    /// The reply says the override does not survive a restart, and names
    /// the file that would make it survive. An operator who assumes
    /// otherwise finds out on the next deploy.
    #[test]
    fn the_start_reply_says_the_override_is_not_written_down() {
        let reply = started_reply("1m");
        assert!(reply.contains("1m"), "{reply}");
        assert!(reply.contains("does not survive a restart"), "{reply}");
        assert!(reply.contains("monitor_interval in dogs.toml"), "{reply}");
    }

    /// The interval is rendered from an operator-supplied value, so the
    /// reply is fitted where it is built rather than assumed short.
    #[test]
    fn an_absurd_interval_cannot_make_a_reply_discord_refuses() {
        let reply = started_reply(&"9".repeat(limits::MESSAGE_CONTENT_LIMIT * 2));
        assert_eq!(reply.chars().count(), limits::MESSAGE_CONTENT_LIMIT);
        assert!(reply.ends_with('\u{2026}'));
    }

    #[test]
    fn nothing_printed_for_a_person_carries_a_dash() {
        assert_no_dashes(&started_reply("1m"));
        assert_no_dashes(already_running_message());
        assert_no_dashes(stopped_message());
        assert_no_dashes(not_running_message());
        assert_no_dashes(no_channel_message());
        assert_no_dashes_deep(&serde_json::to_value(MonitorCommand.data()).expect("json"));
    }
}
