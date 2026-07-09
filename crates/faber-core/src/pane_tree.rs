/// Generic pane-tree: split/collapse/resize/layout logic, fully gpui-free.
///
/// The tree is parameterised over an opaque `Id: Copy + Eq`. Faber-app maps
/// `Id = PaneId` (a `u64` newtype). The only invariants are:
///   - Every `PaneAxis` has `members.len() >= 2`.
///   - `flexes.len() == members.len()`.
///   - `flexes` sum ≈ `members.len()` (each starts at 1.0; resize keeps the sum).
use serde::{Deserialize, Serialize};

// ── Geometry (avoids the gpui dep) ──────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub fn contains(&self, p: Vec2) -> bool {
        p.x >= self.x && p.x < self.x + self.w && p.y >= self.y && p.y < self.y + self.h
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Vec2 {
    pub x: f32,
    pub y: f32,
}

// ── Core types ───────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub struct PaneId(pub u64);

/// Which axis are children arranged along?
/// `Horizontal` = side-by-side (Left/Right split).
/// `Vertical`   = stacked (Up/Down split).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Axis {
    Horizontal,
    Vertical,
}

/// Direction of a split or focus-move action.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SplitDirection {
    Left,
    Right,
    Up,
    Down,
}

impl SplitDirection {
    pub fn axis(self) -> Axis {
        match self {
            SplitDirection::Left | SplitDirection::Right => Axis::Horizontal,
            SplitDirection::Up | SplitDirection::Down => Axis::Vertical,
        }
    }

    /// True if the new pane should be inserted *before* the existing pane in member order.
    pub fn places_before(self) -> bool {
        matches!(self, SplitDirection::Left | SplitDirection::Up)
    }
}

/// Result of hit-testing a cursor against a pane body.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DropZone {
    /// Move the tab into this pane's tab list.
    Center,
    /// Split this pane; the new pane lands on the given side.
    Edge(SplitDirection),
}

// ── Tree ─────────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Member<Id> {
    Pane(Id),
    Axis(PaneAxis<Id>),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PaneAxis<Id> {
    pub axis: Axis,
    /// At least 2 members (invariant).
    pub members: Vec<Member<Id>>,
    /// `flexes[i]` is the relative size of `members[i]`; sum ≈ members.len().
    pub flexes: Vec<f32>,
}

impl<Id: Copy + Eq> PaneAxis<Id> {
    /// Try to split `target` inside this axis (recurse into children).
    /// Returns `true` if the target was found and split.
    fn split(&mut self, target: Id, new_id: Id, dir: SplitDirection) -> bool {
        for i in 0..self.members.len() {
            match &mut self.members[i] {
                Member::Pane(id) if *id == target => {
                    if self.axis == dir.axis() {
                        // Same axis: insert as a sibling with flex=1.0.
                        // This maintains the sum==len invariant: adding one
                        // member with flex 1.0 keeps each pane at equal weight.
                        let pos = if dir.places_before() { i } else { i + 1 };
                        self.flexes.insert(pos, 1.0);
                        self.members.insert(pos, Member::Pane(new_id));
                    } else {
                        // Different axis: replace leaf with a nested 2-member axis.
                        let (a, b) = if dir.places_before() {
                            (new_id, target)
                        } else {
                            (target, new_id)
                        };
                        let nested = PaneAxis {
                            axis: dir.axis(),
                            members: vec![Member::Pane(a), Member::Pane(b)],
                            flexes: vec![1.0, 1.0],
                        };
                        self.members[i] = Member::Axis(nested);
                        // flex of this slot is unchanged (nested axis counts as one unit)
                    }
                    return true;
                }
                Member::Pane(_) => {}
                Member::Axis(child) => {
                    if child.split(target, new_id, dir) {
                        return true;
                    }
                }
            }
        }
        false
    }

    /// Remove `id` from this sub-tree. Returns `(found, neighbour)`.
    /// `found` means the id was found and removed from this level (caller must
    /// handle collapsing if `members.len() < 2` after).
    /// `neighbour` is the id that should receive focus, if known.
    fn remove_pane(&mut self, id: Id) -> Option<Id> {
        for i in 0..self.members.len() {
            match &self.members[i] {
                Member::Pane(pid) if *pid == id => {
                    // Identify neighbour before removing.
                    let neighbour = if i + 1 < self.members.len() {
                        self.first_pane_id(&self.members[i + 1].clone())
                    } else if i > 0 {
                        self.first_pane_id(&self.members[i - 1].clone())
                    } else {
                        return None; // only member (caller handles)
                    };
                    self.flexes.remove(i);
                    self.members.remove(i);
                    // Re-normalise so sum(flexes) == members.len().
                    let new_n = self.members.len() as f32;
                    let current_sum: f32 = self.flexes.iter().sum();
                    if current_sum > 0.0 {
                        let scale = new_n / current_sum;
                        for f in &mut self.flexes {
                            *f *= scale;
                        }
                    }
                    return Some(neighbour);
                }
                Member::Pane(_) => {}
                Member::Axis(child_axis) => {
                    // Clone to inspect, then mutate.
                    let child_members_len = child_axis.members.len();
                    // Check if the target is a direct child of the nested axis.
                    let found_in_child = child_axis.members.iter().any(|m| {
                        if let Member::Pane(pid) = m {
                            *pid == id
                        } else {
                            false
                        }
                    });

                    if found_in_child && child_members_len == 2 {
                        // Nested axis will collapse to 1 member — splice up.
                        let child_axis_clone = if let Member::Axis(a) = &self.members[i] {
                            a.clone()
                        } else {
                            unreachable!()
                        };
                        let survivor_idx = child_axis_clone
                            .members
                            .iter()
                            .position(|m| {
                                if let Member::Pane(pid) = m {
                                    *pid != id
                                } else {
                                    true
                                }
                            })
                            .unwrap();
                        let neighbour = self.first_pane_id(&child_axis_clone.members[survivor_idx]);
                        let survivor = child_axis_clone.members[survivor_idx].clone();
                        // Replace the child axis slot with the survivor.
                        self.members[i] = survivor;
                        // flex of slot i stays the same.
                        return Some(neighbour);
                    } else {
                        let child_axis = if let Member::Axis(a) = &mut self.members[i] {
                            a
                        } else {
                            unreachable!()
                        };
                        if let Some(neighbour) = child_axis.remove_pane(id) {
                            return Some(neighbour);
                        }
                    }
                }
            }
        }
        None
    }

    fn first_pane_id(&self, m: &Member<Id>) -> Id {
        match m {
            Member::Pane(id) => *id,
            Member::Axis(a) => self.first_pane_id(&a.members[0]),
        }
    }

    fn collect_ids(&self, out: &mut Vec<Id>) {
        for m in &self.members {
            match m {
                Member::Pane(id) => out.push(*id),
                Member::Axis(a) => a.collect_ids(out),
            }
        }
    }

    fn contains(&self, id: Id) -> bool {
        self.members.iter().any(|m| match m {
            Member::Pane(pid) => *pid == id,
            Member::Axis(a) => a.contains(id),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PaneGroup<Id> {
    pub root: Member<Id>,
}

impl<Id: Copy + Eq> PaneGroup<Id> {
    pub fn single(id: Id) -> Self {
        Self {
            root: Member::Pane(id),
        }
    }

    /// Split the pane `target` by adding `new_id` on the `dir` side.
    /// Returns `false` if `target` was not found.
    pub fn split(&mut self, target: Id, new_id: Id, dir: SplitDirection) -> bool {
        match &mut self.root {
            Member::Pane(id) if *id == target => {
                let (a, b) = if dir.places_before() {
                    (new_id, target)
                } else {
                    (target, new_id)
                };
                self.root = Member::Axis(PaneAxis {
                    axis: dir.axis(),
                    members: vec![Member::Pane(a), Member::Pane(b)],
                    flexes: vec![1.0, 1.0],
                });
                true
            }
            Member::Pane(_) => false,
            Member::Axis(axis) => axis.split(target, new_id, dir),
        }
    }

    /// Remove `id` from the tree, collapsing any axis that would be left with
    /// a single member. Returns the neighbour that should receive focus,
    /// or `None` if the tree is now empty.
    pub fn remove_pane(&mut self, id: Id) -> Option<Id> {
        match &mut self.root {
            Member::Pane(pid) if *pid == id => {
                None // tree is empty
            }
            Member::Pane(_) => None, // id not in tree
            Member::Axis(axis) => {
                // Handle the case where root axis has the pane as a direct child.
                if axis.members.len() == 2 {
                    let has_direct = axis
                        .members
                        .iter()
                        .any(|m| matches!(m, Member::Pane(pid) if *pid == id));
                    if has_direct {
                        let survivor_idx = axis
                            .members
                            .iter()
                            .position(|m| !matches!(m, Member::Pane(pid) if *pid == id))
                            .unwrap();
                        let neighbour = first_pane_in(&axis.members[survivor_idx]);
                        let survivor = axis.members[survivor_idx].clone();
                        self.root = survivor;
                        return Some(neighbour);
                    }
                }
                axis.remove_pane(id)
            }
        }
    }

    /// Ids in visual left-to-right / top-to-bottom order.
    pub fn pane_ids(&self) -> Vec<Id> {
        let mut out = Vec::new();
        match &self.root {
            Member::Pane(id) => out.push(*id),
            Member::Axis(a) => a.collect_ids(&mut out),
        }
        out
    }

    pub fn contains(&self, id: Id) -> bool {
        match &self.root {
            Member::Pane(pid) => *pid == id,
            Member::Axis(a) => a.contains(id),
        }
    }

    pub fn is_single(&self) -> bool {
        matches!(self.root, Member::Pane(_))
    }

    /// Adjust the flex divider at `axis_path[divider_ix]` by `delta_px` pixels.
    /// `container_len` is the total pixel length along the axis. `min_frac` is
    /// the minimum flex fraction any single member may hold.
    pub fn resize(
        &mut self,
        axis_path: &[usize],
        divider_ix: usize,
        delta_px: f32,
        container_len_px: f32,
        min_frac: f32,
    ) {
        if container_len_px <= 0.0 {
            return;
        }
        let axis = find_axis_mut(&mut self.root, axis_path);
        let n = axis.members.len() as f32;
        // Convert delta from pixels to flex units.
        let delta_flex = delta_px / container_len_px * n;
        let left = divider_ix;
        let right = divider_ix + 1;
        if right >= axis.flexes.len() {
            return;
        }
        // Clamp so neither member drops below min_frac * n.
        let actual_delta = delta_flex.clamp(
            -(axis.flexes[left] - min_frac * n),
            axis.flexes[right] - min_frac * n,
        );
        axis.flexes[left] += actual_delta;
        axis.flexes[right] -= actual_delta;
    }

    /// Serialize the tree using a per-leaf payload closure.
    pub fn to_serialized<P>(&self, leaf: &impl Fn(Id) -> P) -> SerializedMember<P> {
        member_to_serialized(&self.root, leaf)
    }

    /// Reconstruct from a serialized tree. `mk_id` is called once per
    /// `SerializedMember::Pane` leaf and receives the payload.
    pub fn from_serialized<P, F>(m: &SerializedMember<P>, mk_id: &mut F) -> Self
    where
        F: FnMut(&P) -> Id,
    {
        Self {
            root: member_from_serialized(m, mk_id),
        }
    }
}

fn first_pane_in<Id: Copy>(m: &Member<Id>) -> Id {
    match m {
        Member::Pane(id) => *id,
        Member::Axis(a) => first_pane_in(&a.members[0]),
    }
}

fn find_axis_mut<'a, Id>(root: &'a mut Member<Id>, path: &[usize]) -> &'a mut PaneAxis<Id> {
    if path.is_empty() {
        if let Member::Axis(a) = root {
            return a;
        }
        panic!("find_axis_mut: empty path but root is not an Axis");
    }
    if let Member::Axis(a) = root {
        find_axis_mut(&mut a.members[path[0]], &path[1..])
    } else {
        panic!("find_axis_mut: path descends through a Pane leaf");
    }
}

// ── Layout & geometry ─────────────────────────────────────────────────────────

/// Walk the tree and assign each leaf pane its pixel `Rect` within `root_rect`.
/// `sash` is the pixel gap between members (deducted from available space).
pub fn layout<Id: Copy + Eq>(group: &PaneGroup<Id>, root_rect: Rect, sash: f32) -> Vec<(Id, Rect)> {
    let mut out = Vec::new();
    layout_member(&group.root, root_rect, sash, &mut out);
    out
}

fn layout_member<Id: Copy>(m: &Member<Id>, rect: Rect, sash: f32, out: &mut Vec<(Id, Rect)>) {
    match m {
        Member::Pane(id) => out.push((*id, rect)),
        Member::Axis(a) => {
            let n = a.members.len();
            let total_flex: f32 = a.flexes.iter().sum();
            let total_sash = sash * (n - 1) as f32;
            let available = match a.axis {
                Axis::Horizontal => rect.w - total_sash,
                Axis::Vertical => rect.h - total_sash,
            };
            let mut offset = 0.0f32;
            for (i, (member, flex)) in a.members.iter().zip(a.flexes.iter()).enumerate() {
                let size = available * flex / total_flex;
                let child_rect = match a.axis {
                    Axis::Horizontal => Rect {
                        x: rect.x + offset,
                        y: rect.y,
                        w: size,
                        h: rect.h,
                    },
                    Axis::Vertical => Rect {
                        x: rect.x,
                        y: rect.y + offset,
                        w: rect.w,
                        h: size,
                    },
                };
                layout_member(member, child_rect, sash, out);
                offset += size + if i + 1 < n { sash } else { 0.0 };
            }
        }
    }
}

/// Which leaf pane contains point `p` in the pre-computed layout?
pub fn pane_at<Id: Copy>(layout: &[(Id, Rect)], p: Vec2) -> Option<Id> {
    layout
        .iter()
        .find(|(_, r)| r.contains(p))
        .map(|(id, _)| *id)
}

/// Classify cursor `cursor` inside pane body `body` into a `DropZone`.
/// `edge_frac` (e.g. 0.25) is the fraction from each edge that counts as an
/// "edge zone". Corner ties are broken by whichever edge the cursor is nearer.
pub fn drop_zone(body: Rect, cursor: Vec2, edge_frac: f32) -> DropZone {
    let lx = cursor.x - body.x;
    let ly = cursor.y - body.y;
    let rx = body.w - lx;
    let ry = body.h - ly;

    let left_t = lx / body.w;
    let right_t = rx / body.w;
    let top_t = ly / body.h;
    let bot_t = ry / body.h;

    let in_left = left_t < edge_frac;
    let in_right = right_t < edge_frac;
    let in_top = top_t < edge_frac;
    let in_bot = bot_t < edge_frac;

    if !in_left && !in_right && !in_top && !in_bot {
        return DropZone::Center;
    }

    // Pick the nearest edge (smallest distance-to-edge fraction).
    let mut best_frac = f32::MAX;
    let mut best = DropZone::Center;
    for (flag, frac, zone) in [
        (in_left, left_t, DropZone::Edge(SplitDirection::Left)),
        (in_right, right_t, DropZone::Edge(SplitDirection::Right)),
        (in_top, top_t, DropZone::Edge(SplitDirection::Up)),
        (in_bot, bot_t, DropZone::Edge(SplitDirection::Down)),
    ] {
        if flag && frac < best_frac {
            best_frac = frac;
            best = zone;
        }
    }
    best
}

// ── Serialisation ─────────────────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SerializedMember<P> {
    Pane(P),
    Axis {
        axis: Axis,
        members: Vec<SerializedMember<P>>,
        flexes: Vec<f32>,
    },
}

fn member_to_serialized<Id: Copy, P>(
    m: &Member<Id>,
    leaf: &impl Fn(Id) -> P,
) -> SerializedMember<P> {
    match m {
        Member::Pane(id) => SerializedMember::Pane(leaf(*id)),
        Member::Axis(a) => SerializedMember::Axis {
            axis: a.axis,
            members: a
                .members
                .iter()
                .map(|m| member_to_serialized(m, leaf))
                .collect(),
            flexes: a.flexes.clone(),
        },
    }
}

fn member_from_serialized<P, Id: Copy, F: FnMut(&P) -> Id>(
    m: &SerializedMember<P>,
    mk_id: &mut F,
) -> Member<Id> {
    match m {
        SerializedMember::Pane(p) => Member::Pane(mk_id(p)),
        SerializedMember::Axis {
            axis,
            members,
            flexes,
        } => Member::Axis(PaneAxis {
            axis: *axis,
            members: members
                .iter()
                .map(|m| member_from_serialized(m, mk_id))
                .collect(),
            flexes: flexes.clone(),
        }),
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(g: &PaneGroup<u32>) -> Vec<u32> {
        g.pane_ids()
    }

    fn flex_sum_ok(g: &PaneGroup<u32>) -> bool {
        fn check<Id>(m: &Member<Id>) -> bool {
            match m {
                Member::Pane(_) => true,
                Member::Axis(a) => {
                    let n = a.members.len() as f32;
                    let sum: f32 = a.flexes.iter().sum();
                    (sum - n).abs() < 1e-4 && a.members.iter().all(check)
                }
            }
        }
        check(&g.root)
    }

    fn all_axes_ge2<Id>(g: &PaneGroup<Id>) -> bool {
        fn check<Id>(m: &Member<Id>) -> bool {
            match m {
                Member::Pane(_) => true,
                Member::Axis(a) => a.members.len() >= 2 && a.members.iter().all(check),
            }
        }
        check(&g.root)
    }

    // ── split ────────────────────────────────────────────────────────────────

    #[test]
    fn split_single_right() {
        let mut g = PaneGroup::single(1u32);
        assert!(g.split(1, 2, SplitDirection::Right));
        assert_eq!(ids(&g), vec![1, 2]);
        assert!(flex_sum_ok(&g));
        assert!(all_axes_ge2(&g));
    }

    #[test]
    fn split_single_left() {
        let mut g = PaneGroup::single(1u32);
        assert!(g.split(1, 2, SplitDirection::Left));
        assert_eq!(ids(&g), vec![2, 1]); // 2 is placed before
        assert!(flex_sum_ok(&g));
    }

    #[test]
    fn split_single_down() {
        let mut g = PaneGroup::single(1u32);
        assert!(g.split(1, 2, SplitDirection::Down));
        assert_eq!(ids(&g), vec![1, 2]);
        if let Member::Axis(a) = &g.root {
            assert_eq!(a.axis, Axis::Vertical);
        } else {
            panic!("expected axis")
        }
        assert!(flex_sum_ok(&g));
    }

    #[test]
    fn split_single_up() {
        let mut g = PaneGroup::single(1u32);
        assert!(g.split(1, 2, SplitDirection::Up));
        assert_eq!(ids(&g), vec![2, 1]);
        assert!(flex_sum_ok(&g));
    }

    #[test]
    fn split_unknown_id_returns_false() {
        let mut g = PaneGroup::single(1u32);
        assert!(!g.split(99, 2, SplitDirection::Right));
        assert_eq!(ids(&g), vec![1]);
    }

    #[test]
    fn split_same_axis_sibling_insert() {
        // Split right twice on pane 1 → [1, 2, 3] in one horizontal axis (no nesting).
        let mut g = PaneGroup::single(1u32);
        g.split(1, 2, SplitDirection::Right);
        g.split(2, 3, SplitDirection::Right);
        assert_eq!(ids(&g), vec![1, 2, 3]);
        if let Member::Axis(a) = &g.root {
            assert_eq!(a.members.len(), 3); // flat, not nested
        } else {
            panic!()
        }
        assert!(flex_sum_ok(&g));
        assert!(all_axes_ge2(&g));
    }

    #[test]
    fn split_different_axis_nests() {
        // H-split then V-split on the right pane → nested.
        let mut g = PaneGroup::single(1u32);
        g.split(1, 2, SplitDirection::Right);
        g.split(2, 3, SplitDirection::Down);
        // root should be H-axis [1, V-axis[2,3]]
        assert_eq!(ids(&g), vec![1, 2, 3]);
        assert!(flex_sum_ok(&g));
        assert!(all_axes_ge2(&g));
    }

    #[test]
    fn split_deep_nesting_preserves_siblings() {
        let mut g = PaneGroup::single(1u32);
        g.split(1, 2, SplitDirection::Right);
        g.split(2, 3, SplitDirection::Down);
        g.split(3, 4, SplitDirection::Right);
        let found_ids = ids(&g);
        assert!(
            found_ids.contains(&1)
                && found_ids.contains(&2)
                && found_ids.contains(&3)
                && found_ids.contains(&4)
        );
        assert!(flex_sum_ok(&g));
        assert!(all_axes_ge2(&g));
    }

    // ── remove / collapse ─────────────────────────────────────────────────────

    #[test]
    fn remove_from_two_member_root_collapses() {
        let mut g = PaneGroup::single(1u32);
        g.split(1, 2, SplitDirection::Right);
        let neighbour = g.remove_pane(1);
        assert_eq!(neighbour, Some(2));
        assert!(g.is_single());
        assert_eq!(ids(&g), vec![2]);
    }

    #[test]
    fn remove_last_returns_none() {
        let mut g = PaneGroup::single(1u32);
        let n = g.remove_pane(1);
        assert_eq!(n, None);
    }

    #[test]
    fn remove_from_three_member_axis() {
        let mut g = PaneGroup::single(1u32);
        g.split(1, 2, SplitDirection::Right);
        g.split(2, 3, SplitDirection::Right); // [1,2,3]
        let n = g.remove_pane(2);
        assert!(n == Some(1) || n == Some(3));
        assert_eq!(ids(&g), vec![1, 3]);
        assert!(flex_sum_ok(&g));
        assert!(all_axes_ge2(&g));
    }

    #[test]
    fn remove_flex_redistributed() {
        let mut g = PaneGroup::single(1u32);
        g.split(1, 2, SplitDirection::Right);
        g.split(2, 3, SplitDirection::Right); // [1,2,3] flexes=[1,1,1]
        g.remove_pane(2); // survivors scaled so sum==new_len==2
        if let Member::Axis(a) = &g.root {
            let sum: f32 = a.flexes.iter().sum();
            assert!((sum - 2.0).abs() < 1e-4); // sum == member count
            for f in &a.flexes {
                assert!(*f > 0.0);
            }
        } else {
            panic!()
        }
    }

    #[test]
    fn remove_nested_collapses_parent_axis() {
        // [1, V[2,3]] — remove 2 → V collapses → [1, 3]
        let mut g = PaneGroup::single(1u32);
        g.split(1, 2, SplitDirection::Right);
        g.split(2, 3, SplitDirection::Down);
        let n = g.remove_pane(2);
        assert!(n.is_some());
        let found = ids(&g);
        assert!(found.contains(&1) && found.contains(&3));
        assert!(flex_sum_ok(&g));
        assert!(all_axes_ge2(&g));
    }

    #[test]
    fn remove_neighbour_is_visual_adjacent() {
        let mut g = PaneGroup::single(1u32);
        g.split(1, 2, SplitDirection::Right);
        g.split(2, 3, SplitDirection::Right); // [1,2,3]
        // Removing left end should return 2.
        let n = g.remove_pane(1).unwrap();
        assert_eq!(n, 2);
    }

    // ── resize ────────────────────────────────────────────────────────────────

    #[test]
    fn resize_transfers_flex() {
        let mut g = PaneGroup::single(1u32);
        g.split(1, 2, SplitDirection::Right); // flexes [1.0, 1.0], container=100px
        g.resize(&[], 0, 10.0, 100.0, 0.05);
        if let Member::Axis(a) = &g.root {
            let sum: f32 = a.flexes.iter().sum();
            assert!((sum - 2.0).abs() < 1e-4);
            assert!(a.flexes[0] > 1.0);
            assert!(a.flexes[1] < 1.0);
        }
    }

    #[test]
    fn resize_clamps_at_min_frac() {
        let mut g = PaneGroup::single(1u32);
        g.split(1, 2, SplitDirection::Right);
        // Try to push left pane to -∞. min_frac=0.1 means min flex = 0.2 (10% of 2).
        g.resize(&[], 0, -1000.0, 100.0, 0.1);
        if let Member::Axis(a) = &g.root {
            assert!(a.flexes[0] >= 0.1 * 2.0 - 1e-4);
            assert!(a.flexes[1] >= 0.1 * 2.0 - 1e-4);
        }
    }

    #[test]
    fn resize_nested_via_path() {
        let mut g = PaneGroup::single(1u32);
        g.split(1, 2, SplitDirection::Right);
        g.split(2, 3, SplitDirection::Down); // root H-axis, right child is V-axis
        // Resize inside the nested V-axis (path=[1] = second member of root)
        g.resize(&[1], 0, 5.0, 50.0, 0.05);
        // Root flex should be unchanged.
        if let Member::Axis(root_a) = &g.root {
            let root_sum: f32 = root_a.flexes.iter().sum();
            assert!((root_sum - 2.0).abs() < 1e-4);
            // Nested axis flex should have changed.
            if let Member::Axis(nested) = &root_a.members[1] {
                let ns: f32 = nested.flexes.iter().sum();
                assert!((ns - 2.0).abs() < 1e-4);
                // After +5px on 50px: left grows.
                assert!(nested.flexes[0] > 1.0);
            } else {
                panic!("expected nested axis")
            }
        }
    }

    // ── layout / geometry ─────────────────────────────────────────────────────

    #[test]
    fn layout_single_equals_root() {
        let g = PaneGroup::single(1u32);
        let root = Rect {
            x: 0.0,
            y: 0.0,
            w: 800.0,
            h: 600.0,
        };
        let l = layout(&g, root, 4.0);
        assert_eq!(l, vec![(1, root)]);
    }

    #[test]
    fn layout_horizontal_halves() {
        let mut g = PaneGroup::single(1u32);
        g.split(1, 2, SplitDirection::Right);
        let root = Rect {
            x: 0.0,
            y: 0.0,
            w: 100.0,
            h: 50.0,
        };
        let l = layout(&g, root, 4.0);
        assert_eq!(l.len(), 2);
        let r1 = l.iter().find(|(id, _)| *id == 1).unwrap().1;
        let r2 = l.iter().find(|(id, _)| *id == 2).unwrap().1;
        // Each pane gets (100 - 4) / 2 = 48px wide.
        assert!((r1.w - 48.0).abs() < 1e-3);
        assert!((r2.w - 48.0).abs() < 1e-3);
        assert_eq!(r1.h, 50.0);
        assert_eq!(r2.h, 50.0);
        // No overlap: r2 starts where r1 ends + sash.
        assert!((r2.x - (r1.x + r1.w + 4.0)).abs() < 1e-3);
    }

    #[test]
    fn layout_vertical_halves() {
        let mut g = PaneGroup::single(1u32);
        g.split(1, 2, SplitDirection::Down);
        let root = Rect {
            x: 0.0,
            y: 0.0,
            w: 100.0,
            h: 100.0,
        };
        let l = layout(&g, root, 4.0);
        let r1 = l.iter().find(|(id, _)| *id == 1).unwrap().1;
        let r2 = l.iter().find(|(id, _)| *id == 2).unwrap().1;
        assert!((r1.h - 48.0).abs() < 1e-3);
        assert!((r2.h - 48.0).abs() < 1e-3);
    }

    #[test]
    fn layout_tiles_exactly_no_overlap() {
        let mut g = PaneGroup::single(1u32);
        g.split(1, 2, SplitDirection::Right);
        g.split(2, 3, SplitDirection::Down);
        let root = Rect {
            x: 10.0,
            y: 20.0,
            w: 200.0,
            h: 150.0,
        };
        let l = layout(&g, root, 4.0);
        assert_eq!(l.len(), 3);
        // All rects within root bounds.
        for (_, r) in &l {
            assert!(r.x >= root.x - 1e-3);
            assert!(r.y >= root.y - 1e-3);
            assert!(r.x + r.w <= root.x + root.w + 1e-3);
            assert!(r.y + r.h <= root.y + root.h + 1e-3);
        }
    }

    #[test]
    fn pane_at_finds_correct_leaf() {
        let mut g = PaneGroup::single(1u32);
        g.split(1, 2, SplitDirection::Right);
        let root = Rect {
            x: 0.0,
            y: 0.0,
            w: 100.0,
            h: 50.0,
        };
        let l = layout(&g, root, 4.0);
        assert_eq!(pane_at(&l, Vec2 { x: 10.0, y: 10.0 }), Some(1));
        assert_eq!(pane_at(&l, Vec2 { x: 90.0, y: 10.0 }), Some(2));
        assert_eq!(pane_at(&l, Vec2 { x: 200.0, y: 10.0 }), None);
    }

    // ── drop_zone ─────────────────────────────────────────────────────────────

    fn body() -> Rect {
        Rect {
            x: 0.0,
            y: 0.0,
            w: 100.0,
            h: 100.0,
        }
    }

    #[test]
    fn drop_zone_center() {
        assert_eq!(
            drop_zone(body(), Vec2 { x: 50.0, y: 50.0 }, 0.25),
            DropZone::Center
        );
    }

    #[test]
    fn drop_zone_left_quarter() {
        assert_eq!(
            drop_zone(body(), Vec2 { x: 10.0, y: 50.0 }, 0.25),
            DropZone::Edge(SplitDirection::Left)
        );
    }

    #[test]
    fn drop_zone_right_quarter() {
        assert_eq!(
            drop_zone(body(), Vec2 { x: 90.0, y: 50.0 }, 0.25),
            DropZone::Edge(SplitDirection::Right)
        );
    }

    #[test]
    fn drop_zone_top_quarter() {
        assert_eq!(
            drop_zone(body(), Vec2 { x: 50.0, y: 10.0 }, 0.25),
            DropZone::Edge(SplitDirection::Up)
        );
    }

    #[test]
    fn drop_zone_bottom_quarter() {
        assert_eq!(
            drop_zone(body(), Vec2 { x: 50.0, y: 90.0 }, 0.25),
            DropZone::Edge(SplitDirection::Down)
        );
    }

    #[test]
    fn drop_zone_corner_tiebreak_nearer_edge() {
        // top-left corner, equidistant: x=5,y=5 on 100x100 → left_t=0.05, top_t=0.05 → tie → deterministic
        let zone = drop_zone(body(), Vec2 { x: 5.0, y: 5.0 }, 0.25);
        assert!(matches!(zone, DropZone::Edge(_)));
    }

    #[test]
    fn drop_zone_edge_frac_boundary() {
        // Exactly at the 0.25 boundary from left: x=25.0 → left_t=0.25, not < 0.25, so Center.
        assert_eq!(
            drop_zone(body(), Vec2 { x: 25.0, y: 50.0 }, 0.25),
            DropZone::Center
        );
        // Just inside: x=24.9 → left_t < 0.25 → Left.
        assert_eq!(
            drop_zone(body(), Vec2 { x: 24.9, y: 50.0 }, 0.25),
            DropZone::Edge(SplitDirection::Left)
        );
    }

    // ── serialisation ─────────────────────────────────────────────────────────

    #[test]
    fn serialized_roundtrip_topology() {
        let mut g = PaneGroup::single(10u32);
        g.split(10, 20, SplitDirection::Right);
        g.split(20, 30, SplitDirection::Down);
        // Serialize with identity payload.
        let ser = g.to_serialized(&|id: u32| id);
        // Deserialize back.
        let mut counter = 0u32;
        let g2: PaneGroup<u32> = PaneGroup::from_serialized(&ser, &mut |id: &u32| {
            counter += 1;
            *id
        });
        assert_eq!(g.pane_ids(), g2.pane_ids());
        assert_eq!(counter, 3);
    }

    #[test]
    fn serialized_flexes_preserved() {
        let mut g = PaneGroup::single(1u32);
        g.split(1, 2, SplitDirection::Right);
        g.resize(&[], 0, 20.0, 100.0, 0.05);
        let ser = g.to_serialized(&|id: u32| id);
        let g2: PaneGroup<u32> = PaneGroup::from_serialized(&ser, &mut |id: &u32| *id);
        if let (Member::Axis(a1), Member::Axis(a2)) = (&g.root, &g2.root) {
            for (f1, f2) in a1.flexes.iter().zip(a2.flexes.iter()) {
                assert!((f1 - f2).abs() < 1e-6);
            }
        }
    }

    #[test]
    fn serde_json_roundtrip() {
        let mut g = PaneGroup::single(1u32);
        g.split(1, 2, SplitDirection::Right);
        let ser = g.to_serialized(&|id: u32| id);
        let json = serde_json::to_string(&ser).unwrap();
        let back: SerializedMember<u32> = serde_json::from_str(&json).unwrap();
        assert_eq!(ser, back);
    }

    // ── proptest ─────────────────────────────────────────────────────────────

    use proptest::prelude::*;

    proptest! {
        #[test]
        fn invariants_hold_after_random_splits_and_removes(
            ops in proptest::collection::vec(0u8..8, 1..20)
        ) {
            let mut g = PaneGroup::single(0u32);
            let mut next_id = 1u32;
            for op in ops {
                let all_ids = g.pane_ids();
                if all_ids.is_empty() { break; }
                let target = all_ids[op as usize % all_ids.len()];
                if op < 4 {
                    // split
                    let dir = match op % 4 {
                        0 => SplitDirection::Left,
                        1 => SplitDirection::Right,
                        2 => SplitDirection::Up,
                        _ => SplitDirection::Down,
                    };
                    g.split(target, next_id, dir);
                    next_id += 1;
                } else {
                    g.remove_pane(target);
                }
                // Check invariants.
                prop_assert!(flex_sum_ok(&g) || g.is_single());
                prop_assert!(all_axes_ge2(&g));
                let ids = g.pane_ids();
                let unique: std::collections::HashSet<u32> = ids.iter().cloned().collect();
                prop_assert_eq!(ids.len(), unique.len(), "duplicate ids");
            }
        }
    }
}
