//! Open CAD Studio add-on example: the API v24 scene-on-a-panel demo —
//! the FULL architecture showcase behind its own `SCN_` command set.
//!
//! The root example (`example_plugin_panel`) shows the v22 interactive
//! panel roundtrip; THIS sibling shows the v24 scene/widget wire the same
//! way it shows everything else — real frames over the thread-safe host
//! request line, and headless tests that drive the whole flow without a
//! running host.
//!
//! # The `DemoFeature` seam — one capability = one module = one table row
//!
//! Every showcased capability is a self-contained module exposing one
//! uniform surface: an id, its `SCN_` command(s), fixture builder(s)
//! producing its scene / widget-surface slices, its event handling, and
//! an expected-observation note. All of them are registered in the ONE
//! [`FEATURES`] table below; the ribbon groups, the command dispatch, the
//! `SCN_HELP` index, the MCP-parity address list and the flow/refusal
//! grammar are all DATA-DRIVEN off that table. Adding panel functionality
//! = write a module + add one registration line.
//!
//! The features:
//!
//! - **Graph scene demo** ([`mod@scene_graph`], `SCN_DEMO_SCENE`): a
//!   node-and-edge [`PanelScene`] assembled with the fluent
//!   [`SceneBuilder`](ocs_plugin_api::scene_builder::SceneBuilder),
//!   published through `PluginRequest::PanelScenePublish`, then moved
//!   forwards with three atomic `PluginRequest::PanelScenePatch` frames.
//! - **Typed-table widget demo** ([`mod@widgets_table`],
//!   `SCN_DEMO_WIDGETS`): a [`WidgetSurface`] (Tree with lazy branches +
//!   an IconButton armed with `ConfirmPolicy::Prompt` + a Table reference
//!   into the typed-table registry) driven with a cell-level
//!   [`WidgetOp::TableCell`] patch, answering `TablePageRequested` /
//!   `PanelStoreDemand` with the documented snapshot-cycle publish.
//! - **Stale-base conflict demo** ([`mod@recovery`], `SCN_DEMO_STALE`):
//!   a deliberately stale patch is refused `PatchRejected{StaleBase}`;
//!   the plugin re-synchronizes through `PanelSceneSnapshot2` (digest +
//!   leading window) and re-patches from the authoritative revision.
//! - **Interaction roundtrip** ([`mod@state`]):
//!   `HostNotification::PanelSceneEvents` batches merge into plugin state
//!   and queue the merge ACKNOWLEDGMENT that rides the next publish's
//!   event leg (patch frames carry no events field); `SCN_DELIVER` flushes
//!   queued demand answers; `SCN_DEBUG` dumps the wire tallies and the
//!   MCP-parity address list; `SCN_HELP` prints the feature table.
//! - **Drag demo** ([`mod@drag_demo`], `SCN_DEMO_DRAG`): a scene element
//!   carrying [`DragMeta`](ocs_plugin_api::scene::DragMeta) — the host
//!   runs Begin/host-local preview/Settle; the settle arrives as the
//!   mount's `drag_settle` command event, the plugin resolves the
//!   element's meta verbatim out of ITS OWN model (the wire never echoes
//!   it) and moves the node AUTHORITATIVELY with exactly one patch; a
//!   cancel is a lossless no-op (no event, nothing moves).
//! - **Menu + List demo** ([`mod@menu_list`], `SCN_DEMO_MENU`): a Menu
//!   (submenu + shortcut + one confirm-policy item) and an item List
//!   patched onto the widgets surface; invocations and activations ride
//!   the ordinary event stream.
//! - **Lazy-children tree demo** ([`mod@tree_lazy`], `SCN_DEMO_TREE`):
//!   an expandable branch with NO static children (`lazy_children=true`);
//!   every `ChildrenRequested` ask is answered with the NEXT BOUNDED PAGE
//!   (no duplicate fetch for satisfied windows).
//! - **GUI cell-edit closed loop** ([`mod@cell_edit`],
//!   `SCN_DEMO_CELLEDIT`): the host commits a cell edit (two-phase
//!   pointer / keyboard), the plugin receives `TableCellChanged`,
//!   validates it, applies it to its model and acks it with ONE cell
//!   patch; an invalid value is refused with the model untouched.
//! - **Asset + theming demo** ([`mod@asset_scene`], `SCN_DEMO_ASSET`):
//!   one host-managed [`SceneElement::Asset`] plus a theme-token vs
//!   fixed-[`Rgba8`](ocs_plugin_api::scene::Rgba8) swatch pair
//!   (theme-following vs never-changing).
//! - **Owner-law demo** ([`mod@owner_law`], `SCN_DEMO_OWNER`): a second
//!   publisher's publish of the LIVE panel id is refused
//!   `PublisherOwnedByOther` — structured, zero mutation (the headless
//!   double mirrors the multi-plugin host verbatim; the demo never fights
//!   the real host).
//!
//! See `PLUGIN-AUTHORING.md` beside this crate for the plugin-authoring
//! recipe guide; every recipe references the module that embodies it.
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

mod asset_scene;
mod cell_edit;
mod drag_demo;
mod menu_list;
mod owner_law;
mod tree_lazy;

// Keep these fields in sync with `plugin.toml` beside this package.
static MANIFEST: PluginManifest = PluginManifest {
    id: "schoeller.scene_example",
    name: "Scene Example",
    version: "0.4.0",
    description:
        "Reference API v24 add-on: the full scene-UI showcase (graph scenes, typed tables, \
         drag, menus, lazy trees, cell edits, assets, owner law) and its own authoring guide.",
    api_version: ApiVersion::CURRENT,
    ribbon_order: 120,
    xdata_apps: &[],
    command_prefixes: &["SCN_"],
};

const CMD_OPEN: &str = "SCN_OPEN";
/// The panel id as a dynamic command (the host registers it too — the
/// alias the root example's `EPL_PANEL` has).
const CMD_PANEL: &str = "SCN_PANEL";
const CMD_DEMO_SCENE: &str = "SCN_DEMO_SCENE";
const CMD_DEMO_WIDGETS: &str = "SCN_DEMO_WIDGETS";
const CMD_DEMO_STALE: &str = "SCN_DEMO_STALE";
const CMD_DELIVER: &str = "SCN_DELIVER";
const CMD_DEMO_DRAG: &str = "SCN_DEMO_DRAG";
const CMD_DEMO_MENU: &str = "SCN_DEMO_MENU";
const CMD_DEMO_TREE: &str = "SCN_DEMO_TREE";
const CMD_DEMO_CELLEDIT: &str = "SCN_DEMO_CELLEDIT";
const CMD_DEMO_ASSET: &str = "SCN_DEMO_ASSET";
const CMD_DEMO_OWNER: &str = "SCN_DEMO_OWNER";
/// The diagnostic dump (dispatch-addressable AND ribbon-visible): one
/// passive line of the demo's wire tallies, the MCP-parity address list
/// and the recent interaction journal.
const CMD_DEBUG: &str = "SCN_DEBUG";
/// The data-driven feature index: every registered feature's id, commands
/// and expected observation — printed straight off [`FEATURES`].
const CMD_HELP: &str = "SCN_HELP";

// ---------------------------------------------------------------------------
// THE DemoFeature seam: one capability = one module = one table row
// ---------------------------------------------------------------------------

/// The signature of one feature's flow: the send paths against the cached
/// request line, returning whether every frame applied. Refusals are
/// recorded through [`state::DemoState`] (the refusal report quotes the
/// per-run frame journal), and EVERY flow inherits the SCN-050b laws — a
/// rejected frame leaves everything exactly as it was.
pub(crate) type RunFn = fn(&Arc<dyn PluginRequestSender>, &mut state::DemoState) -> bool;

/// The flow-tail shape of the refusal report: the stale-recovery flow
/// quotes its `recovered` fact, every other flow names the frame leg and
/// the claimed revision.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum FlowTail {
    /// `"; recovered=false"` (the stale flow's arm).
    Recovery,
    /// `"; frame = <leg> (next <revision>)"` — the default arm.
    Frame,
}

/// ONE demo capability. Registered in [`FEATURES`]; everything downstream
/// (ribbon, dispatch, help, MCP address list) reads this table.
pub(crate) struct DemoFeature {
    /// Stable feature id (the `SCN_HELP` index and the docs name it).
    pub id: &'static str,
    /// The flow name the refusal report quotes (`Flow::<name> refused`).
    pub flow_name: &'static str,
    /// The `SCN_` commands that dispatch to this feature (the FIRST is
    /// the ribbon tool's command).
    pub commands: &'static [&'static str],
    /// What a user should expect to observe when the command runs.
    pub observation: &'static str,
    /// Ribbon presentation: `(group title, tool label, glyph, large)`.
    /// `None` keeps the feature off the ribbon (dispatch-addressable only).
    pub ribbon: Option<(&'static str, &'static str, &'static str, bool)>,
    /// The refusal-report tail shape (see [`FlowTail`]).
    pub tail: FlowTail,
    /// The feature's flow (the send paths).
    pub run: RunFn,
}

/// THE one registration table. Adding panel functionality = write a
/// module + add one line here; ribbon groups, dispatch, help and the MCP
/// address list all follow.
static FEATURES: [DemoFeature; 10] = [
    DemoFeature {
        id: "graph_scene",
        flow_name: "Scene",
        commands: &[CMD_DEMO_SCENE],
        observation: "four node cards + the clipped legend render; after the run node D and \
                      its edge are ADDED, B's label is REPLACED and the focus marker is \
                      REMOVED (one revision per patch frame)",
        ribbon: Some(("Scenes", "Scene Demo", "\u{229F}", true)),
        tail: FlowTail::Frame,
        run: scene_graph::run_scene_demo,
    },
    DemoFeature {
        id: "typed_table",
        flow_name: "Widgets",
        commands: &[CMD_DEMO_WIDGETS],
        observation: "the mount shows a REAL interactive typed table (the first 8 of 40 \
                      rows); a plugin-side one-cell patch moved row 305's length to 9.75 \
                      before your eyes; edit a cell IN the host to close the loop",
        ribbon: Some(("Scenes", "Widgets", "\u{25A6}", false)),
        tail: FlowTail::Frame,
        run: widgets_table::run_widgets_demo,
    },
    DemoFeature {
        id: "stale_recovery",
        flow_name: "Stale",
        commands: &[CMD_DEMO_STALE],
        observation: "one deliberately stale patch is refused StaleBase (the report names \
                      the host head), then the snapshot2 digest re-synchronizes the \
                      plugin and the re-patch lands the recovered label",
        ribbon: Some(("Scenes", "Stale conflict", "\u{21BA}", false)),
        tail: FlowTail::Recovery,
        run: recovery::run_stale_conflict,
    },
    DemoFeature {
        id: "deliver",
        flow_name: "Deliver",
        commands: &[CMD_DELIVER],
        observation: "queued demand answers (lazy children / table windows) flush as one \
                      snapshot-cycle publish each; nothing new is built",
        ribbon: None,
        tail: FlowTail::Frame,
        run: deliver_only,
    },
    DemoFeature {
        id: "drag",
        flow_name: "Drag",
        commands: &[CMD_DEMO_DRAG],
        observation: "the accent Drag card (drag meta scn-drag:node-1) is armed on the \
                      scene; drag it IN THE HOST — a settle (run this command after the \
                      gesture) moves it authoritatively with exactly ONE patch, while a \
                      cancel moves nothing (cancel emits no event)",
        ribbon: Some(("Showcase", "Drag demo", "\u{2725}", false)),
        tail: FlowTail::Frame,
        run: drag_demo::run_drag_demo,
    },
    DemoFeature {
        id: "menu_list",
        flow_name: "MenuList",
        commands: &[CMD_DEMO_MENU],
        observation: "the panel grows a Menu (submenu + shortcuts + one \
                      ConfirmPolicy::Prompt item) and a List of four layer items; menu \
                      invocations and item activations arrive as ordinary events",
        ribbon: Some(("Showcase", "Menu + List", "\u{2630}", false)),
        tail: FlowTail::Frame,
        run: menu_list::run_menu_list_demo,
    },
    DemoFeature {
        id: "tree_lazy",
        flow_name: "TreeLazy",
        commands: &[CMD_DEMO_TREE],
        observation: "the tree gains the 'Sheets (lazy)' branch (no static children); \
                      expanding it asks ChildrenRequested and every ask is answered with \
                      the NEXT bounded 4-row page — satisfied windows are never \
                      re-answered",
        ribbon: Some(("Showcase", "Lazy tree page", "\u{2503}", false)),
        tail: FlowTail::Frame,
        run: tree_lazy::run_tree_lazy_demo,
    },
    DemoFeature {
        id: "cell_edit",
        flow_name: "CellEdit",
        commands: &[CMD_DEMO_CELLEDIT],
        observation: "THE closed loop: a host-side cell edit (two-phase click or keyboard) \
                      arrives as TableCellChanged, the plugin validates it against its \
                      model, applies it and acks with ONE cell patch — a negative length \
                      is refused with the model untouched",
        ribbon: Some(("Showcase", "Cell edit loop", "\u{270E}", false)),
        tail: FlowTail::Frame,
        run: cell_edit::run_cell_edit_demo,
    },
    DemoFeature {
        id: "asset_theme",
        flow_name: "AssetTheme",
        commands: &[CMD_DEMO_ASSET],
        observation: "the scene gains a host-managed SVG asset (an opaque AssetRef — no \
                      bytes on the wire) and a swatch pair: the theme-token swatch \
                      follows the host theme, the fixed-RGBA swatch never changes",
        ribbon: Some(("Showcase", "Asset + theme", "\u{25E8}", false)),
        tail: FlowTail::Frame,
        run: asset_scene::run_asset_theme_demo,
    },
    DemoFeature {
        id: "owner_law",
        flow_name: "Owner",
        commands: &[CMD_DEMO_OWNER],
        observation: "a second publisher's publish of the LIVE panel id is refused \
                      PublisherOwnedByOther — structured, zero mutation; the headless \
                      double mirrors the multi-plugin host verbatim (this plugin never \
                      fights the real host)",
        ribbon: Some(("Showcase", "Owner law", "\u{2298}", false)),
        tail: FlowTail::Frame,
        run: owner_law::run_owner_probe,
    },
];

/// The refusal-report/flow handle: a borrow of ONE [`FEATURES`] row. The
/// name lives on the row (`flow_name`), so the report grammar is
/// table-driven exactly like the dispatch.
#[derive(Clone, Copy)]
pub(crate) struct Flow(pub(crate) &'static DemoFeature);

impl Flow {
    /// The flow name the refusal report quotes (the printout's WHO).
    fn name(&self) -> &'static str {
        self.0.flow_name
    }

    /// The refusal-report tail shape of this flow.
    fn tail(&self) -> FlowTail {
        self.0.tail
    }

    // The flow handles the tests address the legacy table rows by (kept
    // under cfg(test) so a plain `cargo build` never sees them — the
    // dispatch addresses features through `feature_for_command`).
    #[cfg(test)]
    pub(crate) fn scene() -> Self {
        Flow(&FEATURES[0])
    }
    #[cfg(test)]
    pub(crate) fn stale() -> Self {
        Flow(&FEATURES[2])
    }
    #[cfg(test)]
    pub(crate) fn owner() -> Self {
        Flow(&FEATURES[9])
    }
}

/// The demo's dispatch-addressable command strings (the [`FEATURES`]
/// table's rows, in registration order) — the MCP-parity address list
/// quotes them.
pub(crate) fn feature_commands() -> Vec<&'static str> {
    FEATURES
        .iter()
        .flat_map(|feature| feature.commands.iter().copied())
        .collect()
}

/// The feature a command dispatches to (`None` = not ours — the host's
/// answer stays `false`, exactly the root example's discipline).
fn feature_for_command(cmd: &str) -> Option<&'static DemoFeature> {
    FEATURES
        .iter()
        .find(|feature| feature.commands.contains(&cmd))
}

/// The `SCN_HELP` index: every registered feature's commands and its
/// expected observation, printed straight off the table (the table IS the
/// documentation surface).
fn features_help() -> String {
    let mut out = String::from("Scene Example features (one module each; registered in FEATURES):");
    for feature in FEATURES.iter() {
        out.push_str(&format!("\n  {:?} ({}) — observe: {}", feature.commands, feature.id, feature.observation));
    }
    out.push_str("\n  [\"SCN_OPEN\"/\"SCN_PANEL\"] toggle the panel; [\"SCN_DEBUG\"] dump tallies + MCP addresses.");
    out
}

/// The deliver-only flow: flush the queued demand answers only (the
/// pre-step in [`run_command`] already did the work; the flow itself is
/// the honest no-op).
fn deliver_only(_sender: &Arc<dyn PluginRequestSender>, _state: &mut state::DemoState) -> bool {
    true
}

/// Static panel definition: the v24 surfaces MOUNT on a registered panel —
/// the publish frames name the panel id the registration used (ownership
/// is transport-authenticated), and the body is informational: the demo's
/// state is the plugin's own semantic model, not dialog drafts.
fn demo_panel() -> PanelDef {
    PanelDef::new(scene_graph::PANEL_ID, "Scene Example")
        .with_command(CMD_OPEN)
        .with_width(360.0)
        .with_control(DialogControl::InfoText {
            text: "API v24 scene showcase: SCN_HELP lists every feature and what to \
                   observe; SCN_DEMO_SCENE builds the graph scene, SCN_DEMO_WIDGETS the \
                   typed-table surface, SCN_DEMO_STALE the conflict/recovery loop, \
                   SCN_DEMO_DRAG / SCN_DEMO_MENU / SCN_DEMO_TREE / SCN_DEMO_CELLEDIT / \
                   SCN_DEMO_ASSET / SCN_DEMO_OWNER the rest of the showcase, SCN_DEBUG \
                   dumps the wire tallies + MCP addresses."
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

/// The ribbon tab, built DATA-DRIVEN off [`FEATURES`]: features with a
/// ribbon slot join their titled group in registration order (the first
/// command of the feature is the tool's command).
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
            let mut groups: Vec<RibbonGroup> = Vec::new();
            for feature in FEATURES.iter() {
                let Some((title, label, glyph, large)) = feature.ribbon else {
                    continue;
                };
                let command = feature.commands[0];
                let item = match large {
                    true => RibbonItem::LargeTool(ToolDef {
                        id: command,
                        label,
                        icon: IconKind::Glyph(glyph),
                        event: ModuleEvent::Command(command.to_string()),
                    }),
                    false => RibbonItem::LabeledTool(ToolDef {
                        id: command,
                        label,
                        icon: IconKind::Glyph(glyph),
                        event: ModuleEvent::Command(command.to_string()),
                    }),
                };
                match groups.iter_mut().find(|group| group.title == title) {
                    Some(group) => group.tools.push(item),
                    None => groups.push(RibbonGroup {
                        title,
                        tools: vec![item],
                    }),
                }
            }
            groups
        })
    }
}

/// The refusal report — SELF-EXPLAINING: which flow refused, the
/// structured `PatchRejectCode` with the report's head and the frame's
/// base when a coded reject reached the plugin, or the exact codeless
/// cause when the answer named NO code (a transport failure, or the wire's
/// plain `Error` — on the live host the V4 nested-serving guard's text,
/// carried verbatim so the user's own log names the real cause). The
/// material is THIS run's frame record (`begin_flow` cleared it; the send
/// sites re-recorded), never a stale one.
fn refused_report(state: &state::DemoState, flow: &Flow) -> String {
    let cause = match &state.last_frame {
        None => "the flow stopped at its own gates — no frame reached the wire".to_string(),
        Some(fact) => match &fact.outcome {
            state::FrameOutcome::Rejected(code, head) => {
                let head_text = head
                    .map(|revision| revision.to_string())
                    .unwrap_or_else(|| "—".to_string());
                let base_text = fact
                    .base
                    .map(|base| base.to_string())
                    .unwrap_or_else(|| "—".to_string());
                format!("{code:?}; host head {}; frame base {}", head_text, base_text)
            }
            state::FrameOutcome::AppliedModelLagged(revision) => format!(
                "the frame applied on the wire (host head {}) but the plugin's \
                 model mirror refused — the model lags the host; the next full \
                 publish realigns it",
                revision
            ),
            state::FrameOutcome::Applied => format!(
                "the flow stopped locally after the {} answered (no host rejection)",
                fact.leg
            ),
            state::FrameOutcome::SendFailed(note) => format!(
                "no code named — the request line answered a transport error: {}",
                note
            ),
            state::FrameOutcome::Unserviceable(note) => format!(
                "no code named — the wire answered `Error`, not a structured \
                 reject: {}",
                note
            ),
            state::FrameOutcome::Unexpected => {
                "no code named — the wire's answer decoded as neither an ack \
                  nor a structured reject"
                    .to_string()
            }
        },
    };
    // The flow-specific tail (table-driven off the row): the stale flow's
    // `recovered` fact (the refused arm's recovery never completed); the
    // other flows name the frame the answer came from (its claimed
    // revision when one was claimed).
    let flow_tail = match flow.tail() {
        FlowTail::Recovery => "; recovered=false".to_string(),
        FlowTail::Frame => match &state.last_frame {
            Some(fact) => match fact.next {
                Some(next) => format!("; frame = {} (next {})", fact.leg, next),
                None => format!("; frame = {} (no revision claimed)", fact.leg),
            },
            None => String::new(),
        },
    };
    format!("Scene Example: Flow::{} refused ({}){}", flow.name(), cause, flow_tail)
}

/// One demo command's shape: flush the queued demand answers first (the
/// answers are publishes), then run the command's own flow (resolved from
/// the [`FEATURES`] table), and report what the wire did — the applied arm
/// keeps its tally awareness; the refused arm names the flow, the reject
/// code, the report's head and the frame's base/claimed revision (or the
/// exact codeless cause).
fn run_command(
    sender: &Option<Arc<dyn PluginRequestSender>>,
    state: &mut state::DemoState,
    flow: Flow,
) -> String {
    let Some(sender) = sender else {
        return "Scene Example: no host request line — nothing sent.".to_string();
    };
    // The per-run frame record clears: a refusal print names THIS run's
    // exchange, never a previous one's.
    state.begin_flow();
    let delivered = match state.widgets.is_some() {
        true => widgets_table::deliver_pending(sender, state),
        false => 0,
    };
    let applied = (flow.0.run)(sender, state);
    let delivered_note = match delivered {
        0 => String::new(),
        n => format!("; {n} queued page deliver"),
    };
    match applied {
        true => format!(
            "Scene Example: flow applied{} (sent {}, applied {}, rejected {})",
            delivered_note, state.sent, state.applied, state.rejected
        ),
        false => format!(
            "{}{}",
            refused_report(state, &flow),
            delivered_note
        ),
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
            host.push_info("Scene Example loaded — type SCN_HELP or use the ribbon tools.");
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
            CMD_OPEN | CMD_PANEL => {
                let shown = host.toggle_panel(scene_graph::PANEL_ID, None);
                host.push_info(if shown {
                    "Scene Example panel toggled."
                } else {
                    "Scene Example panel toggle failed."
                });
                true
            }
            // The diagnostic dump: state-only, sender-free — one passive
            // line of the tallies the refusal reports quote, plus the
            // MCP-parity addresses and the recent interaction journal.
            CMD_DEBUG => {
                let info = self.state().debug_report();
                host.push_info(&info);
                true
            }
            // The feature index, printed straight off the FEATURES table.
            CMD_HELP => {
                let info = features_help();
                host.push_info(&info);
                true
            }
            _ => match feature_for_command(cmd) {
                Some(feature) => {
                    let info = run_command(&self.sender(), &mut self.state(), Flow(feature));
                    host.push_info(&info);
                    true
                }
                None => false,
            },
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
    use ocs_plugin_api::host::{HostNotification, PluginRequestError, PluginRequestSender};
    use ocs_plugin_api::ipc::protocol::{PluginRequest, PluginResponse};
    use ocs_plugin_api::manifest::API_VERSION;
    use ocs_plugin_api::scene::{SceneRevision, StableId};
    use ocs_plugin_api::widgets::{WidgetEvent, WidgetRange};

    use crate::fake_host::FakeWireHost;
    use crate::state::DemoState;

    use super::*;

    /// The scripted-request-line double: every frame gets the SAME
    /// scripted answer. The two codeless answer shapes the live host can
    /// produce (the wire's plain `Error` text — the V4 nested-serving
    /// guard's refusal — and a transport-level failure) drive the
    /// no-code-named refusal reports through the plugin's real send paths.
    struct Scripted {
        answer: ScriptedAnswer,
    }

    enum ScriptedAnswer {
        /// The wire's plain `Error` (a codeless refusal).
        Error(String),
        /// The request line itself failed (no answer exists).
        Transport(String),
    }

    impl PluginRequestSender for Scripted {
        fn request(
            &self,
            _req: PluginRequest,
        ) -> Result<PluginResponse, PluginRequestError> {
            match &self.answer {
                ScriptedAnswer::Error(message) => {
                    Ok(PluginResponse::Error(message.clone()))
                }
                ScriptedAnswer::Transport(message) => Err(PluginRequestError(message.clone())),
            }
        }
    }

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

    /// THE refusal report's coded shape, byte-exact against the user's
    /// log grammar: WHICH flow, the `PatchRejectCode`, the report's
    /// current head, the frame's base — and the stale flow's recovery
    /// fact. The record is exactly what `run_stale_conflict`'s refuse
    /// path leaves (base 11, claimed successor 12, the report naming
    /// head 12).
    #[test]
    fn refusal_report_names_flow_code_heads_and_frame() {
        let mut state = DemoState::default();
        state.note_reject(
            "scene patch",
            Some(11),
            Some(12),
            ocs_plugin_api::ipc::protocol::PatchRejectCode::StaleBase,
            Some(ocs_plugin_api::scene::SceneRevision(12)),
        );
        assert_eq!(
            refused_report(&state, &Flow::stale()),
            "Scene Example: Flow::Stale refused (StaleBase; host head 12; frame base 11); \
             recovered=false"
        );
        // The scene flow's twin of the same refusal names the frame leg
        // and the claimed revision instead of the recovery fact.
        assert_eq!(
            refused_report(&state, &Flow::scene()),
            "Scene Example: Flow::Scene refused (StaleBase; host head 12; frame base 11); \
             frame = scene patch (next 12)"
        );
    }

    /// The CODELESS refusal shape — the user's live ×2 signature: an
    /// answer that names no reject code. The live host's two producers
    /// (the wire's plain `Error` text — on the V4 transport, the
    /// nested-serving tab guard — and a transport failure) print their own
    /// text verbatim, never a guess, and leave the structured
    /// `last_reject` untouched (exactly the silent recorded-lag symptom
    /// the old generic line hid).
    #[test]
    fn codeless_refusals_print_the_wire_note_verbatim() {
        let guard_note = "nested plugin request for tab 2 cannot be served \
                          inline while the host dispatches on tab 1; send it \
                          again after that dispatch returns"
            .to_string();
        let sender: std::sync::Arc<dyn PluginRequestSender> = std::sync::Arc::new(Scripted {
            answer: ScriptedAnswer::Error(guard_note.clone()),
        });
        let mut state = DemoState::default();
        let report = run_command(&Some(sender), &mut state, Flow::scene());
        assert_eq!(
            report,
            format!(
                "Scene Example: Flow::Scene refused (no code named — the wire \
                 answered `Error`, not a structured reject: {guard_note}); \
                 frame = scene publish (next 1)"
            )
        );
        assert_eq!(state.unserved_answers, 1);
        assert_eq!(state.send_failures, 0);
        assert_eq!(state.last_reject, None, "no coded reject ever reached us");

        // The transport-failure twin: the request line itself answered an
        // error (no answer exists for the frame). The stale flow's own
        // gates need the model's scene leg first (headless seed — the
        // wire is down anyway).
        let sender: std::sync::Arc<dyn PluginRequestSender> = std::sync::Arc::new(Scripted {
            answer: ScriptedAnswer::Transport("V4 reader thread disconnected".to_string()),
        });
        let mut state = DemoState::default();
        state.scene = Some(crate::scene_graph::demo_graph_scene(4).unwrap());
        let report = run_command(&Some(sender), &mut state, Flow::stale());
        assert_eq!(
            report,
            "Scene Example: Flow::Stale refused (no code named — the request \
             line answered a transport error: V4 reader thread \
             disconnected); recovered=false"
                .to_string()
        );
        assert_eq!(state.send_failures, 1);
        assert_eq!(state.last_reject, None);
    }

    /// The APPLIED arm keeps its tally awareness: the flow phrase, the
    /// delivered-note slot, and the wire counts in one line.
    #[test]
    fn applied_report_keeps_tally_awareness() {
        let host = std::sync::Arc::new(FakeWireHost::new());
        let sender: std::sync::Arc<dyn PluginRequestSender> =
            std::sync::Arc::clone(&host) as std::sync::Arc<dyn PluginRequestSender>;
        let mut state = DemoState::default();
        let report = run_command(&Some(sender), &mut state, Flow::scene());
        assert_eq!(
            report,
            "Scene Example: flow applied (sent 4, applied 4, rejected 0)"
        );

        // The diagnostic dump (SCN_DEBUG's printout): the same tallies,
        // both legs' heads, the pending queues and the run's frame record.
        let dump = state.debug_report();
        assert!(
            dump.contains("sent 4, applied 4, rejected 0, stale rejected 0, recovered 0"),
            "the dump's tallies: {dump}"
        );
        assert!(dump.contains("head 4 | scene head 4, widget head —"), "{dump}");
        assert!(dump.contains("pending ack 0, pending deliver 0"), "{dump}");
        assert!(
            dump.contains("last reject: None | last frame: Some(FrameFact { leg: \"scene patch\", base: Some(3), next: Some(4), outcome: Applied })"),
            "the dump's last frame: {dump}"
        );
    }

    /// THE SEAM: the FEATURES table is the ONE registration point — every
    /// row carries an id, at least one command, an observation note and a
    /// flow; the commands are unique across the whole table (a dispatch
    /// must resolve to exactly ONE feature) and every `SCN_` command the
    /// demo answers is a table row.
    #[test]
    fn features_table_is_the_single_registration_point() {
        let mut commands: Vec<&str> = Vec::new();
        for feature in FEATURES.iter() {
            assert!(!feature.id.is_empty());
            assert!(!feature.commands.is_empty(), "{} exposes a command", feature.id);
            assert!(
                !feature.observation.is_empty(),
                "{} carries its expected observation",
                feature.id
            );
            commands.extend_from_slice(feature.commands);
        }
        let unique = commands.iter().collect::<std::collections::HashSet<_>>();
        assert_eq!(
            unique.len(),
            commands.len(),
            "command strings resolve to exactly one feature: {commands:?}"
        );
        // The whole dispatch-addressable surface is table-driven: the
        // bespoke arms are only the panel toggle and the two state-only
        // dumps.
        for command in commands {
            assert!(feature_for_command(command).is_some());
        }
        assert!(feature_for_command("SCN_NOT_OURS").is_none());
        assert!(feature_for_command(CMD_DEBUG).is_none(), "state-only");
        assert!(feature_for_command(CMD_HELP).is_none(), "state-only");
        assert!(feature_for_command(CMD_OPEN).is_none(), "panel arm");
    }

    /// THE FULL SHOWCASE on ONE cadence: every registered feature's flow
    /// runs headless against the wire double, in table order, and every
    /// frame applies — the one-panel-one-revision-cadence law holds across
    /// all of them, and the ribbon/help/dispatch surfaces all name the
    /// same rows.
    #[test]
    fn all_features_headless_on_one_cadence() {
        let host = std::sync::Arc::new(FakeWireHost::new());
        let sender: std::sync::Arc<dyn PluginRequestSender> =
            std::sync::Arc::clone(&host) as std::sync::Arc<dyn PluginRequestSender>;
        let mut state = DemoState::default();
        for feature in FEATURES.iter() {
            let head_before = state.head;
            assert!(
                (feature.run)(&sender, &mut state),
                "feature {} applies headless",
                feature.id
            );
            assert!(
                state.head > head_before || state.head == head_before,
                "the cadence never rewinds ({})",
                feature.id
            );
        }
        // Both legs live; the heads agree with the host's store.
        assert!(state.scene.is_some());
        assert!(state.widgets.is_some());
        assert_eq!(state.widgets_head(), host.widgets().map(|surface| surface.revision));
        // The cadence head covers both legs (one panel, one cadence).
        assert!(state.head >= state.scene_head().expect("scene leg").0);
        assert!(state.head >= state.widgets_head().expect("widget leg").0);
        // The help index names every feature (the table IS the doc).
        let help = features_help();
        for feature in FEATURES.iter() {
            assert!(help.contains(feature.id), "help names {}: {help}", feature.id);
        }
    }

    /// The MCP-parity observable: the debug dump lists the ADDRESSES an
    /// MCP client would use — `panel_semantics` / `panel_page` /
    /// `panel_widget_invoke` address the very surfaces this plugin
    /// publishes (one host path; the plugin ships no MCP code).
    #[test]
    fn debug_report_lists_mcp_addresses() {
        let host = std::sync::Arc::new(FakeWireHost::new());
        let sender: std::sync::Arc<dyn PluginRequestSender> =
            std::sync::Arc::clone(&host) as std::sync::Arc<dyn PluginRequestSender>;
        let mut state = DemoState::default();
        assert!(scene_graph::run_scene_demo(&sender, &mut state));
        assert!(widgets_table::run_widgets_demo(&sender, &mut state));

        let dump = state.debug_report();
        for address in [
            "panel_semantics(panel \"scn_panel\", surface 90)",
            "tree 100",
            "table 200, cols 201..=204, keys 300..=339 total 40",
            "panel_page(",
            "panel_widget_invoke(commands",
            "scn_delete_node",
            "drag_settle",
        ] {
            assert!(dump.contains(address), "the dump names {address}: {dump}");
        }
    }
}
