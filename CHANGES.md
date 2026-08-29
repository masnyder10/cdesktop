# Statement of Changes

This repository is a fork of [`cdesktop-ai/cdesktop`](https://github.com/cdesktop-ai/cdesktop),
licensed under the Apache License, Version 2.0.

As required by Section 4(b) of the Apache License 2.0, this file records the
modifications made in this fork relative to upstream.

The upstream `LICENSE` and `NOTICE` files are retained unmodified, including
the attributions to `BloopAI/vibe-kanban` (Apache 2.0) and
`farion1231/cc-switch` (MIT).

This fork is not affiliated with, endorsed by, or sponsored by Anthropic.

## Changes

### Windows desktop build support (2026-08-28)

Upstream wires up Tauri but ships no Windows installer; its local build script
is POSIX-only. This fork makes `cargo tauri build` produce working `.msi` and
NSIS `.exe` installers natively on Windows 11.

- **`crates/tauri-app/tauri.conf.json`**
  - `productName`: `cdesktop` -> `cdesktop-mr`, and `identifier`:
    `ai.cdesktop` -> `com.littleway.cdesktop`, so a fork build never collides
    with an upstream install.
  - Added `build.beforeBuildCommand` running
    `pnpm --filter @vibe/local-web run build`. Upstream defined only
    `beforeDevCommand`, so a release build did not produce
    `packages/local-web/dist`, which `crates/server` embeds at compile time via
    `rust-embed`. Without this the app builds successfully but renders a blank
    window.
  - `plugins.updater.endpoints`: replaced the `__TAURI_UPDATE_ENDPOINT__`
    placeholder with `https://localhost/disabled`. The placeholder is not a
    valid URL and fails Tauri config validation; upstream substitutes a real
    endpoint in CI.
  - `bundle.createUpdaterArtifacts`: `true` -> `false`. Signing updater
    artifacts requires upstream's `TAURI_SIGNING_PRIVATE_KEY`, which is not
    available to a fork.

- **`crates/tauri-app/icons/icon.ico`**
  - Regenerated from the existing `icons/icon.png` (512x512) as a
    multi-resolution icon containing 16, 32, 48, 64, 128, and 256 px entries at
    32 bpp. The upstream file contained a single 16x16 entry, which Windows
    upscales into a blurry desktop, Start menu, and taskbar icon. No other icon
    asset was regenerated and the source artwork is unchanged.

- **`crates/tauri-app/src/main.rs`**
  - `create_window` hardcoded `.title("cdesktop")`, which does not follow
    `productName`. Changed to `"cdesktop-mr"` so a fork build is
    distinguishable from an upstream one in the title bar, taskbar, and
    Alt-Tab. This is the only Rust source change in the fork; behaviour is
    otherwise untouched.

- **`WINDOWS-BUILD.md`** (new)
  - Documents prerequisites, exact build commands, artifact locations, the
    `cargo tauri info` Visual Studio detection false negative, and the
    blank-window failure mode.

- **`CHANGES.md`** (new)
  - This file.

### Claude Code history import and storage isolation (2026-08-29)

- **`crates/server/src/routes/claude_import.rs`** (new)
  - Imports existing Claude Code CLI chat history. The CLI stores transcripts as
    JSONL under `~/.claude/projects/<encoded-cwd>/<session-id>.jsonl`, but
    nothing in the desktop app ever read them: `discover_projects` lives in
    `crates/review`, and neither `server` nor `tauri-app` depends on that crate.
  - Transcripts are parsed into `NormalizedEntry` values and materialised into
    cdesktop's own storage (`repos` -> `workspaces` -> `sessions` ->
    `execution_processes` -> `coding_agent_turns`) together with an execution log
    file written in the same JSONL-of-`LogMsg` form the live agent path uses.
    Imported sessions are therefore ordinary cdesktop sessions and render in the
    existing sidebar and transcript viewer with no frontend changes.
  - The working directory is read from the `cwd` field inside the records rather
    than decoded from the directory name, because that encoding is lossy: both
    path separators and underscores become `-`, so `Bounty_Engine` and
    `Bounty-Engine` map to the same directory.
  - Claude's own `ai-title` record supplies the session title, falling back to a
    truncated first prompt.
  - Idempotent: Claude's session id is stored on
    `coding_agent_turns.agent_session_id` and re-imports skip anything already
    present.
  - `~/.claude` is read-only to this code. Transcripts are parsed, never written,
    moved, or reformatted.
  - Endpoints: `GET /api/claude-import/scan` (dry run) and
    `POST /api/claude-import/run`.
  - `POST /api/claude-import/run` accepts `{"refresh": true}` to re-sync sessions
    that were already imported. An import is a point-in-time snapshot, so a
    session still being written when it was imported would otherwise stay frozen
    at that point forever, since the idempotency check skips it. Refresh rewrites
    the transcript file in place, keeping the workspace, session and process ids
    stable, and updates the title (Claude rewrites `ai-title` as a conversation
    develops).
  - Runs automatically shortly after server startup, spawned rather than awaited
    so a large store never delays boot. Only sessions whose transcript is newer
    than the last written copy are rewritten, so a normal launch touches almost
    nothing (2 of 51 files in practice).
  - Emits a `TokenUsageInfo` entry per imported session, which feeds the existing
    `ContextUsageGauge`. Context occupancy is the input side plus both cache
    buckets, excluding output tokens, matching the live gauge. The context window
    is inferred from usage above 200K when the transcript records a plain model
    id without the `[1m]` marker; taking the id at face value would otherwise
    peg a 1M session at 100%.
  - Stamps the imported rows with the transcript's real first and last message
    times. Rows otherwise carry the moment of import, leaving all 51 workspaces
    with an identical timestamp; since the sidebar orders by
    `MAX(execution_processes.completed_at, workspaces.updated_at)`, imported
    sessions had no meaningful order at all. Refresh re-stamps them, so a growing
    session moves up the list.
  - Fidelity: `tool_result` blocks are skipped because `NormalizedEntry` has no
    ToolResult variant, which is also why the live normaliser drops them.
    `thinking` blocks are skipped when empty; in current transcripts the
    reasoning text is not stored, only an opaque `signature`.

- **`crates/server/src/routes/mod.rs`**
  - Declare and merge the `claude_import` router.

- **`crates/utils/src/assets.rs`, `crates/utils/src/lib.rs`**
  - Data and cache directories moved from upstream's
    `ProjectDirs::from("ai", "cdesktop", ...)` to
    `("com", "littleway", "cdesktop-mr")`. Renaming `productName` alone does not
    move the data directory, so a fork build would otherwise share its SQLite
    database and execution logs with an upstream cdesktop install.

- **`crates/tauri-app/src/main.rs`, `crates/tauri-app/Cargo.toml`**
  - Add `tauri-plugin-single-instance`, registered before all other plugins as
    that plugin requires. Without it every launch started another backend server
    against the same SQLite database; a second instance now focuses the existing
    window instead.

### Build-generated files (not hand-edited)

These were regenerated by `tauri-build` as a side effect of building on
Windows, and are recorded here for completeness:

- **`crates/tauri-app/gen/schemas/windows-schema.json`** (new) — emitted the
  first time the app is built on a Windows host, alongside the existing
  `desktop-schema.json`, `linux-schema.json`, and `macOS-schema.json`.
- **`crates/tauri-app/gen/schemas/capabilities.json`** — regenerated from
  `crates/tauri-app/capabilities/default.json`. The committed copy was stale:
  it still read `"Default capabilities for Vibe Kanban desktop app"` while the
  hand-written source already read `"... for cdesktop desktop app"`. This is
  pre-existing upstream drift corrected by the build, not a fork change.
- **`packages/local-web/src/routeTree.gen.ts`** — regenerated by the TanStack
  Router Vite plugin during the frontend build. Content is unchanged; only
  line endings differ.
- **`crates/tauri-app/Cargo.toml`** and
  `crates/tauri-app/gen/schemas/desktop-schema.json` show as modified with an
  empty diff. That is LF -> CRLF normalization from Windows tooling, not a
  content change.

No application features were added or removed, no dependencies were upgraded,
no UI was redesigned, and no analytics or telemetry was introduced.
