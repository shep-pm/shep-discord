//! Starting one streaming session, and resolving the id it filters on.
//!
//! Split out of `main.rs`'s run loop rather than inlined there: resolving
//! `own_id` is its own small piece of reasoning, worth reading (and
//! testing against a fake) apart from the reconnect and config-reread logic
//! around it.

use std::sync::atomic::{AtomicBool, Ordering};

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

/// Whether the last call into [`stream_once`] already printed this cycle's
/// "not streaming yet" notice for an id that has not resolved.
///
/// Reset to `false` the moment resolution succeeds, so an operator whose
/// dog is adopted but stuck unresolved sees exactly one line for the whole
/// outage rather than one every time `main`'s run loop retries, for as long
/// as the outage lasts.
static UNRESOLVED_WARNED: AtomicBool = AtomicBool::new(false);

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
/// # Errors
/// [`Error`] if the initial bus subscription fails; see [`stream::run`] for
/// why nothing after that point is fatal.
pub async fn stream_once(
    live: &Live,
    handshake: Option<&str>,
    config: &Config,
    stop: &mut Stop,
) -> Result<(), Error> {
    let mut names = Names::new();
    if let Ok(roll) = live.flock().await {
        names.refresh(&roll);
    }
    let own_id = match resolve_own_id(handshake, &names) {
        OwnId::Unadopted => None,
        OwnId::Filtered(id) => {
            UNRESOLVED_WARNED.store(false, Ordering::Relaxed);
            Some(id)
        }
        OwnId::Unresolved(name) => {
            if !UNRESOLVED_WARNED.swap(true, Ordering::Relaxed) {
                eprintln!(
                    "shep-discord: {name} is adopted but its own id has not resolved yet, \
                     so this dog cannot yet tell its own lines apart from the rest of the \
                     bus; not streaming until it does"
                );
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
}
