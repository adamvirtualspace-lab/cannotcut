//! The panels docked in a wide window, and how they are arranged: each one is taken up by its
//! title bar (its tabs, its title, its tools) and dropped on another, beside it or in its
//! place. The arrangement is kept in the settings, as nested splits.

use iced::widget::pane_grid::{self, Axis, Configuration, Node};
use serde_json::{Value, json};

/// The deepest an arrangement read from the settings may nest; four panels need three.
const MOST_DEPTH: usize = 8;

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

    fn key(self) -> &'static str {
        match self {
            Self::Media => "media",
            Self::Viewer => "viewer",
            Self::Details => "details",
            Self::Timeline => "timeline",
        }
    }

    fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|part| part.key() == key)
    }
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

/// The arrangement, to keep in the settings.
pub(crate) fn to_value(dock: &pane_grid::State<Part>) -> Value {
    node_value(dock.layout(), dock)
}

fn node_value(node: &Node, dock: &pane_grid::State<Part>) -> Value {
    match node {
        Node::Split {
            axis, ratio, a, b, ..
        } => json!({
            // Rows one over another, or columns side by side.
            "split": if *axis == Axis::Horizontal { "rows" } else { "columns" },
            "ratio": (f64::from(*ratio) * 1000.0).round() / 1000.0,
            "a": node_value(a, dock),
            "b": node_value(b, dock),
        }),
        Node::Pane(pane) => json!(dock.get(*pane).map_or("", |part| part.key())),
    }
}

/// An arrangement kept in the settings, if it holds every panel once.
pub(crate) fn from_value(value: &Value) -> Option<pane_grid::State<Part>> {
    let mut parts = Vec::new();
    let configuration = configuration(value, &mut parts, 0)?;
    let each_once = parts.len() == Part::ALL.len()
        && Part::ALL.iter().all(|part| parts.contains(part));
    each_once.then(|| pane_grid::State::with_configuration(configuration))
}

fn configuration(
    value: &Value,
    parts: &mut Vec<Part>,
    depth: usize,
) -> Option<Configuration<Part>> {
    if depth > MOST_DEPTH {
        return None;
    }
    if let Some(key) = value.as_str() {
        let part = Part::from_key(key)?;
        parts.push(part);
        return Some(Configuration::Pane(part));
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
    Some(Configuration::Split {
        axis,
        ratio,
        a: Box::new(configuration(value.get("a")?, parts, depth + 1)?),
        b: Box::new(configuration(value.get("b")?, parts, depth + 1)?),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_arrangement_comes_back_as_it_was_kept() {
        let kept = to_value(&starting());
        let back = from_value(&kept).expect("the starting arrangement is whole");
        assert_eq!(to_value(&back), kept);
        assert_eq!(kept["split"], "rows");
        assert_eq!(kept["b"], "timeline");
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
        let mut kept = to_value(&starting());
        kept["ratio"] = json!(1.5);
        assert!(from_value(&kept).is_none());
    }
}
