//! Starting one streaming session, and resolving the id it filters on.
//!
//! Split out of `main.rs`'s run loop rather than inlined there: resolving
//! `own_id` is its own small piece of reasoning, worth reading (and
//! testing against a fake) apart from the reconnect and config-reread logic
//! around it.

use crate::{config::Config, error::Error, names::Names, shepherd::Live, stop::Stop, stream};

/// Resolve this dog's own numeric id and drive [`stream::run`] until its
/// subscription ends or `stop` resolves.
///
/// `own_id` comes from a fresh [`Live::flock`]: this dog announced
/// `handshake` in its `Hello` frame (or nothing, if nobody adopted it), and
/// [`Names::id_of`] finds the numeric id the shepherd recorded that name
/// under. Neither a missing handshake nor a flock that has not caught up to
/// this dog's own registration yet is fatal: either one leaves `own_id` at
/// `u32::MAX`, an id no real sheep can ever hold, so
/// [`stream::State::on_event`]'s self-filter simply never matches anything
/// and this dog streams like any other, which only ever happens for a
/// process the shepherd is not supervising in the first place.
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
    let own_id = match live.flock().await {
        Ok(roll) => {
            names.refresh(&roll);
            handshake
                .and_then(|name| names.id_of(name))
                .unwrap_or(u32::MAX)
        }
        Err(_) => u32::MAX,
    };
    let sink = stream::discord::DiscordSink::new(&config.token);
    stream::run(live, config, own_id, names, &sink, stop).await
}
