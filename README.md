# Panel Example (`schoeller.example_panel`)

Reference dynamically-loaded add-on for [Open CAD Studio](../OpenCADStudio)
demonstrating a **dockable side panel** with live control events — the panel
surface counterpart of the ribbon-tab example (
[HakanSeven12/opencad-example-plugin](https://github.com/HakanSeven12/opencad-example-plugin)).

Builds a `cdylib` against [`ocs_plugin_api`](../OpenCADStudio/crates/ocs_plugin_api)
(API v22) and exports the host symbols via `ocs_plugin_api::export_plugin!`.

## What it demonstrates

| API piece | Where |
|-----------|-------|
| `HostApi::register_panel` | Every basic `DialogControl` kind in one docked palette (`epl_panel`, right rail, 320 px). The panel id **and** its `command` both become command-line completions automatically. |
| `HostNotification::PanelAction` | The host streams a `change` event on every text/checkbox/choice/color edit; the plugin tracks the values in `on_notification` (which carries no host, so it must not write). |
| `HostApi::update_panel` | The panel's **Apply** button sends the `EPL_APPLY` command back through the dispatcher; `dispatch` rewrites the summary label from the tracked values. |
| Host-classified buttons | `action: Some("close")` on the **Hide** button — a built-in keyword the host handles itself. |
| Click-only buttons | **Ping** has no `action`, so the host emits a `click` event; the plugin counts pings, reported on the next Apply. |
| `HostApi::toggle_panel` | `EPL_OPEN` (panel `command`) / `EPL_PANEL` (panel id) from the command prompt, or the ribbon tool. |
| `HostNotification::SelectionChangedDigest` (v22) | O(1) selection facts — `{tab_id, revision, count}`, revision monotonic per tab; the plugin skips stale revisions and refreshes the label through a 16 ms-gated `PluginRequest::UpdatePanel` on the thread-safe request line. |
| `EPL_APPLY_SELECTION` / `EPL_REVERT_SELECTION` | One reversible journal per tab: apply journals originals and recolors via a single `UpdateEntitiesTransaction` undo group; revert restores only handles whose state still matches what apply wrote — external edits/deletes come back as counted conflicts, never clobbered — and reselects the restored set. |
| `EPL_INSPECT` | Batch-reads the first 256 selected entities and reports the kind breakdown. |
| Structured panel values (v22) | Checkbox/Choice/Color/Label drafts are typed in state reads (`bool`, `{index,text}`+`options`, `[r,g,b]`, label text) — automation reads real state, not debug strings. |

## Commands

| Command | Description |
|---------|-------------|
| `EPL_OPEN` | Toggle the panel. Same effect via the ribbon **Panels → Panel** tool. |
| `EPL_PANEL` | Alias — the host also registers the panel id itself as a dynamic command. |
| `EPL_APPLY` | Re-render the panel summary label from tracked values (wired to **Apply**). |
| `EPL_APPLY_SELECTION` | Journal + recolor the selection (stroke draft → per-entity colour), one undo group. Wired to **Apply to Sel**. |
| `EPL_REVERT_SELECTION` | Restore journaled originals where unchanged; conflicts reported. Wired to **Revert**. |
| `EPL_INSPECT` | Report the selection's kind breakdown (first 256). Wired to **Inspect**. |

## The verified roundtrip loop (agent ⇄ host ⇄ plugin, over MCP)

This is the loop exercised live against a v22 release host — every step
below is scriptable through the native MCP stdio bridge
(`OpenCADStudio.exe --mcp`); with the panel visible, selection forming and
drafts changing on screen happen **without touching the GUI**.

| Direction | Channel | Payload |
|-----------|---------|---------|
| agent → host | `ocs_execute` / `ocs_read` | `new`, `run`, `select`, `panel_action`, `undo`, `redo`, `entities_set`, `batch`, `state` |
| host → plugin | runner notification stream | `SelectionChangedDigest` (v22), `PanelAction` (typed click/change), document events |
| plugin → host | thread-safe `plugin_request_sender()` | `UpdatePanel` (draft writes), `GetEntitiesBatch`, `UpdateEntitiesTransaction`, `set_selection` |

Verified sequence (one drawing, handles `84`/`85`/`86`):

```sh
# 1. open: the panel id and command are dynamic completions
ocs_execute {"op":"run","cmd":"EPL_OPEN"}            -> ok
# 2. create entities in one batch (single-shot commands return completed)
ocs_execute {"op":"batch","steps":[
  {"op":"run","cmd":"LINE 0,0 10,0"},
  {"op":"run","cmd":"LINE 10,0 10,5"},
  {"op":"run","cmd":"CIRCLE 0,0 5"}]}
# 3. select → the plugin's summary label tracks the digest
ocs_read    {"op":"state"}        -> plugin_panels[epl_panel]
#                          epl_label_summary ends with "… sel 3 rev 2"
# 4. typed UI interaction without a window
ocs_execute {"op":"panel_action","panel_id":"epl_panel",
             "control_id":"epl_button_ping"}           -> the plugin's ping counter (+1)
ocs_execute {"op":"panel_action","panel_id":"epl_panel",
             "control_id":"epl_choice_style","action":"change",
             "value":{"index":2}}                      -> Choice {index:2, text:"Survey"}
ocs_execute {"op":"panel_action","panel_id":"epl_panel",
             "control_id":"epl_number_length","action":"change","value":12}
# 5. reversible write through the host engine
ocs_execute {"op":"run","cmd":"EPL_APPLY_SELECTION"}   -> colours Rgb{46,125,50}, one undo group
# 6. host-native undo/redo moves the colours back and forth;
#    the plugin never re-applies (no clobbering, exact-state journal)
ocs_execute {"op":"undo"} / {"op":"redo"}
# 7. conflict: an external edit stays, the rest restores
ocs_execute {"op":"entities_set","handles":["85"],"color":{"rgb":[10,10,10]}}
ocs_execute {"op":"run","cmd":"EPL_REVERT_SELECTION"}  -> 85 keeps black (conflict), rest ByLayer
```

Rules an agent must keep:

- `request_id` is **mandatory and unique for the whole session** — reusing
  one with a changed payload is refused; the same payload replays its
  original verdict (safe retries).
- Multi-step interactive commands must ride **one `batch`** request: the
  command line is owned by the requesting client until the flow ends; a
  dead owner parks it until its deadline.
- `panel_action` value shapes are RAW (the same shapes the op schema
  advertises): `{"index": 2}` for choices (text optional — it defaults to
  `options[index]`), plain booleans/numbers/strings for the other draft
  kinds. Color is picker-only (drafts reach the plugin via the picker or
  a plugin's own `update_panel`).
- A pure no-op re-apply on an already-green selection journals nothing
  and honestly reports "nothing to apply" — a later revert then reports
  "nothing to revert"; this is exact-state semantics, not a failure.

Measured on the live host (3 entities): `EPL_APPLY_SELECTION` 0.94 s and
`EPL_REVERT_SELECTION` 0.93 s per automation roundtrip (~30 ms in warm
context); the digest wire payload is a constant 33 B where the legacy
handle list is `8·N + 25` B (50 000 entities: 400 025 B — 12 100×).

## The scene example crate (`scene_example_plugin/`)

The API v24 sibling of this panel example — same cdylib shape, same
standalone workspace, same `../OpenCADStudio` path dependency —
demonstrating the scene/widget wire (ADR-008): it declares
`[opencad] api_version = 24` (compiled against the api crate at the v24
tip; plugin version `0.4.0` — the next iteration after this crate's
`0.3.0`), and shows the full publish-or-patch contract headlessly (every
flow is reachable through `cargo test` without a running host; a
`PluginRequestSender` double serves the four v24 tails with the host
store's documented semantics):

| Flow | Where | Wire pieces |
|------|-------|-------------|
| Graph scene demo | `SCN_DEMO_SCENE` / `src/scene_graph.rs` | `PanelScene` via `SceneBuilder` (cards + ports + edges + one `Group.clip` + a label per card), published with `PanelScenePublish`, then three `PanelScenePatch` frames (`Add`/`Replace`/`Remove`, fresh base + strictly greater successor each) |
| Typed-table widget demo | `SCN_DEMO_WIDGETS` / `src/widgets_table.rs` | `WidgetSurface` (Tree with a `lazy_children` branch + `ConfirmPolicy::Prompt` IconButton + Table ref into the typed-table registry), one-cell `WidgetOp::TableCell` patch (≤300 B — the single-cell minimality), `TablePageRequested` / `PanelStoreDemand` answered by the documented snapshot-cycle publish |
| Interaction roundtrip | `SCN_DELIVER` / `src/state.rs` | `HostNotification::PanelSceneEvents` merges selections/commands/activations newest-wins and queues the merge-acknowledgment that rides the next publish's event leg |
| Stale-base conflict demo | `SCN_DEMO_STALE` / `src/recovery.rs` | a stale patch → `PatchRejected{StaleBase}` (the bounded report names the head) → `PanelSceneSnapshot2` digest + leading window → re-patch from the authoritative revision |

Build and test it from its own directory (it self-scopes its workspace
like this crate does): `cd scene_example_plugin && cargo test`. The panel
registers as `scn_panel` (commands `SCN_*`); `SCN_OPEN` toggles it.

## Build

The WSL checkout (`~/opencode/plugin_panel_register`) is the source of truth;
Windows (`C:\work\opencode`) is a build mirror kept in sync via
`../scripts/sync-to-windows.sh`.

```sh
# WSL: type-check
cargo check

# Windows 11 (via the mirror, native toolchain):
../scripts/build-test-windows.sh
```

The path dependency `../OpenCADStudio/crates/ocs_plugin_api` resolves both in
the WSL layout and in the `C:\work\opencode` mirror because
`scripts/sync-to-windows.sh` keeps the two repos side by side.

## Manual install on Windows 11

1. Build on the Windows mirror: `build-test-windows.sh` (or per repo:
   `cmd.exe /c "cd /d C:\work\opencode\example_plugin_panel && cargo build --release"`).
2. Fill `plugin.toml` (the `rustc_version` / `acadrust_source` placeholders —
   see below for the API v14+ note) and create the package folder in
   `%APPDATA%\OpenCADStudio\plugins\`:

   ```sh
   PKG=/mnt/c/Users/$USER/AppData/Roaming/OpenCADStudio/plugins/schoeller.example_panel
   mkdir -p "$PKG"
   cp /mnt/c/work/opencode/example_plugin_panel/target/release/example_plugin_panel.dll \
      "$PKG/schoeller.example_panel-windows-x86_64.dll"
   # rustc_version = output of `rustc --version` on the Windows build side
   # acadrust_source = `source` line of the `opencadcodec` entry in Cargo.lock
   sed \
     -e "s|__RUSTC_VERSION__|<rustc-version>|" \
     -e "s|__ACADRUST_SOURCE__|<acadrust-source>|" \
     plugin.toml > "$PKG/plugin.toml"
   ```

3. Start Open CAD Studio — the panel docks on the right; toggle with
   `EPL_OPEN`.

> **API v14+ note**: from API v14 (self-hosted runner) neither `rustc_version`
> nor `acadrust_source` is enforced at load; plugin.toml carrying them filled
> matches the published-asset shape. Compatibility rides the `wire_schema`
> codec fingerprint and the loader picks the first file with the platform
> extension when no explicit `library` key is declared.

## Install layout

```
%APPDATA%\OpenCADStudio\plugins\
└── schoeller.example_panel\
    ├── schoeller.example_panel-windows-x86_64.dll
    └── plugin.toml
```
