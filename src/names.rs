//! The id to name cache.
//!
//! `BusEvent::LogOut` carries `{ id, line }` and no name: PM2's bus named
//! the process on every frame, so neither source repo this project ports
//! needed a cache at all. Every later module renders a log line by the
//! sheep's name rather than its id, so something has to hold the mapping
//! between a muster-roll refresh and the next one.
//!
//! [`Names`] is deliberately dumb about staleness: it holds exactly what
//! the last [`Names::refresh`] said, and nothing in between. A line that
//! arrives for an id the cache has not (yet, or ever) seen still renders,
//! under a placeholder name, rather than being dropped for lack of one.

use std::collections::HashMap;

use shep_client::shep_core::protocol::ProcessInfo;

/// The id to name mapping, replaced wholesale on every refresh.
///
/// A `HashMap<u32, String>` rather than a `Vec<ProcessInfo>`: nothing here
/// needs a sheep's status, pid, or any other field the muster roll carries,
/// only the name a log line's id maps to.
#[derive(Debug, Clone, Default)]
pub struct Names {
    by_id: HashMap<u32, String>,
}

impl Names {
    /// An empty cache, before the first muster-roll refresh.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace the cache wholesale with `roll`.
    ///
    /// Replaces rather than merges: a sheep deleted between two refreshes
    /// must leave the cache, or its name outlives it and a reused id later
    /// shows the wrong sheep's name.
    pub fn refresh(&mut self, roll: &[ProcessInfo]) {
        self.by_id = roll
            .iter()
            .map(|info| (info.id, info.name.clone()))
            .collect();
    }

    /// The name for `id`, or a placeholder naming the id itself.
    ///
    /// Never `None`: a line from a sheep the cache has not seen, because it
    /// arrived before the first refresh or between two of them, is still a
    /// line worth showing, and a dropped line is a worse failure than an
    /// ungainly name.
    #[must_use]
    pub fn get(&self, id: u32) -> String {
        self.by_id
            .get(&id)
            .cloned()
            .unwrap_or_else(|| format!("sheep {id}"))
    }

    /// The id last seen under `name`, or `None` if no refresh has carried
    /// it.
    ///
    /// Walks the map rather than keeping a reverse index: the flock is
    /// small enough that a second map kept in step for every refresh would
    /// be two things to maintain for no measurable gain over one linear
    /// scan.
    #[must_use]
    pub fn id_of(&self, name: &str) -> Option<u32> {
        self.by_id
            .iter()
            .find(|(_, candidate)| candidate.as_str() == name)
            .map(|(&id, _)| id)
    }
}

#[cfg(test)]
mod tests {
    use shep_client::shep_core::{protocol::ProcessInfo, status::ProcStatus};

    use super::Names;

    fn info(id: u32, name: &str) -> ProcessInfo {
        ProcessInfo::builder(id, name, ProcStatus::Online).build()
    }

    #[test]
    fn an_unknown_id_is_named_rather_than_dropped() {
        let names = Names::new();
        assert_eq!(
            names.get(7),
            "sheep 7",
            "a line from a sheep the cache has not seen is still a line worth showing"
        );
    }

    #[test]
    fn a_refresh_replaces_rather_than_merges() {
        // A sheep deleted between two refreshes must leave the cache, or its
        // name outlives it and a reused id shows the wrong one.
        let mut names = Names::new();
        names.refresh(&[info(1, "web"), info(2, "api")]);
        names.refresh(&[info(1, "web")]);
        assert_eq!(names.get(1), "web");
        assert_eq!(names.get(2), "sheep 2");
    }

    #[test]
    fn a_name_resolves_back_to_its_id() {
        let mut names = Names::new();
        names.refresh(&[info(3, "worker")]);
        assert_eq!(names.id_of("worker"), Some(3));
        assert_eq!(names.id_of("ghost"), None);
    }
}
