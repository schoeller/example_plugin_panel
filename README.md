# Panel Example (`schoeller.example_panel`)

Reference dynamically-loaded add-on for [Open CAD Studio](../OpenCADStudio)
demonstrating a **dockable side panel** with live control events — the panel
surface counterpart of the ribbon-tab example (
[HakanSeven12/opencad-example-plugin](https://github.com/HakanSeven12/opencad-example-plugin)).

Builds a `cdylib` against [`ocs_plugin_api`](../OpenCADStudio/crates/ocs_plugin_api)
(API v21) and exports the host symbols via `ocs_plugin_api::export_plugin!`.

## What it demonstrates

| API piece | Where |
|-----------|-------|
| `HostApi::register_panel` | Every basic `DialogControl` kind in one docked palette (`epl_panel`, right rail, 320 px). The panel id **and** its `command` both become command-line completions automatically. |
| `HostNotification::PanelAction` | The host streams a `change` event on every text/checkbox/choice/color edit; the plugin tracks the values in `on_notification` (which carries no host, so it must not write). |
| `HostApi::update_panel` | The panel's **Apply** button sends the `EPL_APPLY` command back through the dispatcher; `dispatch` rewrites the summary label from the tracked values. |
| Host-classified buttons | `action: Some("close")` on the **Hide** button — a built-in keyword the host handles itself. |
| Click-only buttons | **Ping** has no `action`, so the host emits a `click` event; the plugin counts pings, reported on the next Apply. |
| `HostApi::toggle_panel` | `EPL_OPEN` (panel `command`) / `EPL_PANEL` (panel id) from the command prompt, or the ribbon tool. |

## Commands

| Command | Description |
|---------|-------------|
| `EPL_OPEN` | Toggle the panel. Same effect via the ribbon **Panels → Panel** tool. |
| `EPL_PANEL` | Alias — the host also registers the panel id itself as a dynamic command. |
| `EPL_APPLY` | Re-render the panel summary label from tracked values (wired to **Apply**). |

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
