//! Open CAD Studio add-on example: the API v24 scene-on-a-panel demo.
//!
//! The root example (`example_plugin_panel`) shows the v22 interactive
//! panel roundtrip; THIS sibling shows the v24 scene/widget wire the same
//! way it shows everything else — real frames over the thread-safe host
//! request line, and headless tests that drive the whole flow without a
//! running host:
//!
//! - **Graph scene demo** ([`mod@scene_graph`]): a node-and-edge
//!   [`PanelScene`] assembled with the crate's fluent
//!   [`SceneBuilder`](ocs_plugin_api::scene_builder::SceneBuilder) (cards +
//!   ports + edges + one `Group.clip` + a text label per card), published
//!   through `PluginRequest::PanelScenePublish`, then moved forwards with
//!   three atomic `PluginRequest::PanelScenePatch` frames (`Add` /
//!   `Replace` / `Remove`, each a fresh base revision and a strictly
//!   greater successor).
//! - **Typed-table widget demo** ([`mod@widgets_table`]): a
//!   [`WidgetSurface`](ocs_plugin_api::widgets::WidgetSurface) (Tree with
//!   one `lazy_children` branch + an IconButton armed with
//!   `ConfirmPolicy::Prompt` + a Table reference into the typed-table
//!   registry) driven with a cell-level `WidgetOp::TableCell` patch — the
//!   single-cell minimality the wire proves — and answering
//!   `TablePageRequested` / `PanelStoreDemand` with the documented
//!   snapshot-cycle publish.
//! - **Interaction roundtrip** ([`mod@state`]):
//!   `HostNotification::PanelSceneEvents` batches merge into plugin state —
//!   selections / commands / activations — and queue the merge
//!   ACKNOWLEDGMENT that rides the next publish's event leg (patch frames
//!   carry no events field).
//! - **Stale-base conflict demo** ([`mod@recovery`]): a deliberately stale
//!   patch is refused `PatchRejected{StaleBase}` (the bounded report names
//!   the head); the plugin re-synchronizes through
//!   `PluginRequest::PanelSceneSnapshot2` (digest + leading window) and
//!   re-patches from the authoritative revision.
//!
//! Build with `cargo build --release`; ship the cdylib plus a filled
//! `plugin.toml` (see the repo README).

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use ocs_plugin_api::dialog::{DialogControl, HorizontalAlign, PanelDef};
use ocs_plugin_api::host::{BuiltinPlugin, HostApi, HostNotification, PluginRequestSender};
use ocs_plugin_api::manifest::{ApiVersion, PluginManifest};
use ocs_plugin_api::ribbon::{CadModule, IconKind, ModuleEvent, RibbonGroup, RibbonItem, ToolDef};

#[cfg(test)]
mod fake_host;
mod host_wire;
mod recovery;
mod scene_graph;
mod state;
mod widgets_table;

// Keep these fields in sync with `plugin.toml` beside this package.
static MANIFEST: PluginManifest = PluginManifest {
    id: "schoeller.scene_example",
    name: "Scene Example",
    version: "0.4.0",
    description:
        "Reference API v24 add-on: a scene-on-a-panel demo (graph cards, typed tables, stale-base recovery).",
    api_version: ApiVersion::CURRENT,
    ribbon_order: 120,
    xdata_apps: &[],
    command_prefixes: &["SCN_"],
};

const CMD_OPEN: &str = "SCN_OPEN";
const CMD_DEMO_SCENE: &str = "SCN_DEMO_SCENE";
const CMD_DEMO_WIDGETS: &str = "SCN_DEMO_WIDGETS";
const CMD_DEMO_STALE: &str = "SCN_DEMO_STALE";
const CMD_DELIVER: &str = "SCN_DELIVER";

/// Static panel definition: the v24 surfaces MOUNT on a registered panel —
/// the publish frames name the panel id the registration used (ownership
/// is transport-authenticated), and the body is informational: the demo's
/// state is the plugin's own semantic model, not dialog drafts.
fn demo_panel() -> PanelDef {
    PanelDef::new(scene_graph::PANEL_ID, "Scene Example")
        .with_command(CMD_OPEN)
        .with_width(360.0)
        .with_control(DialogControl::InfoText {
            text: "API v24 scene demo: SCN_DEMO_SCENE builds the graph scene, \
                   SCN_DEMO_WIDGETS the typed-table surface, SCN_DEMO_STALE runs \
                   the conflict/recovery loop."
                .into(),
            align_h: Some(HorizontalAlign::Left),
        })
        .with_control(DialogControl::Label {
            id: "scn_label_status".into(),
            text: "The scene and its widgets are plugin-owned state the host \
                   mounts on this panel."
                .into(),
            align_h: Some(HorizontalAlign::Left),
        })
}

/// The ribbon tab (one group, the two demo tools) — same discoverability
/// shape as the root example.
struct SceneModule;

impl CadModule for SceneModule {
    fn id(&self) -> &'static str {
        "scene_example"
    }
    fn title(&self) -> &'static str {
        "Scene Example"
    }
    fn ribbon_groups(&self) -> &[RibbonGroup] {
        static GROUPS: std::sync::OnceLock<Vec<RibbonGroup>> = std::sync::OnceLock::new();
        GROUPS.get_or_init(|| {
            vec![RibbonGroup {
                title: "Scenes",
                tools: vec![
                    RibbonItem::LargeTool(ToolDef {
                        id: CMD_DEMO_SCENE,
                        label: "Scene Demo",
                        icon: IconKind::Glyph("⊡"),
                        event: ModuleEvent::Command(CMD_DEMO_SCENE.to_string()),
                    }),
                    RibbonItem::LabeledTool(ToolDef {
                        id: CMD_DEMO_WIDGETS,
                        label: "Widgets",
                        icon: IconKind::Glyph("▦"),
                        event: ModuleEvent::Command(CMD_DEMO_WIDGETS.to_string()),
                    }),
                ],
            }]
        })
    }
}

/// Which flow one demo command runs.
enum Flow {
    Scene,
    Widgets,
    Stale,
    /// Flush the queued demand answers only.
    Deliver,
}

/// One demo command's shape: flush the queued demand answers first (the
/// answers are publishes), then run the command's own flow, and report
/// what the wire did.
fn run_command(
    sender: &Option<Arc<dyn PluginRequestSender>>,
    state: &mut state::DemoState,
    flow: Flow,
) -> String {
    let Some(sender) = sender else {
        return "Scene Example: no host request line — nothing sent.".to_string();
    };
    let delivered = match state.widgets.is_some() {
        true => widgets_table::deliver_pending(sender, state),
        false => 0,
    };
    let applied = match flow {
        Flow::Scene => scene_graph::run_scene_demo(sender, state),
        Flow::Widgets => widgets_table::run_widgets_demo(sender, state),
        Flow::Stale => recovery::run_stale_conflict(sender, state),
        Flow::Deliver => true,
    };
    let delivered_note = match delivered {
        0 => String::new(),
        n => format!("; {n} queued page deliver"),
    };
    match applied {
        true => format!("Scene Example: flow applied{delivered_note}"),
        false => format!("Scene Example: the flow was refused{delivered_note}"),
    }
}

/// The plugin entry point: the same lock-and-inner discipline as the root
/// example (state behind `Mutex`; every send through the cached request
/// line; state-only notification handling).
struct SceneExamplePlugin {
    demo: Mutex<state::DemoState>,
}

impl SceneExamplePlugin {
    fn new() -> Self {
        Self {
            demo: Mutex::new(state::DemoState::default()),
        }
    }

    /// State read/mutate handle (tests drive the same lock; a guard is
    /// never held across another state-touching call in this crate).
    pub(crate) fn state(&self) -> MutexGuard<'_, state::DemoState> {
        self.demo.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The sender the flows send through (cached at load).
    fn sender(&self) -> Option<Arc<dyn PluginRequestSender>> {
        self.state().sender.as_ref().map(|s| Arc::clone(s))
    }
}

impl BuiltinPlugin for SceneExamplePlugin {
    fn manifest(&self) -> &'static PluginManifest {
        &MANIFEST
    }

    fn ribbon(&self) -> Box<dyn CadModule> {
        Box::new(SceneModule)
    }

    fn on_load(&mut self, host: &mut dyn HostApi) {
        // Cache the thread-safe request line (v4 runner discipline); the
        // demo panel registers first (the v24 publish frames name the id
        // this registration used), then BOTH legs publish — the graphic
        // scene and the widget surface, the latter going LIVE on the
        // host's mount.
        let sender = host.plugin_request_sender().map(Arc::from);
        {
            let mut state = self.state();
            state.sender = sender.as_ref().map(|s| Arc::clone(s));
        }
        if host.register_panel(demo_panel()) {
            host.push_info("Scene Example loaded — type SCN_OPEN or use the ribbon tool.");
        } else {
            host.push_info("Scene Example loaded, but its panel was refused.");
        }
        let Some(sender) = sender else { return };
        let mut state = self.state();
        let summary = match (
            scene_graph::run_scene_demo(&sender, &mut state),
            widgets_table::run_widgets_demo(&sender, &mut state),
        ) {
            (true, true) => "Scene Example: the graph scene and the widget surface are published.",
            (false, _) => "Scene Example: the scene publish was refused.",
            (_, false) => "Scene Example: the widget publish was refused.",
        };
        host.push_info(summary);
    }

    fn dispatch(&self, host: &mut dyn HostApi, cmd: &str) -> bool {
        match cmd {
            CMD_OPEN | "SCN_PANEL" => {
                let shown = host.toggle_panel(scene_graph::PANEL_ID, None);
                host.push_info(if shown {
                    "Scene Example panel toggled."
                } else {
                    "Scene Example panel toggle failed."
                });
                true
            }
            CMD_DEMO_SCENE => {
                let info = run_command(&self.sender(), &mut self.state(), Flow::Scene);
                host.push_info(&info);
                true
            }
            CMD_DEMO_WIDGETS => {
                let info = run_command(&self.sender(), &mut self.state(), Flow::Widgets);
                host.push_info(&info);
                true
            }
            CMD_DEMO_STALE => {
                let info = run_command(&self.sender(), &mut self.state(), Flow::Stale);
                host.push_info(&info);
                true
            }
            CMD_DELIVER => {
                let info = run_command(&self.sender(), &mut self.state(), Flow::Deliver);
                host.push_info(&info);
                true
            }
            _ => false,
        }
    }

    fn on_notification(&mut self, _command_id: Option<u64>, notification: HostNotification) {
        match notification {
            // The host's drained v24 widget events: merge into the state
            // machine (the routes stream these only to v24 plugins).
            HostNotification::PanelSceneEvents {
                panel_id,
                surface_id,
                events,
            } => {
                if panel_id == scene_graph::PANEL_ID {
                    self.state().handle_events(surface_id, events);
                }
            }
            // The host's page-miss demands (lazy children / table windows):
            // queue them; the answers are publishes on the next dispatch
            // (demand and delivery stay decoupled, and rejections keep ONE
            // mutation path).
            HostNotification::PanelStoreDemand {
                panel_id,
                surface_id,
                parent_id,
                range,
            } => {
                if panel_id == scene_graph::PANEL_ID {
                    let mut state = self.state();
                    // The surface id names the surface the demand addresses:
                    // it must be the one this plugin published (always, once
                    // the widget leg is live).
                    let known = state.widgets_surface_id();
                    if known.is_none_or(|mine| mine == surface_id) {
                        state.on_store_demand(parent_id, range);
                    }
                }
            }
            // A future major's notifications decode through the `Unknown`
            // passthrough — the root example's ignore-discipline: nothing
            // this plugin's business, never a crash on it.
            _ => {}
        }
    }
}

// Emit the C-ABI symbols the host loader looks for: `ocs_plugin_api_version`
// reports the API major this build compiled against (the api crate's
// `API_VERSION` — 24), the same number the pre-load gate reads from
// plugin.toml's `[opencad]`.
ocs_plugin_api::export_plugin!(SceneExamplePlugin::new());

#[cfg(test)]
mod tests {
    use ocs_plugin_api::host::{HostNotification, PluginRequestSender};
    use ocs_plugin_api::manifest::API_VERSION;
    use ocs_plugin_api::scene::{SceneRevision, StableId};
    use ocs_plugin_api::widgets::{WidgetEvent, WidgetRange};

    use crate::fake_host::FakeWireHost;
    use crate::state::DemoState;

    use super::*;

    /// Minimal two-section TOML reader (the manifest pins itself without a
    /// toml dependency).
    fn toml_value(source: &str, section: &str, key: &str) -> Option<String> {
        let mut current: Option<&str> = None;
        for line in source.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some(stripped) = line.strip_prefix('[') {
                current = stripped.strip_suffix(']');
                continue;
            }
            let Some((name, value)) = line.split_once('=') else {
                continue;
            };
            if current == Some(section) && name.trim() == key {
                return Some(value.trim().trim_matches('"').to_string());
            }
        }
        None
    }

    /// The manifest pins API major 24: the compiled static says CURRENT,
    /// the crate constant is 24, and the host accepts exactly that major —
    /// the first thing the loader checks.
    #[test]
    fn manifest_pins_api_major_24() {
        assert_eq!(API_VERSION, 24);
        assert_eq!(MANIFEST.api_version.major, API_VERSION);
        assert_eq!(MANIFEST.api_version, ApiVersion { major: 24 });
        assert!(
            ocs_plugin_api::host_accepts_plugin_version(MANIFEST.api_version.major),
            "the host's loading gate accepts the declared major"
        );
    }

    /// `[plugin]` id/name/version + `[opencad]` api_version / ribbon order
    /// / command prefixes in plugin.toml agree with the static MANIFEST —
    /// the root example's sync discipline, pinned for the v24 sibling.
    #[test]
    fn plugin_toml_matches_static_manifest() {
        let source = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/plugin.toml"));
        assert_eq!(
            toml_value(source, "plugin", "id").as_deref(),
            Some(MANIFEST.id)
        );
        assert_eq!(
            toml_value(source, "plugin", "name").as_deref(),
            Some(MANIFEST.name)
        );
        assert_eq!(
            toml_value(source, "plugin", "version").as_deref(),
            Some(MANIFEST.version),
            "0.4.0 — the plugin-version line the repo keeps in sync with the \
             static MANIFEST (0.3.0 was the v22 era; 0.4.0 is this API-24 era)"
        );
        assert_eq!(
            toml_value(source, "opencad", "api_version")
                .and_then(|value| value.parse::<u32>().ok()),
            Some(MANIFEST.api_version.major),
            "the pre-load gate line pins the API major the crate compiles against (24)"
        );
        assert_eq!(
            toml_value(source, "opencad", "ribbon_order").as_deref(),
            Some("120")
        );
        assert_eq!(MANIFEST.command_prefixes, &["SCN_"]);
        assert!(
            source.contains("\"SCN_\""),
            "the prefix list carries the demo's SCN_ commands"
        );
    }

    /// Event plumbing end to end through the plugin's own
    /// `on_notification` (the runner's v24 route): a published surface id
    /// is required; batch facts merge newest-wins and queue on the ack leg.
    #[test]
    fn on_notification_merges_event_batches() {
        let surface = widgets_table::demo_widget_surface(1).expect("the demo surface");
        let mut plugin = SceneExamplePlugin::new();
        {
            // Publish the widget leg into the plugin's model directly
            // (headless: no HostApi involved).
            let mut state = plugin.state();
            state.widgets = Some(surface);
        }
        let surface_id = StableId(widgets_table::ROOT_GROUP_ID);
        let batch = |panel: &str, ids: Vec<u64>, sid: StableId| -> HostNotification {
            HostNotification::PanelSceneEvents {
                panel_id: panel.to_owned(),
                surface_id: sid,
                events: vec![WidgetEvent::SelectionChanged {
                    surface_id: sid,
                    selected_ids: ids.into_iter().map(StableId).collect(),
                }],
            }
        };
        // Newest wins; each batch joins the pending ack leg.
        plugin.on_notification(None, batch(scene_graph::PANEL_ID, vec![1, 2], surface_id));
        plugin.on_notification(None, batch(scene_graph::PANEL_ID, vec![3], surface_id));
        {
            let state = plugin.state();
            assert_eq!(
                state.selected,
                vec![StableId(3)],
                "newest selection facts replace older ones"
            );
            assert_eq!(state.pending_ack.len(), 2, "both batches await the ack");
        }
        // A different panel id doesn't touch this plugin's state.
        plugin.on_notification(None, batch("other.panel", vec![999], surface_id));
        // A different surface id is dropped too (the frozen identity).
        plugin.on_notification(
            None,
            batch(scene_graph::PANEL_ID, vec![1_000], StableId(77_777)),
        );
        {
            let state = plugin.state();
            assert_eq!(state.selected, vec![StableId(3)]);
            assert_eq!(
                state.pending_ack.len(),
                2,
                "foreign panels and foreign surfaces add nothing"
            );
        }
    }

    /// The store-demand plumbing: a typed-table demand routes by the
    /// parent's identity (the registry table this plugin published); the
    /// tree branch demand keeps the parent + window verbatim.
    #[test]
    fn demand_routing_tables_and_tree() {
        let surface = widgets_table::demo_widget_surface(1).expect("the demo surface");
        let mut plugin = SceneExamplePlugin::new();
        {
            let mut state = plugin.state();
            state.widgets = Some(surface);
        }
        let surface_id = StableId(widgets_table::ROOT_GROUP_ID);
        let demand = |parent: StableId| -> HostNotification {
            HostNotification::PanelStoreDemand {
                panel_id: scene_graph::PANEL_ID.to_owned(),
                surface_id,
                parent_id: Some(parent),
                range: WidgetRange { start: 0, len: 8 },
            }
        };
        plugin.on_notification(None, demand(StableId(widgets_table::TABLE_ID)));
        plugin.on_notification(None, demand(StableId(widgets_table::LAZY_BRANCH_ROW)));
        {
            let state = plugin.state();
            assert_eq!(
                state.pending_deliveries,
                vec![
                    crate::state::PendingDelivery::TablePage {
                        table: StableId(widgets_table::TABLE_ID),
                        first_key: widgets_table::backing_row_key(0),
                        len: 8,
                    },
                    crate::state::PendingDelivery::TreeChildren {
                        parent: Some(StableId(widgets_table::LAZY_BRANCH_ROW)),
                        range: WidgetRange { start: 0, len: 8 },
                    },
                ],
                "the registry table parent -> keyed window; the tree parent -> its children page"
            );
        }
    }

    /// BOTH demos are reachable without a running host, on the same state —
    /// the exact sequence `on_load` runs, with a wire double standing in
    /// for the request line. ONE revision cadence spans both legs; no leg
    /// ever rewinds the other.
    #[test]
    fn both_demos_headless_on_one_revision_cadence() {
        let host = std::sync::Arc::new(FakeWireHost::new());
        let sender: std::sync::Arc<dyn PluginRequestSender> =
            std::sync::Arc::clone(&host) as std::sync::Arc<dyn PluginRequestSender>;
        let mut state = DemoState::default();

        assert!(scene_graph::run_scene_demo(&sender, &mut state));
        assert!(widgets_table::run_widgets_demo(&sender, &mut state));

        // One cadence across both legs: the scene's publish @1 (patches to
        // @4), the widget surface's publish @5, its cell patch @6.
        assert_eq!(state.head, 6);
        assert_eq!(state.scene_head(), Some(SceneRevision(4)));
        assert_eq!(state.widgets_head(), Some(SceneRevision(6)));
        // Every revision claim strictly exceeds both legs' stored heads —
        // the sound property `state`.next_revision guarantees by
        // construction.
        assert!(state.head >= state.scene_head().expect("scene leg").0);
        assert!(state.head >= state.widgets_head().expect("widget leg").0);
    }
}
