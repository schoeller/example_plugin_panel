# Plugin Authoring Guide — panels, scenes and widgets on the API v24 wire

This is the recipe guide for Open CAD Studio plugin authors who want a
**live panel surface**: a scene the host renders and mounts, semantic
widgets the host makes interactive, and the event/patch loop that keeps
plugin and host in agreement. Every recipe references the module of the
[`scene_example_plugin`](.) demo that embodies it — the demo is its own
tutorial: one capability = one module = one registration row, and
`SCN_HELP` prints the whole table.

Read this top to bottom for the first plugin; jump to a recipe when
extending. The API shapes live in
[`OpenCADStudio/crates/ocs_plugin_api`](../../OpenCADStudio/crates/ocs_plugin_api)
(`scene.rs`, `widgets.rs`, `host.rs`, `ipc/protocol.rs`) — this guide
never invents an API; every name below is pinned by that crate's tests.

---

## (a) Manifest + `api_version` 24 + HostApi basics

*Embodied by: the crate root (`lib.rs`, `plugin.toml`).*

1. **Package**: a `cdylib` crate that depends on the api crate by PATH —
   the same checkout the host builds against, so the compiled ABI and the
   wire agree:

   ```toml
   [lib]
   crate-type = ["cdylib"]
   [dependencies]
   ocs_plugin_api = { path = "../../OpenCADStudio/crates/ocs_plugin_api", features = ["host"] }
   ```

2. **Manifest, twice**: the static `PluginManifest` in `lib.rs` and the
   `plugin.toml` beside it must AGREE (`id`, `name`, `version`,
   `api_version`, `ribbon_order`, `command_prefixes`). The gate:

   ```toml
   [opencad]
   api_version = 24          # the pre-load gate — the host's API major
   command_prefixes = ["SCN_"]
   ```

   The host loads the library only after this gate AND the compiled-in
   `ocs_plugin_api_version` symbol agree — `export_plugin!` emits both.
   Pin the sync with a test (`manifest_pins_api_major_24`,
   `plugin_toml_matches_static_manifest` in `lib.rs`).

3. **The plugin struct**: implement [`BuiltinPlugin`] — `manifest()`,
   `ribbon()` (a [`CadModule`]; ribbon groups are data — see the
   `DemoFeature` seam in `lib.rs`), and the three calls the host makes:
   `on_load` (cache `host.plugin_request_sender()` — the thread-safe
   request line — register your panel, then publish), `dispatch` (your
   `SCN_` commands), `on_notification` (state-only; never send from it).

4. **The panel**: `host.register_panel(PanelDef…)` — the panel id is the
   ownership name every v24 frame addresses. Surfaces MOUNT on the
   registered panel; the id is transport-authenticated (see (g)).

## (b) The publish→patch lifecycle (fixture → patch → ack/stale → snapshot2)

*Embodied by: `scene_graph.rs` (scene leg), `widgets_table.rs` (widget
leg), `recovery.rs` (stale-base conflict), the shared send paths in
`host_wire.rs`.*

- **One panel, ONE revision cadence** across both legs (`PanelScene` and
  `WidgetSurface` share the panel's [`SceneRevision`] space). Keep one
  `head` counter; every claim = `head + 1`, strictly above both legs'
  stored heads (`DemoState::next_revision`).
- **Publish** (the snapshot leg): the first frame carries the whole
  bounded fixture (`SceneBuilder::finish()` / a `WidgetSurface` that
  passes `validate()`). A LATER full publish is the snapshot-cycle shape
  — how you answer store demands (see (e)).
- **Patch** (the incremental leg): `PatchBuilder::new(base).add/replace/
  remove(...).finish(successor)` for the scene; `WidgetSurfacePatch`
  (`WidgetOp::Add` / `Update` / `Remove` / `TableCell`) for widgets. The
  base must be the host's head, the successor strictly greater — the
  builder refuses anything else BEFORE the wire sees it.
- **The ack leg**: host-observed events are acknowledged on the NEXT
  PUBLISH's `events` field (patches carry no events field). Take the
  pending batch, send it, and if the frame is REFUSED, restore it — the
  acknowledgment only rode an applied frame.
- **Stale handling**: a refused frame (`PatchRejected{code, report}`)
  moved NOTHING on the host. Mirror that exactly: roll back your revision
  claim, restore the taken ack batch, record the report's head — and let
  it RAISE your claim frontier (`DemoState::note_reject`), so the next
  frame escapes a wedged lane instead of re-colliding with the store
  head.
- **Recovery**: `PanelSceneSnapshot2` answers with a `StoreRow::State`
  digest (bounded summaries; the scene digest carries the authoritative
  revision) plus a bounded leading window. Patch what the SNAPSHOT holds
  — locate your target in the window, rebuild the op from the digest
  revision. The full loop is `recovery.rs::run_stale_conflict`.

## (c) THE EVENT TABLE (every `WidgetEvent` → what a plugin does)

*Embodied by: `state.rs::handle_events` (the router) and the feature
modules that consume each kind.*

Events arrive as `HostNotification::PanelSceneEvents` batches (only
while the panel's widget mount is live; the surface id names your
published surface — drop foreign ones). Every event joins the pending
ack leg; the semantic ones update your model:

| Event | When the host sends it | What the plugin does |
|-------|------------------------|----------------------|
| `SelectionChanged` | tree/list selection moved | merge newest-wins into your selection state |
| `CommandInvoked` | a menu item / icon button / **drag settle** invoked its command | resolve what the command names for the reported target ids; the drag settle (`"drag_settle"`) names the dragged element's address — resolve its `DragMeta` from YOUR model (the wire never echoes the meta) and move the node authoritatively (`drag_demo.rs`) |
| `ItemActivated` | a list item (or tree row) double-activated/committed | record the activation; act on your model (`menu_list.rs`) |
| `TreeExpanded` | a branch expanded/collapsed | host-local presentation state — nothing to do UNLESS the branch was lazy (then the ask arrives as the demand/`ChildrenRequested`, see the next row) |
| `ChildrenRequested` | a lazy branch needs rows | queue a bounded children page; answer by PUBLISHING the window (snapshot cycle) — never re-answer a satisfied window (`tree_lazy.rs`, `widgets_table.rs::deliver_pending`) |
| `TablePageRequested` | a windowing table needs its next rows | queue the keyed window (`first_key` + `len`); answer by publishing the rows (`widgets_table.rs`) |
| `TableCellChanged` | the user committed a cell edit IN THE HOST (two-phase pointer or keyboard) | validate against your model → apply → ack with ONE `WidgetOp::TableCell` patch; an invalid value is refused with the model untouched (`cell_edit.rs`) |

The rule underneath all rows: **events are notifications, patches are
data**. Never answer an event by re-sending a whole collection — the
answer is either the ack leg, or one bounded patch/publish.

## (d) Commands + confirm policies

*Embodied by: `widgets_table.rs` (`CMD_DELETE_NODE` on the IconButton),
`menu_list.rs` (the Menu's items).*

- Widgets carry SEMANTIC command ids (`String`s — deliberately outside
  the stable-id namespace). The host invokes them; the invocation arrives
  as `CommandInvoked { command_id, target_ids }`. You resolve what the
  command names for those targets. There are NO drawing callbacks from
  the plugin — the host draws all chrome (ADR-005).
- `ConfirmPolicy::Prompt { message }` on an `IconActionDef` or a
  `MenuItemDef` makes the host ASK ONCE before invoking. Plugin-side
  there is nothing extra to do: the invocation arrives (or, if the user
  declines, nothing arrives).
- Menu items also carry display `shortcut`s (a HINT — the host never
  reads keys through it; bindings live host-side) and may nest `submenu`
  `MenuDef`s (bounded depth `MAX_MENU_DEPTH`).
- Your own ribbon/command-prompt commands (`SCN_*`) go through
  `dispatch` — in this demo each maps to one `DemoFeature` row's flow
  (`lib.rs::FEATURES`); adding a command = write a module + one row.

## (e) Typed tables + paging

*Embodied by: `widgets_table.rs` (the whole story), `cell_edit.rs`
(interactive edits).*

- Declare the table in the surface's registry (`WidgetSurface.tables`):
  columns with stable ids and [`CellKind`]s, `total_rows: Some(n)` when
  the backing set is larger than the published window, and the FIRST
  window of rows (≤ the payload budgets). `TableRef` widgets point at it
  by `table_id` (a foreign identity space — several refs may share one
  backing table).
- **Stable keys**: a row's key is its identity forever. Page by growing
  the published window from the held keys onward — the held (possibly
  patched) cells stay byte-identical (`extend_table_window`).
- **The plugin's own writes** ride `WidgetOp::TableCell` — ONE op per
  cell (the wire proves a single-cell patch is tiny; see
  `table_cell_patch_minimality`). The op addresses table + row key +
  column id with the skeleton value; the column's kind resolves the typed
  cell.
- **The host's writes** arrive as `TableCellChanged` (the skeleton event
  vocabulary). Validate, apply, ack with the cell patch — the closed loop
  of (c).
- **Paging**: the host asks (`TablePageRequested` /
  `PanelStoreDemand`); you answer with the snapshot cycle — a full
  publish of your current model (window grown, ack leg riding along).
  Demand and delivery stay decoupled; a refused answer publish keeps ONE
  mutation path and obeys (b)'s rollback laws.

## (f) MCP parity is automatic — one path, zero extra work

*Observable at: `SCN_DEBUG`'s `mcp:` section (built by
`DemoState::mcp_addresses`).*

The MCP tools `panel_semantics`, `panel_page` and `panel_widget_invoke`
ride the SAME host path the GUI uses: they address the panel id, the
surface id, the tree/table identities and the command strings your
plugin already publishes. A plugin ships NO MCP code; an agent can read
the semantics, page the stores and invoke the widget commands against
exactly the addresses in this demo's debug dump:

- `panel_semantics(panel "scn_panel", surface 90)` — the whole mounted
  surface (tree 100, button 110, table ref 120, menu 150, list 160).
- `panel_page(table 200, cols 201..=204, keys 300..=339; TreeChildren
  parent 12|13)` — the stores, windowed.
- `panel_widget_invoke(commands …)` — the semantic commands
  (`scn_delete_node`, `scn_menu_*`, `scn_list_layer`, the host's
  `"drag_settle"` hand-off).

The demo PROVES the equivalence headlessly: the fake host serves the
same store semantics the GUI and the MCP tools see, so every test that
reads a page or invokes a command through the wire is the observable
stand-in for an MCP client.

## (g) The laws checklist

Every recipe above inherits these; break one and the wire (or your own
model) will tell you loudly:

1. **Revision discipline** — one cadence per panel; every claim strictly
   above every stored leg's head; a refused claim is burned (the report
   head RAISES the frontier, `note_reject`).
2. **Rollback on reject** — a rejected frame leaves everything exactly
   as it was: model untouched, ack batch restored, queued answers kept.
   Never pre-write your model before the wire applies.
3. **Stable keys** — row keys, column ids, element ids are identities,
   forever. Pages grow from held keys; patches address keys, never
   positions.
4. **Bounded payloads** — respect the crate budgets (rows per payload,
   patch ops, text/uri bytes, event batch). Pages are windows, never
   dumps; every answer is bounded by the ask (and the page budget).
5. **Refusal self-explanation** — when a frame refuses, your user log
   names the flow, the code, the host head and the frame's base (the
   `refused_report` grammar in `lib.rs`); a codeless answer (transport
   error, the wire's plain `Error`) prints its text verbatim.
6. **Owner law** — the FIRST publisher owns the panel's surface store;
   publishing never transfers it; a second publisher is refused
   `PublisherOwnedByOther`, structured, zero mutation (`owner_law.rs`).
   Never fight it — and never send a frame that would regress live state
   (publish the MODEL, not a stale fixture).
7. **State-only notifications** — `on_notification` merges facts and
   queues answers; ALL sending happens on your side of the request line
   (dispatch/on_load/tests), through the cached sender.
8. **Stateless-friendly sends** — the request line is thread-safe
   (`Arc<dyn PluginRequestSender>`); answers decode into a small
   vocabulary (applied / structured reject / codeless / failed /
   unexpected) — never pretend success (`host_wire.rs`).

## (h) Headless testing with the fake_host pattern

*Embodied by: `fake_host.rs` (the double) and every `mod tests` in this
crate.*

- `FakeWireHost` implements `PluginRequestSender` and serves the four
  v24 tails with the host store's documented semantics: publish legs run
  the crate `validate()` gate plus the per-leg revision law (identical
  content at the head = the refresh shape), patch legs apply the crate's
  atomic `apply_patch` and map errors to the structured codes, store
  reads page with checked bounds, snapshot2 leads with the digest.
- The tests drive the plugin's REAL send paths against the double —
  no live host, no GUI. The plugin's own state machine is driven through
  its public methods (`handle_events`, `on_store_demand`, the feature
  flows).
- Refusal paths get their own scripted doubles (`CodedReject`,
  `OwnedByOtherHost`, the `Scripted` codeless/transport pair in
  `lib.rs`): every flow must prove that a rejected frame leaves
  everything exactly as it was, and that the NEXT frame heals from the
  realigned frontier.
- Fixture determinism is pinned byte-for-byte (`bincode::serialize` of a
  fixture twice → equal), and each feature's run is idempotent (a second
  command sends no frame).
- Run it all from the crate directory: `cargo test` (the crate
  self-scopes its workspace). The root `example_plugin_panel` crate's
  tests stay independent and green.
