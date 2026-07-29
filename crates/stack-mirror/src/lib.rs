//! X11 stacking order mirror with level-aware operations and minimum-diff output.
//!
//! Maintains a local mirror of the X server's sibling stacking order and
//! provides level-aware insert/raise/lower operations that emit the minimum
//! set of X `ConfigureWindow` calls to reconcile the server with the model.

/// An X stacking operation to apply.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StackOp {
    /// `ConfigureWindow` with `StackMode::Above` relative to `sibling`.
    Above { window: u32, sibling: u32 },
    /// `ConfigureWindow` with `StackMode::Below` relative to `sibling`.
    Below { window: u32, sibling: u32 },
}

/// Callback trait for applying stacking operations to the X server.
///
/// Implementations should issue `ConfigureWindow` with the appropriate
/// `StackMode` and sibling. Errors are propagated back to the caller.
pub trait StackBackend {
    type Error;
    fn stack_above(&mut self, window: u32, sibling: u32) -> Result<(), Self::Error>;
    fn stack_below(&mut self, window: u32, sibling: u32) -> Result<(), Self::Error>;
}

/// Mirrors the X server's sibling stacking order and provides level-aware
/// operations that maintain correctness with minimum X traffic.
///
/// The order is bottom-to-top: index 0 is the bottommost window,
/// the last index is the topmost.
#[derive(Clone, Debug, Default)]
pub struct StackMirror {
    /// Current order, bottom to top. Each entry is (window_xid, level).
    order: Vec<Entry>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Entry {
    window: u32,
    level: u8,
}

impl StackMirror {
    /// Creates an empty mirror.
    #[must_use]
    pub fn new() -> Self {
        Self { order: Vec::new() }
    }

    /// Seeds the mirror from an existing X stacking order (bottom to top)
    /// with a level function that maps each window to its priority level.
    #[must_use]
    pub fn from_order(windows: &[u32], level_fn: impl Fn(u32) -> u8) -> Self {
        Self {
            order: windows
                .iter()
                .map(|&w| Entry {
                    window: w,
                    level: level_fn(w),
                })
                .collect(),
        }
    }

    /// The current stacking order, bottom to top.
    #[must_use]
    pub fn windows(&self) -> Vec<u32> {
        self.order.iter().map(|e| e.window).collect()
    }

    /// The number of tracked windows.
    #[must_use]
    pub fn len(&self) -> usize {
        self.order.len()
    }

    /// Whether the mirror is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }

    /// Returns true if the window is tracked.
    #[must_use]
    pub fn contains(&self, window: u32) -> bool {
        self.position(window).is_some()
    }

    /// Insert a new window at the top of its level and apply to X.
    ///
    /// If the window is already tracked, it is raised to the top of its level.
    pub fn insert<B: StackBackend>(
        &mut self,
        backend: &mut B,
        window: u32,
        level: u8,
    ) -> Result<(), B::Error> {
        self.raise_to_level_top(backend, window, level)
    }

    /// Remove a window from the mirror. No X operation needed -- the caller
    /// handles destroying or unmapping the window.
    pub fn remove(&mut self, window: u32) {
        self.order.retain(|e| e.window != window);
    }

    /// Raise a window to the top of its level and apply to X.
    /// Used on focus.
    pub fn raise_in_level<B: StackBackend>(
        &mut self,
        backend: &mut B,
        window: u32,
    ) -> Result<(), B::Error> {
        let Some(level) = self.level_of(window) else {
            return Ok(());
        };
        self.raise_to_level_top(backend, window, level)
    }

    /// Lower a window to the bottom of its level and apply to X.
    /// Used on unfocus in upstream-compatible mode.
    pub fn lower_in_level<B: StackBackend>(
        &mut self,
        backend: &mut B,
        window: u32,
    ) -> Result<(), B::Error> {
        let Some(level) = self.level_of(window) else {
            return Ok(());
        };
        self.move_to_level_bottom(backend, window, level)
    }

    /// Change a window's level (e.g. state or layer changed) and reposition.
    pub fn set_level<B: StackBackend>(
        &mut self,
        backend: &mut B,
        window: u32,
        level: u8,
        focused: bool,
    ) -> Result<(), B::Error> {
        if focused {
            self.raise_to_level_top(backend, window, level)
        } else {
            self.move_to_level_bottom(backend, window, level)
        }
    }

    /// Record that `MapWindow` was called, which raises the window to the
    /// absolute top of X siblings. The mirror is updated to match.
    /// Call this AFTER the actual `MapWindow` X request.
    pub fn noted_map(&mut self, window: u32) {
        if let Some(pos) = self.position(window) {
            let entry = self.order.remove(pos);
            self.order.push(entry);
        }
    }

    /// Ensure a transient child is above its parent. If the child is below
    /// the parent, move it immediately above the parent.
    pub fn enforce_transient<B: StackBackend>(
        &mut self,
        backend: &mut B,
        child: u32,
        parent: u32,
    ) -> Result<(), B::Error> {
        let Some(child_pos) = self.position(child) else {
            return Ok(());
        };
        let Some(parent_pos) = self.position(parent) else {
            return Ok(());
        };
        if child_pos > parent_pos {
            // Already above parent.
            return Ok(());
        }
        // Move child to immediately above parent.
        let entry = self.order.remove(child_pos);
        // parent_pos shifted down by 1 after the remove if parent was after child.
        let insert_at = if parent_pos > child_pos {
            parent_pos // was parent_pos - 1 + 1
        } else {
            parent_pos + 1
        };
        self.order.insert(insert_at, entry);
        // Apply to X.
        backend.stack_above(child, parent)?;
        Ok(())
    }

    // --- Internal helpers ---

    fn position(&self, window: u32) -> Option<usize> {
        self.order.iter().position(|e| e.window == window)
    }

    fn level_of(&self, window: u32) -> Option<u8> {
        self.order
            .iter()
            .find(|e| e.window == window)
            .map(|e| e.level)
    }

    /// Move window to the top of `level`. Emits at most one X operation.
    fn raise_to_level_top<B: StackBackend>(
        &mut self,
        backend: &mut B,
        window: u32,
        level: u8,
    ) -> Result<(), B::Error> {
        // Remove if already present.
        let was_present = if let Some(pos) = self.position(window) {
            self.order.remove(pos);
            true
        } else {
            false
        };
        // Find insertion point: after the last entry with level <= this level.
        let insert_at = self
            .order
            .iter()
            .rposition(|e| e.level <= level)
            .map_or(0, |i| i + 1);
        self.order.insert(insert_at, Entry { window, level });
        // Emit X operation if there are neighbors to position relative to.
        if was_present || self.order.len() > 1 {
            self.apply_position(backend, insert_at)?;
        }
        Ok(())
    }

    /// Move window to the bottom of `level`. Emits at most one X operation.
    fn move_to_level_bottom<B: StackBackend>(
        &mut self,
        backend: &mut B,
        window: u32,
        level: u8,
    ) -> Result<(), B::Error> {
        let was_present = if let Some(pos) = self.position(window) {
            self.order.remove(pos);
            true
        } else {
            false
        };
        // Find insertion point: before the first entry with level >= this level.
        let insert_at = self
            .order
            .iter()
            .position(|e| e.level >= level)
            .unwrap_or(self.order.len());
        self.order.insert(insert_at, Entry { window, level });
        if was_present || self.order.len() > 1 {
            self.apply_position(backend, insert_at)?;
        }
        Ok(())
    }

    /// Emit one X operation to position the entry at `index` relative to
    /// its nearest neighbor, matching the mirror order.
    fn apply_position<B: StackBackend>(
        &self,
        backend: &mut B,
        index: usize,
    ) -> Result<(), B::Error> {
        let window = self.order[index].window;
        // Prefer stacking below the next higher entry (more stable).
        if let Some(above) = self.order.get(index + 1) {
            backend.stack_below(window, above.window)?;
        } else if index > 0 {
            // No entry above -- stack above the entry below.
            backend.stack_above(window, self.order[index - 1].window)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[derive(Default)]
    struct RecordBackend {
        ops: RefCell<Vec<StackOp>>,
    }

    impl StackBackend for &RecordBackend {
        type Error = ();
        fn stack_above(&mut self, window: u32, sibling: u32) -> Result<(), ()> {
            self.ops
                .borrow_mut()
                .push(StackOp::Above { window, sibling });
            Ok(())
        }
        fn stack_below(&mut self, window: u32, sibling: u32) -> Result<(), ()> {
            self.ops
                .borrow_mut()
                .push(StackOp::Below { window, sibling });
            Ok(())
        }
    }

    #[test]
    fn insert_maintains_level_order() {
        let backend = RecordBackend::default();
        let mut mirror = StackMirror::new();
        // Insert tiled (level 3), then floating (level 4).
        mirror.insert(&mut &backend, 1, 3).unwrap();
        mirror.insert(&mut &backend, 2, 4).unwrap();
        assert_eq!(mirror.windows(), [1, 2]);
        // Insert another tiled -- goes to top of level 3, below floating.
        mirror.insert(&mut &backend, 3, 3).unwrap();
        assert_eq!(mirror.windows(), [1, 3, 2]);
    }

    #[test]
    fn raise_in_level_moves_to_top_of_level() {
        let backend = RecordBackend::default();
        let mut mirror = StackMirror::new();
        mirror.insert(&mut &backend, 1, 4).unwrap();
        mirror.insert(&mut &backend, 2, 4).unwrap();
        mirror.insert(&mut &backend, 3, 4).unwrap();
        assert_eq!(mirror.windows(), [1, 2, 3]);
        // Raise 1 to top of level 4.
        mirror.raise_in_level(&mut &backend, 1).unwrap();
        assert_eq!(mirror.windows(), [2, 3, 1]);
    }

    #[test]
    fn lower_in_level_moves_to_bottom_of_level() {
        let backend = RecordBackend::default();
        let mut mirror = StackMirror::new();
        mirror.insert(&mut &backend, 1, 4).unwrap();
        mirror.insert(&mut &backend, 2, 4).unwrap();
        mirror.insert(&mut &backend, 3, 4).unwrap();
        assert_eq!(mirror.windows(), [1, 2, 3]);
        // Lower 3 to bottom of level 4.
        mirror.lower_in_level(&mut &backend, 3).unwrap();
        assert_eq!(mirror.windows(), [3, 1, 2]);
    }

    #[test]
    fn focus_cycle_preserves_relative_order() {
        let backend = RecordBackend::default();
        let mut mirror = StackMirror::new();
        // Three floating windows.
        mirror.insert(&mut &backend, 1, 4).unwrap();
        mirror.insert(&mut &backend, 2, 4).unwrap();
        mirror.insert(&mut &backend, 3, 4).unwrap();
        // Focus 1 (raise), then focus 2 (raise).
        mirror.raise_in_level(&mut &backend, 1).unwrap();
        mirror.raise_in_level(&mut &backend, 2).unwrap();
        // Order should be: 3, 1, 2 (2 on top, then 1, then 3).
        assert_eq!(mirror.windows(), [3, 1, 2]);
    }

    #[test]
    fn tiled_stays_below_floating() {
        let backend = RecordBackend::default();
        let mut mirror = StackMirror::new();
        mirror.insert(&mut &backend, 1, 3).unwrap(); // tiled
        mirror.insert(&mut &backend, 2, 4).unwrap(); // floating
        // Raise tiled -- still below floating.
        mirror.raise_in_level(&mut &backend, 1).unwrap();
        assert_eq!(mirror.windows(), [1, 2]);
    }

    #[test]
    fn remove_and_reinsert() {
        let backend = RecordBackend::default();
        let mut mirror = StackMirror::new();
        mirror.insert(&mut &backend, 1, 3).unwrap();
        mirror.insert(&mut &backend, 2, 4).unwrap();
        mirror.remove(1);
        assert_eq!(mirror.windows(), [2]);
        mirror.insert(&mut &backend, 3, 3).unwrap();
        assert_eq!(mirror.windows(), [3, 2]);
    }

    #[test]
    fn transient_enforced_above_parent() {
        let backend = RecordBackend::default();
        let mut mirror = StackMirror::new();
        mirror.insert(&mut &backend, 1, 4).unwrap(); // parent
        mirror.insert(&mut &backend, 2, 4).unwrap(); // unrelated
        // Insert child below parent (simulate wrong initial order).
        mirror.order.insert(
            0,
            Entry {
                window: 3,
                level: 4,
            },
        );
        assert_eq!(mirror.windows(), [3, 1, 2]);
        // Enforce transient: child 3 should go above parent 1.
        mirror.enforce_transient(&mut &backend, 3, 1).unwrap();
        assert_eq!(mirror.windows(), [1, 3, 2]);
        let ops = backend.ops.borrow();
        assert_eq!(
            ops.last(),
            Some(&StackOp::Above {
                window: 3,
                sibling: 1
            })
        );
    }

    #[test]
    fn noted_map_moves_to_top() {
        let backend = RecordBackend::default();
        let mut mirror = StackMirror::new();
        mirror.insert(&mut &backend, 1, 3).unwrap();
        mirror.insert(&mut &backend, 2, 4).unwrap();
        // MapWindow raised 1 to top of X siblings.
        mirror.noted_map(1);
        assert_eq!(mirror.windows(), [2, 1]);
    }

    #[test]
    fn minimum_ops_emitted() {
        let backend = RecordBackend::default();
        let mut mirror = StackMirror::new();
        // First insert -- no ops (nothing to position relative to).
        mirror.insert(&mut &backend, 1, 3).unwrap();
        assert!(backend.ops.borrow().is_empty());
        // Second insert -- one op.
        mirror.insert(&mut &backend, 2, 4).unwrap();
        assert_eq!(backend.ops.borrow().len(), 1);
    }

    #[test]
    fn multiple_floating_focus_order() {
        let backend = RecordBackend::default();
        let mut mirror = StackMirror::new();
        // Four floating windows, inserted in order.
        mirror.insert(&mut &backend, 10, 4).unwrap();
        mirror.insert(&mut &backend, 20, 4).unwrap();
        mirror.insert(&mut &backend, 30, 4).unwrap();
        mirror.insert(&mut &backend, 40, 4).unwrap();
        assert_eq!(mirror.windows(), [10, 20, 30, 40]);

        // Focus 10 (oldest) -- goes to top.
        mirror.raise_in_level(&mut &backend, 10).unwrap();
        assert_eq!(mirror.windows(), [20, 30, 40, 10]);

        // Focus 30 -- goes to top, others stay in relative order.
        mirror.raise_in_level(&mut &backend, 30).unwrap();
        assert_eq!(mirror.windows(), [20, 40, 10, 30]);

        // Focus 20 -- goes to top.
        mirror.raise_in_level(&mut &backend, 20).unwrap();
        assert_eq!(mirror.windows(), [40, 10, 30, 20]);

        // Focus 30 again -- goes to top, 20 drops to second.
        mirror.raise_in_level(&mut &backend, 30).unwrap();
        assert_eq!(mirror.windows(), [40, 10, 20, 30]);
    }

    #[test]
    fn focus_across_levels_doesnt_break_ordering() {
        let backend = RecordBackend::default();
        let mut mirror = StackMirror::new();
        // Two tiled, three floating.
        mirror.insert(&mut &backend, 1, 3).unwrap(); // tiled
        mirror.insert(&mut &backend, 2, 3).unwrap(); // tiled
        mirror.insert(&mut &backend, 3, 4).unwrap(); // floating
        mirror.insert(&mut &backend, 4, 4).unwrap(); // floating
        mirror.insert(&mut &backend, 5, 4).unwrap(); // floating
        assert_eq!(mirror.windows(), [1, 2, 3, 4, 5]);

        // Focus tiled 1 -- top of level 3, still below all floating.
        mirror.raise_in_level(&mut &backend, 1).unwrap();
        assert_eq!(mirror.windows(), [2, 1, 3, 4, 5]);

        // Focus floating 3 -- top of level 4.
        mirror.raise_in_level(&mut &backend, 3).unwrap();
        assert_eq!(mirror.windows(), [2, 1, 4, 5, 3]);

        // Focus tiled 2 -- top of level 3, still below floating.
        mirror.raise_in_level(&mut &backend, 2).unwrap();
        assert_eq!(mirror.windows(), [1, 2, 4, 5, 3]);

        // Focus floating 5 -- top of level 4.
        mirror.raise_in_level(&mut &backend, 5).unwrap();
        assert_eq!(mirror.windows(), [1, 2, 4, 3, 5]);

        // All tiled still below all floating.
        let tiled_max = mirror.windows().iter().position(|&w| w == 2).unwrap();
        let float_min = mirror.windows().iter().position(|&w| w == 4).unwrap();
        assert!(tiled_max < float_min, "tiled must be below floating");
    }

    #[test]
    fn rapid_focus_cycling_three_windows() {
        let backend = RecordBackend::default();
        let mut mirror = StackMirror::new();
        mirror.insert(&mut &backend, 1, 4).unwrap();
        mirror.insert(&mut &backend, 2, 4).unwrap();
        mirror.insert(&mut &backend, 3, 4).unwrap();

        // Cycle: 1, 2, 3, 1, 2, 3
        for _ in 0..2 {
            mirror.raise_in_level(&mut &backend, 1).unwrap();
            assert_eq!(*mirror.windows().last().unwrap(), 1);
            mirror.raise_in_level(&mut &backend, 2).unwrap();
            assert_eq!(*mirror.windows().last().unwrap(), 2);
            mirror.raise_in_level(&mut &backend, 3).unwrap();
            assert_eq!(*mirror.windows().last().unwrap(), 3);
        }
        // After two full cycles, order should be 1, 2, 3 (3 on top).
        assert_eq!(mirror.windows(), [1, 2, 3]);
    }

    #[test]
    fn state_change_tiled_to_floating() {
        let backend = RecordBackend::default();
        let mut mirror = StackMirror::new();
        mirror.insert(&mut &backend, 1, 3).unwrap(); // tiled
        mirror.insert(&mut &backend, 2, 3).unwrap(); // tiled
        mirror.insert(&mut &backend, 3, 4).unwrap(); // floating
        assert_eq!(mirror.windows(), [1, 2, 3]);

        // Window 1 becomes floating (focused).
        mirror.set_level(&mut &backend, 1, 4, true).unwrap();
        assert_eq!(mirror.windows(), [2, 3, 1]);
        // 1 is now above 3 (both floating, 1 most recently focused).
    }

    #[test]
    fn state_change_floating_to_tiled() {
        let backend = RecordBackend::default();
        let mut mirror = StackMirror::new();
        mirror.insert(&mut &backend, 1, 4).unwrap(); // floating
        mirror.insert(&mut &backend, 2, 4).unwrap(); // floating
        mirror.insert(&mut &backend, 3, 3).unwrap(); // tiled
        // Tiled inserted at top of level 3 = before first level >= 3.
        // But 1 is level 4, so 3 goes before 1.
        assert_eq!(mirror.windows(), [3, 1, 2]);

        // Window 2 becomes tiled (unfocused).
        // move_to_level_bottom inserts before first level >= 3, which is 3 itself.
        mirror.set_level(&mut &backend, 2, 3, false).unwrap();
        assert_eq!(mirror.windows(), [2, 3, 1]);

        // Verify levels are correct.
        assert_eq!(mirror.order[0].level, 3); // 2: now tiled, bottom of level
        assert_eq!(mirror.order[1].level, 3); // 3: tiled
        assert_eq!(mirror.order[2].level, 4); // 1: still floating
    }

    #[test]
    fn state_change_to_fullscreen_and_back() {
        let backend = RecordBackend::default();
        let mut mirror = StackMirror::new();
        mirror.insert(&mut &backend, 1, 3).unwrap(); // tiled
        mirror.insert(&mut &backend, 2, 4).unwrap(); // floating
        mirror.insert(&mut &backend, 3, 4).unwrap(); // floating
        assert_eq!(mirror.windows(), [1, 2, 3]);

        // Window 1 goes fullscreen (level 5), focused.
        mirror.set_level(&mut &backend, 1, 5, true).unwrap();
        assert_eq!(mirror.windows(), [2, 3, 1]);
        assert_eq!(mirror.order[2].level, 5);

        // Window 1 back to tiled (level 3), unfocused.
        mirror.set_level(&mut &backend, 1, 3, false).unwrap();
        assert_eq!(mirror.windows(), [1, 2, 3]);
        assert_eq!(mirror.order[0].level, 3);
    }

    #[test]
    fn mixed_operations_stress() {
        let backend = RecordBackend::default();
        let mut mirror = StackMirror::new();

        // Build a desktop: 2 tiled, 3 floating.
        mirror.insert(&mut &backend, 10, 3).unwrap();
        mirror.insert(&mut &backend, 20, 3).unwrap();
        mirror.insert(&mut &backend, 30, 4).unwrap();
        mirror.insert(&mut &backend, 40, 4).unwrap();
        mirror.insert(&mut &backend, 50, 4).unwrap();
        assert_eq!(mirror.windows(), [10, 20, 30, 40, 50]);

        // Focus floating 30.
        mirror.raise_in_level(&mut &backend, 30).unwrap();
        assert_eq!(mirror.windows(), [10, 20, 40, 50, 30]);

        // Focus tiled 10.
        mirror.raise_in_level(&mut &backend, 10).unwrap();
        assert_eq!(mirror.windows(), [20, 10, 40, 50, 30]);

        // 10 goes floating (focused).
        mirror.set_level(&mut &backend, 10, 4, true).unwrap();
        assert_eq!(mirror.windows(), [20, 40, 50, 30, 10]);

        // Remove 50.
        mirror.remove(50);
        assert_eq!(mirror.windows(), [20, 40, 30, 10]);

        // Add new tiled 60.
        mirror.insert(&mut &backend, 60, 3).unwrap();
        assert_eq!(mirror.windows(), [20, 60, 40, 30, 10]);

        // 10 back to tiled (unfocused).
        mirror.set_level(&mut &backend, 10, 3, false).unwrap();
        assert_eq!(mirror.windows(), [10, 20, 60, 40, 30]);

        // All tiled below all floating.
        for (i, &w) in mirror.windows().iter().enumerate() {
            let level = mirror.order[i].level;
            if i > 0 {
                assert!(
                    level >= mirror.order[i - 1].level,
                    "level order violated at index {i}: window {w}"
                );
            }
        }
    }

    #[test]
    fn unfocus_with_lower_preserves_upstream_behavior() {
        let backend = RecordBackend::default();
        let mut mirror = StackMirror::new();
        mirror.insert(&mut &backend, 1, 4).unwrap();
        mirror.insert(&mut &backend, 2, 4).unwrap();
        mirror.insert(&mut &backend, 3, 4).unwrap();

        // Simulate upstream: focus raises, unfocus lowers.
        // Focus 1.
        mirror.raise_in_level(&mut &backend, 1).unwrap();
        assert_eq!(mirror.windows(), [2, 3, 1]);

        // Focus 2, unfocus 1.
        mirror.raise_in_level(&mut &backend, 2).unwrap();
        // After raise(2): [3, 1, 2]
        mirror.lower_in_level(&mut &backend, 1).unwrap();
        // After lower(1): 1 goes to bottom of level 4
        let after_step2 = mirror.windows();
        // 2 should be on top (most recently focused).
        assert_eq!(*after_step2.last().unwrap(), 2, "2 should be on top");
        // 1 should be on bottom (just lowered).
        assert_eq!(after_step2[0], 1, "1 should be on bottom");

        // Focus 3, unfocus 2.
        mirror.raise_in_level(&mut &backend, 3).unwrap();
        mirror.lower_in_level(&mut &backend, 2).unwrap();
        let after_step3 = mirror.windows();
        // 3 should be on top (most recently focused).
        assert_eq!(*after_step3.last().unwrap(), 3, "3 should be on top");
        // 2 should be on bottom (just lowered).
        assert_eq!(after_step3[0], 2, "2 should be on bottom");
    }

    #[test]
    fn layer_above_always_on_top() {
        let backend = RecordBackend::default();
        let mut mirror = StackMirror::new();
        // level 3=tiled, 4=floating, 7=above+floating
        mirror.insert(&mut &backend, 1, 3).unwrap();
        mirror.insert(&mut &backend, 2, 4).unwrap();
        mirror.insert(&mut &backend, 3, 7).unwrap();
        assert_eq!(mirror.windows(), [1, 2, 3]);

        // Focus tiled -- still below everything.
        mirror.raise_in_level(&mut &backend, 1).unwrap();
        assert_eq!(mirror.windows(), [1, 2, 3]);

        // Focus floating -- still below above-layer.
        mirror.raise_in_level(&mut &backend, 2).unwrap();
        assert_eq!(mirror.windows(), [1, 2, 3]);

        // Focus above-layer -- stays on top.
        mirror.raise_in_level(&mut &backend, 3).unwrap();
        assert_eq!(mirror.windows(), [1, 2, 3]);

        // Add another above-layer window.
        mirror.insert(&mut &backend, 4, 7).unwrap();
        assert_eq!(mirror.windows(), [1, 2, 3, 4]);

        // Focus 3 again -- goes above 4.
        mirror.raise_in_level(&mut &backend, 3).unwrap();
        assert_eq!(mirror.windows(), [1, 2, 4, 3]);
    }

    #[test]
    fn ops_count_is_minimal() {
        let backend = RecordBackend::default();
        let mut mirror = StackMirror::new();
        mirror.insert(&mut &backend, 1, 3).unwrap();
        mirror.insert(&mut &backend, 2, 4).unwrap();
        mirror.insert(&mut &backend, 3, 4).unwrap();
        backend.ops.borrow_mut().clear();

        // Raising a window that's already at the top of its level = still 1 op
        // (because we remove and reinsert, position may be same but we still emit).
        mirror.raise_in_level(&mut &backend, 3).unwrap();
        assert_eq!(backend.ops.borrow().len(), 1);
        backend.ops.borrow_mut().clear();

        // Each focus change = exactly 1 op.
        mirror.raise_in_level(&mut &backend, 2).unwrap();
        assert_eq!(backend.ops.borrow().len(), 1);
        backend.ops.borrow_mut().clear();

        // State change = exactly 1 op.
        mirror.set_level(&mut &backend, 1, 4, true).unwrap();
        assert_eq!(backend.ops.borrow().len(), 1);
        backend.ops.borrow_mut().clear();

        // Remove = 0 ops.
        mirror.remove(1);
        assert!(backend.ops.borrow().is_empty());
    }

    #[test]
    fn set_level_repositions() {
        let backend = RecordBackend::default();
        let mut mirror = StackMirror::new();
        mirror.insert(&mut &backend, 1, 3).unwrap(); // tiled
        mirror.insert(&mut &backend, 2, 4).unwrap(); // floating
        mirror.insert(&mut &backend, 3, 4).unwrap(); // floating
        assert_eq!(mirror.windows(), [1, 2, 3]);
        // Change 1 from tiled (3) to floating (4), focused.
        mirror.set_level(&mut &backend, 1, 4, true).unwrap();
        assert_eq!(mirror.windows(), [2, 3, 1]);
        // Change 1 back to tiled (3), unfocused.
        mirror.set_level(&mut &backend, 1, 3, false).unwrap();
        assert_eq!(mirror.windows(), [1, 2, 3]);
    }
}
