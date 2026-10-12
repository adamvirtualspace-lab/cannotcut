//! Reading and writing timelines as OpenTimelineIO JSON.
//!
//! The file is the standard format, so other tools can open it. What cannotcut cannot edit yet
//! (transitions, nested compositions, generated media) is refused by name when reading, instead
//! of being dropped quietly. Metadata, effects and markers it does not understand are carried
//! through unchanged.

use std::fs;
use std::path::Path;
use std::sync::Arc;

use serde_json::{Map, Value, json};

use crate::Error;
use crate::model::{
    Clip, Extras, FrameRange, Item, Media, Rate, Timeline, Track, TrackKind, new_id,
};

/// The metadata key under which cannotcut keeps what the format has no field for.
const OWN_METADATA: &str = "cannotcut";
/// The name OpenTimelineIO gives a clip's media reference when there is only one.
const DEFAULT_MEDIA: &str = "DEFAULT_MEDIA";
/// The rate of a timeline whose file gives no hint of one.
const FALLBACK_RATE: Rate = Rate::new(30, 1);

type Object = Map<String, Value>;

impl Timeline {
    pub fn load(path: &Path) -> Result<Self, Error> {
        let text = fs::read_to_string(path).map_err(|error| Error::new("read project", error))?;
        Self::from_otio(&text)
    }

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
            .map(|track| write_track(track, self.rate))
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
        for child in children(stack) {
            let child = object(child, "a track")?;
            if schema_name(child) != "Track" {
                return Err(unsupported(schema(child), "in place of a track"));
            }
            let track = read_track(child, rate, &mut timeline.next_id)?;
            timeline.tracks.push(Arc::new(track));
        }
        Ok(timeline)
    }
}

fn write_track(track: &Track, rate: Rate) -> Value {
    let children: Vec<Value> = track
        .items()
        .iter()
        .map(|item| match item {
            Item::Clip(clip) => write_clip(clip, rate),
            Item::Gap(duration) => json!({
                "OTIO_SCHEMA": "Gap.1",
                "effects": [],
                "enabled": true,
                "markers": [],
                "metadata": {},
                "name": "",
                "source_range": range(0, *duration, rate),
            }),
        })
        .collect();
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
        "metadata": track.extras.metadata,
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
    }
    // The range is written at the timeline's rate, where its numbers are whole.
    let source_start = media_start(media, rate) + clip.source_start;
    json!({
        "OTIO_SCHEMA": "Clip.2",
        "active_media_reference_key": DEFAULT_MEDIA,
        "effects": clip.extras.effects,
        "enabled": clip.enabled,
        "markers": clip.extras.markers,
        "media_references": { DEFAULT_MEDIA: reference },
        "metadata": clip.extras.metadata,
        "name": clip.name,
        "source_range": range(source_start, clip.duration, rate),
    })
}

/// The media's first frame as a time at the timeline's rate: its start timecode.
fn media_start(media: &Media, rate: Rate) -> i64 {
    media
        .available
        .map_or(0, |available| media.rate.rescale_nearest(available.start, rate))
}

fn read_track(track: &Object, rate: Rate, next_id: &mut u64) -> Result<Track, Error> {
    refuse_source_range(track, "a track")?;
    let kind = match text_of(track, "kind").as_str() {
        "Video" | "" => TrackKind::Video,
        "Audio" => TrackKind::Audio,
        other => return Err(Error(format!("cannotcut has no {other} tracks yet"))),
    };
    let mut items = Vec::new();
    for child in children(track) {
        let child = object(child, "an item on a track")?;
        match schema_name(child) {
            "Clip" => items.push(Item::Clip(read_clip(child, rate, next_id)?)),
            "Gap" => {
                let Some((_, duration)) = read_range(child.get("source_range"))? else {
                    return Err(Error("a gap has no length".to_string()));
                };
                items.push(Item::Gap(frames(duration.0, duration.1, rate)));
            }
            _ => return Err(unsupported(schema(child), "on a track")),
        }
    }
    let mut track = Track {
        name: text_of(track, "name"),
        kind,
        enabled: flag(track, "enabled"),
        extras: extras(track),
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
    let target = match schema_name(reference) {
        "ExternalReference" => Some(text_of(reference, "target_url")),
        "MissingReference" => None,
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
    };
    Ok(Clip {
        id: new_id(next_id),
        name,
        source_start: frames(source_start.0, source_start.1, rate) - media_start(&media, rate),
        duration: frames(source_duration.0, source_duration.1, rate),
        media: Arc::new(media),
        enabled: flag(clip, "enabled"),
        extras: extras(clip),
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
        let text = timeline.to_otio();
        assert_eq!(Timeline::from_otio(&text).unwrap(), timeline);
        // Other tools read whole numbers at the timeline's rate.
        assert!(text.contains("\"rate\": 29.97002997002997"), "{text}");
    }

    #[test]
    fn what_cannot_be_edited_yet_is_refused_by_name() {
        let with_transition = FOREIGN.replace("\"Gap.1\"", "\"Transition.1\"");
        let error = Timeline::from_otio(&with_transition).unwrap_err();
        assert!(error.to_string().contains("Transition.1"), "{error}");

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
