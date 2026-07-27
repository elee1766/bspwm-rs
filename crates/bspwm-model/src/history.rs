use crate::tree::{NodeId, Tree};
use crate::types::HistoryDirection;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Coordinates<M, D> {
    pub monitor: M,
    pub desktop: D,
    pub node: Option<NodeId>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HistoryEntry<M, D> {
    pub location: Coordinates<M, D>,
    pub latest: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct History<M, D> {
    entries: Vec<HistoryEntry<M, D>>,
    needle: Option<usize>,
    pub recording: bool,
}

impl<M: Copy + Eq, D: Copy + Eq> Default for History<M, D> {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            needle: None,
            recording: true,
        }
    }
}

impl<M: Copy + Eq, D: Copy + Eq> History<M, D> {
    #[must_use]
    pub fn entries(&self) -> &[HistoryEntry<M, D>] {
        &self.entries
    }

    pub fn add(&mut self, location: Coordinates<M, D>, focused: bool) {
        if !self.recording {
            return;
        }
        if focused {
            self.needle = None;
        }
        if self
            .entries
            .last()
            .is_some_and(|entry| same_target(&entry.location, &location))
        {
            return;
        }
        for entry in &mut self.entries {
            if same_target(&entry.location, &location) {
                entry.latest = false;
            }
        }
        let entry = HistoryEntry {
            location,
            latest: true,
        };
        if focused || self.entries.is_empty() {
            self.entries.push(entry);
            return;
        }
        let locality = self.entries.iter().rposition(|candidate| {
            if location.node.is_some() {
                candidate.location.desktop == location.desktop
            } else {
                candidate.location.monitor == location.monitor
            }
        });
        let index = locality.map_or_else(
            || {
                if location.node.is_none() {
                    0
                } else {
                    self.entries
                        .iter()
                        .position(|candidate| {
                            candidate.latest && candidate.location.monitor == location.monitor
                        })
                        .unwrap_or(0)
                }
            },
            |index| index + 1,
        );
        self.entries.insert(index, entry);
        self.adjust_needle_for_insert(index);
    }

    pub fn remove_desktop(&mut self, desktop: D) {
        self.retain_with_needle(|entry| entry.location.desktop != desktop);
    }

    pub fn transfer_desktop(&mut self, desktop: D, monitor: M) {
        for entry in &mut self.entries {
            if entry.location.desktop == desktop {
                entry.location.monitor = monitor;
            }
        }
    }

    /// Drops every entry naming one of `dead`.
    ///
    /// The arena frees a node's slot, so an entry left naming it would fail
    /// the very next `tree.node(..)` this type performs. `dead` is short --
    /// it holds the nodes one structural operation freed -- so the linear
    /// membership test costs less than building a set.
    pub fn forget_nodes(&mut self, dead: &[NodeId]) {
        if dead.is_empty() {
            return;
        }
        self.retain_with_needle(|entry| {
            entry
                .location
                .node
                .is_none_or(|candidate| !dead.contains(&candidate))
        });
        self.remove_adjacent_duplicates();
    }

    pub fn remove_node(&mut self, tree: &Tree, node: NodeId, deep: bool) {
        self.retain_with_needle(|entry| {
            entry.location.node.is_none_or(|candidate| {
                if deep {
                    !tree.is_descendant(candidate, node)
                } else {
                    candidate != node
                }
            })
        });
        self.remove_adjacent_duplicates();
    }

    #[must_use]
    pub fn last_node(&self, tree: &Tree, desktop: D, excluded: Option<NodeId>) -> Option<NodeId> {
        self.entries.iter().rev().find_map(|entry| {
            let node = entry.location.node?;
            (entry.latest
                && entry.location.desktop == desktop
                && !tree.node(node).hidden
                && excluded.is_none_or(|excluded| !tree.is_descendant(node, excluded)))
            .then_some(node)
        })
    }

    #[must_use]
    pub fn last_desktop(&self, monitor: M, excluded: D) -> Option<D> {
        self.entries.iter().rev().find_map(|entry| {
            (entry.latest
                && entry.location.monitor == monitor
                && entry.location.desktop != excluded)
                .then_some(entry.location.desktop)
        })
    }

    #[must_use]
    pub fn last_monitor(&self, excluded: M) -> Option<M> {
        self.entries.iter().rev().find_map(|entry| {
            (entry.latest && entry.location.monitor != excluded).then_some(entry.location.monitor)
        })
    }

    #[must_use]
    pub fn rank(&self, node: NodeId) -> u32 {
        self.entries
            .iter()
            .rev()
            .position(|entry| entry.latest && entry.location.node == Some(node))
            .map_or(u32::MAX, |index| u32::try_from(index).unwrap_or(u32::MAX))
    }

    #[must_use]
    pub fn find_newest_node<F>(&self, tree: &Tree, mut matches: F) -> Option<Coordinates<M, D>>
    where
        F: FnMut(Coordinates<M, D>) -> bool,
    {
        self.find_newest(|location| {
            location
                .node
                .is_some_and(|node| !tree.node(node).hidden && matches(location))
        })
    }

    pub fn find_node<F>(
        &mut self,
        tree: &Tree,
        direction: HistoryDirection,
        reference: Option<NodeId>,
        mut matches: F,
    ) -> Option<Coordinates<M, D>>
    where
        F: FnMut(Coordinates<M, D>) -> bool,
    {
        self.find_directional(direction, |entry| {
            entry.location.node.is_some_and(|node| {
                Some(node) != reference && !tree.node(node).hidden && matches(entry.location)
            })
        })
    }

    /// Identical to [`History::find_newest`]; kept for call-site clarity.
    #[must_use]
    pub fn find_newest_desktop<F>(&self, matches: F) -> Option<Coordinates<M, D>>
    where
        F: FnMut(Coordinates<M, D>) -> bool,
    {
        self.find_newest(matches)
    }

    pub fn find_desktop<F>(
        &mut self,
        direction: HistoryDirection,
        reference: D,
        mut matches: F,
    ) -> Option<Coordinates<M, D>>
    where
        F: FnMut(Coordinates<M, D>) -> bool,
    {
        self.find_directional(direction, |entry| {
            entry.location.desktop != reference && matches(entry.location)
        })
    }

    /// Identical to [`History::find_newest`]; kept for call-site clarity.
    #[must_use]
    pub fn find_newest_monitor<F>(&self, matches: F) -> Option<Coordinates<M, D>>
    where
        F: FnMut(Coordinates<M, D>) -> bool,
    {
        self.find_newest(matches)
    }

    pub fn find_monitor<F>(
        &mut self,
        direction: HistoryDirection,
        reference: M,
        mut matches: F,
    ) -> Option<Coordinates<M, D>>
    where
        F: FnMut(Coordinates<M, D>) -> bool,
    {
        self.find_directional(direction, |entry| {
            entry.location.monitor != reference && matches(entry.location)
        })
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.needle = None;
    }

    /// The most recently recorded location `matches` accepts, stale or not.
    #[must_use]
    pub fn find_newest(
        &self,
        mut matches: impl FnMut(Coordinates<M, D>) -> bool,
    ) -> Option<Coordinates<M, D>> {
        self.entries
            .iter()
            .rev()
            .find_map(|entry| matches(entry.location).then_some(entry.location))
    }

    fn find_directional(
        &mut self,
        direction: HistoryDirection,
        mut matches: impl FnMut(HistoryEntry<M, D>) -> bool,
    ) -> Option<Coordinates<M, D>> {
        if self.needle.is_none() || self.recording {
            self.needle = self.entries.len().checked_sub(1);
        }
        let mut index = self.needle?;
        loop {
            let entry = self.entries[index];
            if entry.latest && matches(entry) {
                if !self.recording {
                    self.needle = Some(index);
                }
                return Some(entry.location);
            }
            index = self.next_index(index, direction)?;
        }
    }

    /// Drops the entries `keep` rejects, moving the needle by the number of
    /// entries removed ahead of it.
    ///
    /// Duplicate entries are reachable (`transfer_desktop` can make two entries
    /// equal), so re-finding the needle by value would silently snap it back to
    /// an older duplicate and change later directional searches.
    fn retain_with_needle<F>(&mut self, mut keep: F)
    where
        F: FnMut(&HistoryEntry<M, D>) -> bool,
    {
        let needle = self.needle;
        let mut index = 0;
        let mut kept_before_needle = 0;
        let mut needle_survives = false;
        self.entries.retain(|entry| {
            let kept = keep(entry);
            match needle {
                Some(needle) if index == needle => needle_survives = kept,
                Some(needle) if kept && index < needle => kept_before_needle += 1,
                _ => {}
            }
            index += 1;
            kept
        });
        self.needle = needle_survives.then_some(kept_before_needle);
    }

    fn remove_adjacent_duplicates(&mut self) {
        let mut index = 1;
        while index < self.entries.len() {
            if same_target(
                &self.entries[index - 1].location,
                &self.entries[index].location,
            ) {
                self.entries.remove(index - 1);
                self.adjust_needle_for_remove(index - 1);
            } else {
                index += 1;
            }
        }
    }

    fn adjust_needle_for_insert(&mut self, index: usize) {
        if self.needle.is_some_and(|needle| needle >= index) {
            self.needle = self.needle.map(|needle| needle + 1);
        }
    }

    fn adjust_needle_for_remove(&mut self, index: usize) {
        self.needle = self.needle.and_then(|needle| match needle.cmp(&index) {
            std::cmp::Ordering::Equal => self.entries.len().checked_sub(1),
            std::cmp::Ordering::Greater => Some(needle - 1),
            std::cmp::Ordering::Less => Some(needle),
        });
    }

    fn next_index(&self, index: usize, direction: HistoryDirection) -> Option<usize> {
        match direction {
            HistoryDirection::Older => index.checked_sub(1),
            HistoryDirection::Newer => {
                let next = index + 1;
                (next < self.entries.len()).then_some(next)
            }
        }
    }
}

fn same_target<M: Eq, D: Eq>(first: &Coordinates<M, D>, second: &Coordinates<M, D>) -> bool {
    match (first.node, second.node) {
        (Some(first), Some(second)) => first == second,
        (None, None) => first.desktop == second.desktop,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn location(monitor: u8, desktop: u8, node: Option<NodeId>) -> Coordinates<u8, u8> {
        Coordinates {
            monitor,
            desktop,
            node,
        }
    }

    #[test]
    fn focused_adds_append_and_only_newest_duplicate_is_latest() {
        let mut tree = Tree::default();
        let mut history = History::<u8, u8>::default();
        let first = Coordinates {
            monitor: 1,
            desktop: 1,
            node: Some(tree.add_node(1, 0.5)),
        };
        let second = Coordinates {
            monitor: 1,
            desktop: 1,
            node: Some(tree.add_node(2, 0.5)),
        };
        history.add(first, true);
        history.add(second, true);
        history.add(first, true);
        assert_eq!(history.entries().len(), 3);
        assert!(!history.entries()[0].latest);
        assert!(history.entries()[2].latest);
        assert_eq!(history.rank(first.node.unwrap()), 0);
        assert_eq!(history.rank(second.node.unwrap()), 1);
    }

    #[test]
    fn unfocused_entries_stay_near_their_locality() {
        let mut history = History::<u8, u8>::default();
        history.add(
            Coordinates {
                monitor: 1,
                desktop: 1,
                node: None,
            },
            true,
        );
        history.add(
            Coordinates {
                monitor: 2,
                desktop: 2,
                node: None,
            },
            true,
        );
        history.add(
            Coordinates {
                monitor: 1,
                desktop: 3,
                node: None,
            },
            false,
        );
        assert_eq!(history.entries()[1].location.desktop, 3);
    }

    #[test]
    fn newest_node_search_includes_stale_entries_but_skips_hidden_nodes() {
        let mut tree = Tree::default();
        let old = tree.add_node(1, 0.5);
        let hidden = tree.add_node(2, 0.5);
        tree.node_mut(hidden).hidden = true;
        let mut history = History::default();
        history.add(location(1, 1, Some(old)), true);
        history.add(location(1, 1, Some(hidden)), true);
        history.add(location(2, 2, Some(old)), true);

        assert_eq!(
            history.find_newest_node(&tree, |candidate| candidate.monitor == 1),
            Some(location(1, 1, Some(old)))
        );
        assert_eq!(
            history.find_newest_node(&tree, |_| true),
            Some(location(2, 2, Some(old)))
        );
        assert_eq!(
            history.find_newest_node(&tree, |candidate| candidate.node == Some(hidden)),
            None
        );
    }

    #[test]
    fn directional_node_search_tracks_the_needle_and_only_uses_latest_entries() {
        let mut tree = Tree::default();
        let first = tree.add_node(1, 0.5);
        let second = tree.add_node(2, 0.5);
        let third = tree.add_node(3, 0.5);
        let mut history = History::default();
        history.add(location(1, 1, Some(first)), true);
        history.add(location(1, 1, Some(second)), true);
        history.add(location(1, 1, Some(first)), true);
        history.add(location(1, 1, Some(third)), true);
        history.recording = false;

        assert_eq!(
            history.find_node(&tree, HistoryDirection::Older, Some(third), |_| true),
            Some(location(1, 1, Some(first)))
        );
        assert_eq!(
            history.find_node(&tree, HistoryDirection::Older, Some(first), |_| true),
            Some(location(1, 1, Some(second)))
        );
        assert_eq!(
            history.find_node(&tree, HistoryDirection::Newer, Some(second), |_| true),
            Some(location(1, 1, Some(first)))
        );
    }

    #[test]
    fn directional_node_search_skips_hidden_and_matching_is_applied_last() {
        let mut tree = Tree::default();
        let first = tree.add_node(1, 0.5);
        let hidden = tree.add_node(2, 0.5);
        let third = tree.add_node(3, 0.5);
        tree.node_mut(hidden).hidden = true;
        let mut history = History::default();
        history.add(location(1, 1, Some(first)), true);
        history.add(location(1, 1, Some(hidden)), true);
        history.add(location(2, 2, Some(third)), true);

        assert_eq!(
            history.find_node(&tree, HistoryDirection::Older, Some(third), |candidate| {
                candidate.monitor == 1
            }),
            Some(location(1, 1, Some(first)))
        );
    }

    #[test]
    fn newest_desktop_and_monitor_searches_include_stale_entries() {
        let mut tree = Tree::default();
        let node = tree.add_node(1, 0.5);
        let other = tree.add_node(2, 0.5);
        let mut history = History::default();
        history.add(location(1, 1, Some(node)), true);
        history.add(location(2, 2, Some(other)), true);
        history.add(location(3, 3, Some(node)), true);

        assert_eq!(
            history.find_newest_desktop(|candidate| candidate.monitor == 1),
            Some(location(1, 1, Some(node)))
        );
        assert_eq!(
            history.find_newest_monitor(|candidate| candidate.monitor == 1),
            Some(location(1, 1, Some(node)))
        );
    }

    #[test]
    fn directional_desktop_and_monitor_searches_track_independent_needles() {
        {
            let mut desktop_history = History::default();
            desktop_history.add(location(1, 1, None), true);
            desktop_history.add(location(1, 2, None), true);
            desktop_history.add(location(1, 3, None), true);
            desktop_history.recording = false;

            assert_eq!(
                desktop_history.find_desktop(HistoryDirection::Older, 3, |_| true),
                Some(location(1, 2, None))
            );
            assert_eq!(
                desktop_history.find_desktop(HistoryDirection::Older, 2, |_| true),
                Some(location(1, 1, None))
            );
            assert_eq!(
                desktop_history.find_desktop(HistoryDirection::Newer, 1, |_| true),
                Some(location(1, 2, None))
            );
        }

        {
            let mut monitor_history = History::default();
            monitor_history.add(location(1, 1, None), true);
            monitor_history.add(location(2, 2, None), true);
            monitor_history.add(location(3, 3, None), true);
            monitor_history.recording = false;

            assert_eq!(
                monitor_history.find_monitor(HistoryDirection::Older, 3, |_| true),
                Some(location(2, 2, None))
            );
            assert_eq!(
                monitor_history.find_monitor(HistoryDirection::Older, 2, |_| true),
                Some(location(1, 1, None))
            );
            assert_eq!(
                monitor_history.find_monitor(HistoryDirection::Newer, 1, |_| true),
                Some(location(2, 2, None))
            );
        }
    }

    #[test]
    fn recording_searches_restart_from_the_newest_entry() {
        let mut history = History::default();
        history.add(location(1, 1, None), true);
        history.add(location(2, 2, None), true);
        history.add(location(3, 3, None), true);

        assert_eq!(
            history.find_monitor(HistoryDirection::Older, 3, |_| true),
            Some(location(2, 2, None))
        );
        assert_eq!(
            history.find_monitor(HistoryDirection::Older, 3, |_| true),
            Some(location(2, 2, None))
        );
    }

    #[test]
    fn rank_counts_stale_entries_between_the_tail_and_latest_match() {
        let mut tree = Tree::default();
        let first = tree.add_node(1, 0.5);
        let second = tree.add_node(2, 0.5);
        let third = tree.add_node(3, 0.5);
        let mut history = History::default();
        history.add(location(1, 1, Some(second)), true);
        history.add(location(1, 1, Some(first)), true);
        history.add(location(1, 1, Some(third)), true);
        history.add(location(1, 1, Some(first)), true);

        assert_eq!(history.rank(second), 3);
    }

    #[test]
    fn removals_shift_the_needle_by_index_instead_of_re_finding_it_by_value() {
        let duplicate = HistoryEntry {
            location: location(1, 1, None),
            latest: false,
        };
        let mut history = History::<u8, u8> {
            entries: vec![
                duplicate,
                HistoryEntry {
                    location: location(1, 2, None),
                    latest: true,
                },
                duplicate,
            ],
            needle: Some(2),
            recording: true,
        };

        history.remove_desktop(2);

        // The needle entry survived and only one entry ahead of it went away,
        // so it lands at index 1. Re-finding it by value would have snapped it
        // back to the identical entry at index 0.
        assert_eq!(history.needle, Some(1));
        assert_eq!(history.entries().len(), 2);

        // Dropping the needle entry itself leaves no needle, so the next
        // directional search restarts from the newest entry.
        history.remove_desktop(1);
        assert_eq!(history.needle, None);
        assert!(history.entries().is_empty());
    }
}
