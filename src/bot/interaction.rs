//! Turning one incoming `Interaction` into a `Command` hook call, and
//! making sure a failure to report a failure is never silent.
//!
//! # Why the real dispatch cannot be exercised by a test in this crate
//!
//! [`crate::bot::command::Command::run`], `::autocomplete` and `::button`
//! each take a `&Context`. Nothing outside serenity's own crate can build
//! one: its `shard` field is a `ShardMessenger`, whose only field that
//! matters here (`tx`, an `mpsc::UnboundedSender` the shard runner owns) is
//! `pub(crate)` to serenity, and `ShardMessenger::new` demands a live
//! `&ShardRunner`, which is no more constructible from here than a socket
//! this crate did not open. So [`Handler::interaction_create`] itself, and
//! every real `Command` hook it calls, is reachable only from a live
//! gateway connection; it was verified by hand against a real guild rather
//! than by a test here (see the task report).
//!
//! What the dispatch decides once it has a hook to call, though, does not
//! need a `Context` at all, and is what the tests below pin instead:
//! [`hook_for`] is which hook one interaction kind reaches, the same
//! function [`Handler::interaction_create`] itself calls to decide;
//! [`report_failure`] is the fix for the swallowed follow-up at
//! `interaction.ts:55`, and [`unknown_component_reply`] is what an
//! unrecognised button gets told rather than silence.

use serenity::all::{
    CommandInteraction, ComponentInteraction, Context, CreateInteractionResponseFollowup,
    EventHandler, GuildId, Interaction, InteractionType, Ready,
};

use crate::{
    bot::{
        command::{self, Command, State},
        embed::parse_custom_id,
    },
    error::Error,
    limits,
};

/// Which of a [`Command`]'s three hooks one interaction reaches, or none.
///
/// A pure function of [`Interaction::kind`] rather than of the whole
/// [`Interaction`]: [`InteractionType`] is a plain, constructible enum, so a
/// test can hand this every variant directly with no [`Context`], no
/// gateway, and no hand-built [`CommandInteraction`] behind any of them.
/// This is the one place that mapping is written down; the old test
/// (`dispatch_kind`) hand-wrote a second copy of it against a spy, so the
/// real `match` in [`Handler::interaction_create`] could drift from the
/// test and the test would stay green regardless.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Hook {
    Run,
    Autocomplete,
    Button,
}

fn hook_for(kind: InteractionType) -> Option<Hook> {
    match kind {
        InteractionType::Command => Some(Hook::Run),
        InteractionType::Autocomplete => Some(Hook::Autocomplete),
        InteractionType::Component => Some(Hook::Button),
        // `Ping` only matters to a bot using an HTTP endpoint URL instead
        // of a gateway connection, and no `Command` implements a modal
        // (see this module's own trait doc for why one is not added on
        // spec). `InteractionType` is `#[non_exhaustive]`, so the wildcard
        // covers a variant serenity adds later too.
        InteractionType::Ping | InteractionType::Modal | _ => None,
    }
}

/// The text an unrecognised component gets told, rather than nothing.
///
/// A component whose `custom_id` [`parse_custom_id`] cannot read is either
/// a stale button from a message an older version of this dog drew, or one
/// nobody but a person typing by hand ever produced; either way the old
/// code silently ignored it (`interaction.ts:44`), leaving whoever clicked
/// it staring at Discord's own "This interaction failed" once their client
/// gives up waiting. Answering instead means the ephemeral reply names the
/// actual problem.
fn unknown_component_reply() -> &'static str {
    "This looks like a button, but it is not a button this bot wrote. It may be left over from \
     an older version of this dog."
}

/// Whatever it takes to tell a user something once this dog has already
/// deferred their interaction.
///
/// A trait rather than a bare closure so [`CommandInteraction`] and
/// [`ComponentInteraction`] can each answer it with their own
/// `create_followup` (identically shaped in serenity, but not unified
/// under one trait there), and so [`report_failure`]'s own test can hand
/// it a double that always fails, pinning the swallowed-follow-up fix
/// without a live gateway behind either.
///
/// [`CommandInteraction`]: serenity::all::CommandInteraction
trait Responder {
    /// # Errors
    /// Whatever Discord refused the followup for.
    async fn tell(&self, message: &str) -> Result<(), Error>;
}

/// A [`Responder`] backed by a real `CommandInteraction`.
struct CommandResponder<'a> {
    ctx: &'a Context,
    interaction: &'a CommandInteraction,
}

impl Responder for CommandResponder<'_> {
    async fn tell(&self, message: &str) -> Result<(), Error> {
        self.interaction
            .create_followup(
                self.ctx,
                CreateInteractionResponseFollowup::new()
                    .content(message)
                    .ephemeral(true),
            )
            .await?;
        Ok(())
    }
}

/// A [`Responder`] backed by a real `ComponentInteraction`.
struct ComponentResponder<'a> {
    ctx: &'a Context,
    interaction: &'a ComponentInteraction,
}

impl Responder for ComponentResponder<'_> {
    async fn tell(&self, message: &str) -> Result<(), Error> {
        self.interaction
            .create_followup(
                self.ctx,
                CreateInteractionResponseFollowup::new()
                    .content(message)
                    .ephemeral(true),
            )
            .await?;
        Ok(())
    }
}

/// The stderr line printed for a command or button that itself failed.
fn command_failed_message(label: &str, err: &Error) -> String {
    format!("shep-discord: /{label} failed: {err}")
}

/// The stderr line printed when telling the user about `context` also
/// failed.
///
/// One function for both places this dog tries to tell a user something
/// and has to fall back to stderr when that also fails: [`report_failure`]
/// below, and the unknown-button path in [`Handler::handle_component`].
/// Those two used to be shaped slightly differently by hand; there is
/// nothing about either failure that needs its own wording, so one
/// function answers both.
fn could_not_tell_the_user_message(context: &str, err: &Error) -> String {
    format!("shep-discord: {context}: could not tell the user: {err}")
}

/// Tell the user `message` through `responder`, and hand back the stderr
/// line to print if telling them also fails, so no attempt to say
/// something to a user is ever silent on both ends at once.
///
/// The old code ended this exact path at `.catch(() => {})`
/// (`interaction.ts:55`): a failed followup vanished with nothing on
/// stderr, so an operator watching this dog's own output saw silence
/// twice over, once from the original failure and again from failing to
/// say so.
async fn tell_or_log(context: &str, message: &str, responder: &impl Responder) -> Option<String> {
    responder
        .tell(message)
        .await
        .err()
        .map(|err| could_not_tell_the_user_message(context, &err))
}

/// Tell the user about `outcome`'s failure, and make sure telling them is
/// never silent either, returning every line for the caller to print.
///
/// A pure function of `outcome` and `responder` rather than an inline
/// `eprintln!`, on the usual reason: it lets a test drive the swallowed
/// follow-up fix with a `responder` that always fails, with no `Context`
/// and no gateway behind either.
///
/// The followup content is passed through [`limits::fit`] before it ever
/// reaches `responder`: `err`'s own `Display` is not bounded, and Discord
/// caps a message's content at [`limits::MESSAGE_CONTENT_LIMIT`]
/// characters.
async fn report_failure(
    label: &str,
    outcome: Result<(), Error>,
    responder: &impl Responder,
) -> Vec<String> {
    let Err(err) = outcome else {
        return Vec::new();
    };
    let mut lines = vec![command_failed_message(label, &err)];
    let content = limits::fit(
        &format!("Something went wrong: {err}"),
        limits::MESSAGE_CONTENT_LIMIT,
    );
    if let Some(line) = tell_or_log(&format!("/{label} failed"), &content, responder).await {
        lines.push(line);
    }
    lines
}

/// Everything the gateway needs once it is up: this dog's own state, the
/// commands it answers with, and which guild to register them against.
pub struct Handler {
    state: State,
    commands: Vec<Box<dyn Command>>,
    guild_id: GuildId,
}

impl Handler {
    /// `guild_id` comes from [`crate::config::Config::guild_id`], read
    /// once when [`crate::bot::run`] builds the client for this attempt.
    #[must_use]
    pub fn new(state: State, guild_id: GuildId) -> Self {
        Self {
            state,
            commands: command::registry(),
            guild_id,
        }
    }

    fn find(&self, name: &str) -> Option<&dyn Command> {
        self.commands
            .iter()
            .find(|command| command.name() == name)
            .map(Box::as_ref)
    }
}

#[serenity::async_trait]
impl EventHandler for Handler {
    /// Register every command in [`Self::commands`] against
    /// [`Self::guild_id`] as soon as the gateway is up.
    ///
    /// Runs on every reconnect, not once per process: [`crate::bot::run`]
    /// builds a fresh [`Handler`] on every gateway attempt, and Discord's
    /// own `set_commands` replaces the guild's whole set rather than
    /// appending, so a repeat registration here is harmless and a missed
    /// one after a long-lived reconnect would leave a stale set behind.
    async fn ready(&self, ctx: Context, _ready: Ready) {
        match command::register(&ctx.http, self.guild_id, &self.commands).await {
            Ok(names) => println!(
                "shep-discord: registered slash commands: {}",
                names.join(", ")
            ),
            Err(err) => eprintln!("shep-discord: could not register slash commands: {err}"),
        }
    }

    /// Route one interaction to the `Command` it names, or answer that
    /// there is none.
    ///
    /// [`hook_for`] decides which arm below runs; this `match` only
    /// destructures the variant [`hook_for`] already named, so the two can
    /// never disagree about which kind reaches which hook. Everything
    /// except autocomplete defers ephemerally before the lookup, as both
    /// source repos did: Discord gives three seconds, and a
    /// [`crate::shepherd::Live`] call on a busy shepherd can outlast that.
    /// Autocomplete answers with its own response type instead, so
    /// deferring first would ask Discord twice for the one interaction.
    async fn interaction_create(&self, ctx: Context, interaction: Interaction) {
        match (hook_for(interaction.kind()), interaction) {
            (Some(Hook::Run), Interaction::Command(command_interaction)) => {
                self.handle_command(&ctx, &command_interaction).await;
            }
            (Some(Hook::Autocomplete), Interaction::Autocomplete(command_interaction)) => {
                if let Some(command) = self.find(&command_interaction.data.name) {
                    command
                        .autocomplete(&ctx, &command_interaction, &self.state)
                        .await;
                }
            }
            (Some(Hook::Button), Interaction::Component(component_interaction)) => {
                self.handle_component(&ctx, &component_interaction).await;
            }
            // `None` covers `Ping` and `Modal`, nothing this bot answers,
            // and any variant `hook_for` does not yet know about; the
            // second arm of the tuple never mismatches the first in
            // practice; `hook_for` and `Interaction::kind` agree by
            // construction, and this catches anything else instead of
            // failing to compile against a future serenity release.
            _ => {}
        }
    }
}

impl Handler {
    async fn handle_command(&self, ctx: &Context, interaction: &CommandInteraction) {
        let name = interaction.data.name.clone();
        if let Err(err) = interaction.defer_ephemeral(ctx).await {
            eprintln!("shep-discord: could not defer /{name}: {err}");
            return;
        }
        let Some(command) = self.find(&name) else {
            eprintln!("shep-discord: no command named {name} is registered");
            return;
        };
        let outcome = command.run(ctx, interaction, &self.state).await;
        let responder = CommandResponder { ctx, interaction };
        for line in report_failure(&name, outcome, &responder).await {
            eprintln!("{line}");
        }
    }

    async fn handle_component(&self, ctx: &Context, interaction: &ComponentInteraction) {
        if let Err(err) = interaction.defer_ephemeral(ctx).await {
            eprintln!("shep-discord: could not defer a button: {err}");
            return;
        }
        let responder = ComponentResponder { ctx, interaction };
        if parse_custom_id(&interaction.data.custom_id).is_none() {
            if let Some(line) =
                tell_or_log("an unknown button", unknown_component_reply(), &responder).await
            {
                eprintln!("{line}");
            }
            return;
        }
        // A well formed custom_id, but no command in this task's registry
        // draws a button: `/system` never does, and `/shep` (Task 12) is
        // the first one that will. Every command gets a chance at it
        // rather than a name encoded on the id, because `custom_id`
        // carries only a verb and a sheep id, never which command drew
        // the button. The interaction was already deferred ephemerally
        // above, so a `Command::button` failure that only went to stderr
        // left the user watching a spinner until Discord gave up on it;
        // routing it through `report_failure` closes that the same way
        // `handle_command` already does.
        for command in &self.commands {
            let outcome = command.button(ctx, interaction, &self.state).await;
            for line in report_failure(command.name(), outcome, &responder).await {
                eprintln!("{line}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    /// [`hook_for`] is what [`Handler::interaction_create`] itself calls to
    /// decide which hook an interaction reaches, so pinning every
    /// [`InteractionType`] against it here pins the real mapping rather
    /// than a hand-written copy of it: there is only the one function, and
    /// this is it under test.
    #[test]
    fn each_interaction_kind_reaches_its_own_hook() {
        assert_eq!(hook_for(InteractionType::Command), Some(Hook::Run));
        assert_eq!(
            hook_for(InteractionType::Autocomplete),
            Some(Hook::Autocomplete)
        );
        assert_eq!(hook_for(InteractionType::Component), Some(Hook::Button));
        assert_eq!(hook_for(InteractionType::Ping), None);
        assert_eq!(hook_for(InteractionType::Modal), None);
    }

    struct FailingResponder;

    impl Responder for FailingResponder {
        async fn tell(&self, _message: &str) -> Result<(), Error> {
            Err(Error::Config("the followup itself failed".to_owned()))
        }
    }

    /// A [`Responder`] double that records the last message it was told,
    /// so a test can check exactly what a caller handed it rather than
    /// only whether telling succeeded.
    #[derive(Default)]
    struct RecordingResponder {
        seen: Mutex<Option<String>>,
    }

    impl RecordingResponder {
        fn seen(&self) -> Option<String> {
            self.seen.lock().expect("lock").clone()
        }
    }

    impl Responder for RecordingResponder {
        async fn tell(&self, message: &str) -> Result<(), Error> {
            *self.seen.lock().expect("lock") = Some(message.to_owned());
            Ok(())
        }
    }

    /// The old code ended this path in `.catch(() => {})` at
    /// `interaction.ts:55`, so a failure to report a failure vanished.
    /// Equality against [`command_failed_message`] and
    /// [`could_not_tell_the_user_message`] pins the exact lines this path
    /// prints, not merely that some substring of them showed up.
    #[tokio::test]
    async fn a_failure_to_report_a_failure_still_reaches_stderr() {
        let command_err = Error::Config("the command itself failed".to_owned());
        let followup_err = Error::Config("the followup itself failed".to_owned());
        let lines = report_failure("system", Err(command_err), &FailingResponder).await;
        assert_eq!(
            lines,
            vec![
                command_failed_message(
                    "system",
                    &Error::Config("the command itself failed".to_owned())
                ),
                could_not_tell_the_user_message("/system failed", &followup_err),
            ]
        );
    }

    /// A successful outcome reports nothing: there is no failure to tell
    /// anyone about.
    #[tokio::test]
    async fn a_success_reports_nothing() {
        let lines = report_failure("system", Ok(()), &FailingResponder).await;
        assert!(lines.is_empty(), "{lines:?}");
    }

    /// A followup whose content would exceed Discord's own limit is fitted
    /// to it before `report_failure` ever hands it to a [`Responder`].
    #[tokio::test]
    async fn a_followup_message_fits_discords_own_content_limit() {
        let huge = "x".repeat(3000);
        let responder = RecordingResponder::default();
        let _ = report_failure("system", Err(Error::Config(huge)), &responder).await;
        let seen = responder.seen().expect("report_failure told the user");
        assert!(seen.chars().count() <= limits::MESSAGE_CONTENT_LIMIT);
    }

    /// An unrecognised `custom_id` is answered with [`unknown_component_reply`]
    /// through [`tell_or_log`], the exact message and nothing else.
    #[tokio::test]
    async fn an_unknown_component_id_is_answered_rather_than_ignored() {
        assert!(parse_custom_id("something:else").is_none());
        let responder = RecordingResponder::default();
        let line = tell_or_log("an unknown button", unknown_component_reply(), &responder).await;
        assert_eq!(line, None);
        assert_eq!(responder.seen(), Some(unknown_component_reply().to_owned()));
    }

    /// When telling the user about an unknown button also fails, the
    /// returned line matches [`could_not_tell_the_user_message`] exactly.
    #[tokio::test]
    async fn telling_an_unknown_button_user_logs_when_that_also_fails() {
        let followup_err = Error::Config("the followup itself failed".to_owned());
        let line = tell_or_log(
            "an unknown button",
            unknown_component_reply(),
            &FailingResponder,
        )
        .await
        .expect("a failing responder is reported");
        assert_eq!(
            line,
            could_not_tell_the_user_message("an unknown button", &followup_err)
        );
    }

    #[test]
    fn nothing_printed_for_a_person_carries_a_dash() {
        crate::test_support::assert_no_dashes(unknown_component_reply());
    }
}
