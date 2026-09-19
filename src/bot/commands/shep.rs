//! `/shep`: drive the flock. One subcommand per [`Verb`], plus `list`.
//!
//! This is the port of `discord-pm2`'s most-churned file,
//! `commands/pm2.ts`, at eleven commits. That file gave every command one
//! shared, optional `name` option (`pm2.ts:41`) and caught a missing name
//! down in the service layer, as a thrown error. Discord can refuse it
//! before the interaction is ever sent instead: the six verbs that act on
//! a sheep (`start`, `stop`, `restart`, `reload`, `delete`, `flush`) are
//! six separate subcommands here, each with its own required,
//! autocompleting `name`. `list` and `save` take no options at all.
//! `reopen` is the one verb in between: its own `name` is optional,
//! matching `shep-cli`'s own `ReopenArgs` (a reopen destroys nothing, so
//! it does not need a name the way `stop`/`restart`/`delete` do), and
//! acts on [`SelectorSpec::All`] only when nothing was named.
//!
//! # Why `list` is the one subcommand this file cannot just call `run`
//! for and defer to
//!
//! Every other subcommand asks the shepherd to act and answers with the
//! one sentence [`Live::act`] hands back. `list` instead draws one embed
//! per sheep, through [`embed::process_embed`], and a flock large enough
//! is more than one Discord message can hold: [`crate::limits::MESSAGE_CHARACTER_BUDGET`]
//! is a sum across every embed on one message, not a per-embed limit, the
//! same rule [`crate::stream::pack`] exists for on the log-streaming side.
//! `ShepCommand::answer_list` reuses that module's own [`crate::stream::pack::pack_by_budget`]
//! rather than assuming every sheep's embed fits together the way a
//! single sheep's own worst case, worked out in [`embed::process_embed`]'s
//! doc comment, does not by itself say anything about the sum of several.
//!
//! # What this file does not draw
//!
//! [`embed::process_buttons`] exists, and its own doc comment already
//! names `/shep list` as the caller that will draw it. This file does not:
//! Discord caps one message at five action rows total, and a listing of
//! more than five sheep, packed several to a message on the character
//! budget alone, would blow that count long before it blew the character
//! one. Wiring buttons into a per-embed listing needs its own answer to
//! that (one sheep's row per message, most likely, giving up the packing
//! this file's own budget test proves) and is not part of what this task
//! was asked to build; see the task report.
//!
//! # Autocomplete over the live flock
//!
//! [`ShepCommand::suggestions`] asks the shepherd fresh on every keystroke
//! rather than reading a cache, the one part of `pm2.ts`'s own
//! `autoComplete` worth keeping: a name typed against a stale list can
//! send a verb at a sheep that was renamed or deleted since the cache was
//! built. Unlike the old code, it also filters by what has been typed so
//! far and answers empty rather than the old code's unconditional `all`
//! entry (`pm2.ts:52`), which this dog's own verbs have no equivalent of:
//! every verb here already takes a `name`, required, so there is nothing
//! for an `all` suggestion to mean.

use core::future::Future;
use std::pin::Pin;

use serenity::all::{
    AutocompleteChoice, CommandInteraction, CommandOptionType, Context, CreateAutocompleteResponse,
    CreateCommand, CreateCommandOption, CreateInteractionResponse,
    CreateInteractionResponseFollowup, Permissions, ResolvedOption, ResolvedValue,
};
use shep_client::shep_core::protocol::{ProcessInfo, SelectorSpec};

use crate::{
    bot::{
        command::{Command, State},
        embed,
    },
    error::Error,
    limits,
    shepherd::{Live, Verb},
    stream::pack::pack_by_budget,
};

/// The most suggestions one autocomplete answer may carry. Discord's own
/// limit.
const AUTOCOMPLETE_CHOICE_LIMIT: usize = 25;

/// `/shep`.
pub struct ShepCommand;

/// The eight name-bearing subcommands this file's `data` declares, in
/// registration order, and the [`Verb`] each drives. `list` is not in this
/// table: it has no `Verb` of its own, since it reads the flock rather than
/// acting on it.
const VERBS: &[(&str, Verb)] = &[
    ("start", Verb::Start),
    ("stop", Verb::Stop),
    ("restart", Verb::Restart),
    ("reload", Verb::Reload),
    ("delete", Verb::Delete),
    ("flush", Verb::Flush),
    ("save", Verb::Save),
    ("reopen", Verb::Reopen),
];

fn verb_for_subcommand(name: &str) -> Option<Verb> {
    VERBS
        .iter()
        .find(|(candidate, _)| *candidate == name)
        .map(|(_, verb)| *verb)
}

/// The required, autocompleting `name` option every sheep-targeting
/// subcommand declares.
fn name_option() -> CreateCommandOption {
    CreateCommandOption::new(CommandOptionType::String, "name", "Which sheep, by name.")
        .required(true)
        .set_autocomplete(true)
}

/// The same option as [`name_option`], but optional: `reopen`'s own
/// `name`, on [`selector_for`]'s own reasoning for why `reopen` alone
/// among the verbs that take one does not require it.
fn optional_name_option() -> CreateCommandOption {
    CreateCommandOption::new(CommandOptionType::String, "name", "Which sheep, by name.")
        .required(false)
        .set_autocomplete(true)
}

/// One subcommand that acts on a named sheep, the name required.
fn verb_subcommand(name: &str, description: &str) -> CreateCommandOption {
    CreateCommandOption::new(CommandOptionType::SubCommand, name, description)
        .add_sub_option(name_option())
}

/// One subcommand that takes no options at all: `list` and `save`.
fn bare_subcommand(name: &str, description: &str) -> CreateCommandOption {
    CreateCommandOption::new(CommandOptionType::SubCommand, name, description)
}

/// The text shown when a listing has nothing to draw: every sheep this
/// dog knows about is a dog itself, or the flock is empty.
fn empty_flock_message() -> &'static str {
    "There is nothing in the flock to list."
}

/// Fit a reply from [`Live::act`] to Discord's own content limit before it
/// ever reaches a followup.
///
/// [`Live::act`]'s own sentence joins sheep names with no bound of its
/// own: a fold or a regex selector can match as many sheep as the flock
/// holds, and a listing shepherd's reply is exactly as unbounded as any
/// other operator-supplied string this crate has already had to fit.
fn reply_content(reply: &str) -> String {
    limits::fit(reply, limits::MESSAGE_CONTENT_LIMIT)
}

/// Extract the `name` option's value from a subcommand's own resolved
/// options, or `None` when there is not one: `list` and `save` have no
/// `name` option to find one under, and `reopen`'s own `name` is present
/// only when an operator gave one.
fn name_from(value: &ResolvedValue<'_>) -> Option<String> {
    let ResolvedValue::SubCommand(options) = value else {
        return None;
    };
    options.iter().find_map(|option| {
        if option.name != "name" {
            return None;
        }
        match option.value {
            ResolvedValue::String(name) => Some(name.to_owned()),
            _ => None,
        }
    })
}

/// What `verb` acts on, given the `name` its subcommand resolved to, if
/// any.
///
/// Split out from [`selector_for`] so this decision is testable on its
/// own: [`ResolvedValue`] is `#[non_exhaustive]` in serenity, and a
/// struct-literal [`ResolvedOption`] cannot be built from this crate (see
/// the task report), so a test cannot hand [`selector_for`] a resolved
/// "name is present" case directly. A plain `Option<String>` carries the
/// same fact without that restriction.
///
/// [`Verb::Save`] always resolves to [`SelectorSpec::All`]: it has no
/// `name` option to find one under, and its own selector is unused on
/// [`Live::act`]'s `Save` path regardless of what is passed (see
/// `shepherd`'s own module doc).
///
/// [`Verb::Reopen`] is the one verb whose `name` is optional rather than
/// required or absent, matching `shep-cli`'s own `ReopenArgs`: its doc
/// comment gives the reason, "the selector is optional, defaulting to
/// `DEFAULT_SELECTOR`, where `stop`/`restart`/`delete` all demand one:
/// those destroy something, and a reopen destroys nothing." Present, it
/// selects that sheep; absent, it resolves to `SelectorSpec::All`, the
/// same "reopen everything" behavior this dog had before it gained a
/// `name` option at all, so an operator who never names one keeps the
/// original, blanket reopen.
///
/// Every other verb's subcommand declares `name` required, so Discord
/// never sends one of them without it; an interaction that somehow
/// arrived without one anyway (a stale command definition mid-redeploy,
/// say) resolves to an empty name rather than a panic, and the shepherd
/// is left to refuse a sheep named nothing the way it would refuse any
/// other name it does not know.
fn selector_for_name(verb: Verb, name: Option<String>) -> SelectorSpec {
    match verb {
        Verb::Save => SelectorSpec::All,
        Verb::Reopen => name.map_or(SelectorSpec::All, SelectorSpec::Name),
        _ => SelectorSpec::Name(name.unwrap_or_default()),
    }
}

/// What `verb` acts on, given the subcommand's own resolved options. See
/// [`selector_for_name`] for the actual decision.
fn selector_for(verb: Verb, value: &ResolvedValue<'_>) -> SelectorSpec {
    selector_for_name(verb, name_from(value))
}

impl ShepCommand {
    /// Names in the live flock whose name contains `partial`, for
    /// Discord's own autocomplete, case-insensitively and capped at
    /// [`AUTOCOMPLETE_CHOICE_LIMIT`].
    ///
    /// Empty rather than an error when the shepherd cannot be reached:
    /// Discord shows nothing for a failed autocomplete either way, and an
    /// error surfaced here would be logged once per keystroke rather than
    /// once, the way [`Command::run`]'s own failure is.
    ///
    /// A name past [`limits::AUTOCOMPLETE_CHOICE_NAME_LIMIT`] is dropped
    /// rather than fitted: see that constant's own doc comment for why
    /// shortening it would offer a sheep that does not exist, and why
    /// leaving it in at all would silently empty every suggestion for
    /// that keystroke, not just the one too long.
    async fn suggestions(&self, live: &Live, partial: &str) -> Vec<String> {
        let Ok(flock) = live.flock().await else {
            return Vec::new();
        };
        let needle = partial.to_lowercase();
        flock
            .into_iter()
            .map(|info| info.name)
            .filter(|name| name.to_lowercase().contains(&needle))
            .filter(|name| name.chars().count() <= limits::AUTOCOMPLETE_CHOICE_NAME_LIMIT)
            .take(AUTOCOMPLETE_CHOICE_LIMIT)
            .collect()
    }

    /// The flock `/shep list` draws, with every dog left out when
    /// `ignore_dogs` is set.
    ///
    /// `/shep list` always asks for `true`: an operator reaching for this
    /// dog's own verbs wants the sheep the shepherd tends, not the dogs
    /// tending them, the same population `shep list` itself shows by
    /// default on the command line. The parameter exists so this method's
    /// own test can prove the filter works without also proving `/shep
    /// list`'s own choice of when to apply it.
    ///
    /// # Errors
    /// Whatever [`Live::flock`] could not answer.
    async fn flock_for_listing(
        &self,
        live: &Live,
        ignore_dogs: bool,
    ) -> Result<Vec<ProcessInfo>, Error> {
        let flock = live.flock().await?;
        Ok(if ignore_dogs {
            flock
                .into_iter()
                .filter(|info| info.dog.is_none())
                .collect()
        } else {
            flock
        })
    }

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

    /// Answer `/shep list`: one embed per sheep, packed several to a
    /// message under [`crate::limits::MESSAGE_CHARACTER_BUDGET`] and
    /// [`crate::limits::EMBED_MAX_COUNT`] rather than one message per
    /// sheep or, worse, every sheep on one message regardless of size.
    /// See the module doc for why this is the one subcommand that answers
    /// with more than a sentence.
    async fn answer_list(
        &self,
        ctx: &Context,
        interaction: &CommandInteraction,
        state: &State,
    ) -> Result<(), Error> {
        let flock = self.flock_for_listing(&state.live, true).await?;
        if flock.is_empty() {
            return self.answer(ctx, interaction, empty_flock_message()).await;
        }

        for group in pack_by_budget(
            flock,
            limits::MESSAGE_CHARACTER_BUDGET,
            limits::EMBED_MAX_COUNT,
            embed::embed_character_count,
        ) {
            let embeds = group.iter().map(embed::process_embed).collect();
            interaction
                .create_followup(
                    ctx,
                    CreateInteractionResponseFollowup::new()
                        .embeds(embeds)
                        .ephemeral(true),
                )
                .await?;
        }
        Ok(())
    }

    /// Answer a verb subcommand: ask the shepherd to act, and relay the
    /// one sentence it hands back.
    async fn answer_verb(
        &self,
        verb: Verb,
        selector: SelectorSpec,
        ctx: &Context,
        interaction: &CommandInteraction,
        state: &State,
    ) -> Result<(), Error> {
        let reply = state.live.act(verb, selector).await?;
        self.answer(ctx, interaction, &reply_content(&reply)).await
    }
}

impl Command for ShepCommand {
    fn data(&self) -> CreateCommand {
        CreateCommand::new(self.name())
            .description("Drive the flock shep tends.")
            .default_member_permissions(Permissions::ADMINISTRATOR)
            .add_option(verb_subcommand(
                "start",
                "Restart a sheep that is already registered.",
            ))
            .add_option(verb_subcommand("stop", "Stop a sheep, staying registered."))
            .add_option(verb_subcommand("restart", "Restart a sheep."))
            .add_option(verb_subcommand(
                "reload",
                "Replace a sheep with a fresh instance, one at a time.",
            ))
            .add_option(verb_subcommand("delete", "Stop and deregister a sheep."))
            .add_option(verb_subcommand("flush", "Empty a sheep's log files."))
            .add_option(bare_subcommand("list", "List every sheep in the flock."))
            .add_option(bare_subcommand("save", "Write the muster roll now."))
            .add_option(
                CreateCommandOption::new(
                    CommandOptionType::SubCommand,
                    "reopen",
                    "Reopen a sheep's log files, or every sheep's, for an external rotator.",
                )
                .add_sub_option(optional_name_option()),
            )
    }

    fn name(&self) -> &'static str {
        "shep"
    }

    fn run<'a>(
        &'a self,
        ctx: &'a Context,
        interaction: &'a CommandInteraction,
        state: &'a State,
    ) -> Pin<Box<dyn Future<Output = Result<(), Error>> + Send + 'a>> {
        Box::pin(async move {
            let options = interaction.data.options();
            // Discord only ever sends the one subcommand `data` declared
            // the operator picked; this crate's own registration is the
            // only thing that could put anything else here, and that is a
            // bug in this file to catch in review or in
            // `the_verbs_that_need_a_name_declare_it_required`, not a
            // shape an operator can reach.
            let sub: &ResolvedOption<'_> = match options.first() {
                Some(sub) => sub,
                None => return Ok(()),
            };

            if sub.name == "list" {
                return self.answer_list(ctx, interaction, state).await;
            }

            let Some(verb) = verb_for_subcommand(sub.name) else {
                return Ok(());
            };
            let selector = selector_for(verb, &sub.value);
            self.answer_verb(verb, selector, ctx, interaction, state)
                .await
        })
    }

    fn autocomplete<'a>(
        &'a self,
        ctx: &'a Context,
        interaction: &'a CommandInteraction,
        state: &'a State,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        Box::pin(async move {
            let Some(focused) = interaction.data.autocomplete() else {
                return;
            };
            let choices = self.suggestions(&state.live, focused.value).await;
            let response = CreateAutocompleteResponse::new().set_choices(
                choices
                    .into_iter()
                    .map(AutocompleteChoice::from)
                    .collect::<Vec<_>>(),
            );
            if let Err(err) = interaction
                .create_response(ctx, CreateInteractionResponse::Autocomplete(response))
                .await
            {
                eprintln!("shep-discord: /shep autocomplete failed: {err}");
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use shep_client::shep_core::protocol::{Request, Response};

    use super::*;
    use crate::test_support::{dog_sample, sample, test_live};

    /// The old code marked `name` optional for every verb at `pm2.ts:46` and
    /// caught the missing case as a thrown error at the service layer. Discord
    /// can refuse it before the interaction is ever sent.
    #[test]
    fn the_verbs_that_need_a_name_declare_it_required() {
        let json = serde_json::to_value(ShepCommand.data()).expect("json");
        let sub: Vec<_> = json["options"]
            .as_array()
            .expect("options")
            .iter()
            .collect();
        for verb in ["start", "stop", "restart", "reload", "delete", "flush"] {
            let option = sub.iter().find(|o| o["name"] == verb).expect(verb);
            let name_option = option["options"]
                .as_array()
                .expect("sub options")
                .iter()
                .find(|o| o["name"] == "name")
                .expect("name");
            assert_eq!(name_option["required"], true, "{verb} needs a name");
        }
        for verb in ["list", "save", "reopen"] {
            assert!(sub.iter().any(|o| o["name"] == verb), "{verb} missing");
        }
    }

    /// `reopen` is the one verb whose `name` is present but optional,
    /// matching `shep-cli`'s own `ReopenArgs`: a reopen destroys nothing,
    /// so it does not need one the way `stop`, `restart`, and `delete` do
    /// (see [`selector_for_name`]'s own doc comment). `list` and `save`
    /// still take no options at all.
    #[test]
    fn reopen_declares_an_optional_autocompleting_name() {
        let json = serde_json::to_value(ShepCommand.data()).expect("json");
        let sub: Vec<_> = json["options"]
            .as_array()
            .expect("options")
            .iter()
            .collect();
        let reopen = sub.iter().find(|o| o["name"] == "reopen").expect("reopen");
        let name_option = reopen["options"]
            .as_array()
            .expect("sub options")
            .iter()
            .find(|o| o["name"] == "name")
            .expect("name");
        assert_eq!(
            name_option["required"], false,
            "reopen's name must not be required"
        );
        assert_eq!(
            name_option["autocomplete"], true,
            "reopen's name must autocomplete"
        );

        for verb in ["list", "save"] {
            let option = sub.iter().find(|o| o["name"] == verb).expect(verb);
            assert!(
                option["options"].as_array().is_none_or(Vec::is_empty),
                "{verb} should take no options"
            );
        }
    }

    /// `reopen` acts on everything when nothing is named, the same
    /// blanket behavior this dog had before it gained a `name` option at
    /// all.
    #[test]
    fn reopen_with_no_name_selects_everything() {
        assert_eq!(selector_for_name(Verb::Reopen, None), SelectorSpec::All);
    }

    /// `reopen` given a name selects only that sheep, unlike `save`,
    /// which has no target at all regardless of what is passed.
    #[test]
    fn reopen_with_a_name_selects_that_sheep() {
        assert_eq!(
            selector_for_name(Verb::Reopen, Some("web".to_owned())),
            SelectorSpec::Name("web".to_owned())
        );
    }

    #[test]
    fn save_always_selects_everything_regardless_of_a_name() {
        assert_eq!(selector_for_name(Verb::Save, None), SelectorSpec::All);
        assert_eq!(
            selector_for_name(Verb::Save, Some("web".to_owned())),
            SelectorSpec::All
        );
    }

    /// Every other verb still names the sheep it was given, or an empty
    /// name in the structurally-unreachable case Discord's own required
    /// option never actually lets through (see [`selector_for_name`]'s
    /// own doc comment).
    #[test]
    fn a_verb_needing_a_name_selects_the_name_it_was_given() {
        assert_eq!(
            selector_for_name(Verb::Start, Some("web".to_owned())),
            SelectorSpec::Name("web".to_owned())
        );
        assert_eq!(
            selector_for_name(Verb::Start, None),
            SelectorSpec::Name(String::new())
        );
    }

    #[tokio::test]
    async fn autocomplete_offers_the_live_flock() {
        let (live, mut fake) = test_live().await;
        fake.expect(Request::ListFlock)
            .answer(Response::Flock(vec![sample("web"), sample("api")]));
        let offered = ShepCommand.suggestions(&live, "").await;
        assert_eq!(offered, vec!["web", "api"]);
    }

    #[tokio::test]
    async fn autocomplete_answers_empty_rather_than_erroring() {
        // Discord shows nothing for a failed autocomplete either way, and an
        // error here would be logged once per keystroke.
        let (live, mut fake) = test_live().await;
        fake.expect(Request::ListFlock).answer_error("no shepherd");
        assert!(ShepCommand.suggestions(&live, "").await.is_empty());
    }

    #[tokio::test]
    async fn ignore_dogs_hides_dogs_from_the_listing() {
        let (live, mut fake) = test_live().await;
        fake.expect(Request::ListFlock)
            .answer(Response::Flock(vec![sample("web"), dog_sample("bark")]));
        let listed = ShepCommand
            .flock_for_listing(&live, true)
            .await
            .expect("ok");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, "web");
    }

    /// Autocomplete filters case-insensitively by what has been typed so
    /// far, unlike the old code's unfiltered, unconditional `all` entry
    /// (`pm2.ts:52`).
    #[tokio::test]
    async fn autocomplete_filters_by_the_typed_partial() {
        let (live, mut fake) = test_live().await;
        fake.expect(Request::ListFlock).answer(Response::Flock(vec![
            sample("web"),
            sample("worker"),
            sample("api"),
        ]));
        let offered = ShepCommand.suggestions(&live, "WO").await;
        assert_eq!(offered, vec!["worker"]);
    }

    /// A name past [`limits::AUTOCOMPLETE_CHOICE_NAME_LIMIT`] cannot be
    /// sent as a working suggestion at all (Discord would answer it back
    /// as the argument to send, and a truncated one would name a sheep
    /// that does not exist), so it is dropped rather than offered. The
    /// other suggestions in the same call still come back: one bad name
    /// must not empty the whole response.
    #[tokio::test]
    async fn a_name_past_the_choice_limit_is_dropped_not_offered() {
        let (live, mut fake) = test_live().await;
        let huge = "x".repeat(limits::AUTOCOMPLETE_CHOICE_NAME_LIMIT + 1);
        fake.expect(Request::ListFlock)
            .answer(Response::Flock(vec![sample("web"), sample(&huge)]));
        let offered = ShepCommand.suggestions(&live, "").await;
        assert_eq!(offered, vec!["web"]);
    }

    /// Every verb this command names round-trips through
    /// [`verb_for_subcommand`], and `list` names no [`Verb`] at all.
    #[test]
    fn every_declared_subcommand_but_list_names_a_verb() {
        let json = serde_json::to_value(ShepCommand.data()).expect("json");
        for option in json["options"].as_array().expect("options") {
            let name = option["name"].as_str().expect("name");
            if name == "list" {
                assert_eq!(verb_for_subcommand(name), None);
            } else {
                assert!(verb_for_subcommand(name).is_some(), "{name} names no Verb");
            }
        }
    }

    /// A reply from `Live::act` past Discord's own content limit is fitted
    /// to it, the same shape of gap [`crate::bot::interaction::report_failure`]
    /// already closed for a failure message.
    #[test]
    fn a_reply_past_the_content_limit_is_fitted_not_refused() {
        let huge = "x".repeat(limits::MESSAGE_CONTENT_LIMIT * 2);
        let fitted = reply_content(&huge);
        assert_eq!(fitted.chars().count(), limits::MESSAGE_CONTENT_LIMIT);
    }

    #[test]
    fn a_short_reply_is_unchanged() {
        assert_eq!(reply_content("started web"), "started web");
    }

    /// Discord caps one message at [`crate::limits::EMBED_MAX_COUNT`]
    /// embeds, independently of the character sum. Thirty bare
    /// `sample_numbered` sheep cost only a few hundred characters apiece,
    /// nowhere near [`crate::limits::MESSAGE_CHARACTER_BUDGET`], so every
    /// split this test forces comes from the count cap alone; a broken
    /// character-summing branch in `pack_by_budget` would not be caught
    /// here. See [`two_worst_case_sheep_exceed_the_character_budget`] for
    /// the half of the same proof that drives a split through the
    /// character budget instead, with the count cap nowhere near reached.
    #[test]
    fn thirty_sheep_exceed_the_embed_count_cap() {
        let flock: Vec<ProcessInfo> = (0..30).map(sample_numbered).collect();
        let groups = pack_by_budget(
            flock,
            limits::MESSAGE_CHARACTER_BUDGET,
            limits::EMBED_MAX_COUNT,
            embed::embed_character_count,
        );

        assert!(groups.len() > 1, "thirty sheep landed on one message");
        let mut seen = 0;
        for group in &groups {
            assert!(group.len() <= limits::EMBED_MAX_COUNT, "{}", group.len());
            let total: usize = group.iter().map(embed::embed_character_count).sum();
            assert!(
                total <= limits::MESSAGE_CHARACTER_BUDGET,
                "{total} over the message budget"
            );
            seen += group.len();
        }
        assert_eq!(seen, 30, "a sheep was lost or duplicated while packing");
    }

    /// The thing this task must not get wrong, and the half of the proof
    /// [`thirty_sheep_exceed_the_embed_count_cap`] does not touch: two
    /// sheep at [`crate::test_support::worst_case_sample`]'s own worst
    /// case (about 4,566 characters each, `process_embed`'s own doc
    /// comment) sum to roughly 9,132, over
    /// [`crate::limits::MESSAGE_CHARACTER_BUDGET`]'s 6,000 while sitting
    /// at two embeds, far under [`crate::limits::EMBED_MAX_COUNT`]'s ten.
    /// A packer whose character-summing branch were broken outright would
    /// still pass the count-cap test above, since that split never
    /// depends on it; this is the test that actually touches the boundary
    /// the character budget names.
    #[test]
    fn two_worst_case_sheep_exceed_the_character_budget() {
        let flock = vec![
            crate::test_support::worst_case_sample(1),
            crate::test_support::worst_case_sample(2),
        ];
        let groups = pack_by_budget(
            flock,
            limits::MESSAGE_CHARACTER_BUDGET,
            limits::EMBED_MAX_COUNT,
            embed::embed_character_count,
        );

        assert_eq!(groups.len(), 2, "two worst-case sheep shared one message");
        let mut seen = 0;
        for group in &groups {
            assert_eq!(group.len(), 1, "a worst-case sheep shared its message");
            let total: usize = group.iter().map(embed::embed_character_count).sum();
            assert!(
                total <= limits::MESSAGE_CHARACTER_BUDGET,
                "{total} over the message budget"
            );
            seen += group.len();
        }
        assert_eq!(seen, 2, "a sheep was lost or duplicated while packing");
    }

    /// One sheep, named `sheep-<id>`, distinct per `id` so a packing test
    /// can build a flock without every row colliding on the same name.
    fn sample_numbered(id: u32) -> ProcessInfo {
        ProcessInfo::builder(
            id,
            format!("sheep-{id}"),
            shep_client::shep_core::status::ProcStatus::Online,
        )
        .build()
    }

    #[test]
    fn nothing_printed_for_a_person_carries_a_dash() {
        crate::test_support::assert_no_dashes(empty_flock_message());
        // Walks every subcommand and sub-option description too, not just
        // this command's own top-level one: `no_registered_commands_json_carries_a_dash_at_any_depth`
        // in `bot::command` already sweeps the whole registry this way,
        // but this file's own dash test predates that fix and is kept
        // consistent with it rather than left checking only one level.
        let json = serde_json::to_value(ShepCommand.data()).expect("json");
        crate::test_support::assert_no_dashes_deep(&json);
    }
}
