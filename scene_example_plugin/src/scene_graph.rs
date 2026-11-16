//! The graph-scene demo: a small node-and-edge [`PanelScene`] built with the
//! crate's fluent [`SceneBuilder`], published through
//! `PluginRequest::PanelScenePublish`, then moved forwards with three real
//! `PluginRequest::PanelScenePatch` frames (`Add` / `Replace` / `Remove` —
//! the wire ops' names), each naming a fresh base revision and a strictly
//! greater successor.
//!
//! Id scheme (plugin-assigned [`StableId`]s, unique across the scene):
//!
//! ```text
//! node k owns:   group 10·k, card 10·k+1, port E 10·k+2,
//!                port W 10·k+3, label 10·k+4      (k = 0..3)
//! legend:        group 60 (with a clip rect), text 61, marker 62
//! edges:         70 (A→B), 71 (B→C), 72 (C→D — added by patch 1)
//! focus marker:  80 (removed by patch 3)
//! ```

use std::sync::Arc;

use ocs_plugin_api::host::PluginRequestSender;
use ocs_plugin_api::scene::{
    MarkerKind, PanelScene, PanelScenePatch, Rect2D, SceneColor, SceneElement, SceneError,
    SceneRevision, StableId, ThemeColor, Vec2,
};
use ocs_plugin_api::scene_builder::{PatchBuilder, SceneBuilder};

use crate::host_wire;
use crate::state::{DemoState, FrameOutcome};

/// The demo panel id — the same id the panel registration used; ownership
/// is transport-authenticated, so this field is a name, not a credential.
pub const PANEL_ID: &str = "scn_panel";

/// Marker the patch-3 demo removes (a standalone annotation at the root).
pub const FOCUS_MARKER: u64 = 80;

/// Label text the recovery re-patch writes (the recovered state's marker).
pub const RECOVERED_LABEL: &str = "Node B (re-synchronized)";

const CARD_SIZE: [f64; 2] = [160.0, 64.0];
const TEXT_SIZE: f32 = 14.0;

/// One node card in the currently open container of `scene`: an inner
/// group (card rect + one port per side + one text label) with a display
/// name. The chained builder carries the caller's lifetime — the fluent
/// `Result<&mut Self, _>` arms.
fn add_node_card<'a>(
    scene: &'a mut SceneBuilder,
    k: u64,
    origin: [f64; 2],
    label: &str,
) -> Result<&'a mut SceneBuilder, SceneError> {
    let base = 10 * k;
    let [x, y] = origin;
    let card_mid_right = [x + CARD_SIZE[0], y + CARD_SIZE[1] / 2.0];
    let card_mid_left = [x, y + CARD_SIZE[1] / 2.0];
    let label_anchor = [x + 8.0, y + 18.0];
    scene
        .group(base)?
        .name(Some(label))
        .rect(base + 1, origin, CARD_SIZE)?
        .port(base + 2, card_mid_right)?
        .port(base + 3, card_mid_left)?
        .text(
            base + 4,
            label,
            label_anchor,
            TEXT_SIZE,
            SceneColor::Theme(ThemeColor::Normal),
        )?
        .end_group();
    Ok(scene)
}

/// Harvest ONE root element from its own [`SceneBuilder`] sub-scope: the
/// patch ops (`PatchBuilder::add`) carry [`SceneElement`] payloads, and a
/// sub-scope build gives the same builder-driven shapes. The inner
/// `finish()` runs the same validator on the sub-scope; the demo's helpers
/// add exactly one root by construction (pinned by the tests).
pub(crate) fn built_element(
    build: impl FnOnce(&mut SceneBuilder) -> Result<&mut SceneBuilder, SceneError>,
) -> Result<SceneElement, SceneError> {
    let mut builder = SceneBuilder::new(0);
    build(&mut builder)?;
    let staged = builder.finish()?;
    let mut elements = staged.elements;
    debug_assert_eq!(elements.len(), 1, "demo element builders stage one root");
    Ok(elements.swap_remove(0))
}

/// The legend group: the one `Group.clip` demo case (children render only
/// inside the clip). Built as a literal because the builder's `group()`
/// opens clip-less groups; [`SceneBuilder::element`] is the documented
/// escape hatch that inserts a pre-built subtree (its ids stay ledgered).
fn legend_group() -> SceneElement {
    SceneElement::Group {
        id: StableId(60),
        name: Some("Legend".into()),
        actions: Vec::new(),
        drag: None,
        children: vec![
            SceneElement::Text {
                id: StableId(61),
                name: None,
                actions: Vec::new(),
                drag: None,
                content: "Graph legend: edges route port to port.".to_string(),
                pos: Vec2([48.0, 372.0]),
                size: TEXT_SIZE,
                color: SceneColor::Theme(ThemeColor::Muted),
            },
            SceneElement::Marker {
                id: StableId(62),
                name: None,
                actions: Vec::new(),
                drag: None,
                at: Vec2([52.0, 404.0]),
                kind: MarkerKind::Diamond,
                color: SceneColor::Theme(ThemeColor::Success),
            },
        ],
        clip: Some(Rect2D {
            pos: Vec2([40.0, 360.0]),
            size: Vec2([260.0, 72.0]),
        }),
    }
}

/// The demo's initial graph scene (nodes A/B/C + legend, two edges, the
/// focus marker) at the given revision.
pub fn demo_graph_scene(revision: u64) -> Result<PanelScene, SceneError> {
    let mut scene = SceneBuilder::new(revision);
    add_node_card(&mut scene, 0, [40.0, 40.0], "Node A")?;
    add_node_card(&mut scene, 1, [320.0, 40.0], "Node B")?;
    add_node_card(&mut scene, 2, [320.0, 240.0], "Node C")?;
    scene.element(legend_group());
    scene
        .edge(70, 12, 23, vec![Vec2([200.0, 72.0]), Vec2([320.0, 72.0])])?
        .name(Some("A to B"));
    scene
        .edge(
            71,
            22,
            33,
            vec![
                Vec2([480.0, 72.0]),
                Vec2([480.0, 272.0]),
                Vec2([320.0, 272.0]),
            ],
        )?
        .name(Some("B to C"));
    scene
        .marker(
            FOCUS_MARKER,
            [260.0, 200.0],
            MarkerKind::Arrow,
            SceneColor::Theme(ThemeColor::Warning),
        )?
        .name(Some("Focus"));
    scene.finish()
}

/// Node "D" as a whole-subtree element (the `Add` op's payload of patch 1).
fn node_d() -> Result<SceneElement, SceneError> {
    built_element(|scene| add_node_card(scene, 3, [560.0, 240.0], "Node D"))
}

/// Edge C→D (the second `Add` op of patch 1): routes node C's east port
/// (id 32) to node D's west port (id 43) as a straight two-point route.
fn edge_c_to_d() -> Result<SceneElement, SceneError> {
    built_element(|scene| scene.edge(72, 32, 43, vec![Vec2([480.0, 272.0]), Vec2([560.0, 272.0])]))
}

/// B's label as a full replacement element (the `Replace` op of patch 2 is
/// keyed by the element's own id — the whole subtree moves with it; a Text
/// is a leaf, so the swap is exactly the new run).
fn node_b_label() -> SceneElement {
    SceneElement::Text {
        id: StableId(24),
        name: None,
        actions: Vec::new(),
        drag: None,
        content: "Node B (second pass)".to_string(),
        pos: Vec2([328.0, 58.0]),
        size: TEXT_SIZE,
        color: SceneColor::Theme(ThemeColor::Normal),
    }
}

type PatchStep = fn(SceneRevision, &mut DemoState) -> Result<PanelScenePatch, SceneError>;

/// Patch 1 (Add): node D plus its route.
fn add_node_d_patch(
    base: SceneRevision,
    state: &mut DemoState,
) -> Result<PanelScenePatch, SceneError> {
    let successor = state.next_revision();
    let mut builder = PatchBuilder::new(base.0);
    builder.add(None, node_d()?);
    builder.add(None, edge_c_to_d()?);
    builder.finish(successor.0)
}

/// Patch 2 (Replace): B's label gets a new run (the successor must exceed
/// the base — enforced by `PatchBuilder::finish` before the send).
fn relabel_b_patch(
    base: SceneRevision,
    state: &mut DemoState,
) -> Result<PanelScenePatch, SceneError> {
    let successor = state.next_revision();
    PatchBuilder::new(base.0)
        .replace(node_b_label())
        .finish(successor.0)
}

/// Patch 3 (Remove): the focus marker's subtree goes away.
fn drop_marker_patch(
    base: SceneRevision,
    state: &mut DemoState,
) -> Result<PanelScenePatch, SceneError> {
    let successor = state.next_revision();
    PatchBuilder::new(base.0)
        .remove(FOCUS_MARKER)
        .finish(successor.0)
}

/// The snapshot leg of the scene demo: the whole bounded scene at the
/// next revision. This is ALSO the ensure-base step the other scene-leg
/// features call (only when the model holds no scene yet — a fresh
/// fixture publish replaces the model, so callers gate it on
/// `state.scene.is_none()`). Every exchange records its [`FrameFact`]
/// (the refusal report's material); a non-applied publish RESTORES the
/// taken ack batch — the merge acknowledgment only rode an APPLIED frame;
/// the next publish still owes it.
pub(crate) fn publish_scene_base(
    sender: &Arc<dyn PluginRequestSender>,
    state: &mut DemoState,
) -> bool {
    let scene = match demo_graph_scene(state.next_revision().0) {
        Ok(scene) => scene,
        Err(_) => return false,
    };
    let claimed = scene.revision.0;
    let acks = std::mem::take(&mut state.pending_ack);
    state.sent += 1;
    match host_wire::send_scene_publish(sender, PANEL_ID, scene.clone(), acks.clone()) {
        host_wire::WireOutcome::Applied { revision, .. } => {
            debug_assert_eq!(revision, scene.revision);
            state.applied += 1;
            state.note_frame("scene publish", None, Some(claimed), FrameOutcome::Applied);
            state.scene = Some(scene);
            true
        }
        host_wire::WireOutcome::Rejected { code, report } => {
            state.pending_ack = acks;
            state.note_reject(
                "scene publish",
                None,
                Some(claimed),
                code,
                report.current_revision,
            );
            false
        }
        host_wire::WireOutcome::Unserviceable(message) => {
            state.pending_ack = acks;
            state.note_frame("scene publish", None, Some(claimed), FrameOutcome::Unserviceable(message));
            false
        }
        host_wire::WireOutcome::Failed(error) => {
            state.pending_ack = acks;
            state.note_frame("scene publish", None, Some(claimed), FrameOutcome::SendFailed(error));
            false
        }
        host_wire::WireOutcome::Unexpected => {
            state.pending_ack = acks;
            state.note_frame("scene publish", None, Some(claimed), FrameOutcome::Unexpected);
            false
        }
    }
}

/// Publish the graph scene and walk the three forward patches. Every frame
/// carries the piggybacked event-ack leg (empties the pending batch);
/// successes mirror the op into the plugin's own model so the next patch's
/// base is honest.
pub fn run_scene_demo(sender: &Arc<dyn PluginRequestSender>, state: &mut DemoState) -> bool {
    // (1) Snapshot leg: the whole bounded scene at the next revision.
    if !publish_scene_base(sender, state) {
        return false;
    }

    // (2..=4) The three forward patches, one revision each, each built
    // against the leg head the store then holds.
    let steps: [PatchStep; 3] = [add_node_d_patch, relabel_b_patch, drop_marker_patch];
    for step in steps {
        let Some(base) = state.scene_head() else {
            return false;
        };
        let Ok(patch) = step(base, state) else {
            return false;
        };
        let base_revision = patch.base_revision.0;
        let claimed = patch.revision.0;
        state.sent += 1;
        match host_wire::send_scene_patch(sender, PANEL_ID, patch.clone()) {
            host_wire::WireOutcome::Applied { revision, .. } => {
                state.applied += 1;
                let Some(scene) = state.scene.as_mut() else {
                    return false;
                };
                if scene.apply_patch(patch).is_err() {
                    state.note_frame(
                        "scene patch",
                        Some(base_revision),
                        Some(claimed),
                        FrameOutcome::AppliedModelLagged(revision.0),
                    );
                    return false;
                }
                debug_assert_eq!(scene.revision, revision, "model and host agree");
                state.note_frame(
                    "scene patch",
                    Some(base_revision),
                    Some(claimed),
                    FrameOutcome::Applied,
                );
            }
            host_wire::WireOutcome::Rejected { code, report } => {
                state.note_reject(
                    "scene patch",
                    Some(base_revision),
                    Some(claimed),
                    code,
                    report.current_revision,
                );
                return false;
            }
            host_wire::WireOutcome::Unserviceable(message) => {
                state.note_frame("scene patch", Some(base_revision), Some(claimed), FrameOutcome::Unserviceable(message));
                return false;
            }
            host_wire::WireOutcome::Failed(error) => {
                state.note_frame("scene patch", Some(base_revision), Some(claimed), FrameOutcome::SendFailed(error));
                return false;
            }
            _ => {
                state.note_frame("scene patch", Some(base_revision), Some(claimed), FrameOutcome::Unexpected);
                return false;
            }
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use ocs_plugin_api::ipc::protocol::{PluginRequest, SceneOrWidgetSurface};
    use ocs_plugin_api::scene::{SceneError, MAX_PATCH_OPS, MAX_TEXT_BYTES};

    use crate::fake_host::FakeWireHost;

    use super::*;

    #[test]
    fn graph_scene_demo_builds_and_validates() {
        let scene = demo_graph_scene(1).expect("the demo scene is structurally valid");
        assert_eq!(scene.revision, SceneRevision(1));
        let summary = scene.scene_summary();
        let counts = summary.by_kind;
        assert_eq!(counts.groups, 4, "nodes A/B/C + the legend group");
        assert_eq!(counts.rects, 3, "one card rect per node");
        assert_eq!(counts.ports, 6, "two ports per node");
        assert_eq!(counts.texts, 4, "one label per node + the legend run");
        assert_eq!(counts.markers, 2, "legend diamond + focus arrow");
        assert_eq!(counts.edges, 2);
        assert_eq!(summary.nodes, counts.total());
        // The unique-id law over the whole tree already ran inside finish().
    }

    #[test]
    fn graph_scene_demo_is_byte_deterministic() {
        // Identical args -> identical bytes (the builder is a pure function
        // of its inputs; the wire carries exactly these bytes).
        let a = bincode::serialize(&demo_graph_scene(7).unwrap()).unwrap();
        let b = bincode::serialize(&demo_graph_scene(7).unwrap()).unwrap();
        assert_eq!(a.len(), b.len());
        assert_eq!(a, b);
    }

    #[test]
    fn api_fixture_graph_scene_is_byte_deterministic() {
        let a = bincode::serialize(&ocs_plugin_api::fixture_graph_scene(1, 3, 7)).unwrap();
        let b = bincode::serialize(&ocs_plugin_api::fixture_graph_scene(1, 3, 7)).unwrap();
        assert_eq!(a, b, "fixture_graph_scene: identical args, identical bytes");
    }

    #[test]
    fn scene_and_patch_frames_roundtrip_serde() {
        // A publish frame and a patch frame both survive the wire encode:
        // bincode serialize -> bytes -> deserialize -> equal values.
        let scene = demo_graph_scene(3).unwrap();
        let frame = PluginRequest::PanelScenePublish {
            panel_id: PANEL_ID.to_owned(),
            surface: SceneOrWidgetSurface::Scene(scene.clone()),
            events: Vec::new(),
        };
        let round: PluginRequest =
            bincode::deserialize(&bincode::serialize(&frame).unwrap()).unwrap();
        let PluginRequest::PanelScenePublish {
            panel_id,
            surface,
            events,
        } = round
        else {
            panic!("the publish frame decodes as itself");
        };
        assert_eq!(panel_id, PANEL_ID);
        let SceneOrWidgetSurface::Scene(published) = surface else {
            panic!("the scene leg stays the scene leg");
        };
        assert_eq!(published, scene);
        assert!(events.is_empty());

        let patch = PatchBuilder::new(3)
            .add(None, node_d().unwrap())
            .add(None, edge_c_to_d().unwrap())
            .remove(FOCUS_MARKER)
            .finish(4)
            .unwrap();
        let round_patch: PanelScenePatch =
            bincode::deserialize(&bincode::serialize(&patch).unwrap()).unwrap();
        assert_eq!(round_patch, patch);
        assert_eq!(round_patch.ops.len(), 3);
    }

    #[test]
    fn patches_advance_the_scene_head_with_real_ops() {
        // The model walk the plugin mirrors (the fake host applies the same
        // frames): publish @1 then Add/Replace/Remove @2/3/4.
        let mut scene = demo_graph_scene(1).unwrap();
        let add = PatchBuilder::new(1)
            .add(None, node_d().unwrap())
            .add(None, edge_c_to_d().unwrap())
            .finish(2)
            .unwrap();
        let report = scene.apply_patch_report(add).unwrap();
        assert_eq!(report.revision, SceneRevision(2));
        assert_eq!(scene.scene_summary().by_kind.rects, 4, "node D's card");

        let relabel = PatchBuilder::new(2)
            .replace(node_b_label())
            .finish(3)
            .unwrap();
        let report = scene.apply_patch_report(relabel).unwrap();
        assert_eq!(report.revision, SceneRevision(3));
        assert_eq!(
            report.touched_subtree_ids,
            vec![StableId(24)],
            "Replace names the target id as its touched top"
        );

        let drop = PatchBuilder::new(3).remove(FOCUS_MARKER).finish(4).unwrap();
        let report = scene.apply_patch_report(drop).unwrap();
        assert_eq!(report.revision, SceneRevision(4));
        assert_eq!(
            report.retired_ids,
            vec![StableId(FOCUS_MARKER)],
            "Remove retires the subtree root id (a leaf marker)"
        );
        assert_eq!(
            scene.scene_summary().by_kind.markers,
            1,
            "the legend diamond stays"
        );
    }

    #[test]
    fn over_budget_patch_refused_at_the_builder() {
        let mut builder = PatchBuilder::new(0);
        for index in 0..=MAX_PATCH_OPS {
            // MAX_PATCH_OPS+1 Add ops: the finish gate refuses the whole
            // patch (first crossing, exact counts).
            builder.add(
                None,
                SceneElement::Rect {
                    id: StableId(200_000 + index as u64),
                    name: None,
                    actions: Vec::new(),
                    drag: None,
                    pos: Vec2([0.0, 0.0]),
                    size: Vec2([1.0, 1.0]),
                    fill: None,
                    stroke: None,
                },
            );
        }
        let Err(SceneError::PatchOpBudgetExceeded { ops, max }) = builder.finish(1) else {
            panic!("the op-budget gate must refuse");
        };
        assert_eq!(ops, MAX_PATCH_OPS + 1);
        assert_eq!(max, MAX_PATCH_OPS);
    }

    #[test]
    fn over_budget_text_is_a_structural_refuse() {
        let run = "x".repeat(MAX_TEXT_BYTES + 1);
        let scene = SceneBuilder::new(1)
            .text(
                700,
                &run,
                [0.0, 0.0],
                TEXT_SIZE,
                SceneColor::Theme(ThemeColor::Normal),
            )
            .unwrap()
            .finish();
        assert!(matches!(scene, Err(SceneError::TextBudgetExceeded { .. })));
    }

    #[test]
    fn scene_demo_full_flow_headless() {
        // The demo WITHOUT a running host: the send paths against a
        // `PluginRequestSender` double that serves the v24 attach contract.
        let host: Arc<FakeWireHost> = Arc::new(FakeWireHost::new());
        let sender: Arc<dyn PluginRequestSender> =
            Arc::clone(&host) as Arc<dyn PluginRequestSender>;
        let mut state = DemoState::default();
        assert!(super::run_scene_demo(&sender, &mut state));
        assert_eq!(state.sent, 4, "1 publish + 3 patches");
        assert_eq!(state.applied, 4);
        assert_eq!(state.rejected, 0);
        assert_eq!(state.head, 4, "the revision cadence advanced per frame");

        // The fake's stored scene matches the plugin's model byte for byte,
        // and the recovery read lines up with the same head.
        assert_eq!(host.scene_head(), Some(SceneRevision(4)));
        assert_eq!(host.scene().as_ref(), state.scene.as_ref());
        assert_eq!(host.scene_summary().by_kind.rects, 4);
        assert_eq!(
            host.snapshot_digest_scene_revision(),
            Some(SceneRevision(4))
        );
        // The wire facts, in arrival order: one snapshot leg, three patch
        // legs (nothing else rode the request line).
        assert_eq!(
            host.served(),
            vec![
                "PanelScenePublish",
                "PanelScenePatch",
                "PanelScenePatch",
                "PanelScenePatch",
            ]
        );
    }

    /// The scene-leg twin of the widget-cycle restoration: a REFUSED
    /// publish restores the taken ack batch (the frame did not apply, so
    /// the acknowledgment is still owed) and the reject report's head
    /// raises the claim frontier above BOTH stored legs (the one-cadence
    /// law), so the next run publishes cleanly past the wedge.
    #[test]
    fn refused_scene_publish_restores_acks_and_realigns_frontier() {
        use ocs_plugin_api::ipc::protocol::PatchRejectCode;
        use ocs_plugin_api::widgets::WidgetEvent;

        let host: Arc<FakeWireHost> = Arc::new(FakeWireHost::new());
        let sender: Arc<dyn PluginRequestSender> =
            Arc::clone(&host) as Arc<dyn PluginRequestSender>;
        let mut state = DemoState::default();
        assert!(super::run_scene_demo(&sender, &mut state));
        assert!(crate::widgets_table::run_widgets_demo(&sender, &mut state));
        assert_eq!(state.head, 6);

        // The live loop's pending state: one merged event batch.
        let surface_id = state.widgets_surface_id().expect("published");
        state.handle_events(
            surface_id,
            vec![WidgetEvent::SelectionChanged {
                surface_id,
                selected_ids: vec![StableId(1)],
            }],
        );

        // THE WEDGE: the frontier rewound below both legs (the
        // lost-response shape).
        state.head = 0;
        assert!(!super::run_scene_demo(&sender, &mut state));
        assert_eq!(
            state.pending_ack.len(),
            1,
            "the refused publish's ack batch is owed again"
        );
        assert_eq!(
            state.head, 6,
            "the report's head raised the frontier above BOTH stored legs"
        );
        assert_eq!(
            state.last_reject,
            Some((PatchRejectCode::NonMonotonicRevision, Some(SceneRevision(4)))),
            "naming the scene leg's own head (the report's recovery fact)"
        );
        assert_eq!(host.scene_head(), Some(SceneRevision(4)), "zero mutation");

        // THE HEAL: the next run publishes from the realigned frontier.
        assert!(super::run_scene_demo(&sender, &mut state));
        assert_eq!(state.head, 10, "4 scene frames from 7");
        assert_eq!(host.scene_head(), Some(SceneRevision(10)));
    }
}
