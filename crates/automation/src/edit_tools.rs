//! Typed editing tools: the everyday jobs (assemble, cut, trim, title, effects, export, save) as
//! MCP tools with real JSON Schemas, so an agent needn't learn each command's `params` from
//! `command_list`. Each tool is a thin wrapper over one or two engine commands; everything else
//! stays reachable through `command_run`. Times are in seconds; ids come from `timeline_summary`,
//! `project_inspect` or `sequence_inspect`.

use std::sync::PoisonError;

use filmcraft_time::Tick;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::service::RequestContext;
use rmcp::{ErrorData as McpError, RoleServer, tool, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::AutomationError;
use crate::server::{Backend, FilmcraftMcp, fail, join_error, ok_json, png_result, wrap};

/// Longest time a tool accepts, in seconds (a day): keeps tick arithmetic far from overflow.
const MAX_SECONDS: f64 = 86_400.0;
/// Most frames a contact sheet renders.
pub const MAX_SHEET_FRAMES: u32 = 16;

/// `v` as a time on the timeline: finite, not negative, at most [`MAX_SECONDS`].
fn time_arg(name: &str, v: f64) -> Result<f64, String> {
    if v.is_finite() && (0.0..=MAX_SECONDS).contains(&v) { Ok(v) } else { Err(format!("`{name}` must be between 0 and {MAX_SECONDS} seconds, got {v}")) }
}

/// `v` as a signed amount of time: finite, at most [`MAX_SECONDS`] either way.
fn delta_arg(name: &str, v: f64) -> Result<f64, String> {
    if v.is_finite() && v.abs() <= MAX_SECONDS { Ok(v) } else { Err(format!("`{name}` must be between -{MAX_SECONDS} and {MAX_SECONDS} seconds, got {v}")) }
}

fn ticks(seconds: f64) -> i64 {
    Tick::from_seconds_f64(seconds).0
}

fn seconds_of(ticks: &Value) -> Value {
    ticks.as_i64().map_or(Value::Null, |t| json!(Tick(t).seconds()))
}

/// The export format a file name implies (`out.mp4` → h264), as `filmcraft-cli export` guesses it.
pub fn format_for(path: &str) -> Option<&'static str> {
    let ext = path.rsplit_once('.')?.1.to_ascii_lowercase();
    Some(match ext.as_str() {
        "mp4" | "m4v" => "h264",
        "mov" => "prores",
        "mxf" => "mxf-op1a",
        "png" => "png",
        "gif" => "gif",
        "wav" => "wav",
        "tif" | "tiff" => "tiff",
        "bmp" => "bmp",
        "aif" | "aiff" => "aiff",
        _ => return None,
    })
}

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Edge {
    /// The clip's start.
    In,
    /// The clip's end.
    Out,
}

impl Edge {
    fn as_str(self) -> &'static str {
        match self {
            Edge::In => "in",
            Edge::Out => "out",
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
pub enum EffectKind {
    Video,
    Audio,
    VideoTransition,
    AudioTransition,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct SequenceNewParams {
    /// Sequence name (default "Sequence NN").
    #[serde(default)]
    pub name: Option<String>,
    /// Project item id of a clip: the sequence takes its size and frame rate and starts with the
    /// clip on it (the easiest way to start an edit).
    #[serde(default)]
    pub from_item: Option<u64>,
    /// Frame width (default 1920; ignored with `from_item`).
    #[serde(default)]
    pub width: Option<u32>,
    /// Frame height (default 1080).
    #[serde(default)]
    pub height: Option<u32>,
    /// Frame rate, e.g. 23.976, 25, 29.97, 30, 60 (default 23.976).
    #[serde(default)]
    pub fps: Option<f64>,
    /// Number of video tracks (default 3).
    #[serde(default)]
    pub video_tracks: Option<u32>,
    /// Number of audio tracks (default 3).
    #[serde(default)]
    pub audio_tracks: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ClipPlaceParams {
    /// Project item id to place (from `media_import` or `project_inspect`).
    pub item: u64,
    /// Track for the picture, e.g. "V1" (default: the targeted video track). Naming an audio track
    /// ("A2") places the sound alone.
    #[serde(default)]
    pub track: Option<String>,
    /// Track for the sound, e.g. "A1" (default: the targeted audio track).
    #[serde(default)]
    pub audio_track: Option<String>,
    /// Where the clip starts on the timeline, in seconds (default: the end of the sequence, so
    /// repeated calls assemble clips one after another).
    #[serde(default)]
    pub at_seconds: Option<f64>,
    /// Where in the source media to start, in seconds (default: its In mark, or 0).
    #[serde(default)]
    pub source_in_seconds: Option<f64>,
    /// How much of the source to use, in seconds (default: to its Out mark or end).
    #[serde(default)]
    pub duration_seconds: Option<f64>,
    /// Insert (push later clips right) instead of overwrite (default false).
    #[serde(default)]
    pub insert: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ClipSplitParams {
    /// Where to cut, in seconds on the timeline.
    pub at_seconds: f64,
    /// Only this clip (and its linked sound or picture). Default: every clip on every unlocked
    /// track under `at_seconds`.
    #[serde(default)]
    pub clip: Option<u64>,
    /// Only this track, e.g. "V1" or "A1".
    #[serde(default)]
    pub track: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ClipTrimParams {
    /// Clip id (from `timeline_summary`).
    pub clip: u64,
    /// Which edge to move.
    pub edge: Edge,
    /// How far to move the edge, in seconds: positive moves it later, negative earlier. Trimming
    /// the in edge by +1 drops the clip's first second; the out edge by -1 drops its last second.
    #[serde(default)]
    pub delta_seconds: Option<f64>,
    /// Or how far in whole frames.
    #[serde(default)]
    pub delta_frames: Option<i64>,
    /// Ripple: close or open the gap by moving later clips too (default false: leave a gap).
    #[serde(default)]
    pub ripple: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ClipMoveParams {
    /// Clip id.
    pub clip: u64,
    /// New start on the timeline, in seconds.
    pub to_seconds: f64,
    /// Destination track, e.g. "V2" (default: the track it is on).
    #[serde(default)]
    pub track: Option<String>,
    /// Insert (push clips at the destination right) instead of overwrite (default false).
    #[serde(default)]
    pub insert: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ClipDeleteParams {
    /// Clip ids to remove (their linked sound or picture goes too).
    pub clips: Vec<u64>,
    /// Close the gap they leave (default true); false leaves a gap.
    #[serde(default)]
    pub ripple: Option<bool>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct EffectListParams {
    /// Only effects of this kind.
    #[serde(default)]
    pub kind: Option<EffectKind>,
    /// Only effects whose id, name or folder contains this text (case-insensitive).
    #[serde(default)]
    pub filter: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct EffectApplyParams {
    /// Clip ids.
    pub clips: Vec<u64>,
    /// Effect id or name, e.g. "gaussian_blur", "Gaussian Blur", "Black & White", "lumetri"
    /// (`effect_list` lists them). Set its parameters with `command_run` `effects.setParam`.
    pub effect: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TransitionAddParams {
    /// The clip whose edge gets the transition.
    pub clip: u64,
    /// Which edge (default: the edit nearest the playhead).
    #[serde(default)]
    pub edge: Option<Edge>,
    /// Transition id or name (default Cross Dissolve for video, Constant Power for audio).
    #[serde(default)]
    pub effect: Option<String>,
    /// Length in frames (default: Settings ▸ Timeline, one second).
    #[serde(default)]
    pub frames: Option<i64>,
    /// An audio transition on the clip's sound instead of a video one.
    #[serde(default)]
    pub audio: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TitleAddParams {
    /// The text; `\n` starts a new line.
    pub text: String,
    /// Where the title starts on the timeline, in seconds (default 0).
    #[serde(default)]
    pub at_seconds: Option<f64>,
    /// How long it stays up, in seconds (default 5).
    #[serde(default)]
    pub duration_seconds: Option<f64>,
    /// Font size in pixels (default 100).
    #[serde(default)]
    pub size: Option<f64>,
    /// [x, y] in frame pixels of the text's alignment point on its first baseline (default: the
    /// centre of the frame).
    #[serde(default)]
    pub position: Option<[f64; 2]>,
    /// Font family name.
    #[serde(default)]
    pub font: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ExportVideoParams {
    /// Output file (absolute path). The format follows the extension unless `preset` or
    /// `format` is given: .mp4 → H.264, .mov → ProRes, .mxf, .gif, .png (numbered stills), .wav, .aiff.
    pub path: String,
    /// Export preset name (`command_run` `export.presets.list` lists them).
    #[serde(default)]
    pub preset: Option<String>,
    /// h264 | hevc | prores | dnxhr | apv | mjpeg | mxf-op1a | mxf-opatom | png | tiff | bmp | gif | wav | aiff
    #[serde(default)]
    pub format: Option<String>,
    /// Output width (default: the sequence's).
    #[serde(default)]
    pub width: Option<u32>,
    /// Output height (default: the sequence's).
    #[serde(default)]
    pub height: Option<u32>,
    /// 0–100 for the formats that take one.
    #[serde(default)]
    pub quality: Option<u8>,
    /// Include sound (default true).
    #[serde(default)]
    pub audio: Option<bool>,
    /// Export only from here, in seconds (default: the start).
    #[serde(default)]
    pub start_seconds: Option<f64>,
    /// Export only up to here, in seconds (default: the end).
    #[serde(default)]
    pub end_seconds: Option<f64>,
    /// Normalise the mix to this integrated loudness, e.g. -14 (streaming) or -23 (EBU R128).
    #[serde(default)]
    pub loudness_lufs: Option<f64>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ProjectSaveParams {
    /// Save as this .fcproj file (absolute path); omit to save over the project's file.
    #[serde(default)]
    pub path: Option<String>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ContactSheetParams {
    /// Number of frames, 1–16 (default 8), evenly spaced.
    #[serde(default)]
    pub count: Option<u32>,
    /// First frame's time in seconds (default 0).
    #[serde(default)]
    pub start_seconds: Option<f64>,
    /// Last frame's time in seconds (default: the sequence's last frame).
    #[serde(default)]
    pub end_seconds: Option<f64>,
    /// Longest side of the whole sheet in pixels (default 1280).
    #[serde(default)]
    pub max_side: Option<u32>,
}

/// The argument's value, or the tool's error result naming what is wrong with it.
macro_rules! try_arg {
    ($e:expr) => {
        match $e {
            Ok(v) => v,
            Err(m) => return Ok(fail(m)),
        }
    };
}

impl FilmcraftMcp {
    /// The active sequence with times in seconds: tracks and their clips, ready for the typed tools.
    pub(crate) async fn summary(&self) -> Result<Value, AutomationError> {
        let q = self.run("sequence.inspect", json!({})).await?;
        let track = |label: String, t: &Value| {
            json!({
                "track": label,
                "name": t["name"],
                "id": t["id"],
                "locked": t["locked"],
                "clips": t["items"].as_array().into_iter().flatten().map(|c| json!({
                    "clip": c["clip"], "item": c["item"], "name": c["name"],
                    "start": seconds_of(&c["start"]), "end": seconds_of(&c["end"]),
                    "sourceIn": seconds_of(&c["sourceIn"]), "sourceOut": seconds_of(&c["sourceOut"]),
                    "speed": c["speed"], "link": c["link"],
                    "effects": c["effects"].as_array().into_iter().flatten().map(|e| e["effect"].clone()).collect::<Vec<_>>(),
                })).collect::<Vec<_>>(),
                "transitions": t["transitions"].as_array().into_iter().flatten().map(|x| json!({
                    "effect": x["effect"], "start": seconds_of(&x["start"]), "duration": seconds_of(&x["duration"]),
                })).collect::<Vec<_>>(),
            })
        };
        // "V1", "A2": what the tools' `track` arguments take
        let tracks = |k: &str, prefix: char| {
            q[k].as_array().into_iter().flatten().enumerate().map(|(i, t)| track(format!("{prefix}{}", i.saturating_add(1)), t)).collect::<Vec<_>>()
        };
        Ok(json!({
            "sequence": q["id"],
            "name": q["name"],
            "width": q["settings"]["width"],
            "height": q["settings"]["height"],
            "duration": seconds_of(&q["duration"]),
            "playhead": seconds_of(&q["playhead"]),
            "video": tracks("video", 'V'),
            "audio": tracks("audio", 'A'),
            "markers": q["markers"].as_array().into_iter().flatten().map(|m| json!({"name": m["name"], "start": seconds_of(&m["start"]), "comment": m["comment"]})).collect::<Vec<_>>(),
        }))
    }

    /// `result` with the timeline after the edit beside it, so an agent sees what the edit did
    /// without another call.
    async fn with_summary(&self, result: Result<Value, AutomationError>) -> Result<CallToolResult, McpError> {
        match result {
            Ok(v) => {
                let timeline = self.summary().await.unwrap_or(Value::Null);
                Ok(ok_json(&json!({"result": v, "timeline": timeline})))
            }
            Err(e) => Ok(fail(e)),
        }
    }

    /// The track a clip is on, by id.
    async fn track_of(&self, clip: u64) -> Option<u64> {
        let q = self.run("sequence.inspect", json!({})).await.ok()?;
        ["video", "audio"]
            .iter()
            .flat_map(|k| q[*k].as_array().cloned().unwrap_or_default())
            .find_map(|t| t["items"].as_array()?.iter().any(|c| c["clip"].as_u64() == Some(clip)).then(|| t["id"].as_u64()).flatten())
    }

    /// The `file.exportMedia` params for `export_video`, with `wait: true`.
    fn export_params(p: &ExportVideoParams) -> Result<Value, String> {
        let mut v = json!({"path": p.path, "wait": true});
        match (&p.preset, &p.format) {
            (Some(preset), f) => {
                v["preset"] = json!(preset);
                if let Some(f) = f {
                    v["format"] = json!(f);
                }
            }
            (None, Some(f)) => v["format"] = json!(f),
            (None, None) => {
                let f = format_for(&p.path).ok_or_else(|| format!("can't tell the format from `{}`: give `format` or `preset`", p.path))?;
                v["format"] = json!(f);
            }
        }
        for (k, x) in [("width", p.width), ("height", p.height)] {
            if let Some(x) = x {
                v[k] = json!(x);
            }
        }
        if let Some(q) = p.quality {
            v["quality"] = json!(q.min(100));
        }
        if let Some(a) = p.audio {
            v["audio"] = json!(a);
        }
        if let Some(l) = p.loudness_lufs {
            if !(l.is_finite() && (-70.0..=0.0).contains(&l)) {
                return Err(format!("`loudness_lufs` must be between -70 and 0, got {l}"));
            }
            v["loudnessLufs"] = json!(l);
        }
        if p.start_seconds.is_some() || p.end_seconds.is_some() {
            v["range"] = json!("custom");
            if let Some(s) = p.start_seconds {
                v["startSeconds"] = json!(time_arg("start_seconds", s)?);
            }
            if let Some(e) = p.end_seconds {
                v["endSeconds"] = json!(time_arg("end_seconds", e)?);
            }
        }
        Ok(v)
    }
}

#[tool_router(router = edit_router, vis = "pub(crate)")]
impl FilmcraftMcp {
    #[tool(
        title = "Timeline summary",
        description = "The active sequence in seconds: video and audio tracks with their clips (clip id, project item id, name, start/end on the timeline, sourceIn/sourceOut in the media, effects), transitions and markers. Use it for the ids and times the clip_* tools take.",
        annotations(read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false)
    )]
    async fn timeline_summary(&self) -> Result<CallToolResult, McpError> {
        wrap(self.summary().await)
    }

    #[tool(
        title = "New sequence",
        description = "Create a sequence and make it active. With `from_item` it matches that clip's size and frame rate and starts with the clip on it.",
        annotations(read_only_hint = false, destructive_hint = false, idempotent_hint = false, open_world_hint = false)
    )]
    async fn sequence_new(&self, Parameters(p): Parameters<SequenceNewParams>) -> Result<CallToolResult, McpError> {
        let mut v = json!({});
        if let Some(n) = p.name {
            v["name"] = json!(n);
        }
        if let Some(i) = p.from_item {
            v["fromItem"] = json!(i);
        }
        for (k, x) in [("width", p.width), ("height", p.height), ("video", p.video_tracks), ("audio", p.audio_tracks)] {
            if let Some(x) = x {
                v[k] = json!(x);
            }
        }
        if let Some(f) = p.fps {
            v["fps"] = json!(f);
        }
        let r = self.run("file.newSequence", v).await;
        self.with_summary(r).await
    }

    #[tool(
        title = "Place a clip",
        description = "Put a project item (video, audio, still) on the timeline. By default it goes at the end of the sequence, so repeated calls assemble an edit in order. `source_in_seconds` / `duration_seconds` pick the part of the media to use.",
        annotations(read_only_hint = false, destructive_hint = true, idempotent_hint = false, open_world_hint = false)
    )]
    async fn clip_place(&self, Parameters(p): Parameters<ClipPlaceParams>) -> Result<CallToolResult, McpError> {
        let at = match p.at_seconds {
            Some(s) => try_arg!(time_arg("at_seconds", s)),
            None => match self.run("sequence.inspect", json!({})).await {
                Ok(q) => q["duration"].as_i64().map_or(0.0, |t| Tick(t).seconds()),
                Err(_) => return Ok(fail("no active sequence: create one with sequence_new (from_item makes one that fits the clip)")),
            },
        };
        let mut v = json!({"item": p.item, "time": ticks(at), "insert": p.insert.unwrap_or(false)});
        if let Some(t) = p.track {
            v["track"] = json!(t);
        }
        if let Some(t) = p.audio_track {
            v["audioTrack"] = json!(t);
        }
        if let Some(s) = p.source_in_seconds {
            v["sourceIn"] = json!(ticks(try_arg!(time_arg("source_in_seconds", s))));
        }
        if let Some(d) = p.duration_seconds {
            let d = try_arg!(time_arg("duration_seconds", d));
            if d <= 0.0 {
                return Ok(fail("`duration_seconds` must be more than 0"));
            }
            v["duration"] = json!(ticks(d));
        }
        let r = self.run("timeline.place", v).await;
        self.with_summary(r).await
    }

    #[tool(
        title = "Split clips",
        description = "Cut clips in two at a time (the razor). Default: every clip under `at_seconds` on unlocked tracks; `clip` or `track` limits it.",
        annotations(read_only_hint = false, destructive_hint = true, idempotent_hint = false, open_world_hint = false)
    )]
    async fn clip_split(&self, Parameters(p): Parameters<ClipSplitParams>) -> Result<CallToolResult, McpError> {
        let at = try_arg!(time_arg("at_seconds", p.at_seconds));
        let mut v = json!({"time": ticks(at)});
        if let Some(c) = p.clip {
            v["clip"] = json!(c);
        }
        if let Some(t) = p.track {
            v["track"] = json!(t);
        }
        let r = match self.run("timeline.razor", v).await {
            Ok(r) if r["cuts"].as_u64() == Some(0) => {
                return Ok(fail(format!("nothing to split at {at} s (no clip there, or the time is already an edit point)")));
            }
            r => r,
        };
        self.with_summary(r).await
    }

    #[tool(
        title = "Trim a clip",
        description = "Move a clip's in or out edge by `delta_seconds` (or `delta_frames`). Positive moves the edge later: in +1 drops the first second, out -1 drops the last second. `ripple` moves later clips with it, so no gap is left.",
        annotations(read_only_hint = false, destructive_hint = true, idempotent_hint = false, open_world_hint = false)
    )]
    async fn clip_trim(&self, Parameters(p): Parameters<ClipTrimParams>) -> Result<CallToolResult, McpError> {
        let mut v = json!({"clip": p.clip, "edge": p.edge.as_str(), "mode": if p.ripple.unwrap_or(false) { "ripple" } else { "regular" }});
        match (p.delta_seconds, p.delta_frames) {
            (Some(s), None) => v["delta"] = json!(ticks(try_arg!(delta_arg("delta_seconds", s)))),
            (None, Some(f)) => v["deltaFrames"] = json!(f.clamp(-10_000_000, 10_000_000)),
            _ => return Ok(fail("give exactly one of `delta_seconds` and `delta_frames`")),
        }
        let r = self.run("timeline.trim", v).await;
        self.with_summary(r).await
    }

    #[tool(
        title = "Move a clip",
        description = "Move a clip (and its linked sound or picture) to a new start time, optionally onto another track.",
        annotations(read_only_hint = false, destructive_hint = true, idempotent_hint = false, open_world_hint = false)
    )]
    async fn clip_move(&self, Parameters(p): Parameters<ClipMoveParams>) -> Result<CallToolResult, McpError> {
        let to = try_arg!(time_arg("to_seconds", p.to_seconds));
        let track = match p.track {
            Some(t) => json!(t),
            None => match self.track_of(p.clip).await {
                Some(t) => json!(t),
                None => return Ok(fail(format!("no clip {} in the active sequence", p.clip))),
            },
        };
        let v = json!({"moves": [{"clip": p.clip, "track": track, "time": ticks(to)}], "insert": p.insert.unwrap_or(false)});
        let r = self.run("timeline.move", v).await;
        self.with_summary(r).await
    }

    #[tool(
        title = "Delete clips",
        description = "Remove clips from the timeline. By default later clips move up to close the gap (ripple delete); `ripple: false` leaves the gap.",
        annotations(read_only_hint = false, destructive_hint = true, idempotent_hint = false, open_world_hint = false)
    )]
    async fn clip_delete(&self, Parameters(p): Parameters<ClipDeleteParams>) -> Result<CallToolResult, McpError> {
        if p.clips.is_empty() {
            return Ok(fail("`clips` is empty"));
        }
        let id = if p.ripple.unwrap_or(true) { "edit.rippleDelete" } else { "edit.clear" };
        let r = self.run(id, json!({"clips": p.clips})).await;
        self.with_summary(r).await
    }

    #[tool(
        title = "List effects",
        description = "Effects and transitions you can apply: id, name, kind, folder path and parameter ids.",
        annotations(read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false)
    )]
    async fn effect_list(&self, Parameters(p): Parameters<EffectListParams>) -> Result<CallToolResult, McpError> {
        let mut v = json!({});
        if let Some(k) = p.kind {
            v["kind"] = json!(format!("{k:?}"));
        }
        let f = p.filter.unwrap_or_default().to_ascii_lowercase();
        let r = self.run("effects.list", v).await.map(|all| {
            Value::Array(
                all.as_array()
                    .into_iter()
                    .flatten()
                    .filter(|e| f.is_empty() || ["id", "name", "path"].iter().any(|k| e[*k].as_str().is_some_and(|s| s.to_ascii_lowercase().contains(&f))))
                    .map(|e| json!({"id": e["id"], "name": e["name"], "kind": e["kind"], "path": e["path"], "params": e["params"]}))
                    .collect(),
            )
        });
        wrap(r)
    }

    #[tool(
        title = "Apply an effect",
        description = "Apply a video or audio effect to clips, e.g. \"Gaussian Blur\", \"Black & White\", \"lumetri\". Change its parameters with command_run effects.setParam {clip, effect: index, param, value}.",
        annotations(read_only_hint = false, destructive_hint = false, idempotent_hint = false, open_world_hint = false)
    )]
    async fn effect_apply(&self, Parameters(p): Parameters<EffectApplyParams>) -> Result<CallToolResult, McpError> {
        if p.clips.is_empty() {
            return Ok(fail("`clips` is empty"));
        }
        let r = self.run("effects.apply", json!({"clips": p.clips, "effect": p.effect})).await;
        self.with_summary(r).await
    }

    #[tool(
        title = "Add a transition",
        description = "Put a transition (default Cross Dissolve) on a clip's in or out edge; between two clips it spans the cut.",
        annotations(read_only_hint = false, destructive_hint = false, idempotent_hint = false, open_world_hint = false)
    )]
    async fn transition_add(&self, Parameters(p): Parameters<TransitionAddParams>) -> Result<CallToolResult, McpError> {
        let mut v = json!({"clip": p.clip});
        if let Some(e) = p.edge {
            v["edge"] = json!(e.as_str());
        }
        if let Some(e) = p.effect {
            v["effect"] = json!(e);
        }
        if let Some(f) = p.frames {
            if !(1..=100_000).contains(&f) {
                return Ok(fail(format!("`frames` must be between 1 and 100000, got {f}")));
            }
            v["frames"] = json!(f);
        }
        let id = if p.audio.unwrap_or(false) { "sequence.applyAudioTransition" } else { "sequence.applyVideoTransition" };
        let r = self.run(id, v).await;
        self.with_summary(r).await
    }

    #[tool(
        title = "Add a title",
        description = "Add a text title as a new graphic clip on the lowest free video track above the picture at that time.",
        annotations(read_only_hint = false, destructive_hint = false, idempotent_hint = false, open_world_hint = false)
    )]
    async fn title_add(&self, Parameters(p): Parameters<TitleAddParams>) -> Result<CallToolResult, McpError> {
        let at = try_arg!(time_arg("at_seconds", p.at_seconds.unwrap_or(0.0)));
        let dur = try_arg!(time_arg("duration_seconds", p.duration_seconds.unwrap_or(5.0)));
        let mut v = json!({"text": p.text, "time": ticks(at), "seconds": dur, "newClip": true});
        if let Some(s) = p.size {
            if !(s.is_finite() && s > 0.0 && s <= 10_000.0) {
                return Ok(fail(format!("`size` must be between 0 and 10000 pixels, got {s}")));
            }
            v["size"] = json!(s);
        }
        if let Some([x, y]) = p.position {
            if !(x.is_finite() && y.is_finite()) {
                return Ok(fail("`position` must be two finite numbers"));
            }
            v["position"] = json!([x, y]);
        }
        if let Some(f) = p.font {
            v["font"] = json!(f);
        }
        let r = self.run("graphics.newText", v).await;
        self.with_summary(r).await
    }

    #[tool(
        title = "Export the video",
        description = "Render the active sequence to a file and wait until it is written (reports MCP progress; cancellable). The format follows the extension (.mp4 = H.264) unless `preset` or `format` is given; `start_seconds`/`end_seconds` export part of it.",
        annotations(read_only_hint = false, destructive_hint = false, idempotent_hint = true, open_world_hint = false)
    )]
    async fn export_video(&self, Parameters(p): Parameters<ExportVideoParams>, context: RequestContext<RoleServer>) -> Result<CallToolResult, McpError> {
        let v = try_arg!(Self::export_params(&p));
        Ok(self.run_long("file.exportMedia", v, &context).await)
    }

    #[tool(
        title = "Save the project",
        description = "Save the project (.fcproj). Nothing is saved automatically: save before you finish. With `path` it saves there and the project takes that file.",
        annotations(read_only_hint = false, destructive_hint = false, idempotent_hint = true, open_world_hint = false)
    )]
    async fn project_save(&self, Parameters(p): Parameters<ProjectSaveParams>) -> Result<CallToolResult, McpError> {
        let r = match p.path {
            Some(path) => self.run("file.saveAs", json!({"path": path})).await,
            None => self.run("file.save", json!({})).await,
        };
        wrap(r)
    }

    #[tool(
        title = "Undo",
        description = "Undo the last edit.",
        annotations(read_only_hint = false, destructive_hint = false, idempotent_hint = false, open_world_hint = false)
    )]
    async fn edit_undo(&self) -> Result<CallToolResult, McpError> {
        let r = self.run("edit.undo", json!({})).await;
        self.with_summary(r).await
    }

    #[tool(
        title = "Redo",
        description = "Redo the last undone edit.",
        annotations(read_only_hint = false, destructive_hint = false, idempotent_hint = false, open_world_hint = false)
    )]
    async fn edit_redo(&self) -> Result<CallToolResult, McpError> {
        let r = self.run("edit.redo", json!({})).await;
        self.with_summary(r).await
    }

    #[tool(
        title = "Render a contact sheet",
        description = "One PNG with up to 16 evenly spaced frames of the active sequence, left to right, top to bottom (the text lists each frame's time). Use it to check an edit at a glance before exporting.",
        annotations(read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false)
    )]
    async fn render_contact_sheet(&self, Parameters(p): Parameters<ContactSheetParams>) -> Result<CallToolResult, McpError> {
        let count = p.count.unwrap_or(8);
        if !(1..=MAX_SHEET_FRAMES).contains(&count) {
            return Ok(fail(format!("`count` must be between 1 and {MAX_SHEET_FRAMES}, got {count}")));
        }
        let start = try_arg!(time_arg("start_seconds", p.start_seconds.unwrap_or(0.0)));
        let end = match p.end_seconds {
            Some(e) => Some(try_arg!(time_arg("end_seconds", e))),
            None => None,
        };
        let max_side = p.max_side.unwrap_or(1280).clamp(64, 4096);
        let Backend::Headless(s) = &*self.backend else {
            return Ok(fail("render_contact_sheet renders headless; in bridge mode use render_frame at each time"));
        };
        let s = s.clone();
        let r = tokio::task::spawn_blocking(move || contact_sheet(&s, count, start, end, max_side)).await.unwrap_or_else(|e| Err(join_error(e)));
        Ok(match r {
            Ok((png, times)) => {
                let times: Vec<String> = times.iter().map(|t| format!("{t:.2}")).collect();
                png_result(&png, &format!("contact sheet, frames at seconds: {}", times.join(", ")))
            }
            Err(e) => fail(e),
        })
    }
}

/// Render `count` frames between `start` and `end` (default: the last frame) and tile them in a
/// grid whose longest side is `max_side`. Returns the PNG and each frame's time in seconds.
fn contact_sheet(
    s: &std::sync::Mutex<filmcraft_engine::Session>,
    count: u32,
    start: f64,
    end: Option<f64>,
    max_side: u32,
) -> Result<(Vec<u8>, Vec<f64>), AutomationError> {
    let g = s.lock().unwrap_or_else(PoisonError::into_inner);
    let q = g.active_sequence().ok_or_else(|| AutomationError::Other("no active sequence".into()))?;
    let (w, h) = (q.settings.width.max(1), q.settings.height.max(1));
    let last = (q.duration() - q.settings.frame_rate.frame_duration()).max(Tick::ZERO).seconds();
    let end = end.unwrap_or(last).min(last).max(start);
    let cols = (1..=count).find(|c| c * c >= count).unwrap_or(count).max(1);
    let rows = count.div_ceil(cols).max(1);
    // tile size: the grid's longest side is max_side, the tiles keep the sequence's shape
    let fit = (max_side as f32 / (cols as f32 * w as f32).max(rows as f32 * h as f32)).min(1.0);
    let (tw, th) = (((w as f32 * fit) as u32).max(1), ((h as f32 * fit) as u32).max(1));
    let mut sheet = image::RgbaImage::from_pixel(tw.saturating_mul(cols), th.saturating_mul(rows), image::Rgba([0, 0, 0, 255]));
    let mut times = Vec::new();
    for i in 0..count {
        let t = if count == 1 { start } else { start + (end - start) * f64::from(i) / f64::from(count - 1) };
        let img = g.try_render_program_at(fit, Tick::from_seconds_f64(t)).map_err(|e| AutomationError::Other(e.to_string()))?;
        let tile = image::RgbaImage::from_raw(img.w as u32, img.h as u32, img.over_black_rgba8()).ok_or_else(|| AutomationError::Other("bad frame".into()))?;
        let tile = if tile.dimensions() == (tw, th) { tile } else { image::imageops::resize(&tile, tw, th, image::imageops::FilterType::Triangle) };
        image::imageops::overlay(&mut sheet, &tile, i64::from((i % cols) * tw), i64::from((i / cols) * th));
        times.push(t);
    }
    drop(g);
    let (sw, sh) = sheet.dimensions();
    let png = crate::png_rgba(sw, sh, sheet.into_raw(), 0)?;
    Ok((png, times))
}

#[cfg(test)]
mod tests {
    use filmcraft_engine::Session;

    use super::*;

    fn text(r: &CallToolResult) -> String {
        r.content.iter().filter_map(|c| c.as_text()).map(|t| t.text.clone()).collect::<Vec<_>>().join("\n")
    }

    fn ok(r: Result<CallToolResult, McpError>) -> Value {
        let r = r.unwrap();
        assert_ne!(r.is_error, Some(true), "{}", text(&r));
        serde_json::from_str(&text(&r)).unwrap()
    }

    fn err(r: Result<CallToolResult, McpError>) -> String {
        let r = r.unwrap();
        assert_eq!(r.is_error, Some(true), "expected an error: {}", text(&r));
        text(&r)
    }

    fn clips(timeline: &Value, track: &str) -> Vec<Value> {
        timeline["video"].as_array().unwrap().iter().chain(timeline["audio"].as_array().unwrap()).find(|t| t["track"] == track).unwrap()["clips"]
            .as_array()
            .unwrap()
            .clone()
    }

    fn close(a: &Value, b: f64) -> bool {
        a.as_f64().is_some_and(|a| (a - b).abs() < 0.05)
    }

    /// A 320x180 still written to a temp dir.
    fn still(name: &str) -> (std::path::PathBuf, String) {
        let dir = std::env::temp_dir().join(format!("filmcraft-mcp-edit-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("frame.png");
        image::RgbaImage::from_fn(320, 180, |x, _| image::Rgba([(x % 256) as u8, 80, 160, 255])).save(&path).unwrap();
        (dir, path.to_string_lossy().to_string())
    }

    /// The everyday edit from an empty project: import, a sequence from the clip, assemble, cut,
    /// trim, move, delete, title, effect, transition, look at it, save, undo.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_edit_from_an_empty_project() {
        let (dir, png) = still("assemble");
        let m = FilmcraftMcp::headless(Session::default());
        let r = ok(m.media_import(Parameters(crate::server::ImportParams { paths: Some(vec![png]), ..Default::default() })).await);
        let item = r["items"][0].as_u64().unwrap();
        // no sequence yet: placing says how to make one
        assert!(
            err(m
                .clip_place(Parameters(ClipPlaceParams {
                    item,
                    track: None,
                    audio_track: None,
                    at_seconds: None,
                    source_in_seconds: None,
                    duration_seconds: None,
                    insert: None
                }))
                .await)
            .contains("sequence_new")
        );
        let r = ok(m
            .sequence_new(Parameters(SequenceNewParams {
                name: Some("Cut".into()),
                width: Some(640),
                height: Some(360),
                fps: Some(25.0),
                ..Default::default()
            }))
            .await);
        assert_eq!(r["timeline"]["width"], 640);
        // two 2 s clips, appended one after the other
        for _ in 0..2 {
            ok(m.clip_place(Parameters(ClipPlaceParams {
                item,
                track: Some("V1".into()),
                audio_track: None,
                at_seconds: None,
                source_in_seconds: None,
                duration_seconds: Some(2.0),
                insert: None,
            }))
            .await);
        }
        let t = ok(m.timeline_summary().await);
        let v1 = clips(&t, "V1");
        assert_eq!(v1.len(), 2, "{t}");
        assert!(close(&v1[1]["start"], 2.0) && close(&v1[1]["end"], 4.0), "{t}");
        assert!(close(&t["duration"], 4.0), "{t}");
        // split the second clip at 3 s
        let r = ok(m.clip_split(Parameters(ClipSplitParams { at_seconds: 3.0, clip: None, track: Some("V1".into()) })).await);
        let v1 = clips(&r["timeline"], "V1");
        assert_eq!(v1.len(), 3, "{r}");
        // trim half a second off the first clip's head, rippling the rest up
        let first = v1[0]["clip"].as_u64().unwrap();
        let r =
            ok(m.clip_trim(Parameters(ClipTrimParams { clip: first, edge: Edge::In, delta_seconds: Some(0.5), delta_frames: None, ripple: Some(true) })).await);
        assert!(close(&r["timeline"]["duration"], 3.5), "{r}");
        // move the last piece to V2 at 0 s
        let last = clips(&r["timeline"], "V1").last().unwrap()["clip"].as_u64().unwrap();
        let r = ok(m.clip_move(Parameters(ClipMoveParams { clip: last, to_seconds: 0.0, track: Some("V2".into()), insert: None })).await);
        assert_eq!(clips(&r["timeline"], "V2").len(), 1, "{r}");
        assert_eq!(clips(&r["timeline"], "V1").len(), 2, "{r}");
        // delete it again
        let r = ok(m.clip_delete(Parameters(ClipDeleteParams { clips: vec![last], ripple: Some(false) })).await);
        assert!(clips(&r["timeline"], "V2").is_empty(), "{r}");
        // a title over the first second, an effect, a dissolve between the two clips
        let r = ok(m
            .title_add(Parameters(TitleAddParams {
                text: "Hello".into(),
                at_seconds: Some(0.0),
                duration_seconds: Some(1.0),
                size: Some(48.0),
                position: None,
                font: None,
            }))
            .await);
        assert!(r["result"]["clip"].is_u64(), "{r}");
        let fx = ok(m.effect_list(Parameters(EffectListParams { kind: Some(EffectKind::Video), filter: Some("blur".into()) })).await);
        assert!(fx.as_array().unwrap().iter().any(|e| e["id"] == "gaussian_blur"), "{fx}");
        let r = ok(m.effect_apply(Parameters(EffectApplyParams { clips: vec![first], effect: "Gaussian Blur".into() })).await);
        assert!(clips(&r["timeline"], "V1")[0]["effects"].as_array().unwrap().iter().any(|e| e == "gaussian_blur"), "{r}");
        let r = ok(m.transition_add(Parameters(TransitionAddParams { clip: first, edge: Some(Edge::Out), effect: None, frames: Some(10), audio: None })).await);
        let transitions = r["timeline"]["video"][0]["transitions"].as_array().unwrap().clone();
        assert_eq!(transitions.len(), 1, "{r}");
        // look at it
        let sheet = m.render_contact_sheet(Parameters(ContactSheetParams { count: Some(4), max_side: Some(320), ..Default::default() })).await.unwrap();
        assert_ne!(sheet.is_error, Some(true), "{}", text(&sheet));
        assert!(sheet.content.iter().any(|c| c.as_image().is_some()));
        assert!(text(&sheet).contains("0.00"), "{}", text(&sheet));
        // save, then undo the transition
        let proj = dir.join("cut.fcproj").to_string_lossy().to_string();
        ok(m.project_save(Parameters(ProjectSaveParams { path: Some(proj.clone()) })).await);
        assert!(std::path::Path::new(&proj).exists());
        let r = ok(m.edit_undo().await);
        assert!(r["timeline"]["video"][0]["transitions"].as_array().unwrap().is_empty(), "{r}");
        ok(m.edit_redo().await);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `sequence_new {from_item}` makes a sequence that fits the clip, with the clip on it.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_sequence_from_a_clip() {
        let (dir, png) = still("from-item");
        let m = FilmcraftMcp::headless(Session::default());
        let r = ok(m.media_import(Parameters(crate::server::ImportParams { text: Some(format!("\n{png}\n")), ..Default::default() })).await);
        let item = r["items"][0].as_u64().unwrap();
        let r = ok(m.sequence_new(Parameters(SequenceNewParams { from_item: Some(item), ..Default::default() })).await);
        assert_eq!(clips(&r["timeline"], "V1").len(), 1, "{r}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Never crash: hostile numbers and ids are error results, and the session keeps working.
    #[tokio::test(flavor = "multi_thread")]
    async fn hostile_arguments_are_errors() {
        let mut s = Session::default();
        s.execute("file.openDemoProject", json!({})).unwrap();
        let m = FilmcraftMcp::headless(s);
        for at in [f64::NAN, f64::INFINITY, -1.0, 1e300] {
            err(m.clip_split(Parameters(ClipSplitParams { at_seconds: at, clip: None, track: None })).await);
            err(m.clip_move(Parameters(ClipMoveParams { clip: 1, to_seconds: at, track: None, insert: None })).await);
            err(m
                .title_add(Parameters(TitleAddParams {
                    text: "x".into(),
                    at_seconds: Some(at),
                    duration_seconds: None,
                    size: None,
                    position: None,
                    font: None,
                }))
                .await);
            err(m.render_contact_sheet(Parameters(ContactSheetParams { start_seconds: Some(at), ..Default::default() })).await);
        }
        for d in [f64::NAN, f64::NEG_INFINITY, -1e300] {
            err(m.clip_trim(Parameters(ClipTrimParams { clip: 1, edge: Edge::Out, delta_seconds: Some(d), delta_frames: None, ripple: None })).await);
        }
        err(m.clip_trim(Parameters(ClipTrimParams { clip: 1, edge: Edge::Out, delta_seconds: None, delta_frames: None, ripple: None })).await);
        err(m.clip_trim(Parameters(ClipTrimParams { clip: u64::MAX, edge: Edge::Out, delta_seconds: None, delta_frames: Some(i64::MIN), ripple: None })).await);
        for count in [0, 17, u32::MAX] {
            err(m.render_contact_sheet(Parameters(ContactSheetParams { count: Some(count), ..Default::default() })).await);
        }
        err(m.clip_move(Parameters(ClipMoveParams { clip: u64::MAX, to_seconds: 1.0, track: None, insert: None })).await);
        err(m.clip_delete(Parameters(ClipDeleteParams { clips: vec![], ripple: None })).await);
        err(m.effect_apply(Parameters(EffectApplyParams { clips: vec![u64::MAX], effect: "no such effect".into() })).await);
        err(m.transition_add(Parameters(TransitionAddParams { clip: 1, edge: None, effect: None, frames: Some(i64::MAX), audio: None })).await);
        err(m
            .title_add(Parameters(TitleAddParams {
                text: "x".into(),
                at_seconds: None,
                duration_seconds: None,
                size: Some(f64::NAN),
                position: None,
                font: None,
            }))
            .await);
        err(m
            .title_add(Parameters(TitleAddParams {
                text: "x".into(),
                at_seconds: None,
                duration_seconds: None,
                size: None,
                position: Some([f64::NAN, 0.0]),
                font: None,
            }))
            .await);
        err(m
            .clip_place(Parameters(ClipPlaceParams {
                item: 1,
                track: None,
                audio_track: None,
                at_seconds: Some(0.0),
                source_in_seconds: None,
                duration_seconds: Some(0.0),
                insert: None,
            }))
            .await);
        err(m
            .clip_place(Parameters(ClipPlaceParams {
                item: u64::MAX,
                track: None,
                audio_track: None,
                at_seconds: None,
                source_in_seconds: None,
                duration_seconds: None,
                insert: None,
            }))
            .await);
        err(m.media_import(Parameters(crate::server::ImportParams::default())).await);
        // a huge sheet is capped, not allocated
        let r = m.render_contact_sheet(Parameters(ContactSheetParams { count: Some(16), max_side: Some(u32::MAX), ..Default::default() })).await.unwrap();
        assert_ne!(r.is_error, Some(true), "{}", text(&r));
        // still serving
        ok(m.timeline_summary().await);
    }

    #[test]
    fn export_params_follow_the_extension() {
        let p = |path: &str| ExportVideoParams {
            path: path.into(),
            preset: None,
            format: None,
            width: None,
            height: None,
            quality: None,
            audio: None,
            start_seconds: None,
            end_seconds: None,
            loudness_lufs: None,
        };
        assert_eq!(FilmcraftMcp::export_params(&p("/o/a.MP4")).unwrap()["format"], "h264");
        assert!(FilmcraftMcp::export_params(&p("/o/a.xyz")).unwrap_err().contains("format"));
        let v =
            FilmcraftMcp::export_params(&ExportVideoParams { start_seconds: Some(1.0), end_seconds: Some(2.0), quality: Some(200), ..p("/o/a.mov") }).unwrap();
        assert_eq!(
            (v["format"].as_str(), v["range"].as_str(), v["quality"].as_u64(), v["wait"].as_bool()),
            (Some("prores"), Some("custom"), Some(100), Some(true))
        );
        assert!(FilmcraftMcp::export_params(&ExportVideoParams { end_seconds: Some(f64::NAN), ..p("/o/a.mp4") }).is_err());
        assert!(FilmcraftMcp::export_params(&ExportVideoParams { loudness_lufs: Some(f64::NAN), ..p("/o/a.mp4") }).is_err());
        assert_eq!(FilmcraftMcp::export_params(&ExportVideoParams { loudness_lufs: Some(-14.0), ..p("/o/a.mp4") }).unwrap()["loudnessLufs"], -14.0);
        let v = FilmcraftMcp::export_params(&ExportVideoParams { preset: Some("YouTube 1080p".into()), ..p("/o/a.bin") }).unwrap();
        assert!(v.get("format").is_none() && v["preset"] == "YouTube 1080p", "{v}");
        assert_eq!(format_for("out/clip.MXF"), Some("mxf-op1a"));
        assert_eq!(format_for("noext"), None);
    }
}
