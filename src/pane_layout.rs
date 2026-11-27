use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const DIVIDER: f32 = 6.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Axis {
    Across,
    Down,
}

impl Axis {
    pub fn extent(self, width: f32, height: f32) -> f32 {
        match self {
            Self::Across => width,
            Self::Down => height,
        }
    }

    pub fn can_split(self, width: f32, height: f32) -> bool {
        self.extent(width, height)
            >= match self {
                Self::Across => 240.0,
                Self::Down => 164.0,
            }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PaneLayout {
    Leaf(usize),
    Split {
        axis: Axis,
        ratio: f32,
        first: Box<Self>,
        second: Box<Self>,
    },
}

impl PaneLayout {
    pub fn pair(axis: Axis, ratio: f32, first: Self, second: Self) -> Self {
        Self::Split {
            axis,
            ratio: ratio.clamp(0.2, 0.8),
            first: Box::new(first),
            second: Box::new(second),
        }
    }

    pub fn first_pane(&self) -> usize {
        match self {
            Self::Leaf(index) => *index,
            Self::Split { first, .. } => first.first_pane(),
        }
    }

    pub fn has_split_titlebar(&self) -> bool {
        match self {
            Self::Leaf(_) => false,
            Self::Split { axis, first, .. } => *axis == Axis::Across || first.has_split_titlebar(),
        }
    }

    pub fn leaves(&self, panes: &mut Vec<usize>) {
        match self {
            Self::Leaf(index) => panes.push(*index),
            Self::Split { first, second, .. } => {
                first.leaves(panes);
                second.leaves(panes);
            }
        }
    }

    pub fn titlebar_groups(&self, files_left: f32, files_width: f32, strip_left: f32, strip_width: f32) -> Vec<(usize, f32)> {
        let mut starts = Vec::new();
        self.collect_titlebar_starts(files_left, files_width, &mut starts);
        let count = starts.len();
        let minimum = (strip_width / count as f32).min(48.);
        let strip_right = strip_left + strip_width;
        let mut left = strip_left;
        starts.iter().enumerate().map(|(index, &(pane, _))| {
            let right = starts.get(index + 1).map_or(strip_right, |&(_, next)| {
                let last_possible = strip_right - minimum * (count - index - 1) as f32;
                next.clamp((left + minimum).min(last_possible), last_possible)
            });
            let width = (right - left).max(0.);
            left = right;
            (pane, width)
        }).collect()
    }

    fn collect_titlebar_starts(&self, left: f32, width: f32, starts: &mut Vec<(usize, f32)>) {
        match self {
            Self::Leaf(pane) => starts.push((*pane, left)),
            Self::Split { axis: Axis::Across, ratio, first, second } => {
                let (before, after) = split_extents(width, *ratio);
                let divider = DIVIDER.min(width);
                first.collect_titlebar_starts(left, before, starts);
                let second_index = starts.len();
                second.collect_titlebar_starts(left + before + divider, after, starts);
                starts[second_index].1 = left + before + divider / 2.;
            }
            Self::Split { axis: Axis::Down, first, .. } => {
                first.collect_titlebar_starts(left, width, starts);
            }
        }
    }

    pub fn valid_for(&self, pane_count: usize) -> bool {
        let mut panes = Vec::new();
        self.leaves(&mut panes);
        panes.sort_unstable();
        panes.into_iter().eq(0..pane_count)
    }

    pub fn split(&mut self, target: usize, added: usize, axis: Axis, before: bool) -> bool {
        match self {
            Self::Leaf(index) if *index == target => {
                let (first, second) = if before {
                    (added, target)
                } else {
                    (target, added)
                };
                *self = Self::pair(axis, 0.5, Self::Leaf(first), Self::Leaf(second));
                true
            }
            Self::Split { first, second, .. } => {
                first.split(target, added, axis, before)
                    || second.split(target, added, axis, before)
            }
            Self::Leaf(_) => false,
        }
    }

    pub fn without(self, removed: usize) -> Option<Self> {
        match self {
            Self::Leaf(index) if index == removed => None,
            Self::Leaf(index) => Some(Self::Leaf(index - usize::from(index > removed))),
            Self::Split {
                axis,
                ratio,
                first,
                second,
            } => match (first.without(removed), second.without(removed)) {
                (Some(first), Some(second)) => Some(Self::pair(axis, ratio, first, second)),
                (Some(node), None) | (None, Some(node)) => Some(node),
                (None, None) => None,
            },
        }
    }

    pub fn sibling(&self, pane: usize) -> Option<usize> {
        let Self::Split { first, second, .. } = self else {
            return None;
        };
        match (&**first, &**second) {
            (Self::Leaf(index), other) | (other, Self::Leaf(index)) if *index == pane => {
                Some(other.first_pane())
            }
            _ => first.sibling(pane).or_else(|| second.sibling(pane)),
        }
    }

    pub fn branch_mut(&mut self, path: &[bool]) -> Option<&mut Self> {
        let Some((&second_child, rest)) = path.split_first() else {
            return Some(self);
        };
        match self {
            Self::Split { first, second, .. } => {
                if second_child {
                    second.branch_mut(rest)
                } else {
                    first.branch_mut(rest)
                }
            }
            Self::Leaf(_) => None,
        }
    }

    pub fn pane_width(&self, pane: usize, width: f32) -> Option<f32> {
        match self {
            Self::Leaf(index) => (*index == pane).then_some(width),
            Self::Split {
                axis: Axis::Across,
                ratio,
                first,
                second,
            } => {
                let (left, right) = split_extents(width, *ratio);
                first
                    .pane_width(pane, left)
                    .or_else(|| second.pane_width(pane, right))
            }
            Self::Split { first, second, .. } => first
                .pane_width(pane, width)
                .or_else(|| second.pane_width(pane, width)),
        }
    }
}

pub fn split_extents(total: f32, ratio: f32) -> (f32, f32) {
    let available = (total - DIVIDER).max(0.0);
    let first = available * ratio.clamp(0.2, 0.8);
    (first, available - first)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SavedPane {
    pub paths: Vec<PathBuf>,
    pub active: usize,
    #[serde(default)]
    pub views: Vec<SavedTabView>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SavedTabView {
    pub view: crate::settings::ViewMode,
    pub icon_scale: f32,
    pub compact: bool,
    pub sort: crate::fs_model::SortKey,
    pub sort_desc: bool,
    #[serde(deserialize_with = "crate::settings::load_detail_widths")]
    pub detail_widths: [f32; crate::settings::DETAIL_COLUMNS],
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PaneSession {
    pub layout: PaneLayout,
    pub panes: Vec<SavedPane>,
    pub active: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titlebar_dividers_follow_panes_and_keep_tabs_clear_of_window_controls() {
        let layout = PaneLayout::pair(Axis::Across, 0.6, PaneLayout::Leaf(0), PaneLayout::Leaf(1));
        assert_eq!(layout.titlebar_groups(200., 606., 70., 800.), [(0, 493.), (1, 307.)]);
        assert_eq!(layout.titlebar_groups(200., 606., 70., 250.), [(0, 202.), (1, 48.)]);
    }

    #[test]
    fn titlebar_groups_only_include_panes_on_the_top_edge() {
        let mut layout = PaneLayout::pair(Axis::Across, 0.6, PaneLayout::Leaf(0), PaneLayout::Leaf(1));
        assert!(layout.split(1, 2, Axis::Down, false));
        assert!(layout.split(2, 3, Axis::Across, false));
        assert!(layout.has_split_titlebar());
        assert_eq!(layout.titlebar_groups(200., 606., 70., 800.), [(0, 493.), (1, 307.)]);
    }

    #[test]
    fn nested_splits_keep_siblings_and_collapse_only_the_removed_branch() {
        let mut layout = PaneLayout::Leaf(0);
        assert!(layout.split(0, 1, Axis::Down, false));
        assert!(!layout.has_split_titlebar());
        assert!(layout.split(1, 2, Axis::Across, true));
        assert!(!layout.has_split_titlebar());
        assert!(layout.split(2, 3, Axis::Down, true));
        assert!(layout.valid_for(4));
        assert_eq!(layout.sibling(3), Some(2));
        assert_eq!(layout.pane_width(0, 506.0), Some(506.0));
        assert_eq!(layout.pane_width(3, 506.0), Some(250.0));

        let layout = layout.without(2).unwrap();
        assert!(layout.valid_for(3));
        let mut order = Vec::new();
        layout.leaves(&mut order);
        assert_eq!(order, [0, 2, 1]);
        let layout = layout.without(0).unwrap();
        assert_eq!(layout.first_pane(), 1);
        assert_eq!(layout.pane_width(1, 506.0), Some(250.0));
        assert!(!layout.without(1).unwrap().has_split_titlebar());
    }

    #[test]
    fn saved_layout_round_trips_and_rejects_duplicate_leaves() {
        let layout = PaneLayout::pair(Axis::Down, 0.58, PaneLayout::Leaf(1), PaneLayout::Leaf(0));
        let encoded = serde_json::to_string(&layout).unwrap();
        let restored: PaneLayout = serde_json::from_str(&encoded).unwrap();
        assert!(restored.valid_for(2));
        assert_eq!(restored.first_pane(), 1);
        assert!(
            !PaneLayout::pair(Axis::Across, 0.5, PaneLayout::Leaf(0), PaneLayout::Leaf(0))
                .valid_for(2)
        );
    }
}
