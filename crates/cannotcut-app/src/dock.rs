//! The panels docked in a wide window, and how they are arranged: each one is taken up by its
//! title bar (its tabs, its title, its tools) and dropped on another, beside it or in its
//! place. A panel can also leave for a window of its own, and comes back where it was. The
//! arrangement is kept in the settings, as nested splits.

use iced::widget::pane_grid::{self, Axis, Configuration, Node};
use serde_json::{Value, json};

/// The deepest an arrangement read from the settings may nest; four panels need three.
const MOST_DEPTH: usize = 8;
/// The share of the window a panel takes when it comes back and what was beside it is gone.
const EDGE_SHARE: f32 = 0.25;

/// A panel that can be docked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Part {
    Media,
    Viewer,
    Details,
    Timeline,
}

impl Part {
    const ALL: [Part; 4] = [Self::Media, Self::Viewer, Self::Details, Self::Timeline];

    pub(crate) fn key(self) -> &'static str {
        match self {
            Self::Media => "media",
            Self::Viewer => "viewer",
            Self::Details => "details",
            Self::Timeline => "timeline",
        }
    }

    pub(crate) fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|part| part.key() == key)
    }

    /// What the panel is called, on its own window.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Media => "Media",
            Self::Viewer => "Viewer",
            Self::Details => "Details",
            Self::Timeline => "Timeline",
        }
    }
}

/// An arrangement of panels: one panel, or a region split in two.
#[derive(Clone, Debug)]
enum Tree {
    Part(Part),
    Split {
        axis: Axis,
        ratio: f32,
        a: Box<Tree>,
        b: Box<Tree>,
    },
}

impl Tree {
    /// The arrangement of `dock`.
    fn of(dock: &pane_grid::State<Part>) -> Self {
        fn walk(node: &Node, dock: &pane_grid::State<Part>) -> Tree {
            match node {
                Node::Split {
                    axis, ratio, a, b, ..
                } => Tree::Split {
                    axis: *axis,
                    ratio: *ratio,
                    a: Box::new(walk(a, dock)),
                    b: Box::new(walk(b, dock)),
                },
                // Every pane in the dock holds a part.
                Node::Pane(pane) => Tree::Part(dock.get(*pane).copied().unwrap_or(Part::Viewer)),
            }
        }
        walk(dock.layout(), dock)
    }

    fn dock(self) -> pane_grid::State<Part> {
        fn configuration(tree: Tree) -> Configuration<Part> {
            match tree {
                Tree::Part(part) => Configuration::Pane(part),
                Tree::Split { axis, ratio, a, b } => Configuration::Split {
                    axis,
                    ratio,
                    a: Box::new(configuration(*a)),
                    b: Box::new(configuration(*b)),
                },
            }
        }
        pane_grid::State::with_configuration(configuration(self))
    }

    fn parts(&self, parts: &mut Vec<Part>) {
        match self {
            Tree::Part(part) => parts.push(*part),
            Tree::Split { a, b, .. } => {
                a.parts(parts);
                b.parts(parts);
            }
        }
    }

    /// Whether this holds exactly `parts`, in any order.
    fn holds_exactly(&self, parts: &[Part]) -> bool {
        let mut here = Vec::new();
        self.parts(&mut here);
        here.len() == parts.len() && parts.iter().all(|part| here.contains(part))
    }

    fn value(&self) -> Value {
        match self {
            Tree::Part(part) => json!(part.key()),
            Tree::Split { axis, ratio, a, b } => json!({
                // Rows one over another, or columns side by side.
                "split": if *axis == Axis::Horizontal { "rows" } else { "columns" },
                "ratio": (f64::from(*ratio) * 1000.0).round() / 1000.0,
                "a": a.value(),
                "b": b.value(),
            }),
        }
    }

    /// Takes `part` out, its neighbour taking the room; where it was, unless it is all there is.
    fn remove(&mut self, part: Part) -> Option<Back> {
        let Tree::Split { axis, ratio, a, b } = self else {
            return None;
        };
        let (axis, ratio) = (*axis, *ratio);
        let is_part = |tree: &Tree| matches!(tree, Tree::Part(found) if *found == part);
        let left = if is_part(a) {
            Some((true, (**b).clone()))
        } else if is_part(b) {
            Some((false, (**a).clone()))
        } else {
            None
        };
        let Some((first, other)) = left else {
            return a.remove(part).or_else(|| b.remove(part));
        };
        let mut beside = Vec::new();
        other.parts(&mut beside);
        *self = other;
        Some(Back {
            beside,
            axis,
            ratio,
            first,
        })
    }

    /// Puts `part` back beside what was beside it, if that is still together somewhere.
    fn insert(&mut self, part: Part, back: &Back) -> bool {
        if self.holds_exactly(&back.beside) {
            let beside = Box::new(self.clone());
            let part = Box::new(Tree::Part(part));
            let (a, b) = if back.first { (part, beside) } else { (beside, part) };
            *self = Tree::Split {
                axis: back.axis,
                ratio: back.ratio,
                a,
                b,
            };
            return true;
        }
        match self {
            Tree::Part(_) => false,
            Tree::Split { a, b, .. } => a.insert(part, back) || b.insert(part, back),
        }
    }

    /// Puts `part` back where it was, or else along that side of everything.
    fn put_back(&mut self, part: Part, back: &Back) {
        if self.insert(part, back) {
            return;
        }
        let rest = Box::new(self.clone());
        let part = Box::new(Tree::Part(part));
        *self = if back.first {
            Tree::Split {
                axis: back.axis,
                ratio: EDGE_SHARE,
                a: part,
                b: rest,
            }
        } else {
            Tree::Split {
                axis: back.axis,
                ratio: 1.0 - EDGE_SHARE,
                a: rest,
                b: part,
            }
        };
    }
}

/// Where a panel that left the dock was: the panels beside it, and on which side of them.
#[derive(Clone, Debug)]
pub(crate) struct Back {
    beside: Vec<Part>,
    axis: Axis,
    ratio: f32,
    /// Whether it was left of them, or over them.
    first: bool,
}

/// Takes `part` out of `dock`, for a window of its own; where it was, unless it is the only
/// panel left there.
pub(crate) fn take_out(dock: &mut pane_grid::State<Part>, part: Part) -> Option<Back> {
    let mut tree = Tree::of(dock);
    let back = tree.remove(part)?;
    *dock = tree.dock();
    Some(back)
}

/// Puts `part` back in `dock` where `back` says it was.
pub(crate) fn put_back(dock: &mut pane_grid::State<Part>, part: Part, back: &Back) {
    let mut tree = Tree::of(dock);
    tree.put_back(part, back);
    *dock = tree.dock();
}

/// The panels as they start: media, viewer and details side by side over the timeline.
pub(crate) fn starting() -> pane_grid::State<Part> {
    let pane = |part| Box::new(Configuration::Pane(part));
    pane_grid::State::with_configuration(Configuration::Split {
        axis: Axis::Horizontal,
        ratio: 0.58,
        a: Box::new(Configuration::Split {
            axis: Axis::Vertical,
            ratio: 0.25,
            a: pane(Part::Media),
            b: Box::new(Configuration::Split {
                axis: Axis::Vertical,
                ratio: 0.65,
                a: pane(Part::Viewer),
                b: pane(Part::Details),
            }),
        }),
        b: pane(Part::Timeline),
    })
}

/// The arrangement, to keep in the settings, with the panels in windows of their own put back
/// where they came from.
pub(crate) fn to_value<'a>(
    dock: &pane_grid::State<Part>,
    away: impl IntoIterator<Item = (Part, &'a Back)>,
) -> Value {
    let mut tree = Tree::of(dock);
    for (part, back) in away {
        tree.put_back(part, back);
    }
    tree.value()
}

/// An arrangement kept in the settings, if it holds every panel once.
pub(crate) fn from_value(value: &Value) -> Option<pane_grid::State<Part>> {
    let mut parts = Vec::new();
    let tree = tree_of_value(value, &mut parts, 0)?;
    let each_once = parts.len() == Part::ALL.len()
        && Part::ALL.iter().all(|part| parts.contains(part));
    each_once.then(|| tree.dock())
}

fn tree_of_value(value: &Value, parts: &mut Vec<Part>, depth: usize) -> Option<Tree> {
    if depth > MOST_DEPTH {
        return None;
    }
    if let Some(key) = value.as_str() {
        let part = Part::from_key(key)?;
        parts.push(part);
        return Some(Tree::Part(part));
    }
    let axis = match value.get("split")?.as_str()? {
        "rows" => Axis::Horizontal,
        "columns" => Axis::Vertical,
        _ => return None,
    };
    let ratio = value.get("ratio")?.as_f64()? as f32;
    if !(ratio > 0.0 && ratio < 1.0) {
        return None;
    }
    Some(Tree::Split {
        axis,
        ratio,
        a: Box::new(tree_of_value(value.get("a")?, parts, depth + 1)?),
        b: Box::new(tree_of_value(value.get("b")?, parts, depth + 1)?),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kept(dock: &pane_grid::State<Part>) -> Value {
        to_value(dock, [])
    }

    #[test]
    fn an_arrangement_comes_back_as_it_was_kept() {
        let kept_value = kept(&starting());
        let back = from_value(&kept_value).expect("the starting arrangement is whole");
        assert_eq!(kept(&back), kept_value);
        assert_eq!(kept_value["split"], "rows");
        assert_eq!(kept_value["b"], "timeline");
    }

    #[test]
    fn an_arrangement_missing_a_panel_or_holding_one_twice_is_refused() {
        let missing = json!({"split": "rows", "ratio": 0.5, "a": "media", "b": "timeline"});
        assert!(from_value(&missing).is_none());
        let twice = json!({
            "split": "rows", "ratio": 0.5,
            "a": {"split": "columns", "ratio": 0.5, "a": "media", "b": "media"},
            "b": {"split": "columns", "ratio": 0.5, "a": "viewer", "b": "timeline"},
        });
        assert!(from_value(&twice).is_none());
    }

    #[test]
    fn an_arrangement_with_a_ratio_out_of_range_is_refused() {
        let mut kept_value = kept(&starting());
        kept_value["ratio"] = json!(1.5);
        assert!(from_value(&kept_value).is_none());
    }

    #[test]
    fn a_panel_taken_out_comes_back_where_it_was() {
        let starting_value = kept(&starting());
        for part in [Part::Media, Part::Viewer, Part::Details, Part::Timeline] {
            let mut dock = starting();
            let back = take_out(&mut dock, part).expect("there are other panels");
            assert_eq!(dock.len(), 3);
            assert!(dock.iter().all(|(_, here)| *here != part));
            // Kept as if it were there, while it is away.
            assert_eq!(to_value(&dock, [(part, &back)]), starting_value);
            put_back(&mut dock, part, &back);
            assert_eq!(kept(&dock), starting_value, "{part:?}");
        }
    }

    #[test]
    fn a_panel_comes_back_along_the_side_when_its_neighbours_were_parted() {
        let mut dock = starting();
        // The media panel had the viewer and the details beside it.
        let back = take_out(&mut dock, Part::Media).expect("there are other panels");
        let details = take_out(&mut dock, Part::Details).expect("there are other panels");
        put_back(&mut dock, Part::Media, &back);
        let value = kept(&dock);
        assert_eq!(value["split"], "columns");
        assert_eq!(value["a"], "media");
        put_back(&mut dock, Part::Details, &details);
        assert_eq!(dock.len(), 4);
    }

    #[test]
    fn the_last_panel_cannot_be_taken_out() {
        let mut dock = from_value(&json!({
            "split": "rows", "ratio": 0.5,
            "a": {"split": "columns", "ratio": 0.5, "a": "media", "b": "viewer"},
            "b": {"split": "columns", "ratio": 0.5, "a": "details", "b": "timeline"},
        }))
        .expect("whole");
        for part in [Part::Media, Part::Viewer, Part::Details] {
            assert!(take_out(&mut dock, part).is_some());
        }
        assert!(take_out(&mut dock, Part::Timeline).is_none());
    }
}
