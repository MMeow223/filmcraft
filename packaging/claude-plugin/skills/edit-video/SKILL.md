---
name: edit-video
description: Edit video with FilmCraft's MCP tools. Use when the user asks to cut, trim, assemble, splice, shorten, caption, title, colour, mix or export video or audio files, make a highlight reel or rough cut, or edit a FilmCraft project (.fcproj) — even when they don't name FilmCraft.
---

# Editing video with FilmCraft

FilmCraft is a non-linear video editor (Premiere Pro-class) with an MCP server. The `filmcraft`
server runs it headless: an empty project, no window. `filmcraft-live` drives the desktop app the
user has open (`filmcraft --control 9876`), so they can watch; use it only when they ask for that.

## The loop

1. **Import**: `media_import {paths: ["/abs/clip1.mp4", …]}` → project item ids in `items`.
   Paths must be absolute; check `errors` in the result.
2. **Sequence**: `sequence_new {from_item: <id>}` makes a sequence that matches the first clip's size
   and frame rate, with that clip on it. Use `width`/`height`/`fps` instead for a fixed delivery
   format (1080x1920 for vertical, 1920x1080 at 25 or 30 for most landscape).
3. **Assemble**: `clip_place {item}` appends at the end of the sequence. Use
   `source_in_seconds` + `duration_seconds` to take only part of a clip, `at_seconds` to put it
   somewhere else, `track: "V2"` to put it above, `insert: true` to push later clips right.
4. **Cut**: `clip_split {at_seconds}`, then `clip_delete {clips: [id]}` (closes the gap by
   default). `clip_trim {clip, edge: "in"|"out", delta_seconds, ripple: true}` shortens or
   lengthens one end: in +1 drops the first second, out -1 drops the last. `clip_move` repositions.
5. **Dress**: `title_add {text, at_seconds, duration_seconds}`, `transition_add {clip, edge}`
   (Cross Dissolve by default), `effect_apply {clips, effect}`; `effect_list {filter}` finds names.
6. **Look** before you export: `render_contact_sheet` (up to 16 frames in one image) for the whole
   cut, `render_frame {seconds}` for one moment. Check titles are readable and nothing is black
   where it shouldn't be.
7. **Export**: `export_video {path: "/abs/out.mp4"}` waits until the file is written. The
   extension picks the format (.mp4 H.264, .mov ProRes, .gif, .wav). `start_seconds`/`end_seconds`
   export part of it; `width`/`height` scale it; `loudness_lufs: -14` normalises the sound for
   streaming.
8. **Save**: `project_save {path: "/abs/name.fcproj"}`. Nothing is saved automatically; save
   whenever the user may want to reopen the edit (they can open it in the FilmCraft app).

Every edit tool returns `{result, timeline}`: the timeline is the same as `timeline_summary` (tracks
`V1`, `A1`… with clip ids, items, names, and start/end/sourceIn/sourceOut in seconds). Read ids
from it rather than guessing. A wrong edit: `edit_undo`.

## Things to know

- Time is in seconds on the timeline. A clip's `sourceIn`/`sourceOut` are seconds in its media.
- Video with sound lands as two linked clips (V1 + A1). Split, trim, move and delete act on both.
- Clip ids change when a clip is split; take fresh ids from the returned timeline.
- To find good cut points in footage you haven't seen, place it, then `render_contact_sheet` with
  `start_seconds`/`end_seconds` around the part you are looking at.
- Probe a file before importing with `filmcraft-cli probe <file>` in a shell when you need its
  codec, size or duration up front.

## Beyond the typed tools

About 600 engine commands cover the rest: colour (`lumetri.*`), audio (`mixer.*`,
`essentialSound.*`, `clip.audioGain`), captions (`captions.*`), markers, speed
(`clip.speedDuration`), multicam, nesting, and EDL/FCPXML/OTIO/AAF interchange.

- `command_list {filter: "caption"}` lists ids with a `params` doc for each.
- `command_run {id, params}` runs one; `command_batch {steps}` runs several.
- Engine params take ticks (254 016 000 000 per second), or `seconds`, `frame` or `timecode`
  where the doc says `time`.
- Set effect parameters with `command_run {id: "effects.setParam", params: {clip, effect: <index
  in the clip's effects>, param, value}}`; `effect_list` shows each effect's parameter ids.

## Example: "keep the best 10 seconds of interview.mp4 and add a title"

```
media_import {paths: ["/Users/me/interview.mp4"]}                → items: [3]
sequence_new {from_item: 3}                                       → V1/A1 clip 7, 0–94.2 s
render_contact_sheet {count: 12}                                  → pick 31–41 s
clip_split {at_seconds: 41}; clip_split {at_seconds: 31}
clip_delete {clips: [<clip after 41 s>]}; clip_delete {clips: [<clip before 31 s>]}
title_add {text: "Meet Ana", at_seconds: 0, duration_seconds: 3}
render_contact_sheet {count: 6}                                   → check
export_video {path: "/Users/me/interview-10s.mp4"}
project_save {path: "/Users/me/interview.fcproj"}
```
