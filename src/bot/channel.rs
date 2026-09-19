//! The monitor channel: the four things this dog does to a message in it,
//! and reading its own past messages back after a restart.
//!
//! # Why there is a trait here at all
//!
//! [`Board`] is to [`crate::bot::monitor`] what [`crate::stream::Sink`] is
//! to [`crate::stream::state::State`]: the one seam the network sits
//! behind, so every decision the monitor makes about posting, editing and
//! deleting is testable with no token, no gateway and no channel. [`Live`]
//! is the only implementation that reaches Discord, and it is the only
//! thing in this module a test cannot exercise.
//!
//! # Why rediscovery replaces a bulk delete
//!
//! The old code emptied the monitor channel on every boot
//! (`utils.ts:37`), because its message ids lived in memory and a restart
//! lost them, leaving embeds nothing would ever touch again. Deleting is
//! the wrong half of that trade: it loses the channel's history, it costs
//! a request per message, and Discord refuses to bulk delete anything
//! older than two weeks, so a dog that had been up a while could not even
//! finish the job.
//!
//! Nothing needs to be lost, because the id is already written down. Every
//! button [`crate::bot::embed::process_buttons`] draws carries the sheep's
//! own numeric id in its `custom_id`, so a message this bot wrote names
//! the sheep it belongs to, on the message itself, for as long as the
//! message exists. [`rediscover`] reads that back and the monitor adopts
//! the messages it left behind instead of replacing them.

use std::collections::HashMap;

use serenity::all::{
    ActionRowComponent, ButtonKind, ChannelId, CreateActionRow, CreateEmbed, CreateMessage,
    EditMessage, GetMessages, Http, MessageId, UserId,
};

use crate::{bot::embed::parse_custom_id, error::Error};

/// How many past messages one [`Board::recent`] call reads.
///
/// Discord's own per-fetch ceiling. The old code used the same number
/// (`utils.ts:41`) and stopped there, but it was sweeping the channel with
/// a bulk delete, where missing a message cost nothing because the next
/// sweep caught it. Here a miss is permanent: a sheep whose message was
/// not seen gets a second one posted, and nothing ever cleans up the
/// first, so one unpaginated fetch would cost a flock of more than a
/// hundred sheep one orphan per sheep per restart. [`rediscover`] pages
/// instead, and this is the size of one page.
pub const RECENT_MESSAGE_LIMIT: u8 = 100;

/// One past message in the monitor channel, reduced to the three facts
/// [`rediscover`] reads.
///
/// A projection rather than a `serenity::model::channel::Message`:
/// `Message` is `#[non_exhaustive]` with a couple of dozen required
/// fields, so nothing outside serenity can build one, and a [`Board`] fake
/// that had to hand back real `Message` values would not be buildable in a
/// test at all. Carrying only what is read keeps the whole adoption
/// decision a pure function of plain data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Posted {
    /// The message's own id, which is what an edit or a delete needs.
    pub id: MessageId,
    /// Who wrote it. A channel an operator also talks in carries messages
    /// this dog must not touch.
    pub author: UserId,
    /// Every `custom_id` on every button the message carries, in the order
    /// Discord reports them.
    pub custom_ids: Vec<String>,
}

/// What the monitor can do to its channel.
///
/// The futures are spelled `impl Future<Output = ...> + Send` rather than
/// written as `async fn`: [`crate::bot::monitor`] spawns an interval task
/// that holds one of these across an `await`, and `tokio::spawn` needs
/// that future to be `Send`. A plain `async fn` in a trait promises
/// nothing about auto traits, so a generic caller could not spawn one
/// without naming a bound Rust has no stable way to name. Every
/// implementation is still written as an ordinary `async fn`.
pub trait Board {
    /// Post one sheep's embed and its buttons as a new message, and hand
    /// back the id to edit next time.
    ///
    /// # Errors
    /// [`Error::Discord`] when Discord refuses the message.
    fn post(
        &self,
        embed: CreateEmbed,
        buttons: CreateActionRow,
    ) -> impl Future<Output = Result<MessageId, Error>> + Send;

    /// Replace what `id` currently shows with `embed` and `buttons`.
    ///
    /// # Errors
    /// [`Error::Discord`] when Discord refuses the edit, the message
    /// having been deleted by hand included.
    fn edit(
        &self,
        id: MessageId,
        embed: CreateEmbed,
        buttons: CreateActionRow,
    ) -> impl Future<Output = Result<(), Error>> + Send;

    /// Remove the message `id` names.
    ///
    /// # Errors
    /// [`Error::Discord`] when Discord refuses the delete.
    fn delete(&self, id: MessageId) -> impl Future<Output = Result<(), Error>> + Send;

    /// One page of the channel's past messages, newest first, reduced to
    /// what [`rediscover`] reads: the newest [`RECENT_MESSAGE_LIMIT`] of
    /// them, or the same many from just before the message `before`
    /// names.
    ///
    /// A page rather than "the recent ones", because one fetch cannot
    /// cover a channel with more sheep in it than Discord will hand back
    /// at once; see [`RECENT_MESSAGE_LIMIT`].
    ///
    /// # Errors
    /// [`Error::Discord`] when Discord refuses the read.
    fn recent(
        &self,
        before: Option<MessageId>,
    ) -> impl Future<Output = Result<Vec<Posted>, Error>> + Send;
}

/// Which sheep `message` is the monitor message for, or `None` for a
/// message this dog did not write as one.
///
/// Reads the first `custom_id` [`parse_custom_id`] accepts and keeps that
/// id, ignoring the verb: the five buttons on a monitor message all carry
/// the same sheep, so the first one that parses settles it, and a message
/// carrying no button this dog's own scheme recognises is somebody else's
/// message or one an older version of this dog wrote under the old
/// `verb:name` scheme.
#[must_use]
pub fn sheep_id_of(message: &Posted) -> Option<u32> {
    message
        .custom_ids
        .iter()
        .find_map(|raw| parse_custom_id(raw).map(|(_verb, id)| id))
}

/// Every monitor message `me` already wrote in the channel, by the sheep
/// it belongs to.
///
/// Keeps the newest message for a sheep and leaves any older duplicate
/// alone rather than deleting it: a page answers newest first, so the
/// first entry for an id is the one an operator is looking at, and this
/// function reads the channel rather than changing it. A duplicate can
/// only exist where a previous run posted twice, which is the failure
/// [`crate::bot::monitor`]'s per-sheep guard exists to prevent in the
/// first place.
///
/// # Why this pages, and what stops it paging forever
///
/// `wanted` is the flock the caller is about to draw, which it has to read
/// anyway for the first refresh, and it is what makes paging terminate
/// early in the ordinary case. Two conditions end the loop, and either one
/// is enough:
///
/// - every sheep in `wanted` has been found, so nothing further back can
///   matter; or
/// - a page came back short of [`RECENT_MESSAGE_LIMIT`], meaning the
///   channel has no more messages to give.
///
/// So a dedicated monitor channel costs about one request per hundred
/// sheep, and a channel an operator also talks in costs at most as many
/// requests as it takes to find the whole flock rather than reading the
/// channel's whole history. An empty `wanted`, which is what a failed
/// muster-roll read hands over, reads exactly one page: better than
/// nothing, and no worse than the single fetch this used to do.
///
/// A message found for a sheep NOT in `wanted` is still adopted. It is an
/// orphan from a sheep deleted while this dog was down, and adopting it is
/// what lets the first refresh sweep it away, since a sheep it has a
/// message for and the flock does not is exactly what
/// [`crate::bot::monitor::Monitor::update_all`] deletes.
///
/// # Errors
/// [`Error::Discord`] when Discord refuses to hand back a page.
pub async fn rediscover<B: Board>(
    board: &B,
    me: UserId,
    wanted: &[u32],
) -> Result<HashMap<u32, MessageId>, Error> {
    let mut found: HashMap<u32, MessageId> = HashMap::new();
    let mut before: Option<MessageId> = None;

    loop {
        let page = board.recent(before).await?;
        let full_page = page.len() >= usize::from(RECENT_MESSAGE_LIMIT);
        let oldest = page.last().map(|message| message.id);

        for message in page {
            if message.author != me {
                continue;
            }
            if let Some(sheep) = sheep_id_of(&message) {
                found.entry(sheep).or_insert(message.id);
            }
        }

        let all_found = wanted.iter().all(|sheep| found.contains_key(sheep));
        let Some(oldest) = oldest else {
            return Ok(found);
        };
        if all_found || !full_page {
            return Ok(found);
        }
        before = Some(oldest);
    }
}

/// The one [`Board`] that reaches Discord.
///
/// Holds an [`Http`] rather than a [`serenity::Client`], the same reason
/// [`crate::stream::discord::DiscordSink`] does: posting, editing,
/// deleting and reading a channel are all REST routes, and none of them
/// needs the gateway connection a `Client` would open a second time.
pub struct Live {
    http: Http,
    channel: ChannelId,
}

impl Live {
    /// A board authenticated as the bot `token` names, writing to
    /// `channel`.
    ///
    /// `Http::new` makes no network call of its own; the token is only
    /// checked against Discord on the first request.
    #[must_use]
    pub fn new(token: &str, channel: u64) -> Self {
        Self {
            http: Http::new(token),
            channel: ChannelId::new(channel),
        }
    }

    /// Which user this bot is, which is what [`rediscover`] filters on.
    ///
    /// Asked of Discord rather than read from a cache: this crate builds
    /// serenity without the `cache` feature (see `Cargo.toml`), so there
    /// is no `current_user` to read, and the answer cannot change while
    /// the token does not.
    ///
    /// # Errors
    /// [`Error::Discord`] when Discord refuses to say.
    pub async fn me(&self) -> Result<UserId, Error> {
        Ok(self.http.get_current_user().await?.id)
    }
}

impl Board for Live {
    async fn post(&self, embed: CreateEmbed, buttons: CreateActionRow) -> Result<MessageId, Error> {
        let message = CreateMessage::new().embed(embed).components(vec![buttons]);
        Ok(self.channel.send_message(&self.http, message).await?.id)
    }

    async fn edit(
        &self,
        id: MessageId,
        embed: CreateEmbed,
        buttons: CreateActionRow,
    ) -> Result<(), Error> {
        let message = EditMessage::new().embed(embed).components(vec![buttons]);
        self.channel.edit_message(&self.http, id, message).await?;
        Ok(())
    }

    async fn delete(&self, id: MessageId) -> Result<(), Error> {
        self.channel.delete_message(&self.http, id).await?;
        Ok(())
    }

    async fn recent(&self, before: Option<MessageId>) -> Result<Vec<Posted>, Error> {
        let mut query = GetMessages::new().limit(RECENT_MESSAGE_LIMIT);
        if let Some(before) = before {
            query = query.before(before);
        }
        let messages = self.channel.messages(&self.http, query).await?;
        Ok(messages
            .into_iter()
            .map(|message| Posted {
                id: message.id,
                author: message.author.id,
                custom_ids: message
                    .components
                    .iter()
                    .flat_map(|row| &row.components)
                    .filter_map(|component| match component {
                        ActionRowComponent::Button(button) => match &button.data {
                            ButtonKind::NonLink { custom_id, .. } => Some(custom_id.clone()),
                            // A link button carries a URL and no id at
                            // all, and this dog draws neither kind.
                            ButtonKind::Link { .. } | ButtonKind::Premium { .. } => None,
                        },
                        // `ActionRowComponent` is `#[non_exhaustive]`, so
                        // the wildcard covers a component kind serenity
                        // adds later too. None of them is a button, and
                        // only a button carries a sheep id here.
                        ActionRowComponent::SelectMenu(_)
                        | ActionRowComponent::InputText(_)
                        | _ => None,
                    })
                    .collect(),
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use crate::test_support::CountingChannel;

    use super::*;

    /// A channel holding one message per sheep for `count` sheep, newest
    /// first the way Discord answers, so sheep `count` is the newest and
    /// sheep 1 the oldest.
    fn channel_of(count: u32) -> CountingChannel {
        let board = CountingChannel::new();
        board.preload(
            (1..=count)
                .rev()
                .map(|sheep| message_from(u64::from(sheep), ME, &[format!("restart:{sheep}")]))
                .collect(),
        );
        board
    }

    /// One past message, written by `author`, carrying `custom_ids` on its
    /// buttons.
    fn message_from(id: u64, author: u64, custom_ids: &[impl AsRef<str>]) -> Posted {
        Posted {
            id: MessageId::new(id),
            author: UserId::new(author),
            custom_ids: custom_ids
                .iter()
                .map(|raw| raw.as_ref().to_owned())
                .collect(),
        }
    }

    /// One past message this bot wrote.
    fn message_with_buttons(custom_ids: &[&str]) -> Posted {
        message_from(1, ME, custom_ids)
    }

    /// The bot's own user id, for the author filter.
    const ME: u64 = 500;

    /// shep restarts a dog, and the cached message ids live in memory. The
    /// id is already encoded in every button this bot wrote, so
    /// rediscovery is free. This is what replaces the bulk delete at
    /// `utils.ts:37`.
    #[test]
    fn a_monitor_message_is_recognised_by_the_buttons_it_carries() {
        let message = message_with_buttons(&["restart:7", "stop:7", "delete:7"]);
        assert_eq!(sheep_id_of(&message), Some(7));
    }

    #[test]
    fn a_message_this_bot_did_not_write_is_not_adopted() {
        assert_eq!(sheep_id_of(&message_with_buttons(&[])), None);
        assert_eq!(
            sheep_id_of(&message_with_buttons(&["something:else"])),
            None
        );
    }

    /// A flock larger than one page must still be adopted whole. One
    /// unpaginated fetch left every sheep past the hundredth unadopted,
    /// and each of those got a second message posted that nothing would
    /// ever clean up: one orphan per sheep per restart, permanently,
    /// because unlike the bulk delete this replaces there is no later
    /// sweep to catch it.
    #[tokio::test]
    async fn a_flock_spanning_more_than_one_page_is_adopted_whole() {
        let sheep = u32::from(RECENT_MESSAGE_LIMIT) + 50;
        let board = channel_of(sheep);
        let wanted: Vec<u32> = (1..=sheep).collect();

        let found = rediscover(&board, UserId::new(ME), &wanted)
            .await
            .expect("ok");

        assert_eq!(found.len() as u32, sheep, "every sheep is adopted");
        assert_eq!(found[&1], MessageId::new(1), "the oldest page included");
        assert_eq!(board.pages(), 2, "150 sheep is two pages of 100");
    }

    /// Paging stops as soon as the flock is accounted for, so a channel an
    /// operator also talks in is not read back to its beginning.
    #[tokio::test]
    async fn paging_stops_once_every_sheep_in_the_flock_is_found() {
        let board = channel_of(u32::from(RECENT_MESSAGE_LIMIT) + 50);

        let found = rediscover(&board, UserId::new(ME), &[150, 149])
            .await
            .expect("ok");

        assert_eq!(board.pages(), 1, "both sheep are on the newest page");
        assert_eq!(
            found.len(),
            usize::from(RECENT_MESSAGE_LIMIT),
            "the page it did read is adopted whole, orphans included, so the first refresh can \
             sweep the sheep that are no longer in the flock"
        );
    }

    /// A failed muster-roll read leaves the caller with no flock to aim
    /// at. One page is still better than nothing, and is what this used to
    /// do unconditionally.
    #[tokio::test]
    async fn an_unknown_flock_reads_one_page_rather_than_the_whole_channel() {
        let board = channel_of(u32::from(RECENT_MESSAGE_LIMIT) + 50);

        let found = rediscover(&board, UserId::new(ME), &[]).await.expect("ok");

        assert_eq!(board.pages(), 1);
        assert_eq!(found.len(), usize::from(RECENT_MESSAGE_LIMIT));
    }

    /// The old scheme put the sheep's NAME in the id (`process.ts:127`).
    /// A message an older version of this dog wrote must not be adopted
    /// under a sheep id it never carried.
    #[test]
    fn a_button_from_the_old_name_keyed_scheme_is_not_adopted() {
        assert_eq!(sheep_id_of(&message_with_buttons(&["restart:web"])), None);
    }

    #[tokio::test]
    async fn rediscovery_adopts_this_bots_own_messages_and_nobody_elses() {
        let board = CountingChannel::new();
        board.preload(vec![
            message_from(10, ME, &["restart:1"]),
            message_from(11, 999, &["restart:2"]),
            message_from(12, ME, &["hello"]),
        ]);

        let found = rediscover(&board, UserId::new(ME), &[1]).await.expect("ok");

        assert_eq!(
            found,
            HashMap::from([(1, MessageId::new(10))]),
            "only this bot's own monitor messages are adopted"
        );
    }

    /// Two messages for one sheep can only come from a run that posted
    /// twice. The newest is the one an operator is looking at, and
    /// `recent` answers newest first.
    #[tokio::test]
    async fn a_duplicate_from_an_older_run_keeps_the_newest_message() {
        let board = CountingChannel::new();
        board.preload(vec![
            message_from(20, ME, &["restart:1"]),
            message_from(19, ME, &["restart:1"]),
        ]);

        let found = rediscover(&board, UserId::new(ME), &[1]).await.expect("ok");

        assert_eq!(found, HashMap::from([(1, MessageId::new(20))]));
    }
}
