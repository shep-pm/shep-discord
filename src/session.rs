//! Starting one streaming session, and resolving the id it filters on.
//!
//! Split out of `main.rs`'s run loop rather than inlined there: resolving
//! `own_id` is its own small piece of reasoning, worth reading (and
//! testing against a fake) apart from the reconnect and config-reread logic
//! around it.

use crate::{config::Config, error::Error, names::Names, shepherd::Live, stop::Stop, stream};

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
    /// `handshake` resolved to a numeric id. [`stream::State::on_event`]
    /// filters that id's own lines out of the stream.
    Filtered(u32),
    /// `handshake` names a process this dog announced itself as, but
    /// nothing in the current [`Names`] cache resolves it to an id yet,
    /// whether because the last [`Live::flock`] failed outright or because
    /// it simply has not caught up to this dog's own registration. Either
    /// way shep spawned this process and its output is already on the bus,
    /// so streaming now would feed it straight back into the channel this
    /// dog writes to: the exact bug this project exists to fix, reached a
    /// different way.
    Unresolved(&'a str),
}

/// Resolve `handshake` against `names`.
fn resolve_own_id<'a>(handshake: Option<&'a str>, names: &Names) -> OwnId<'a> {
    match handshake {
        None => OwnId::Unadopted,
        Some(name) => match names.id_of(name) {
            Some(id) => OwnId::Filtered(id),
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
/// `main`'s own run loop already retries on a fixed interval, so returning
/// here without error is enough to try again on the next pass rather than
/// needing a retry loop of its own.
///
/// `unresolved_warned` is the warn-once state [`warn_once`] threads across
/// calls: `main`'s own run loop owns it for the lifetime of the process, the
/// same way it owns `stop`, so a dog stuck unresolved prints exactly one
/// line for the whole outage rather than one on every retry.
///
/// # Errors
/// [`Error`] if the initial bus subscription fails; see [`stream::run`] for
/// why nothing after that point is fatal.
pub async fn stream_once(
    live: &Live,
    handshake: Option<&str>,
    config: &Config,
    unresolved_warned: &mut bool,
    stop: &mut Stop,
) -> Result<(), Error> {
    let mut names = Names::new();
    if let Ok(roll) = live.flock().await {
        names.refresh(&roll);
    }
    let own_id = match resolve_own_id(handshake, &names) {
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
    let sink = stream::discord::DiscordSink::new(&config.token);
    stream::run(live, config, own_id, names, &sink, stop).await
}

#[cfg(test)]
mod tests {
    use shep_client::shep_core::{protocol::ProcessInfo, status::ProcStatus};

    use super::*;

    fn info(id: u32, name: &str) -> ProcessInfo {
        ProcessInfo::builder(id, name, ProcStatus::Online).build()
    }

    /// No handshake means nobody adopted this process, so there is no id to
    /// filter and none is needed.
    #[test]
    fn no_handshake_needs_no_filter() {
        let names = Names::new();
        assert_eq!(resolve_own_id(None, &names), OwnId::Unadopted);
    }

    /// A handshake the current roll can name resolves to that row's id.
    #[test]
    fn a_handshake_the_roll_knows_resolves() {
        let mut names = Names::new();
        names.refresh(&[info(9, "shep-discord")]);
        assert_eq!(
            resolve_own_id(Some("shep-discord"), &names),
            OwnId::Filtered(9)
        );
    }

    /// A handshake naming a process the current roll has no row for yet
    /// (an empty cache, standing in for both a failed flock and one that
    /// has not caught up) must not resolve as if nothing needed filtering:
    /// this dog IS supervised, and its lines are on the bus regardless of
    /// whether this cache has caught up to that fact.
    #[test]
    fn a_handshake_the_roll_cannot_yet_name_stays_unresolved() {
        let names = Names::new();
        assert_eq!(
            resolve_own_id(Some("shep-discord"), &names),
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
