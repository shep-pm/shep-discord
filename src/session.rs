//! Starting one streaming session, and resolving the id it filters on.
//!
//! Split out of [`crate::run`]'s loop rather than inlined there: resolving
//! `own_id` is its own small piece of reasoning, worth reading (and
//! testing against a fake) apart from the reconnect and config-reread logic
//! around it.

use shep_client::shep_core::protocol::ProcessInfo;

use std::sync::Arc;

use crate::{
    bot::monitor,
    config::Config,
    error::Error,
    names::Names,
    shepherd::Live,
    stop::Stop,
    stream::{self, Sink},
};

/// What `handshake` and the current [`Names`] cache resolve to.
///
/// Kept as its own pure function of `resolve_own_id` below, rather than
/// inlined into [`stream_once`], so the two cases that matter can be
/// exercised by a test with no [`Live`], no socket, and no bot behind them.
#[derive(Debug, PartialEq, Eq)]
enum OwnId<'a> {
    /// Nobody adopted this process: shep did not spawn it, so none of its
    /// output is on the bus. There is nothing to filter, and filtering
    /// nothing is correct rather than merely harmless.
    Unadopted,
    /// `handshake` resolved to a numeric id. [`stream::state::State::on_event`]
    /// filters that id's own lines out of the stream.
    Filtered(u32),
    /// `handshake` names a process this dog announced itself as, but
    /// nothing in the current muster roll is a dog under that name yet,
    /// whether because the last [`Live::flock`] failed outright or because
    /// it simply has not caught up to this dog's own registration. Either
    /// way shep spawned this process and its output is already on the bus,
    /// so streaming now would feed it straight back into the channel this
    /// dog writes to: the exact bug this project exists to fix, reached a
    /// different way.
    Unresolved(&'a str),
}

/// Resolve `handshake` against `roll`, the current muster roll.
///
/// Matches on the name AND the row being a dog, never the name alone.
/// `ProcessInfo::dog` is `Some` for a dog's own row and `None` for every
/// sheep's, so an operator who names a sheep the same as this dog cannot
/// make this resolve to the sheep's id: matching the name by itself would,
/// and a dog that filtered a sheep's id out of the stream while publishing
/// its own lines unfiltered is the founding bug of this project, reached a
/// third way.
fn resolve_own_id<'a>(handshake: Option<&'a str>, roll: &[ProcessInfo]) -> OwnId<'a> {
    match handshake {
        None => OwnId::Unadopted,
        Some(name) => match roll
            .iter()
            .find(|info| info.name == name && info.dog.is_some())
        {
            Some(info) => OwnId::Filtered(info.id),
            None => OwnId::Unresolved(name),
        },
    }
}

/// Whether this cycle's resolution should print the "not streaming yet"
/// notice, threading the warn-once state through `warned` rather than a
/// process-global.
///
/// A pure function of two facts: was the previous call's outcome
/// unresolved, and is this one. Kept apart from the I/O in [`stream_once`]
/// so the dedup transition itself, warn once, stay silent while still
/// unresolved, reset to silent the moment resolution succeeds, is testable
/// with no [`Live`] and no socket behind it. A `static` living inside
/// `stream_once` could not offer that: nothing in this crate could drive
/// the warn-once transition without a live session, which is also the same
/// module-global shape `assessment.md` names as a defect in the code this
/// project ports, the `lastSnapshot` race at `system.ts:27`.
fn warn_once(resolved: bool, warned: &mut bool) -> bool {
    if resolved {
        *warned = false;
        false
    } else {
        !core::mem::replace(warned, true)
    }
}

/// The message printed once per outage when this dog is adopted but its own
/// id has not resolved yet.
///
/// A function rather than an inline `eprintln!` so the dash check can reach
/// the text directly.
fn unresolved_message(name: &str) -> String {
    format!(
        "shep-discord: {name} is adopted but its own id has not resolved yet, so this dog \
         cannot yet tell its own lines apart from the rest of the bus; not streaming until it \
         does"
    )
}

/// Resolve this dog's own numeric id and drive [`stream::run`] until its
/// subscription ends or `stop` resolves.
///
/// `own_id` comes from a fresh [`Live::flock`], read once up front: if
/// `handshake` is `None`, nobody adopted this process, so shep captures
/// none of its output and there is nothing to filter. If `handshake` is
/// `Some`, this dog was adopted, which means shep spawned it and its lines
/// ARE on the bus; a flock read that fails, or one that succeeds but has
/// not yet caught up to this dog's own registration, both leave that
/// name's id unresolved. Streaming in that state, filtered against nothing
/// because there is no id yet to filter against, is the exact bug this
/// project exists to fix, so this dog does not stream this cycle at all.
/// [`crate::run::run`] already retries on a fixed interval, so returning
/// here without error is enough to try again on the next pass rather than
/// needing a retry loop of its own.
///
/// `sink` and `wired` are this process's own, built once by
/// [`crate::run::run`] on the first cycle that resolves a config and
/// handed to every later one. Each carries a `serenity::all::Http`, which
/// is where serenity keeps what it has learned about a channel's rate
/// limits, so building either per cycle would throw that away on every
/// reconnect and every reread of `dogs.toml`. `wired` is the one live
/// monitor wired to the one board, handed on so a `process.*` event
/// redraws the sheep it names as it happens; it is `None` when
/// `dogs.toml` names no `monitor_channel`, and even when it is `Some` it
/// draws nothing at all while the refresh task is off. See
/// [`monitor::watch::Wired`] for both.
///
/// `unresolved_warned` is the warn-once state [`warn_once`] threads across
/// calls: [`crate::run::run`] owns it for the lifetime of the process, the
/// same way it owns `stop`, so a dog stuck unresolved prints exactly one
/// line for the whole outage rather than one on every retry.
///
/// # Errors
/// [`Error`] if the initial bus subscription fails; see [`stream::run`] for
/// why nothing after that point is fatal.
pub async fn stream_once<S: Sink>(
    live: &Live,
    handshake: Option<&str>,
    config: &Config,
    sink: &S,
    wired: Option<&Arc<monitor::watch::Wired>>,
    unresolved_warned: &mut bool,
    stop: &mut Stop,
) -> Result<(), Error> {
    let roll = live.flock().await.unwrap_or_default();
    let mut names = Names::new();
    names.refresh(&roll);
    let own_id = match resolve_own_id(handshake, &roll) {
        OwnId::Unadopted => None,
        OwnId::Filtered(id) => {
            warn_once(true, unresolved_warned);
            Some(id)
        }
        OwnId::Unresolved(name) => {
            if warn_once(false, unresolved_warned) {
                eprintln!("{}", unresolved_message(name));
            }
            return Ok(());
        }
    };
    stream::run(live, config, own_id, names, sink, wired, stop).await
}

#[cfg(test)]
mod tests {
    use shep_client::shep_core::{protocol::DogSource, status::ProcStatus};

    use super::*;

    /// A sheep's row: no `dog` set, the same as a peer daemon reports for
    /// every entry that is not a dog.
    fn sheep(id: u32, name: &str) -> ProcessInfo {
        ProcessInfo::builder(id, name, ProcStatus::Online).build()
    }

    /// A dog's own row, under whatever name it announced.
    fn dog(id: u32, name: &str) -> ProcessInfo {
        ProcessInfo::builder(id, name, ProcStatus::Online)
            .dog(Some(DogSource::BuiltIn))
            .build()
    }

    /// No handshake means nobody adopted this process, so there is no id to
    /// filter and none is needed.
    #[test]
    fn no_handshake_needs_no_filter() {
        assert_eq!(resolve_own_id(None, &[]), OwnId::Unadopted);
    }

    /// A handshake the current roll names a dog row for resolves to that
    /// row's id.
    #[test]
    fn a_handshake_the_roll_knows_resolves() {
        assert_eq!(
            resolve_own_id(Some("shep-discord"), &[dog(9, "shep-discord")]),
            OwnId::Filtered(9)
        );
    }

    /// A handshake naming a process the current roll has no row for yet
    /// (an empty roll, standing in for both a failed flock and one that has
    /// not caught up) must not resolve as if nothing needed filtering: this
    /// dog IS supervised, and its lines are on the bus regardless of whether
    /// this roll has caught up to that fact.
    #[test]
    fn a_handshake_the_roll_cannot_yet_name_stays_unresolved() {
        assert_eq!(
            resolve_own_id(Some("shep-discord"), &[]),
            OwnId::Unresolved("shep-discord")
        );
    }

    /// An operator who names a sheep the same as this dog must not make
    /// this resolve to the sheep's id: matching the name alone would filter
    /// the sheep's lines out of the stream while leaving this dog's own
    /// unfiltered, the founding bug of this project reached a third way.
    #[test]
    fn a_dog_sharing_a_name_with_a_sheep_resolves_to_the_dog() {
        assert_eq!(
            resolve_own_id(
                Some("shep-discord"),
                &[dog(9, "shep-discord"), sheep(1, "shep-discord")]
            ),
            OwnId::Filtered(9)
        );
    }

    /// A sheep alone under this dog's handshake name must stay unresolved,
    /// never resolve to the sheep's own id.
    #[test]
    fn a_sheep_alone_under_the_name_stays_unresolved() {
        assert_eq!(
            resolve_own_id(Some("shep-discord"), &[sheep(1, "shep-discord")]),
            OwnId::Unresolved("shep-discord")
        );
    }

    /// The dedup transition `warn_once` drives: an unresolved call warns,
    /// a second unresolved call in a row does not, and a resolved call in
    /// between resets it so the next outage warns again. Impossible to
    /// exercise before this was a pure function of `warned` rather than a
    /// process-global `stream_once` alone could flip.
    #[test]
    fn warn_once_warns_once_per_outage_and_resets_on_success() {
        let mut warned = false;
        assert!(warn_once(false, &mut warned), "first outage cycle warns");
        assert!(
            !warn_once(false, &mut warned),
            "a repeat of the same outage stays silent"
        );
        assert!(
            !warn_once(true, &mut warned),
            "a resolved call never warns, and resets the state"
        );
        assert!(
            warn_once(false, &mut warned),
            "a fresh outage after a reset warns again"
        );
    }

    #[test]
    fn nothing_printed_for_a_person_carries_a_dash() {
        crate::test_support::assert_no_dashes(&unresolved_message("shep-discord"));
    }
}
