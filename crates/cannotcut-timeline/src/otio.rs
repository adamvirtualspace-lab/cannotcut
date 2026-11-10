//! Reading and writing timelines as OpenTimelineIO JSON.
//!
//! The file is the standard format, so other tools can open it. What cannotcut cannot edit yet
//! (transitions, generated media) is refused by name when reading, instead of being dropped
//! quietly. A linked project used inside another is a clip whose media is the other `.otio`
//! file; one kept inside it (a compound clip) is a stack of tracks in place of a clip, as
//! OpenTimelineIO nests timelines. See the `nest` module. Metadata, effects and markers it does
//! not understand are carried through unchanged.

use std::fs;
use std::path::Path;
use std::sync::Arc;

use serde_json::{Map, Value, json};

use crate::Error;
use crate::model::{
    Clip, Extras, FrameRange, Item, Media, Rate, SPEEDS, Timeline, Track, TrackKind, new_id,
};
use crate::transform::{Transform, keyframes_from_value, keyframes_value};
use crate::title::Title;
use crate::transition::{Transition, TransitionKind};

/// The metadata key under which cannotcut keeps what the format has no field for.
const OWN_METADATA: &str = "cannotcut";
/// The `generator_kind` of a text clip's media.
const TITLE_GENERATOR: &str = "cannotcut.text";
/// The name OpenTimelineIO gives a clip's media reference when there is only one.
const DEFAULT_MEDIA: &str = "DEFAULT_MEDIA";
/// The rate of a timeline whose file gives no hint of one.
const FALLBACK_RATE: Rate = Rate::new(30, 1);

type Object = Map<String, Value>;

impl Timeline {
    /// Writes the file under another name first and swaps it in, so a crash while saving cannot
    /// leave half a project behind.
    pub fn save(&self, path: &Path) -> Result<(), Error> {
        let mut partial = path.as_os_str().to_owned();
        partial.push(".saving");
        let partial = Path::new(&partial);
        fs::write(partial, self.to_otio()).map_err(|error| Error::new("write project", error))?;
        fs::rename(partial, path).map_err(|error| Error::new("write project", error))
    }

    pub fn to_otio(&self) -> String {
        let mut metadata = self.metadata.clone();
        metadata.insert(OWN_METADATA.to_string(), json!({ "rate": rate_json(self.rate) }));
        let tracks: Vec<Value> = self
            .tracks()
            .enumerate()
            .map(|(index, track)| write_track(track, index, self.rate))
            .collect();
        let timeline = json!({
            "OTIO_SCHEMA": "Timeline.1",
            "metadata": metadata,
            "name": self.name,
            "global_start_time": time(self.start, self.rate),
            "tracks": {
                "OTIO_SCHEMA": "Stack.1",
                "children": tracks,
                "effects": self.stack.effects,
                "enabled": true,
                "markers": self.stack.markers,
                "metadata": self.stack.metadata,
                "name": "tracks",
                "source_range": null,
            },
        });
        let mut text = serde_json::to_string_pretty(&timeline)
            .expect("a JSON value with text keys always serializes");
        text.push('\n');
        text
    }

    pub fn from_otio(text: &str) -> Result<Self, Error> {
        // Some Windows tools start a UTF-8 file with a byte-order mark, which JSON does not
        // allow; it says nothing, so it goes.
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);
        let root: Value =
            serde_json::from_str(text).map_err(|error| Error::new("read project", error))?;
        let root = object(&root, "the project file")?;
        if schema_name(root) != "Timeline" {
            return Err(Error(format!(
                "the file holds a {}, not a timeline",
                schema(root)
            )));
        }
        let mut metadata = child_object(root, "metadata");
        let own = metadata.remove(OWN_METADATA);
        let stack = match root.get("tracks") {
            Some(stack) => object(stack, "the timeline's tracks")?,
            None => return Err(Error("the timeline has no tracks".to_string())),
        };
        refuse_source_range(stack, "the timeline's stack")?;

        let global_start = read_time(root.get("global_start_time"))?;
        let rate = own
            .as_ref()
            .and_then(read_own_rate)
            .or_else(|| global_start.and_then(|(_, rate)| Rate::from_f64(rate)))
            .or_else(|| first_rate(stack))
            .unwrap_or(FALLBACK_RATE);

        let mut timeline = Timeline {
            name: text_of(root, "name"),
            metadata,
            rate,
            start: global_start.map_or(0, |(value, from)| frames(value, from, rate)),
            tracks: Vec::new(),
            stack: extras(stack),
            next_id: 0,
        };
        for (index, child) in children(stack).enumerate() {
            let child = object(child, "a track")?;
            if schema_name(child) != "Track" {
                return Err(unsupported(schema(child), "in place of a track"));
            }
            let track = read_track(child, index, rate, &mut timeline.next_id)?;
            timeline.tracks.push(Arc::new(track));
        }
        Ok(timeline)
    }
}

/// The track at `index` of its stack, for the file. Whether it is magnetic is written down
/// only where it differs from what a file without it means: the main track (the first)
/// magnetic, the others not.
fn write_track(track: &Track, index: usize, rate: Rate) -> Value {
    let children: Vec<Value> = track
        .items()
        .iter()
        .flat_map(|item| match item {
            Item::Clip(clip) => {
                let inside = clip.media.project.as_ref().filter(|_| clip.media.is_inside());
                let written = match inside {
                    Some(nested) => write_inside(clip, &nested.timeline, rate),
                    None => write_clip(clip, rate),
                };
                // A transition stands between the clips it joins.
                let way_in = clip.transition_in.map(|way| write_transition(&way, rate));
                way_in.into_iter().chain([written]).collect::<Vec<_>>()
            }
            Item::Gap(duration) => vec![json!({
                "OTIO_SCHEMA": "Gap.1",
                "effects": [],
                "enabled": true,
                "markers": [],
                "metadata": {},
                "name": "",
                "source_range": range(0, *duration, rate),
            })],
        })
        .collect();
    let mut metadata = track.extras.metadata.clone();
    let mut own = serde_json::Map::new();
    if track.muted {
        own.insert("muted".to_string(), json!(true));
    }
    if track.magnetic != (index == 0) {
        own.insert("magnetic".to_string(), json!(track.magnetic));
    }
    if !own.is_empty() {
        metadata.insert(OWN_METADATA.to_string(), Value::Object(own));
    }
    json!({
        "OTIO_SCHEMA": "Track.1",
        "children": children,
        "effects": track.extras.effects,
        "enabled": track.enabled,
        "kind": match track.kind {
            TrackKind::Video => "Video",
            TrackKind::Audio => "Audio",
        },
        "markers": track.extras.markers,
        "metadata": metadata,
        "name": track.name,
        "source_range": null,
    })
}

fn write_clip(clip: &Clip, rate: Rate) -> Value {
    let media = &*clip.media;
    let mut metadata = media.metadata.clone();
    if media.available.is_none() {
        // Without an available range nothing else in the file says how fast the media runs.
        metadata.insert(OWN_METADATA.to_string(), json!({ "rate": rate_json(media.rate) }));
    }
    let mut reference = json!({
        "OTIO_SCHEMA": "MissingReference.1",
        "available_image_bounds": null,
        "available_range": media
            .available
            .map(|available| range(available.start, available.duration, media.rate)),
        "metadata": metadata,
        "name": media.name,
    });
    if let Some(target) = &media.target {
        reference["OTIO_SCHEMA"] = json!("ExternalReference.1");
        reference["target_url"] = json!(target);
    } else if let Some(title) = &media.title {
        reference["OTIO_SCHEMA"] = json!("GeneratorReference.1");
        reference["generator_kind"] = json!(TITLE_GENERATOR);
        reference["parameters"] = title.value();
    }
    // The range is written at the timeline's rate, where its numbers are whole.
    let source_start = media_start(media, rate) + clip.source_start;
    let mut clip_metadata = clip.extras.metadata.clone();
    let mut own = serde_json::Map::new();
    if clip.volume != 1.0 {
        own.insert("volume".to_string(), json!(clip.volume));
    }
    if !clip.transform.is_identity() {
        own.insert("transform".to_string(), clip.transform.value());
    }
    if !clip.keyframes.is_empty() {
        own.insert("keyframes".to_string(), keyframes_value(&clip.keyframes));
    }
    if !own.is_empty() {
        clip_metadata.insert(OWN_METADATA.to_string(), Value::Object(own));
    }
    json!({
        "OTIO_SCHEMA": "Clip.2",
        "active_media_reference_key": DEFAULT_MEDIA,
        "effects": effects_with_speed(clip),
        "enabled": clip.enabled,
        "markers": clip.extras.markers,
        "media_references": { DEFAULT_MEDIA: reference },
        "metadata": clip_metadata,
        "name": clip.name,
        "source_range": range(source_start, clip.duration, rate),
    })
}

/// A clip's way in, as OpenTimelineIO has transitions: over the cut, here all of it after the
/// cut, where the clip before carries on beneath the clip coming in.
fn write_transition(transition: &Transition, rate: Rate) -> Value {
    json!({
        "OTIO_SCHEMA": "Transition.1",
        "name": transition.kind.name(),
        "metadata": { OWN_METADATA: { "kind": transition.kind.key() } },
        "transition_type": transition.kind.otio_type(),
        "in_offset": time(0, rate),
        "out_offset": time(transition.duration, rate),
    })
}

/// A transition from a file: ours by its kind; others' as a dissolve, as long as both its
/// offsets together.
fn read_transition(transition: &Object, rate: Rate) -> Result<Transition, Error> {
    let kind = transition
        .get("metadata")
        .and_then(|metadata| metadata.get(OWN_METADATA))
        .and_then(|own| own.get("kind"))
        .and_then(Value::as_str)
        .and_then(TransitionKind::from_key)
        .unwrap_or(TransitionKind::Dissolve);
    let offset = |name: &str| -> Result<i64, Error> {
        Ok(read_time(transition.get(name))?
            .map_or(0, |(value, from_rate)| frames(value, from_rate, rate)))
    };
    Ok(Transition {
        kind,
        duration: offset("in_offset")?.max(0) + offset("out_offset")?.max(0),
    })
}

/// A clip that plays a project kept inside this one, `inner`: a stack of its tracks in place of
/// the clip. What the stack has no field for (the project's rate, the clip's volume) goes in its
/// metadata.
fn write_inside(clip: &Clip, inner: &Timeline, rate: Rate) -> Value {
    let mut own = json!({ "rate": rate_json(inner.rate) });
    if clip.volume != 1.0 {
        own["volume"] = json!(clip.volume);
    }
    if !clip.transform.is_identity() {
        own["transform"] = clip.transform.value();
    }
    if !clip.keyframes.is_empty() {
        own["keyframes"] = keyframes_value(&clip.keyframes);
    }
    if inner.name != clip.name {
        own["name"] = json!(inner.name);
    }
    if !inner.metadata.is_empty() {
        own["metadata"] = Value::Object(inner.metadata.clone());
    }
    let mut metadata = clip.extras.metadata.clone();
    metadata.insert(OWN_METADATA.to_string(), own);
    let tracks: Vec<Value> = inner
        .tracks()
        .enumerate()
        .map(|(index, track)| write_track(track, index, inner.rate))
        .collect();
    json!({
        "OTIO_SCHEMA": "Stack.1",
        "children": tracks,
        "effects": effects_with_speed(clip),
        "enabled": clip.enabled,
        "markers": clip.extras.markers,
        "metadata": metadata,
        "name": clip.name,
        "source_range": range(clip.source_start, clip.duration, rate),
    })
}

/// The media's first frame as a time at the timeline's rate: its start timecode.
fn media_start(media: &Media, rate: Rate) -> i64 {
    media
        .available
        .map_or(0, |available| media.rate.rescale_nearest(available.start, rate))
}

fn read_track(
    track: &Object,
    index: usize,
    rate: Rate,
    next_id: &mut u64,
) -> Result<Track, Error> {
    refuse_source_range(track, "a track")?;
    let kind = match text_of(track, "kind").as_str() {
        "Video" | "" => TrackKind::Video,
        "Audio" => TrackKind::Audio,
        other => return Err(Error(format!("cannotcut has no {other} tracks yet"))),
    };
    let mut items = Vec::new();
    // A transition read, for the clip after it.
    let mut way_in: Option<Transition> = None;
    for child in children(track) {
        let child = object(child, "an item on a track")?;
        match schema_name(child) {
            "Clip" | "Stack" => {
                let mut clip = if schema_name(child) == "Clip" {
                    read_clip(child, rate, next_id)?
                } else {
                    read_inside(child, rate, next_id)?
                };
                clip.transition_in = way_in
                    .take()
                    .map(|way| Transition {
                        duration: way.duration.min(clip.duration),
                        ..way
                    })
                    .filter(|way| way.duration >= 1);
                items.push(Item::Clip(clip));
            }
            "Transition" => way_in = Some(read_transition(child, rate)?),
            "Gap" => {
                way_in = None;
                let Some((_, duration)) = read_range(child.get("source_range"))? else {
                    return Err(Error("a gap has no length".to_string()));
                };
                items.push(Item::Gap(frames(duration.0, duration.1, rate)));
            }
            _ => return Err(unsupported(schema(child), "on a track")),
        }
    }
    let mut extras = extras(track);
    let own = extras.metadata.remove(OWN_METADATA);
    let mut track = Track {
        name: text_of(track, "name"),
        kind,
        enabled: flag(track, "enabled"),
        muted: own
            .as_ref()
            .and_then(|own| own.get("muted")?.as_bool())
            .unwrap_or(false),
        magnetic: own
            .as_ref()
            .and_then(|own| own.get("magnetic")?.as_bool())
            .unwrap_or(index == 0),
        extras,
        items,
    };
    track.tidy();
    Ok(track)
}

fn read_clip(clip: &Object, rate: Rate, next_id: &mut u64) -> Result<Clip, Error> {
    let name = text_of(clip, "name");
    // Clip.1 has a single media reference; Clip.2 has several by name, one of them in use.
    let reference = clip.get("media_reference").or_else(|| {
        let active = clip.get("active_media_reference_key")?.as_str()?;
        clip.get("media_references")?.get(active)
    });
    let reference = match reference {
        Some(reference) => object(reference, "a media reference")?,
        None => return Err(Error(format!("clip \"{name}\" has no media"))),
    };
    let mut title = None;
    let target = match schema_name(reference) {
        "ExternalReference" => Some(text_of(reference, "target_url")),
        "MissingReference" => None,
        "GeneratorReference" if text_of(reference, "generator_kind") == TITLE_GENERATOR => {
            title = Some(Title::from_value(
                reference.get("parameters").unwrap_or(&Value::Null),
            ));
            None
        }
        _ => return Err(unsupported(schema(reference), "as a clip's media")),
    };
    let available = read_range(reference.get("available_range"))?;
    let source = read_range(clip.get("source_range"))?.or(available);
    let Some((source_start, source_duration)) = source else {
        return Err(Error(format!("clip \"{name}\" has no length")));
    };

    let mut media_metadata = child_object(reference, "metadata");
    let own = media_metadata.remove(OWN_METADATA);
    let media_rate = own
        .as_ref()
        .and_then(read_own_rate)
        .or_else(|| available.and_then(|(_, duration)| Rate::from_f64(duration.1)))
        .or_else(|| Rate::from_f64(source_duration.1))
        .ok_or_else(|| Error(format!("clip \"{name}\" has no usable frame rate")))?;
    let media = Media {
        target,
        rate: media_rate,
        available: available.map(|(start, duration)| FrameRange {
            start: frames(start.0, start.1, media_rate),
            duration: frames(duration.0, duration.1, media_rate),
        }),
        name: text_of(reference, "name"),
        metadata: media_metadata,
        // Loaded by `Timeline::load`, which knows where the file is.
        project: None,
        title,
    };
    let mut extras = extras(clip);
    let speed = take_speed(&mut extras.effects);
    let own = extras.metadata.remove(OWN_METADATA);
    let volume = own
        .as_ref()
        .and_then(|own| own.get("volume")?.as_f64())
        .filter(|volume| volume.is_finite() && *volume >= 0.0)
        .map_or(1.0, |volume| volume as f32);
    let transform = own
        .as_ref()
        .and_then(|own| own.get("transform"))
        .map_or(Transform::IDENTITY, Transform::from_value);
    let keyframes = own
        .as_ref()
        .and_then(|own| own.get("keyframes"))
        .map(keyframes_from_value)
        .unwrap_or_default();
    Ok(Clip {
        id: new_id(next_id),
        name,
        source_start: frames(source_start.0, source_start.1, rate) - media_start(&media, rate),
        duration: frames(source_duration.0, source_duration.1, rate),
        media: Arc::new(media),
        enabled: flag(clip, "enabled"),
        volume,
        transform,
        keyframes,
        transition_in: None,
        speed,
        extras,
    })
}

/// A stack of tracks on a track: a clip playing the project it makes up, kept inside this one.
fn read_inside(stack: &Object, rate: Rate, next_id: &mut u64) -> Result<Clip, Error> {
    let name = text_of(stack, "name");
    let mut extras = extras(stack);
    let speed = take_speed(&mut extras.effects);
    let own = extras.metadata.remove(OWN_METADATA).unwrap_or(Value::Null);
    let inner_rate = read_own_rate(&own)
        .or_else(|| first_rate(stack))
        .unwrap_or(rate);
    let mut inner = Timeline {
        name: own
            .get("name")
            .and_then(Value::as_str)
            .map_or_else(|| name.clone(), str::to_string),
        metadata: own
            .get("metadata")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default(),
        rate: inner_rate,
        start: 0,
        tracks: Vec::new(),
        stack: Extras::default(),
        next_id: 0,
    };
    for (index, child) in children(stack).enumerate() {
        let child = object(child, "a track")?;
        if schema_name(child) != "Track" {
            return Err(unsupported(schema(child), "in place of a track"));
        }
        let track = read_track(child, index, inner_rate, &mut inner.next_id)?;
        inner.tracks.push(Arc::new(track));
    }
    let media = Media::inside(inner);
    let length = media
        .available
        .map_or(0, |available| inner_rate.rescale(available.duration, rate));
    let (source_start, duration) = match read_range(stack.get("source_range"))? {
        Some((start, duration)) => (
            frames(start.0, start.1, rate),
            frames(duration.0, duration.1, rate),
        ),
        None => (0, length),
    };
    if duration <= 0 {
        return Err(Error(format!("compound clip \"{name}\" has no length")));
    }
    let volume = own
        .get("volume")
        .and_then(Value::as_f64)
        .filter(|volume| volume.is_finite() && *volume >= 0.0)
        .map_or(1.0, |volume| volume as f32);
    let transform = own
        .get("transform")
        .map_or(Transform::IDENTITY, Transform::from_value);
    let keyframes = own
        .get("keyframes")
        .map(keyframes_from_value)
        .unwrap_or_default();
    Ok(Clip {
        id: new_id(next_id),
        name,
        media: Arc::new(media),
        source_start,
        duration,
        enabled: flag(stack, "enabled"),
        volume,
        transform,
        keyframes,
        transition_in: None,
        speed,
        extras,
    })
}

fn rate_json(rate: Rate) -> Value {
    json!({ "num": rate.num, "den": rate.den })
}

fn read_own_rate(own: &Value) -> Option<Rate> {
    let rate = own.get("rate")?;
    let part = |name: &str| {
        u32::try_from(rate.get(name)?.as_u64()?)
            .ok()
            .filter(|part| *part > 0)
    };
    Some(Rate::new(part("num")?, part("den")?))
}

fn time(value: i64, rate: Rate) -> Value {
    json!({
        "OTIO_SCHEMA": "RationalTime.1",
        "rate": rate.as_f64(),
        "value": value as f64,
    })
}

fn range(start: i64, duration: i64, rate: Rate) -> Value {
    json!({
        "OTIO_SCHEMA": "TimeRange.1",
        "duration": time(duration, rate),
        "start_time": time(start, rate),
    })
}

/// A time as the file has it: a count and the rate it is counted at. None for null.
fn read_time(time: Option<&Value>) -> Result<Option<(f64, f64)>, Error> {
    let Some(time) = time.filter(|time| !time.is_null()) else {
        return Ok(None);
    };
    let part = |name: &str| time.get(name).and_then(Value::as_f64);
    match (part("value"), part("rate")) {
        (Some(value), Some(rate)) if value.is_finite() && rate.is_finite() && rate > 0.0 => {
            Ok(Some((value, rate)))
        }
        _ => Err(Error(format!("{time} is not a time"))),
    }
}

/// A range's start and duration. None for null.
fn read_range(range: Option<&Value>) -> Result<Option<((f64, f64), (f64, f64))>, Error> {
    let Some(range) = range.filter(|range| !range.is_null()) else {
        return Ok(None);
    };
    match (
        read_time(range.get("start_time"))?,
        read_time(range.get("duration"))?,
    ) {
        (Some(start), Some(duration)) => Ok(Some((start, duration))),
        _ => Err(Error(format!("{range} is not a time range"))),
    }
}

/// A time from the file as a whole number of frames at `rate`.
fn frames(value: f64, from_rate: f64, rate: Rate) -> i64 {
    (value / from_rate * rate.as_f64()).round() as i64
}

/// The rate of the first clip or gap in the stack, for files that state no rate of their own.
fn first_rate(stack: &Object) -> Option<Rate> {
    children(stack)
        .flat_map(|track| track.as_object().into_iter().flat_map(children))
        .find_map(|item| read_range(item.get("source_range")).ok()?)
        .and_then(|(_, duration)| Rate::from_f64(duration.1))
}

fn object<'a>(value: &'a Value, what: &str) -> Result<&'a Object, Error> {
    value
        .as_object()
        .ok_or_else(|| Error(format!("{what} is not an object")))
}

/// The object's type with its version, such as `Clip.2`.
fn schema(object: &Object) -> &str {
    object
        .get("OTIO_SCHEMA")
        .and_then(Value::as_str)
        .unwrap_or("thing without a type")
}

fn schema_name(object: &Object) -> &str {
    let schema = schema(object);
    schema.split_once('.').map_or(schema, |(name, _)| name)
}

fn children(object: &Object) -> impl Iterator<Item = &Value> {
    object
        .get("children")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
}

fn text_of(object: &Object, key: &str) -> String {
    object
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// A true-or-false field that counts as true when the file leaves it out.
fn flag(object: &Object, key: &str) -> bool {
    object.get(key).and_then(Value::as_bool).unwrap_or(true)
}

fn child_object(object: &Object, key: &str) -> Object {
    object
        .get(key)
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default()
}

/// A clip's speed, from the effect OpenTimelineIO gives it, taken out of its effects; 1 when it
/// has none. A frozen frame (no speed) is left in and the clip plays as recorded.
fn take_speed(effects: &mut Vec<Value>) -> f64 {
    let speed_of = |effect: &Value| {
        let effect = effect.as_object()?;
        (schema_name(effect) == "LinearTimeWarp")
            .then(|| effect.get("time_scalar")?.as_f64())
            .flatten()
            .filter(|scalar| scalar.is_finite() && *scalar > 0.0)
    };
    match effects.iter().position(|effect| speed_of(effect).is_some()) {
        Some(index) => {
            let speed = speed_of(&effects.remove(index)).unwrap_or(1.0);
            speed.clamp(*SPEEDS.start(), *SPEEDS.end())
        }
        None => 1.0,
    }
}

/// A clip's effects as written: those read with it, and its speed when it has one.
fn effects_with_speed(clip: &Clip) -> Vec<Value> {
    let mut effects = clip.extras.effects.clone();
    if clip.speed != 1.0 {
        effects.push(json!({
            "OTIO_SCHEMA": "LinearTimeWarp.1",
            "name": "",
            "effect_name": "LinearTimeWarp",
            "metadata": {},
            "time_scalar": clip.speed,
        }));
    }
    effects
}

fn extras(object: &Object) -> Extras {
    let list = |key: &str| {
        object
            .get(key)
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    };
    Extras {
        metadata: child_object(object, "metadata"),
        effects: list("effects"),
        markers: list("markers"),
    }
}

fn refuse_source_range(object: &Object, what: &str) -> Result<(), Error> {
    match object.get("source_range") {
        Some(range) if !range.is_null() => Err(Error(format!(
            "{what} is trimmed as a whole, which cannotcut cannot open yet"
        ))),
        _ => Ok(()),
    }
}

fn unsupported(schema: &str, place: &str) -> Error {
    Error(format!(
        "the project has a {schema} {place}, which cannotcut cannot open yet"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Edit, NewClip, Placement};

    /// What OpenTimelineIO itself writes for a 23.976 timeline: a clip from timecoded media, a
    /// gap, and a clip in the older single-reference form whose media is missing.
    const FOREIGN: &str = r#"{
        "OTIO_SCHEMA": "Timeline.1",
        "metadata": { "note": "from elsewhere" },
        "name": "Rough cut",
        "global_start_time": { "OTIO_SCHEMA": "RationalTime.1", "rate": 23.976023976023978, "value": 86400.0 },
        "tracks": {
            "OTIO_SCHEMA": "Stack.1",
            "metadata": {}, "name": "tracks", "source_range": null,
            "effects": [], "markers": [], "enabled": true,
            "children": [
                {
                    "OTIO_SCHEMA": "Track.1",
                    "metadata": { "locked": true }, "name": "V1", "source_range": null,
                    "effects": [], "markers": [], "enabled": true, "kind": "Video",
                    "children": [
                        {
                            "OTIO_SCHEMA": "Clip.2",
                            "metadata": {}, "name": "interview",
                            "source_range": {
                                "OTIO_SCHEMA": "TimeRange.1",
                                "duration": { "OTIO_SCHEMA": "RationalTime.1", "rate": 23.976023976023978, "value": 48.0 },
                                "start_time": { "OTIO_SCHEMA": "RationalTime.1", "rate": 23.976023976023978, "value": 86424.0 }
                            },
                            "effects": [],
                            "markers": [ { "OTIO_SCHEMA": "Marker.2", "name": "good take" } ],
                            "enabled": true,
                            "media_references": {
                                "DEFAULT_MEDIA": {
                                    "OTIO_SCHEMA": "ExternalReference.1",
                                    "metadata": {}, "name": "",
                                    "available_range": {
                                        "OTIO_SCHEMA": "TimeRange.1",
                                        "duration": { "OTIO_SCHEMA": "RationalTime.1", "rate": 23.976023976023978, "value": 1000.0 },
                                        "start_time": { "OTIO_SCHEMA": "RationalTime.1", "rate": 23.976023976023978, "value": 86400.0 }
                                    },
                                    "available_image_bounds": null,
                                    "target_url": "file:///media/interview.mov"
                                }
                            },
                            "active_media_reference_key": "DEFAULT_MEDIA"
                        },
                        {
                            "OTIO_SCHEMA": "Gap.1",
                            "metadata": {}, "name": "",
                            "source_range": {
                                "OTIO_SCHEMA": "TimeRange.1",
                                "duration": { "OTIO_SCHEMA": "RationalTime.1", "rate": 23.976023976023978, "value": 12.0 },
                                "start_time": { "OTIO_SCHEMA": "RationalTime.1", "rate": 23.976023976023978, "value": 0.0 }
                            },
                            "effects": [], "markers": [], "enabled": true
                        },
                        {
                            "OTIO_SCHEMA": "Clip.1",
                            "metadata": {}, "name": "lost",
                            "source_range": {
                                "OTIO_SCHEMA": "TimeRange.1",
                                "duration": { "OTIO_SCHEMA": "RationalTime.1", "rate": 47.952047952047955, "value": 20.0 },
                                "start_time": { "OTIO_SCHEMA": "RationalTime.1", "rate": 47.952047952047955, "value": 10.0 }
                            },
                            "effects": [], "markers": [], "enabled": false,
                            "media_reference": {
                                "OTIO_SCHEMA": "MissingReference.1",
                                "metadata": {}, "name": "", "available_range": null, "available_image_bounds": null
                            }
                        }
                    ]
                }
            ]
        }
    }"#;

    #[test]
    fn a_file_from_another_tool_is_understood() {
        let timeline = Timeline::from_otio(FOREIGN).unwrap();
        let film = Rate::new(24000, 1001);
        assert_eq!(timeline.name, "Rough cut");
        assert_eq!(timeline.rate(), film);
        assert_eq!(timeline.start, 86400);
        assert_eq!(timeline.duration(), 48 + 12 + 10);

        let track = timeline.track(0).unwrap();
        assert_eq!(track.name, "V1");
        assert_eq!(track.extras.metadata["locked"], json!(true));
        let [Item::Clip(interview), Item::Gap(12), Item::Clip(lost)] = track.items() else {
            panic!("unexpected items: {:?}", track.items());
        };

        // The source range counts in the media's timecode; the clip counts from its first frame.
        assert_eq!((interview.source_start, interview.duration), (24, 48));
        assert_eq!(interview.media.rate, film);
        assert_eq!(
            interview.media.available,
            Some(FrameRange {
                start: 86400,
                duration: 1000
            })
        );
        assert_eq!(interview.media.path(), Some("/media/interview.mov".into()));
        assert_eq!(interview.media_frame(3, film), 27);
        assert_eq!(interview.extras.markers.len(), 1);

        // A range at twice the timeline's rate is half as many timeline frames.
        assert_eq!((lost.source_start, lost.duration), (5, 10));
        assert_eq!(lost.media.target, None);
        assert_eq!(lost.media.rate, Rate::new(48000, 1001));
        assert!(!lost.enabled);
        assert_eq!(lost.media_frame(0, film), 10);
    }

    #[test]
    fn saving_and_loading_gives_the_same_timeline() {
        let foreign = Timeline::from_otio(FOREIGN).unwrap();
        assert_eq!(Timeline::from_otio(&foreign.to_otio()).unwrap(), foreign);
        // As Windows PowerShell writes it: with a byte-order mark first.
        let marked = format!("\u{feff}{FOREIGN}");
        assert_eq!(Timeline::from_otio(&marked).unwrap(), foreign);

        let rate = Rate::new(30000, 1001);
        let sixty = Arc::new(Media::from_path(
            Path::new(r"C:\clips\screen recording.mp4"),
            Rate::new(60, 1),
            53451,
        ));
        let mut timeline = Timeline::new("My project", rate);
        let edits = [
            Edit::AddTrack {
                index: 1,
                kind: TrackKind::Audio,
                name: "Music".to_string(),
            },
            Edit::Place {
                track: 0,
                at: 0,
                clip: NewClip::whole("screen", sixty.clone(), rate).unwrap(),
                mode: Placement::Insert,
            },
            Edit::Split { track: 0, at: 500 },
            Edit::Place {
                track: 1,
                at: 90,
                clip: NewClip {
                    name: "late".to_string(),
                    media: sixty,
                    source_start: 7,
                    duration: 33,
                },
                mode: Placement::Overwrite,
            },
        ];
        for edit in &edits {
            timeline.apply(edit).unwrap();
        }
        let (_, late) = timeline.track(1).unwrap().clips().next().unwrap();
        let late = late.id;
        timeline
            .apply(&Edit::SetVolume {
                clip: late,
                volume: 0.35,
            })
            .unwrap();
        timeline
            .apply(&Edit::SetMuted {
                track: 1,
                muted: true,
            })
            .unwrap();
        let text = timeline.to_otio();
        assert_eq!(Timeline::from_otio(&text).unwrap(), timeline);
        // Other tools read whole numbers at the timeline's rate.
        assert!(text.contains("\"rate\": 29.97002997002997"), "{text}");
    }

    #[test]
    fn which_tracks_are_magnetic_is_kept() {
        // A file that does not say: the main track is, the others are not.
        let foreign = Timeline::from_otio(FOREIGN).unwrap();
        let magnetic: Vec<bool> = foreign.tracks().map(|track| track.magnetic).collect();
        assert!(magnetic[0] && magnetic[1..].iter().all(|magnetic| !magnetic));

        let mut timeline = Timeline::new("magnets", Rate::new(30, 1));
        let overlay = Edit::AddTrack {
            index: 1,
            kind: TrackKind::Video,
            name: "Overlay".to_string(),
        };
        timeline.apply(&overlay).unwrap();
        let text = timeline.to_otio();
        // As files without it would be read: nothing written.
        assert!(!text.contains("magnetic"), "{text}");
        for (track, magnetic) in [(0, false), (1, true)] {
            timeline.apply(&Edit::SetMagnetic { track, magnetic }).unwrap();
        }
        let back = Timeline::from_otio(&timeline.to_otio()).unwrap();
        assert_eq!(back, timeline);
        assert!(!back.track(0).unwrap().magnetic && back.track(1).unwrap().magnetic);
    }

    #[test]
    fn speed_changes_length_media_and_sound_and_is_kept() {
        let rate = Rate::new(30, 1);
        let mut timeline = Timeline::new("speed", rate);
        let media = Arc::new(Media::from_path(Path::new("a.mp4"), rate, 300));
        let clip = |source_start, duration| crate::NewClip {
            name: "a".to_string(),
            media: media.clone(),
            source_start,
            duration,
        };
        let put = |timeline: &mut Timeline, at, clip| {
            let place = Edit::Place {
                track: 0,
                at,
                clip,
                mode: crate::Placement::Insert,
            };
            timeline.apply(&place).unwrap().unwrap()
        };
        let first = put(&mut timeline, 0, clip(100, 100));
        put(&mut timeline, 100, clip(0, 50));
        // Twice as fast: half as long, the clip after moves up, the media goes by twice as fast.
        timeline.apply(&Edit::SetSpeed { clip: first, speed: 2.0 }).unwrap();
        let (_, fast, _) = timeline.clip(first).unwrap();
        assert_eq!(fast.duration, 50);
        assert_eq!(fast.media_frame(10, rate), 120);
        assert_eq!(timeline.duration(), 100);
        // Half as fast: the same 100 frames of media over 200; the media's last 100 frames
        // would make 200 more, and no further.
        timeline.apply(&Edit::SetSpeed { clip: first, speed: 0.5 }).unwrap();
        let (_, slow, _) = timeline.clip(first).unwrap();
        assert_eq!(slow.duration, 200);
        let edge = |frames| Edit::Trim {
            clip: first,
            edge: crate::Edge::End,
            frames,
            ripple: true,
        };
        assert!(timeline.clone().apply(&edge(200)).is_ok());
        assert!(timeline.clone().apply(&edge(201)).is_err());
        assert!(timeline.apply(&Edit::SetSpeed { clip: first, speed: 50.0 }).is_err());

        let sounds = timeline.sounds(48_000);
        assert_eq!(sounds[0].speed, 0.5);
        assert_eq!((sounds[0].begins, sounds[0].ends), (0, 200 * 1600));
        let text = timeline.to_otio();
        assert!(text.contains("LinearTimeWarp.1"), "{text}");
        assert_eq!(Timeline::from_otio(&text).unwrap(), timeline);
    }

    #[test]
    fn text_clips_are_kept_and_not_heard() {
        let rate = Rate::new(30, 1);
        let mut timeline = Timeline::new("words", rate);
        let mut title = Title::new("Hello\nworld");
        title.color = [10, 20, 30, 255];
        let place = Edit::Place {
            track: 0,
            at: 15,
            clip: crate::NewClip::title(title.clone(), 90, rate),
            mode: crate::Placement::Overwrite,
        };
        timeline.apply(&place).unwrap();
        let text = timeline.to_otio();
        assert!(text.contains("GeneratorReference.1"), "{text}");
        let back = Timeline::from_otio(&text).unwrap();
        assert_eq!(back, timeline);
        let (clip, offset) = back.picture_at(20).unwrap();
        assert_eq!((offset, clip.duration), (5, 90));
        assert_eq!(clip.media.title.as_ref(), Some(&title));
        assert_eq!(clip.name, "Hello");
        assert!(back.sounds(48_000).is_empty());
        assert!(back.shots_at(20)[0].file(None).is_none());
    }

    #[test]
    fn what_cannot_be_edited_yet_is_refused_by_name() {
        let with_effect = FOREIGN.replace("\"Gap.1\"", "\"Effect.1\"");
        let error = Timeline::from_otio(&with_effect).unwrap_err();
        assert!(error.to_string().contains("Effect.1"), "{error}");

        let error = Timeline::from_otio(r#"{ "OTIO_SCHEMA": "Clip.2" }"#).unwrap_err();
        assert!(error.to_string().contains("Clip.2"), "{error}");
        assert!(Timeline::from_otio("not json").is_err());
    }

    #[test]
    fn projects_are_saved_to_disk_and_loaded_back() {
        let path = std::env::temp_dir().join(format!("cannotcut-test-{}.otio", std::process::id()));
        let timeline = Timeline::from_otio(FOREIGN).unwrap();
        timeline.save(&path).unwrap();
        // Saving over an existing project replaces it.
        timeline.save(&path).unwrap();
        assert_eq!(Timeline::load(&path).unwrap(), timeline);
        fs::remove_file(&path).unwrap();
    }
}
