//! A bounded queue that coalesces log lines into groups.
//!
//! One line at a time is a bad shape for a Discord message: a sheep
//! printing a stack trace one frame at a time would send one message per
//! frame, and a busy sheep would exhaust rate limits before saying
//! anything useful. [`Buffer`] holds lines until [`Buffer::drain`] is
//! called, at which point it folds lines that arrived close together, from
//! the same sheep, into one [`Group`] apiece.
//!
//! # Why a real window and not "whatever showed up since last tick"
//!
//! The old code drained one group per flush and left the rest for the next
//! tick, so a backlog never cleared under load; see the note on
//! [`Buffer::drain`] for the fix. Coalescing on a fixed window rather than
//! "everything currently queued" also keeps a group's meaning stable: two
//! lines a minute apart from the same sheep are two events, not one, and
//! folding them together because they happened to still be queued at the
//! same drain would make a group's boundary depend on how busy the queue
//! was rather than on when the lines were written.

use std::collections::VecDeque;

/// One line of output, timestamped and already resolved to a sheep's name.
///
/// [`crate::names::Names::get`] is where the name comes from; this type
/// carries it rather than a bare id, so nothing downstream of the buffer
/// needs the cache that produced it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    /// When the line was written, in milliseconds against whatever clock
    /// the caller uses. Only ever compared to another `Line`'s `at_ms`
    /// from the same buffer, so the clock's epoch does not matter.
    pub at_ms: u64,
    /// The sheep that printed the line.
    pub name: String,
    /// The line's text, one line, no trailing newline.
    pub text: String,
}

/// Several lines from one sheep, folded into one because they arrived
/// inside the same coalescing window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    /// The sheep every line in this group came from.
    pub name: String,
    /// The first line's timestamp: the group's own start.
    pub at_ms: u64,
    /// Every line's text, joined with `\n` in arrival order.
    pub text: String,
}

/// A bounded FIFO of [`Line`]s, drained into [`Group`]s.
///
/// A `VecDeque` rather than a `Vec`: [`Buffer::push`] pops the oldest line
/// off the front when the buffer is over capacity, which is an O(1) pop
/// only from the front of a deque.
#[derive(Debug, Clone)]
pub struct Buffer {
    lines: VecDeque<Line>,
    capacity: usize,
    dropped: u64,
}

impl Buffer {
    /// An empty buffer holding at most `capacity` lines at once.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            lines: VecDeque::new(),
            capacity,
            dropped: 0,
        }
    }

    /// Add `line`, dropping the oldest line first if the buffer is already
    /// at capacity.
    ///
    /// A sheep in a crash loop printing a stack trace per millisecond is
    /// the case this guards: the old buffer grew without limit, so that
    /// sheep alone could exhaust this process's memory. Dropping the
    /// oldest line rather than refusing the newest keeps the buffer useful
    /// as a window onto the most recent output, which is what an operator
    /// watching a crash loop actually wants to see.
    pub fn push(&mut self, line: Line) {
        if self.lines.len() >= self.capacity {
            // A zero capacity leaves nothing in `lines` to evict: `pop_front`
            // on an empty deque returns `None`, and only a `Some` is an
            // actual drop. Counting unconditionally here counted an eviction
            // that never happened, so `take_dropped` over-reported for the
            // life of the process whenever a buffer's capacity was zero.
            if self.lines.pop_front().is_some() {
                self.dropped += 1;
            }
        }
        self.lines.push_back(line);
    }

    /// Fold every queued line into groups and return all of them, leaving
    /// the buffer empty.
    ///
    /// A line starts a new group unless the previous line in the group
    /// shares its `name` and the line's `at_ms` falls inside
    /// `[group.at_ms, group.at_ms + coalesce_ms)`: a half-open window, so a
    /// line landing exactly `coalesce_ms` after the group started opens the
    /// next group rather than joining this one.
    ///
    /// Returns every group the queue holds, not one. The old code drained
    /// a single group per flush at `Discord.ts:87`, so a backlog never
    /// cleared: every tick moved one group's worth of lines and left the
    /// rest queued, and a sheep logging faster than the flush interval
    /// grew that backlog forever. Draining everything on every call is
    /// what makes a flush interval simply a flush interval again.
    pub fn drain(&mut self, coalesce_ms: u64) -> Vec<Group> {
        let mut groups = Vec::new();
        while let Some(first) = self.lines.pop_front() {
            let mut group = Group {
                name: first.name,
                at_ms: first.at_ms,
                text: first.text,
            };
            while let Some(next) = self.lines.front() {
                let in_window = next.at_ms < group.at_ms + coalesce_ms;
                if next.name != group.name || !in_window {
                    break;
                }
                let next = self.lines.pop_front().expect("front just matched Some");
                group.text.push('\n');
                group.text.push_str(&next.text);
            }
            groups.push(group);
        }
        groups
    }

    /// The number of lines dropped for capacity since the last call, then
    /// reset to zero.
    ///
    /// Taken rather than read, so a caller that reports it (a periodic
    /// "N lines dropped" notice, say) reports each dropped line exactly
    /// once instead of repeating the same count on every later call.
    pub fn take_dropped(&mut self) -> u64 {
        core::mem::take(&mut self.dropped)
    }
}

#[cfg(test)]
mod tests {
    use super::{Buffer, Line};

    fn line(at_ms: u64, name: &str, text: &str) -> Line {
        Line {
            at_ms,
            name: name.to_owned(),
            text: text.to_owned(),
        }
    }

    #[test]
    fn lines_inside_the_window_join_into_one_group() {
        let mut buffer = Buffer::new(100);
        buffer.push(line(1_000, "web", "first"));
        buffer.push(line(1_400, "web", "second"));
        let groups = buffer.drain(1_000);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].text, "first\nsecond");
    }

    #[test]
    fn a_line_past_the_window_starts_a_new_group() {
        let mut buffer = Buffer::new(100);
        buffer.push(line(1_000, "web", "first"));
        buffer.push(line(2_000, "web", "second"));
        let groups = buffer.drain(1_000);
        assert_eq!(
            groups.len(),
            2,
            "the window is half open: [at, at + coalesce)"
        );
    }

    /// The old code handled exactly one group per flush at `Discord.ts:87`, so
    /// a backlog never cleared: every tick moved one group and left the rest.
    #[test]
    fn drain_returns_every_group_not_one() {
        let mut buffer = Buffer::new(100);
        for i in 0..5 {
            buffer.push(line(i * 5_000, "web", "line"));
        }
        assert_eq!(buffer.drain(1_000).len(), 5);
        assert!(
            buffer.drain(1_000).is_empty(),
            "drain leaves the buffer empty"
        );
    }

    #[test]
    fn two_sheep_in_one_window_do_not_share_a_group() {
        let mut buffer = Buffer::new(100);
        buffer.push(line(1_000, "web", "from web"));
        buffer.push(line(1_100, "api", "from api"));
        let groups = buffer.drain(1_000);
        assert_eq!(
            groups.len(),
            2,
            "a group carries one sheep's name, so it holds one sheep's lines"
        );
    }

    /// The old buffer grew without limit. A sheep in a crash loop printing a
    /// stack trace per millisecond is the case that matters.
    #[test]
    fn over_capacity_the_oldest_lines_go_and_the_count_is_kept() {
        let mut buffer = Buffer::new(3);
        for i in 0..5 {
            buffer.push(line(i * 5_000, "web", "line"));
        }
        assert_eq!(buffer.take_dropped(), 2);
        assert_eq!(buffer.take_dropped(), 0, "taking the count clears it");
        assert_eq!(buffer.drain(1_000).len(), 3);
    }

    /// The first push into a zero capacity buffer has nothing queued yet to
    /// evict, so `pop_front` returns `None` rather than a line it actually
    /// dropped. Before the fix, `dropped` went up on this call regardless,
    /// counting an eviction that never happened.
    #[test]
    fn the_first_push_into_a_zero_capacity_buffer_reports_no_drop() {
        let mut buffer = Buffer::new(0);
        buffer.push(line(0, "web", "line"));
        assert_eq!(
            buffer.take_dropped(),
            0,
            "the deque was empty, so pop_front had nothing to evict"
        );
    }
}
