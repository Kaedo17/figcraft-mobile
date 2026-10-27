# FigCraft architecture

> **Last reviewed:** 2026-10-10 · **Last updated:** 2026-10-26 · **Change:** minor (Android: the built-in file browser serves every file command over the app's storage; previously minor: Android groundwork: the app is a library and `figcraft-android` is the NativeActivity entry crate) · **Target:** Figma Design

How FigCraft is built today. Engine first, a thin swappable UI, everything a command; a Figma-shaped
data model (a scene graph of frames with auto layout, components and variables). ~108,800 lines of
Rust (2026-10-10).

## Crates and layers

`cargo xtask layers` enforces the layering (`xtask/src/layers.rs`): a crate may depend only on
lower layers. Nothing below L6 depends on egui, eframe, rfd or winit, so the UI crate can be
replaced.

| Layer | Crate | Lines | Role |
|---|---|---|---|
| L0 | `figcraft-geom` | ~0.5k | Affine transforms, shape path builders (rounded and smoothed corners, polygons, stars, arcs) on kurbo |
| L0 | `figcraft-color` | ~1.3k | Colours (HSB/HSL, hex/CSS, Display P3), paints (solid, gradients, image), blend modes |
| L1 | `figcraft-doc` | ~8.5k | Document model: pages, node tree in a persistent map (`imbl`, stable ids), properties, components, styles, variables, prototype data, comments, versions. Serde everywhere |
| L2 | `figcraft-layout` | ~1.8k | Auto layout (horizontal, vertical, wrap, grid; hug/fill/fixed; min/max), constraints, layout grid geometry |
| L2 | `figcraft-text` | ~3.1k | Font database (optional embedded craft-fonts + system scan), shaping (harfrust), bidi, line layout, glyph outlines (skrifa), variable fonts |
| L2 | `figcraft-collab` | ~3.4k | Real-time collaboration core (no networking): synced object/property model, change sets, fractional positions, server ordering and validation, client reconciliation, wire messages |
| L2 | `figcraft-plugin` | ~1.0k | Plugin manifest and the WebAssembly sandbox (wasmi, metered, memory cap, no file system, network or clock) |
| L2 | `figcraft-pathops` | ~1.1k | Booleans (linesweeper), outline stroke with alignment, arrowheads |
| L3 | `figcraft-render` | ~5.4k | Scene → pixels on tiny-skia (CPU): isolated layers for opacity/blend/effects, clips, masks, shadows, blurs (uniform, progressive, background), noise, texture, glass, text; banded in parallel; retained rendering of only what changed; levels of detail when zoomed out |
| L3 | `figcraft-format` | ~5.2k | Native `.figcraft` (zip of `document.json` + images; streamed JSON), local library files, versions, REST API file JSON import (`rest/`) |
| L4 | `figcraft-svg` | ~3.7k | SVG export and import (usvg) |
| L4 | `figcraft-pdf` | ~2.3k | PDF export (krilla) with print options |
| L5 | `figcraft-engine` | ~39.3k | Sessions, undo history (`Arc` snapshots), the command registry (`cmd/*`, `spec!`), pointer tools and vector edit mode, snapping and measuring, components sync, libraries, prototype runtime, Dev Mode code generation, the menu catalog (`catalog.rs`) and the keymap (`keymap.rs`), a panic guard |
| L6 | `figcraft-ui-egui` | ~31.9k | The UI3-style frontend: canvas (render worker), toolbar, layers, Design/Prototype/Inspect panels, colour picker, menus, dialogs, presenter, comments, accessibility (AccessKit), the control channel server |
| L6 | `figcraft-mcp` | ~2.4k | MCP server (JSON-RPC over stdio), headless or bridged to a running app |
| L6 | `figcraft-collab-net` | ~3.0k | Live sessions over the network: the WebSocket transport (tungstenite, a reader and a writer thread per connection), the hub that runs a collaboration core server per open file for the session host the app embeds and for `figcraft-server` (behind a `Backend`), the joining and managing clients (publishing), invite links, presence relay and limits; the browser client; the only crate allowed networking dependencies |
| apps | `figcraft`, `figcraft-android`, `figcraft-cli`, `figcraft-web`, `figcraft-server` | ~3.2k | Desktop app (library + binary; eframe + wgpu), Android build (NativeActivity entry, `cargo apk`), command line, browser build (WASM, WebGPU/WebGL2), collaboration server (files, share links, persistence; [`server.md`](server.md)) |
| plugins | `plugins/` (own workspace) | | Rust plugin SDK and two example plugins, built for wasm32 by `cargo xtask plugins` ([`plugins.md`](plugins.md)) |
| tools | `xtask` | | `ci`, `layers`, `assets`, `parity`, `version`, `icon`, `wasm`, `plugins`, `bench` |

`apps/figcraft-android` is a `cdylib` trampoline: Android's `NativeActivity` loads it and calls its
unmangled `android_main`, which opens the shared app library (`figcraft::run_android`, the same code
the desktop binary runs). The unmangled export is why that crate does not inherit the workspace
lints: edition 2024 spells it `#[unsafe(no_mangle)]` and there is no safe spelling of a required
export symbol, so the crate carries the workspace's clippy denials itself and holds no unsafe code.
Its manifest (`package.metadata.android`) carries the APK identity, the SDK levels and the
activity's `configChanges`; the app-private storage it resolves hands the shared library its config
folder (preferences, recovery copies, installed plugins).

## Data model

A `Document` holds pages, a persistent map of nodes (frames, groups, sections, rectangles,
ellipses, polygons, stars, lines, vector networks, text with styled runs, boolean groups,
components, component sets, instances, slices), styles, variable collections, comments, versions
and images by hash. Instances keep their main's id and per-layer overrides; the engine keeps their
materialised children in sync when a main changes (directly for value, size and add/remove edits,
by a rebuild otherwise). Snapshots are cheap (`Arc` and structural sharing), which gives unlimited
undo with one step per gesture.

## Commands: one path for every surface

Every user-visible behaviour is a command in `crates/engine/src/cmd/*` (455 ids after #101)
with an id, label, parameter docs and undo flag. The menus (generated from the catalog), keyboard
shortcuts, Actions palette, CLI (`figcraft-cli run`), JSON control channel (`--control <port>`) and
MCP server all dispatch the same ids, so agents can do anything a person can; WebAssembly plugins call them too, within their manifest's permissions. Commands never open
dialogs themselves; they request UI, and programmatic callers drop those requests.
`cargo xtask parity` measures the catalog's coverage ([`parity-checklist.md`](parity-checklist.md)).

## Rendering

The CPU renderer is the single source of pixels for the canvas, exports and the browser. The
desktop canvas renders on a worker thread at the device pixel ratio and keeps its last image,
re-rendering only the region a pan uncovers or an edit touches; overlays (selection, handles,
guides, labels) are drawn by egui on top. A seeded random walk checks the retained canvas against
full renders. SVG and PDF export walk the same scene and embed rasters where those formats can't
express an effect.

## File I/O

`.figcraft` is the native format (atomic saves, autosave checkpoints; the desktop app keeps
recovery copies of unsaved documents that reopen after a crash or a forced quit). Imports: REST API
file JSON with variables and images, SVG, raster images, clipboard
content. Exports: PNG, JPG, WebP, SVG, PDF, code. There is no `.fig` reader yet
([`file-format-parity.md`](file-format-parity.md)).

## Host-provided storage (Android)

A host with no system file picker tells the app where its files live through `Services`
(`app_files_dir`) instead of a `cfg` in the dialogs: with it set, every file command — Open, Save
As, Export, Import, Place image, Choose image, Add library, plugin and variables files and folder
picks — is served by the built-in [`file_browser`](../crates/ui-egui/src/file_browser.rs), which
answers exactly as the system dialog would. The browser's navigation is clamped inside the host's
roots: *Up one level* stops at the top of the root being browsed, the home-folder button goes to
the app's `Documents` folder, a typed pick outside the roots is refused as not found, and a folder
outside the roots lands on the top of the root the browser is in. A host with a system picker
(desktop) sets nothing and keeps its dialogs — byte for byte as before; the web keeps its own flow.

A host may offer a second root (`shared_storage_root`) — Android's shared internal storage
(`/storage/emulated/0`) — drawn as a switcher between "App files" and "Shared storage". Entering it
needs Android's "All files access" grant, which an app can never hold silently: `shared_storage_granted`
re-checks it at every tap, and a tap without it calls `shared_storage_grant` (the system settings
page that toggles it) and notes what to do. The grant hooks come from `apps/figcraft-android`'s
`android_access.rs`, the entry crate's JNI to the activity's JVM — the workspace's one `unsafe`
exception, passed to the app as plain closures (`figcraft::SharedStorageAccess`). Nothing else
changes: the desktop sets no shared root, so no switcher is drawn.

The Android build splits its app-private data directory once (`apps/figcraft/src/android_storage.rs`):

```text
<files>/FigCraft/           the app's folder — the config root: preferences, the recovery copies
                            and the installed plugins (unchanged from the groundwork step)
<files>/FigCraft/Documents/ where the browser starts and saves land
/storage/emulated/0/        the second root, behind the "All files access" grant
```

## Robustness

Workspace lints deny `unwrap`, `expect`, `panic!` and friends in shipped code; `unsafe_code` is
forbidden. `figcraft_engine::guard` turns a panicking command into an error and rolls the document
back; the desktop app also guards each UI frame. Junk-parameter sweeps run every command with bad
input.

## Revision history

| Date | Change | Summary |
|---|---|---|
| 2026-10-27 | minor | Android: vendored winit (storytold/winit rev a517b3846) with mouse/trackpad pointer routing; `stamp_android_modifiers` in `crates/ui-egui` tracks Ctrl/Shift/Alt and stamps each frame's events; 50% interface size preset; contributor entry; all Part 1–6 Android work complete and device-tested |
| 2026-10-27 | minor | Android: shared internal storage is the file browser's second root, behind Android's "All files access" grant (checked and asked for over the entry crate's JNI, `apps/figcraft-android/src/android_access.rs`); a finger on the canvas background pans the view while a press on a layer stays the tool's |
| 2026-10-26 | minor | Android: `Services::app_files_dir` serves every file command in the new built-in file browser, clamped to the app's storage (shared storage waits for the grant hooks); the app-private storage splits into the app folder (the config root) and its `Documents/` |
| 2026-10-26 | minor | Android groundwork: `apps/figcraft` is a library (`figcraft::run`/`run_android`) with the desktop binary as a shim, and `apps/figcraft-android` is the NativeActivity entry crate whose app-private storage resolves the config folder |
| 2026-10-11 | minor | `collab-net` hosts through one hub for the app's sessions and the standalone server; added the `figcraft-server` app |
| 2026-10-10 | minor | Merged main after #101–#103: WASM plugins (plugins area 0% → 35% breadth, 20% ready; ecosystem 5% → 20%) and the multiplayer core without networking (multiplayer 5% → 15% breadth, 5% ready; collaboration 8% → 12%); menu breadth 97.5% → 98.5% (200/203); ready for real work ~46% → ~47%; full parity 420–790 h → 390–745 h; beta 210–380 h → 200–365 h. Fixed the stated weight sums (areas 1–16 sum to 94, total 113; percentages were already computed with those) |
| 2026-10-10 | major | Created from the code as of PR #99 |
