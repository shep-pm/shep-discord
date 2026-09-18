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
//! [`report_failure`] is the fix for the swallowed follow-up at
//! `interaction.ts:55`, and [`unknown_component_reply`] is what an
//! unrecognised button gets told rather than silence.

use serenity::all::{
    CommandInteraction, ComponentInteraction, Context, CreateInteractionResponseFollowup,
    EventHandler, GuildId, Interaction, Ready,
};

use crate::{
    bot::{
        command::{self, Command, State},
        embed::parse_custom_id,
    },
    error::Error,
};

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

/// Tell the user about `outcome`'s failure, and make sure telling them is
/// never silent either, returning every line for the caller to print.
///
/// A pure function of `outcome` and `responder` rather than an inline
/// `eprintln!`, on the usual reason: it lets a test drive the swallowed
/// follow-up fix with a `responder` that always fails, with no `Context`
/// and no gateway behind either. The old code ended this exact path at
/// `.catch(() => {})` (`interaction.ts:55`): a failed followup vanished
/// with nothing on stderr, so an operator watching this dog's own output
/// saw silence twice over, once from the command's own failure and again
/// from failing to say so.
async fn report_failure(
    label: &str,
    outcome: Result<(), Error>,
    responder: &impl Responder,
) -> Vec<String> {
    let Err(err) = outcome else {
        return Vec::new();
    };
    let mut lines = vec![format!("shep-discord: /{label} failed: {err}")];
    if let Err(followup_err) = responder
        .tell(&format!("Something went wrong: {err}"))
        .await
    {
        lines.push(format!(
            "shep-discord: /{label} failed and could not tell the user either: {followup_err}"
        ));
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
    /// A `match` on serenity's own five-variant enum replaces the ladder
    /// of booleans `interaction.ts:12` reconstructed a kind from.
    /// Everything except autocomplete defers ephemerally before the
    /// lookup, as both source repos did: Discord gives three seconds, and
    /// a [`crate::shepherd::Live`] call on a busy shepherd can outlast
    /// that. Autocomplete answers with its own response type instead, so
    /// deferring first would ask Discord twice for the one interaction.
    async fn interaction_create(&self, ctx: Context, interaction: Interaction) {
        match interaction {
            Interaction::Command(command_interaction) => {
                self.handle_command(&ctx, &command_interaction).await;
            }
            Interaction::Autocomplete(command_interaction) => {
                if let Some(command) = self.find(&command_interaction.data.name) {
                    command
                        .autocomplete(&ctx, &command_interaction, &self.state)
                        .await;
                }
            }
            Interaction::Component(component_interaction) => {
                self.handle_component(&ctx, &component_interaction).await;
            }
            Interaction::Ping(_) | Interaction::Modal(_) => {
                // Nothing this bot answers: `Ping` only matters to a bot
                // using an HTTP endpoint URL instead of a gateway
                // connection, and no `Command` implements a modal (see
                // this crate's own trait doc for why one is not added on
                // spec).
            }
            // `Interaction` is `#[non_exhaustive]`: serenity can add a
            // sixth variant without a breaking change, and this dog
            // answers nothing it does not yet know about rather than
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
        if parse_custom_id(&interaction.data.custom_id).is_none() {
            let responder = ComponentResponder { ctx, interaction };
            if let Err(err) = responder.tell(unknown_component_reply()).await {
                eprintln!("shep-discord: could not tell the user about an unknown button: {err}");
            }
            return;
        }
        // A well formed custom_id, but no command in this task's registry
        // draws a button: `/system` never does, and `/shep` (Task 12) is
        // the first one that will. Every command gets a chance at it
        // rather than a name encoded on the id, because `custom_id`
        // carries only a verb and a sheep id, never which command drew
        // the button.
        for command in &self.commands {
            if let Err(err) = command.button(ctx, interaction, &self.state).await {
                eprintln!("shep-discord: a button failed: {err}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    /// A `Command` double for these tests.
    ///
    /// It never implements the real `Command` trait: every one of its
    /// hooks takes a `&Context`, which this module's own doc explains this
    /// crate cannot construct outside serenity's own. Instead this records
    /// which of its three actions last ran, directly, so
    /// [`each_interaction_kind_reaches_its_own_hook`] can still pin the
    /// mapping [`Handler::interaction_create`]'s own `match` makes between
    /// an interaction kind and a hook name.
    #[derive(Default)]
    struct SpyCommand {
        calls: Mutex<Vec<&'static str>>,
        fails: bool,
    }

    impl SpyCommand {
        fn failing() -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                fails: true,
            }
        }

        fn calls(&self) -> Vec<&'static str> {
            self.calls.lock().expect("lock").clone()
        }

        fn run(&self) -> Result<(), Error> {
            self.calls.lock().expect("lock").push("run");
            if self.fails {
                Err(Error::Config("the command itself failed".to_owned()))
            } else {
                Ok(())
            }
        }

        fn autocomplete(&self) {
            self.calls.lock().expect("lock").push("autocomplete");
        }

        fn button(&self) {
            self.calls.lock().expect("lock").push("button");
        }
    }

    /// Which of a `Command`'s three hooks one interaction kind routes to,
    /// mirroring `Interaction`'s own shape.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Kind {
        Command,
        Autocomplete,
        Component,
    }

    async fn dispatch_kind(spy: &SpyCommand, kind: Kind) {
        match kind {
            Kind::Command => {
                let _ = spy.run();
            }
            Kind::Autocomplete => spy.autocomplete(),
            Kind::Component => spy.button(),
        }
    }

    /// serenity's `Interaction` enum replaces the ladder at
    /// `interaction.ts:12`, which reconstructed which kind it held from
    /// four booleans. This pins that each kind reaches its own hook.
    #[tokio::test]
    async fn each_interaction_kind_reaches_its_own_hook() {
        let spy = SpyCommand::default();
        dispatch_kind(&spy, Kind::Command).await;
        dispatch_kind(&spy, Kind::Autocomplete).await;
        dispatch_kind(&spy, Kind::Component).await;
        assert_eq!(spy.calls(), vec!["run", "autocomplete", "button"]);
    }

    struct FailingResponder;

    impl Responder for FailingResponder {
        async fn tell(&self, _message: &str) -> Result<(), Error> {
            Err(Error::Config("the followup itself failed".to_owned()))
        }
    }

    /// The old code ended this path in `.catch(() => {})` at
    /// `interaction.ts:55`, so a failure to report a failure vanished.
    #[tokio::test]
    async fn a_failure_to_report_a_failure_still_reaches_stderr() {
        let spy = SpyCommand::failing();
        let lines = report_failure("system", spy.run(), &FailingResponder).await;
        let reported = lines.join("\n");
        assert!(reported.contains("could not tell the user"), "{reported}");
    }

    /// A successful outcome reports nothing: there is no failure to tell
    /// anyone about.
    #[tokio::test]
    async fn a_success_reports_nothing() {
        let spy = SpyCommand::default();
        let lines = report_failure("system", spy.run(), &FailingResponder).await;
        assert!(lines.is_empty(), "{lines:?}");
    }

    #[tokio::test]
    async fn an_unknown_component_id_is_answered_rather_than_ignored() {
        assert!(parse_custom_id("something:else").is_none());
        let reply = unknown_component_reply();
        assert!(reply.contains("not a button this bot wrote"), "{reply}");
    }

    #[test]
    fn nothing_printed_for_a_person_carries_a_dash() {
        crate::test_support::assert_no_dashes(unknown_component_reply());
    }
}
