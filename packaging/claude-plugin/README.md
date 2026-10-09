# FilmCraft plugin for Claude Code

Lets Claude edit video with FilmCraft: import media, cut, trim, add titles, effects and transitions,
look at frames, export, and save a project you can open in the FilmCraft app.

It adds:

- **`filmcraft` MCP server**: a headless FilmCraft session (no window) with typed editing tools,
  frame rendering and access to every engine command. See [docs/agents.md](../../docs/agents.md) §1.
- **`filmcraft-live` MCP server**: the same tools plus `ui_*`, driving the desktop app started with
  `filmcraft --control 9876`, so you can watch Claude work. It connects on first use, so it does no
  harm when the app isn't running.
- **`edit-video` skill**: how to use the tools for an edit, from import to export.

## Install

```sh
claude plugin marketplace add storytold/filmcraft
claude plugin install filmcraft@filmcraft
```

(or `/plugin marketplace add storytold/filmcraft` and `/plugin install filmcraft@filmcraft` in a session)

The servers run `filmcraft-cli`, found in this order:

1. `$FILMCRAFT_CLI`, the path to the binary.
2. `filmcraft-cli` on `PATH`. The release packages install it (see the main README's Downloads).
3. `target/release/filmcraft-cli` in a FilmCraft checkout: the one the plugin is in, `$FILMCRAFT_REPO`,
   or the directory Claude Code was started in. If it hasn't been built yet and `cargo` is installed,
   the launcher builds it first.

Building takes a few minutes the first time, longer than Claude Code waits for a server to start. Build
it before your first session (`cargo build --release -p filmcraft-cli`), or reconnect with `/mcp`
once the build has finished.

Inside this repository you don't need the plugin: the root `.mcp.json` starts the same launcher
(`filmcraft` = live app, `filmcraft-headless` = demo project).

## Try it

> Import ~/Movies/trip.mp4, cut it down to the best 20 seconds with a title "Lisbon" at the start, and
> export it to ~/Movies/trip-20s.mp4.

Claude imports the clip, makes a sequence, looks at a contact sheet, splits and deletes, adds the
title, checks the result and exports. Ask it to save the project to reopen the edit in FilmCraft.

## Troubleshooting

- **`filmcraft` failed to connect**: run `packaging/claude-plugin/bin/filmcraft-mcp </dev/null` in a
  shell. It says which `filmcraft-cli` it found, or why it found none.
- **The `ui_*` tools fail**: start the app with `filmcraft --control 9876` and use `filmcraft-live`.
- **Export is slow**: rendering is on the CPU. Export a part with `start_seconds`/`end_seconds`, or
  smaller with `width`/`height`, to check an edit first.
