//! The contract every slash command implements, the state they all read,
//! and getting the set of them in front of Discord.

use core::future::Future;
use std::{
    pin::Pin,
    sync::{Arc, Mutex},
};

use serenity::all::{
    CommandInteraction, ComponentInteraction, Context, CreateCommand, GuildId, Http,
};

use crate::{bot::monitor::Monitor, config::Config, error::Error, names::Names, shepherd::Live};

/// The state every [`Command`] reads to reach the shepherd, this dog's own
/// resolved config, and the current id-to-name cache.
///
/// Named `bot::State`. [`crate::stream::state::State`] is a second, unrelated
/// type in this crate, built for the log-buffering pipeline; the two share
/// no fields and no lifecycle, so neither is ever called just "State" in a
/// doc comment that a reader could take for the other one.
///
/// `std::sync::Mutex` around `names` rather than `tokio::sync::Mutex`:
/// [`Names::refresh`] and [`Names::get`] are both synchronous, so nothing
/// here ever holds this lock across an `.await`, and a plain mutex is the
/// simpler tool for a lock that is only ever held across a few synchronous
/// field reads.
///
/// `Clone` because [`crate::bot::run`] rebuilds a fresh [`interaction::Handler`]
/// on every gateway reconnect attempt: every field is an `Arc`, so cloning
/// shares the same shepherd session, config, and name cache across
/// attempts rather than copying any of them.
///
/// [`interaction::Handler`]: crate::bot::interaction::Handler
#[derive(Clone)]
pub struct State {
    pub live: Arc<Live>,
    pub config: Arc<Config>,
    pub names: Arc<Mutex<Names>>,
    /// The live monitor, shared with whatever else drives it: `/monitor`
    /// starts and stops the refresh task through this, and the log
    /// stream's own bus subscription redraws a sheep through the same
    /// one. An `Arc<Monitor>` rather than an `Arc<Mutex<Monitor>>`:
    /// [`Monitor`] takes `&self` throughout and does its own locking, per
    /// sheep, precisely so that one slow Discord edit cannot stall every
    /// other sheep behind it. See [`crate::bot::monitor`]'s module doc.
    pub monitor: Arc<Monitor>,
}

/// One slash command: the payload that registers it and what runs when
/// somebody invokes it.
///
/// `run`, `autocomplete` and `button` return a boxed future rather than
/// being declared `async fn`: [`registry`] and [`register`] both hold
/// commands behind `dyn Command`, and a native `async fn` in a trait
/// cannot be called through a trait object, only through a generic bound.
/// [`crate::stream::Sink`] makes the opposite choice for the opposite
/// reason, argued in its own module doc: it is used generically only, and
/// never behind `dyn`.
///
/// There is no `modal` hook. Neither source repo this project ports
/// implements one, and a trait method nothing implements is a method that
/// rots rather than one that is ready for later.
pub trait Command: Send + Sync {
    /// The registration payload Discord is told about: name, description,
    /// options, and the permission gate every command in this dog's
    /// [`registry`] carries.
    fn data(&self) -> CreateCommand;

    /// The name Discord invokes this command by.
    ///
    /// A second, required method rather than reading `data().name` back
    /// out: [`CreateCommand`] never gives that field a public getter (it
    /// only takes a name in [`CreateCommand::new`] and serializes one out
    /// again), so the one place that needs it for a lookup, matching an
    /// incoming interaction's own name against a registered
    /// [`Command`], would otherwise have to serialize `data()` to JSON on
    /// every dispatch just to read a string back out of it.
    fn name(&self) -> &'static str;

    /// Answer a `/name ...` invocation.
    ///
    /// # Errors
    /// Whatever the command itself could not do. The caller decides how to
    /// tell the user; this only decides what happened.
    fn run<'a>(
        &'a self,
        ctx: &'a Context,
        interaction: &'a CommandInteraction,
        state: &'a State,
    ) -> Pin<Box<dyn Future<Output = Result<(), Error>> + Send + 'a>>;

    /// Answer an autocomplete request for one of this command's options.
    ///
    /// A no-op by default: most commands take no autocompleting option,
    /// and Discord's own reading of silence here is an empty suggestion
    /// list, never an error shown to the user.
    fn autocomplete<'a>(
        &'a self,
        _ctx: &'a Context,
        _interaction: &'a CommandInteraction,
        _state: &'a State,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        Box::pin(async {})
    }

    /// Answer a button this command's own embed put in front of a user.
    ///
    /// A no-op by default, on [`Self::autocomplete`]'s own reasoning: only
    /// a command that draws buttons needs to override it.
    ///
    /// # Errors
    /// Whatever answering the button could not do.
    fn button<'a>(
        &'a self,
        _ctx: &'a Context,
        _interaction: &'a ComponentInteraction,
        _state: &'a State,
    ) -> Pin<Box<dyn Future<Output = Result<(), Error>> + Send + 'a>> {
        Box::pin(async { Ok(()) })
    }
}

/// Every slash command this dog answers, boxed so [`register`] and
/// [`crate::bot::interaction::Handler`] can hold them in one collection.
///
/// `/system`, `/shep` and `/monitor`, the whole set this dog answers.
#[must_use]
pub fn registry() -> Vec<Box<dyn Command>> {
    vec![
        Box::new(crate::bot::commands::system::System),
        Box::new(crate::bot::commands::shep::ShepCommand),
        Box::new(crate::bot::commands::monitor::MonitorCommand),
    ]
}

/// Register every command in `commands` with `guild_id`, replacing
/// whatever set Discord already has for it.
///
/// Runs unconditionally, on every boot. The old code skipped this step
/// under `NODE_ENV=development` (`register.ts:12`); that skip is not
/// ported, because it silently hid a newly added command for as long as
/// the flag stayed set, which is exactly the condition under which a
/// command gets added.
///
/// # Errors
/// Whatever `GuildId::set_commands` returns: an invalid token, a payload
/// the guild refuses, or the request itself failing.
pub async fn register(
    http: &Http,
    guild_id: GuildId,
    commands: &[Box<dyn Command>],
) -> Result<Vec<String>, serenity::Error> {
    let payload: Vec<CreateCommand> = commands.iter().map(|command| command.data()).collect();
    let registered = guild_id.set_commands(http, payload).await?;
    Ok(registered.into_iter().map(|command| command.name).collect())
}

#[cfg(test)]
mod tests {
    use serenity::all::Permissions;

    use super::*;

    #[test]
    fn every_registered_command_is_admin_gated() {
        // Both source repos gated all three on Administrator. A command
        // that restarts production is not one an unprivileged member gets
        // to try. `Some` alone would pass on any value, wrong permission
        // bit included, so the exact serialized bitfield is what is
        // checked; serenity serializes it as that bitfield's own decimal
        // string.
        let admin = Permissions::ADMINISTRATOR.bits().to_string();
        for command in registry() {
            let json = serde_json::to_value(command.data()).expect("json");
            assert_eq!(
                json["default_member_permissions"], admin,
                "{} is not permission gated",
                json["name"]
            );
        }
    }

    #[test]
    fn no_two_commands_share_a_name() {
        let mut names: Vec<String> = registry()
            .iter()
            .map(|c| {
                serde_json::to_value(c.data()).expect("json")["name"]
                    .as_str()
                    .expect("name")
                    .to_owned()
            })
            .collect();
        names.sort();
        let count = names.len();
        names.dedup();
        assert_eq!(
            names.len(),
            count,
            "a duplicate name silently shadows a command"
        );
    }

    /// No registered command's serialized JSON reaches a person with a
    /// dash anywhere in it, at any depth. `/system`'s own dash check only
    /// ever reached [`crate::bot::commands::system::not_sampling_message`];
    /// nothing swept `CreateCommand`'s own serialized form until commit
    /// `09b50c5`, and that fix read only the top-level `description`,
    /// which was every level `/system` had. `/shep` was the first command
    /// in this crate to nest a subcommand and a sub-option under it, and a
    /// flat check would have missed both: this walks the whole tree so a
    /// tenth level of nesting later is covered without anyone remembering
    /// to extend the check by hand again.
    #[test]
    fn no_registered_commands_json_carries_a_dash_at_any_depth() {
        for command in registry() {
            let json = serde_json::to_value(command.data()).expect("json");
            crate::test_support::assert_no_dashes_deep(&json);
        }
    }

    /// The three commands this dog answers, pinned by name so the two
    /// generic tests above cannot pass vacuously against an empty or
    /// incomplete registry.
    #[test]
    fn the_registry_carries_every_command_this_dog_answers() {
        let names: Vec<&'static str> = registry().iter().map(|c| c.name()).collect();
        assert_eq!(names, vec!["system", "shep", "monitor"]);
    }
}
