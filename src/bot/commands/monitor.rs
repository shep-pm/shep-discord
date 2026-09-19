//! `/monitor`: turn the live monitor on and off while the dog is running.
//!
//! Three subcommands and no options. `monitor_interval` in `dogs.toml` is
//! what decides whether the monitor runs from boot; this command is the
//! runtime override, and it says so in its own reply, because a setting an
//! operator expects to persist and does not is worse than one that never
//! claimed to.
//!
//! Every sentence this command answers with is a function below rather
//! than an inline literal, the same shape `/system` uses: it is what lets
//! the dash check reach the text, and what lets a test read the reply
//! without a `Context` to answer through.

use core::future::Future;
use std::pin::Pin;

use serenity::all::{
    CommandInteraction, ComponentInteraction, Context, CreateCommand,
    CreateInteractionResponseFollowup, Permissions,
};
use shep_client::shep_core::{protocol::SelectorSpec, values::UpDuration};

use crate::{
    bot::{
        channel,
        command::{Command, State},
        commands::bare_subcommand,
        embed::parse_custom_id,
        monitor::refresh::{self, Refresh},
    },
    config::MIN_MONITOR_INTERVAL_MS,
    error::Error,
    limits,
    shepherd::Live,
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

/// What `/monitor update` answers with when there is no monitor to
/// redraw.
///
/// Says what to do about it, where `/monitor stop`'s own version of this
/// does not need to: somebody who asked for a redraw wants the board
/// current, and the next step is one command away.
fn nothing_to_redraw_message() -> &'static str {
    "The monitor is not running, so there is nothing to redraw. Start it with /monitor start."
}

/// What `/monitor update` answers with once it has redrawn the flock.
///
/// Names the count, because the interesting failure is a monitor that
/// answers cheerfully having drawn nothing: an empty flock and a flock
/// this dog could not read look identical in the channel, and the second
/// one also prints to stderr.
///
/// Fitted to [`limits::MESSAGE_CONTENT_LIMIT`] like every other reply
/// here. Nothing interpolated can reach that limit today, since a count
/// of sheep is at most twenty digits, so this one is consistency rather
/// than a bound the input can actually exceed; [`started_reply`] renders
/// an operator-supplied value and is the one that genuinely needs it.
fn redrew_reply(count: usize) -> String {
    let sentence = if count == 0 {
        "The monitor is up to date. There was nothing in the flock to draw.".to_owned()
    } else {
        format!("Redrew {count} sheep. The next scheduled refresh is unchanged.")
    };
    limits::fit(&sentence, limits::MESSAGE_CONTENT_LIMIT)
}

/// What `/monitor start` answers with when `dogs.toml` names no channel.
fn no_channel_message() -> &'static str {
    "No monitor_channel is set in the [discord] section of dogs.toml, so there is nowhere to \
     draw the monitor."
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
    /// Builds a fresh [`Refresh`] rather than holding one: a board is an
    /// HTTP client and a channel id, neither of which reaches the network
    /// until the first request, so there is nothing to keep open between
    /// starts.
    fn start(&self, state: &State) -> String {
        let Some(channel) = state.config.monitor_channel else {
            return no_channel_message().to_owned();
        };
        let interval = interval_for(state.config.monitor_interval);
        let board = channel::Live::new(&state.config.token, channel);
        if refresh::start(
            &state.monitor,
            Refresh::new(board, &state.config, interval, &state.live),
        ) {
            started_reply(&interval.to_string())
        } else {
            already_running_message().to_owned()
        }
    }
}

impl MonitorCommand {
    /// Redraw the flock now, and say what happened.
    ///
    /// Goes through `Monitor::refresh_now`, which is the same
    /// `update_all` the ticker calls, so a manual redraw racing a
    /// scheduled one or a bus event cannot post a second message for one
    /// sheep: the per-sheep guard serialises them and the second to
    /// arrive edits what the first posted. See that method for why this
    /// is safe to run out of turn.
    ///
    /// Builds its own board for the same reason [`MonitorCommand::start`]
    /// does: it is an HTTP client and a channel id, neither of which
    /// reaches the network until a request.
    ///
    /// # Errors
    /// Whatever the muster-roll read could not answer. A single sheep's
    /// failed draw is not an error here; it is printed and left out of
    /// the count.
    async fn update(&self, state: &State) -> Result<String, Error> {
        let Some(channel) = state.config.monitor_channel else {
            return Ok(no_channel_message().to_owned());
        };
        let board = channel::Live::new(&state.config.token, channel);
        let redrawn = refresh::refresh_now(
            &state.monitor,
            &board,
            &state.live,
            state.config.ignore_dogs,
        )
        .await?;
        Ok(match redrawn {
            Some(count) => redrew_reply(count),
            None => nothing_to_redraw_message().to_owned(),
        })
    }
}

impl MonitorCommand {
    /// Do what a monitor button asks, and hand back the sentence the
    /// shepherd answered with. `None` for an id this command did not
    /// write.
    ///
    /// The buttons are the reason
    /// [`crate::bot::embed::custom_id`] keys on a numeric sheep id rather
    /// than a name, and this is the one place in the crate that builds a
    /// [`SelectorSpec::Id`]: the id came off a button this dog drew, so it
    /// names the sheep that embed is about even if somebody has since
    /// renamed it.
    ///
    /// Fitted against [`limits::MESSAGE_CONTENT_LIMIT`] here rather than
    /// at the caller, for the reason `/shep`'s own `reply_content` gives:
    /// [`Live::act`] joins sheep names into its sentence with no bound of
    /// its own, and a selector can match as many sheep as the flock holds.
    ///
    /// # Errors
    /// Whatever [`Live::act`] could not do: a shepherd that cannot be
    /// reached, or one that refused the verb.
    async fn act_on_button(&self, live: &Live, custom_id: &str) -> Result<Option<String>, Error> {
        let Some((verb, id)) = parse_custom_id(custom_id) else {
            return Ok(None);
        };
        let reply = live.act(verb, SelectorSpec::Id(id)).await?;
        Ok(Some(limits::fit(&reply, limits::MESSAGE_CONTENT_LIMIT)))
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
            .add_option(bare_subcommand(
                "update",
                "Redraw every sheep now, without waiting for the next refresh.",
            ))
            .add_option(bare_subcommand("stop", "Stop refreshing the monitor."))
    }

    fn name(&self) -> &'static str {
        "monitor"
    }

    /// Every button in the monitor channel is one this command drew, and
    /// no other command in this dog draws any.
    fn handles_button(&self, custom_id: &str) -> bool {
        parse_custom_id(custom_id).is_some()
    }

    /// Act on a button under a sheep's monitor embed.
    ///
    /// The monitor's whole point is a message an operator can act from,
    /// and until this existed a click deferred, found no handler, and left
    /// the operator watching a spinner while the sheep did not restart.
    ///
    /// The reply is ephemeral, like every other reply this dog sends: the
    /// monitor message itself is the shared state, and it is redrawn by
    /// the `process.*` event the verb causes rather than by this handler,
    /// so the channel shows the result to everyone and the sentence goes
    /// only to whoever clicked.
    fn button<'a>(
        &'a self,
        ctx: &'a Context,
        interaction: &'a ComponentInteraction,
        state: &'a State,
    ) -> Pin<Box<dyn Future<Output = Result<(), Error>> + Send + 'a>> {
        Box::pin(async move {
            let Some(reply) = self
                .act_on_button(&state.live, &interaction.data.custom_id)
                .await?
            else {
                return Ok(());
            };
            interaction
                .create_followup(
                    ctx,
                    CreateInteractionResponseFollowup::new()
                        .content(reply)
                        .ephemeral(true),
                )
                .await?;
            Ok(())
        })
    }

    fn run<'a>(
        &'a self,
        ctx: &'a Context,
        interaction: &'a CommandInteraction,
        state: &'a State,
    ) -> Pin<Box<dyn Future<Output = Result<(), Error>> + Send + 'a>> {
        Box::pin(async move {
            let options = interaction.data.options();
            // Discord only ever sends one of the three subcommands `data`
            // declares; anything else here is a bug in this file's own
            // registration, not a shape an operator can reach.
            let Some(sub) = options.first() else {
                return Ok(());
            };

            let reply = match sub.name {
                "start" => self.start(state),
                "update" => self.update(state).await?,
                "stop" => {
                    // Awaited rather than fired and forgotten: see
                    // `Monitor::stop` for why the caller is the only
                    // party that can wait, and why a start straight after
                    // a stop would otherwise run two tasks.
                    if state.monitor.stop().await {
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
    use shep_client::shep_core::protocol::{Request, Response};

    use crate::{
        bot::embed::custom_id,
        shepherd::Verb,
        test_support::{assert_no_dashes, assert_no_dashes_deep, sample, test_live},
    };

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
    fn the_command_declares_exactly_start_update_and_stop() {
        let json = serde_json::to_value(MonitorCommand.data()).expect("json");
        let names: Vec<&str> = json["options"]
            .as_array()
            .expect("options")
            .iter()
            .map(|option| option["name"].as_str().expect("name"))
            .collect();
        // The spec names all three (`/monitor start|update|stop`). The
        // plan specified two and this dog shipped two until the gap was
        // found, so the set is pinned by name rather than by count.
        assert_eq!(names, vec!["start", "update", "stop"]);
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
        // The whole string, not three `contains` calls against it: the
        // output is deterministic and fully known, so naming three
        // clauses would let a wording regression in either of the others
        // pass in silence.
        assert_eq!(
            started_reply("1m"),
            "The monitor is on, refreshing every 1m. This is a runtime override and it does not \
             survive a restart: set monitor_interval in dogs.toml to have it start on its own."
        );
    }

    /// The interval is rendered from an operator-supplied value, so the
    /// reply is fitted where it is built rather than assumed short.
    #[test]
    fn an_absurd_interval_cannot_make_a_reply_discord_refuses() {
        let reply = started_reply(&"9".repeat(limits::MESSAGE_CONTENT_LIMIT * 2));
        assert_eq!(reply.chars().count(), limits::MESSAGE_CONTENT_LIMIT);
        assert!(reply.ends_with('\u{2026}'));
    }

    /// A redraw names what it drew, and the empty flock is its own
    /// sentence rather than "Redrew 0 sheep", which reads like a failure.
    #[test]
    fn a_redraw_reply_names_what_it_drew() {
        assert_eq!(
            redrew_reply(4),
            "Redrew 4 sheep. The next scheduled refresh is unchanged."
        );
        assert_eq!(
            redrew_reply(0),
            "The monitor is up to date. There was nothing in the flock to draw."
        );
    }

    /// Somebody who asked for a redraw and has no monitor running is told
    /// the command that fixes that, since it is the obvious next thing
    /// they want.
    #[test]
    fn a_refused_redraw_says_how_to_start_the_monitor() {
        assert_eq!(
            nothing_to_redraw_message(),
            "The monitor is not running, so there is nothing to redraw. Start it with /monitor \
             start."
        );
    }

    /// The four replies that had only a dash check, asserted whole.
    ///
    /// Their neighbours above are pinned word for word, and these four
    /// were not, which is the same gap that once let a mangled string
    /// ship: a dash check passes on any wording at all, including a
    /// sentence a bad edit has cut in half. Every one of them is a fixed
    /// string with nothing interpolated, so there is no reason to assert
    /// anything less than the whole of it.
    #[test]
    fn the_four_fixed_replies_say_exactly_what_they_should() {
        assert_eq!(already_running_message(), "The monitor is already running.");
        assert_eq!(
            stopped_message(),
            "The monitor is off. The messages already in the channel stay as they are, and the \
             next start picks them up again rather than posting a second set."
        );
        assert_eq!(not_running_message(), "The monitor is not running.");
        assert_eq!(
            no_channel_message(),
            "No monitor_channel is set in the [discord] section of dogs.toml, so there is \
             nowhere to draw the monitor."
        );
    }

    /// A click on Restart under a sheep's embed reaches the shepherd as
    /// `SelectorSpec::Id`, and the operator gets the sentence the shepherd
    /// answered with. Before this handler existed the click deferred,
    /// matched no command, and left a spinner running while the sheep did
    /// not restart.
    #[tokio::test]
    async fn a_button_acts_on_the_sheep_its_id_names() {
        let (live, mut fake) = test_live().await;
        fake.expect(Request::Restart {
            selector: SelectorSpec::Id(7),
        })
        .answer(Response::Restarted {
            accepted: vec![sample("web")],
            refused: Vec::new(),
        });

        let reply = MonitorCommand
            .act_on_button(&live, &custom_id(Verb::Restart, 7))
            .await
            .expect("ok");

        assert_eq!(reply.as_deref(), Some("restarted web"));
    }

    /// A verb the shepherd refuses comes back as an error rather than a
    /// cheerful reply, so `report_failure` tells the operator instead of
    /// the click looking like it worked.
    #[tokio::test]
    async fn a_refused_verb_reaches_the_operator_as_a_failure() {
        let (live, mut fake) = test_live().await;
        fake.expect(Request::Stop {
            selector: SelectorSpec::Id(7),
        })
        .answer_wrong_shape("the shepherd refused the stop");

        let outcome = MonitorCommand
            .act_on_button(&live, &custom_id(Verb::Stop, 7))
            .await;

        assert!(
            matches!(outcome, Err(Error::Unexpected { .. })),
            "{outcome:?}"
        );
    }

    /// An id from the old `verb:name` scheme is not this command's to
    /// answer, so it falls through to the dispatch's own reply rather
    /// than reaching the shepherd.
    #[tokio::test]
    async fn an_unreadable_id_is_not_claimed_and_asks_the_shepherd_nothing() {
        let (live, _fake) = test_live().await;

        assert!(!MonitorCommand.handles_button("restart:web"));
        assert_eq!(
            MonitorCommand
                .act_on_button(&live, "restart:web")
                .await
                .expect("ok"),
            None
        );
    }

    /// Every button the monitor draws is one this command claims, so no
    /// click can reach the dispatch's unclaimed branch.
    #[test]
    fn every_button_the_monitor_draws_is_claimed_by_this_command() {
        for verb in [
            Verb::Start,
            Verb::Stop,
            Verb::Restart,
            Verb::Delete,
            Verb::Flush,
        ] {
            assert!(
                MonitorCommand.handles_button(&custom_id(verb, 1)),
                "{verb:?}"
            );
        }
    }

    #[test]
    fn nothing_printed_for_a_person_carries_a_dash() {
        assert_no_dashes(&started_reply("1m"));
        assert_no_dashes(&redrew_reply(4));
        assert_no_dashes(&redrew_reply(0));
        assert_no_dashes(nothing_to_redraw_message());
        assert_no_dashes(already_running_message());
        assert_no_dashes(stopped_message());
        assert_no_dashes(not_running_message());
        assert_no_dashes(no_channel_message());
        assert_no_dashes_deep(&serde_json::to_value(MonitorCommand.data()).expect("json"));
    }
}
