//! The drag demo: a scene element carrying a
//! [`DragMeta`](ocs_plugin_api::scene::DragMeta) — the host runs the
//! Begin / host-local preview / Settle lifecycle (SCN-021's rail; the
//! previews never reach the wire) and hands the plugin ONE semantic
//! completion per gesture.
//!
//! # The landed contract this demo mirrors (the real host's law)
//!
//! - **Settle** → ONE `WidgetEvent::CommandInvoked` bound to the dragged
//!   element's address, `"drag_settle"` being the mount's v1 command
//!   spelling. The event carries surface id + command + target address —
//!   NOTHING else (by wire pin). `DragMeta.opaque` is NEVER echoed by the
//!   host: the plugin resolves the meta VERBATIM out of its own model,
//!   where it published it (the documented INT-5 law — the meta is the
//!   plugin's own key and never needed the host to remember it).
//! - **Cancel** → NO event at all (a lossless no-op for the plugin): the
//!   demo's counters note settles only, and a cancel observably moves
//!   nothing (no event → no queued move → no patch → the scene stands).
//! - **The restore contract**: the plugin's move is AUTHORITATIVE — one
//!   `Replace` patch from the model's own base revision. A rejected
//!   completion is healed by the ordinary publish/patch discipline
//!   (`note_reject`'s claim rollback + frontier realignment; the host
//!   restores a rejected interactive completion itself via patch — the
//!   plugin-side twin is: a refused settle patch leaves the node exactly
//!   where it was and the move stays queued for the next run).
//!
//! Id space (scene namespace, past the graph demo's 0..80 and the
//! recovery's 24): the draggable card is a Rect at id **600**.

use std::sync::Arc;

use ocs_plugin_api::host::PluginRequestSender;
use ocs_plugin_api::scene::{
    DragMeta, SceneColor, SceneElement, StableId, Stroke, ThemeColor, Vec2,
};
use ocs_plugin_api::scene_builder::PatchBuilder;

use crate::host_wire;
use crate::scene_graph::{self, PANEL_ID};
use crate::state::{DemoState, FrameOutcome};

/// The draggable card's stable id (the settle event's target address).
pub const DRAG_ELEMENT_ID: u64 = 600;
/// The card's display name.
pub const DRAG_ELEMENT_LABEL: &str = "Drag card";
/// The plugin-opaque drag marker published WITH the element (the host
/// returns the meta verbatim in its settle FACT; the event itself never
/// carries it — the plugin resolves it from here).
pub const DRAG_META_OPAQUE: &str = "scn-drag:node-1";
/// The settle command id the mount binds (its v1 spelling).
pub const DRAG_SETTLE_COMMAND: &str = "drag_settle";
/// The authoritative move the demo applies per settle (scene units — the
/// demo's own drop semantics: the card lands one offset up-right).
pub const SETTLE_MOVE_OFFSET: [f64; 2] = [40.0, -40.0];

const CARD_SIZE: [f64; 2] = [120.0, 48.0];
const HOME: [f64; 2] = [40.0, 300.0];

/// The themed 1-unit outline the card strokes with.
fn normal_stroke() -> Stroke {
    Stroke {
        color: SceneColor::Theme(ThemeColor::Normal),
        width: 1.0,
    }
}

/// The draggable card: an accent-filled Rect whose `drag` accessory
/// carries the plugin-opaque meta.
pub fn draggable_element() -> SceneElement {
    SceneElement::Rect {
        id: StableId(DRAG_ELEMENT_ID),
        name: Some(DRAG_ELEMENT_LABEL.to_string()),
        actions: vec!["drag".to_string()],
        drag: Some(DragMeta {
            opaque: Some(DRAG_META_OPAQUE.to_string()),
        }),
        pos: Vec2(HOME),
        size: Vec2(CARD_SIZE),
        fill: Some(SceneColor::Theme(ThemeColor::Accent)),
        stroke: Some(normal_stroke()),
    }
}

/// The element's `drag` meta, if it carries one.
fn drag_meta_of(element: &SceneElement) -> Option<&DragMeta> {
    match element {
        SceneElement::Group { drag, .. }
        | SceneElement::Rect { drag, .. }
        | SceneElement::Path { drag, .. }
        | SceneElement::CubicPath { drag, .. }
        | SceneElement::Text { drag, .. }
        | SceneElement::Asset { drag, .. }
        | SceneElement::Port { drag, .. }
        | SceneElement::Marker { drag, .. }
        | SceneElement::Edge { drag, .. } => drag.as_ref(),
    }
}

/// Depth-first lookup of one element by stable id in the model.
fn find_element<'a>(elements: &'a [SceneElement], id: StableId) -> Option<&'a SceneElement> {
    for element in elements {
        if element.stable_id() == id {
            return Some(element);
        }
        if let SceneElement::Group { children, .. } = element {
            if let Some(found) = find_element(children, id) {
                return Some(found);
            }
        }
    }
    None
}

/// `true` while the model's scene holds the element id.
pub(crate) fn holds_element(state: &DemoState, id: StableId) -> bool {
    state
        .scene
        .as_ref()
        .is_some_and(|scene| find_element(&scene.elements, id).is_some())
}

/// The settle fact (ONE per gesture, the host's exactly-once guarantee):
/// resolve the address against the plugin's own model — the verbatim
/// `DragMeta` source — and queue the authoritative move. An address
/// without drag meta is a defensive refuse (never queued).
pub(crate) fn note_settle(state: &mut DemoState, address: StableId) {
    let meta = state
        .scene
        .as_ref()
        .and_then(|scene| find_element(&scene.elements, address))
        .and_then(drag_meta_of)
        .cloned();
    match meta {
        Some(meta) => {
            state.drag_settles += 1;
            state.pending_moves.push(address);
            state.journal_note(format!(
                "drag settle: address {} resolved, meta opaque {:?} (verbatim from the \
                 plugin's own model) — one authoritative move queued",
                address.0,
                meta.opaque.as_deref().unwrap_or("—")
            ));
        }
        None => {
            state.drag_unresolved += 1;
            state.journal_note(format!(
                "drag settle: address {} carries no drag meta in the plugin's model — \
                 not queued",
                address.0
            ));
        }
    }
}

/// The moved card (the demo's drop semantics: HOME + one offset per
/// settle — a pure function of the model's current position, so every
/// settle lands exactly one offset from wherever the card is).
fn moved_element(state: &DemoState, address: StableId) -> Option<SceneElement> {
    let element = find_element(&state.scene.as_ref()?.elements, address)?.clone();
    let SceneElement::Rect {
        pos: Vec2(current), ..
    } = &element
    else {
        return None;
    };
    let moved = SceneElement::Rect {
        id: StableId(DRAG_ELEMENT_ID),
        name: Some(DRAG_ELEMENT_LABEL.to_string()),
        actions: vec!["drag".to_string()],
        drag: Some(DragMeta {
            opaque: Some(DRAG_META_OPAQUE.to_string()),
        }),
        pos: Vec2([
            current[0] + SETTLE_MOVE_OFFSET[0],
            current[1] + SETTLE_MOVE_OFFSET[1],
        ]),
        size: Vec2(CARD_SIZE),
        fill: Some(SceneColor::Theme(ThemeColor::Accent)),
        stroke: Some(normal_stroke()),
    };
    Some(moved)
}

/// One flush's outcome: the move APPLIED (mirrored), REFUSED by the wire
/// (restored — the next run retries), or DROPPED locally (the model no
/// longer holds the address — never retried).
enum MoveResult {
    Applied,
    Refused,
    Dropped,
}

/// Flush ONE queued settle move: exactly ONE `Replace` patch per settle,
/// built against the model's own base revision and mirrored through
/// `apply_patch` on apply. A refusal reports through the ordinary frame
/// journal — the restore contract's plugin-side twin.
fn flush_one_move(
    sender: &Arc<dyn PluginRequestSender>,
    state: &mut DemoState,
    address: StableId,
) -> MoveResult {
    let Some(base) = state.scene_head() else {
        return MoveResult::Refused;
    };
    let Some(moved) = moved_element(state, address) else {
        state.journal_note(format!(
            "drag move: address {} is not in the model any more — the move is dropped",
            address.0
        ));
        return MoveResult::Dropped;
    };
    let successor = state.next_revision();
    let Ok(patch) = PatchBuilder::new(base.0).replace(moved).finish(successor.0) else {
        // The claim made no frame (the builder refused before the wire):
        // roll the claim back so the ledger keeps matching real frames.
        state.head -= 1;
        return MoveResult::Refused;
    };
    let base_revision = patch.base_revision.0;
    let claimed = patch.revision.0;
    state.sent += 1;
    match host_wire::send_scene_patch(sender, PANEL_ID, patch.clone()) {
        host_wire::WireOutcome::Applied { revision, .. } => {
            state.applied += 1;
            let Some(scene) = state.scene.as_mut() else {
                return MoveResult::Refused;
            };
            if scene.apply_patch(patch).is_err() {
                state.note_frame(
                    "scene patch",
                    Some(base_revision),
                    Some(claimed),
                    FrameOutcome::AppliedModelLagged(revision.0),
                );
                return MoveResult::Refused;
            }
            debug_assert_eq!(scene.revision, revision, "model and host agree");
            state.drag_moves_applied += 1;
            state.note_frame(
                "scene patch",
                Some(base_revision),
                Some(claimed),
                FrameOutcome::Applied,
            );
            state.journal_note(format!(
                "drag move: address {} moved authoritatively (one Replace patch, \
                 revision {})",
                address.0, claimed
            ));
            state.note_status(format!(
                "settle applied — node {} moved (one patch at revision {})",
                address.0, claimed
            ));
            MoveResult::Applied
        }
        host_wire::WireOutcome::Rejected { code, report } => {
            state.note_reject(
                "scene patch",
                Some(base_revision),
                Some(claimed),
                code,
                report.current_revision,
            );
            MoveResult::Refused
        }
        host_wire::WireOutcome::Unserviceable(message) => {
            state.note_frame("scene patch", Some(base_revision), Some(claimed), FrameOutcome::Unserviceable(message));
            MoveResult::Refused
        }
        host_wire::WireOutcome::Failed(error) => {
            state.note_frame("scene patch", Some(base_revision), Some(claimed), FrameOutcome::SendFailed(error));
            MoveResult::Refused
        }
        host_wire::WireOutcome::Unexpected => {
            state.note_frame("scene patch", Some(base_revision), Some(claimed), FrameOutcome::Unexpected);
            MoveResult::Refused
        }
    }
}

/// Flush every queued settle move (one patch each, in arrival order). On
/// the first refusal the failed move returns to the FRONT of the queue
/// (the rest stay queued behind it) and the flush reports failure — the
/// refused frame leaves everything exactly as it was, and the next run
/// retries the same move from the realigned frontier.
pub(crate) fn flush_moves(
    sender: &Arc<dyn PluginRequestSender>,
    state: &mut DemoState,
) -> (usize, bool) {
    let mut moves = std::mem::take(&mut state.pending_moves);
    let mut applied = 0usize;
    while !moves.is_empty() {
        let address = moves.remove(0);
        match flush_one_move(sender, state, address) {
            MoveResult::Applied => applied += 1,
            MoveResult::Dropped => {}
            MoveResult::Refused => {
                // Restore: the failed move back at the FRONT, the
                // unprocessed rest behind it (arrival order preserved —
                // the next run retries the same move from the realigned
                // frontier).
                moves.insert(0, address);
                state.pending_moves = moves;
                return (applied, false);
            }
        }
    }
    (applied, true)
}

/// The drag feature's flow: ensure the scene leg exists, arm the
/// draggable card (one `Add` patch — skipped when the model already holds
/// it, so the command is idempotent), then flush the queued settle moves.
pub fn run_drag_demo(
    sender: &Arc<dyn PluginRequestSender>,
    state: &mut DemoState,
) -> bool {
    // Ensure-base: a fresh fixture publish REPLACES the model, so it only
    // runs when the model holds no scene.
    if state.scene.is_none() && !scene_graph::publish_scene_base(sender, state) {
        return false;
    }
    // Arm: the draggable card, one Add patch (idempotent).
    if !holds_element(state, StableId(DRAG_ELEMENT_ID)) {
        let Some(base) = state.scene_head() else {
            return false;
        };
        let successor = state.next_revision();
        let Ok(patch) =
            PatchBuilder::new(base.0).add(None, draggable_element()).finish(successor.0)
        else {
            state.head -= 1;
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
                state.journal_note(format!(
                    "drag: the draggable card armed at id {} (meta opaque {:?}) — \
                     drag it in the host, then run the command again to move it",
                    DRAG_ELEMENT_ID, DRAG_META_OPAQUE
                ));
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
            host_wire::WireOutcome::Unexpected => {
                state.note_frame("scene patch", Some(base_revision), Some(claimed), FrameOutcome::Unexpected);
                return false;
            }
        }
    }
    // The loop's second half: the queued settles move authoritatively.
    let (moved, clean) = flush_moves(sender, state);
    let _ = moved;
    clean
}

#[cfg(test)]
mod tests {
    use ocs_plugin_api::host::{PluginRequestError, PluginRequestSender};
    use ocs_plugin_api::ipc::protocol::{PatchRejectCode, PluginRequest, PluginResponse};
    use ocs_plugin_api::scene::SceneRevision;
    use ocs_plugin_api::widgets::WidgetEvent;

    use crate::fake_host::FakeWireHost;
    use crate::state::DemoState;

    use super::*;

    /// The scripted coded-reject double: every mutating frame is refused
    /// `StaleBase` naming `head` (the wire's structured refusal shape the
    /// rollback laws are proven against).
    struct CodedReject {
        head: u64,
    }

    impl PluginRequestSender for CodedReject {
        fn request(
            &self,
            _req: PluginRequest,
        ) -> Result<PluginResponse, PluginRequestError> {
            Ok(PluginResponse::PatchRejected {
                code: PatchRejectCode::StaleBase,
                report: ocs_plugin_api::ipc::protocol::PatchRejectReport {
                    current_revision: Some(SceneRevision(self.head)),
                    failed_op: None,
                },
            })
        }
    }

    /// The fixture is byte-deterministic (the meta rides inside it).
    #[test]
    fn drag_fixture_is_byte_deterministic() {
        let a = bincode::serialize(&draggable_element()).unwrap();
        let b = bincode::serialize(&draggable_element()).unwrap();
        assert_eq!(a, b);
        assert!(
            matches!(
                draggable_element(),
                SceneElement::Rect {
                    drag: Some(DragMeta { opaque: Some(_) }),
                    ..
                }
            ),
            "the card carries its opaque meta"
        );
    }

    /// THE settle loop, headless: arm → settle event → exactly ONE patch
    /// moves the card authoritatively → counters note the settle and the
    /// move 1:1 → a cancel (no event) moves nothing.
    #[test]
    fn drag_settle_moves_node_via_exactly_one_patch() {
        let host = Arc::new(FakeWireHost::new());
        let sender: Arc<dyn PluginRequestSender> =
            Arc::clone(&host) as Arc<dyn PluginRequestSender>;
        let mut state = DemoState::default();

        // Arm (publishes the scene, then patches the card in) — plus the
        // widget leg, exactly what `on_load` publishes: the host streams
        // the settle's event batch only through a LIVE widget mount.
        assert!(run_drag_demo(&sender, &mut state));
        assert!(crate::widgets_table::publish_widget_base(&sender, &mut state));
        let _armed_head = host.scene_head().expect("scene published").0;
        let card_rect = |elements: &[SceneElement]| {
            elements
                .iter()
                .find(|element| element.stable_id() == StableId(DRAG_ELEMENT_ID))
                .cloned()
                .expect("the card is live")
        };
        let before = card_rect(&host.scene().expect("scene").elements);
        let served_after_arm = host.served().len();

        // The host's settle hand-off: ONE CommandInvoked at the card.
        let surface_id = StableId(crate::widgets_table::ROOT_GROUP_ID);
        state.handle_events(
            surface_id,
            vec![WidgetEvent::CommandInvoked {
                surface_id,
                command_id: DRAG_SETTLE_COMMAND.to_string(),
                target_ids: vec![StableId(DRAG_ELEMENT_ID)],
            }],
        );
        assert_eq!(state.drag_settles, 1, "the settle resolved against the model");
        assert_eq!(state.pending_moves, vec![StableId(DRAG_ELEMENT_ID)]);

        // The flush: exactly ONE patch moves the card.
        assert!(run_drag_demo(&sender, &mut state));
        assert_eq!(state.drag_moves_applied, 1, "one settle, one move");
        assert_eq!(
            host.served().len() - served_after_arm,
            1,
            "exactly one frame (the move patch) rode the wire"
        );
        assert_eq!(host.served().last().copied(), Some("PanelScenePatch"));
        let after = card_rect(&host.scene().expect("scene").elements);
        assert_ne!(before, after, "the card moved");
        // The scene leg's head sits at the panel's ONE cadence head (the
        // move claimed the next revision of the SHARED space — the widget
        // leg's publish sits between, so the scene leg jumped past it).
        assert_eq!(host.scene_head(), Some(SceneRevision(state.head)));
        // The journal prints the loop legibly.
        assert!(
            state
                .journal
                .iter()
                .any(|line| line.contains("drag settle: address 600 resolved")),
            "the settle is legible: {:?}",
            state.journal
        );
        assert!(
            state
                .journal
                .iter()
                .any(|line| line.contains("moved authoritatively")),
            "the move is legible: {:?}",
            state.journal
        );
        assert!(state.pending_moves.is_empty(), "the queue drained");

        // A CANCEL: no event exists, so nothing is queued and the next
        // run moves nothing (the lossless no-op, mirrored).
        let head_before_cancel = host.scene_head();
        assert!(run_drag_demo(&sender, &mut state));
        assert_eq!(host.scene_head(), head_before_cancel, "cancel moves nothing");
        assert_eq!(state.drag_settles, 1, "cancel emits no event to count");
        assert_eq!(
            state.drag_settles, state.drag_moves_applied,
            "settles and moves stay 1:1"
        );
    }

    /// A settle naming an address without drag meta is refused locally
    /// (defensive — never queued), and a REFUSED move patch leaves
    /// everything exactly as it was: the card stays put, the move stays
    /// queued, and the reject's report head realigns the claim frontier
    /// so the next run's retry applies cleanly (the restore contract).
    #[test]
    fn refused_move_restores_and_retries() {
        let host = Arc::new(FakeWireHost::new());
        let sender: Arc<dyn PluginRequestSender> =
            Arc::clone(&host) as Arc<dyn PluginRequestSender>;
        let mut state = DemoState::default();
        assert!(run_drag_demo(&sender, &mut state));
        // The widget leg too: the settle batch rides a LIVE widget mount.
        assert!(crate::widgets_table::publish_widget_base(&sender, &mut state));

        // A foreign address: no meta in the model → refused, not queued.
        state.handle_events(
            StableId(crate::widgets_table::ROOT_GROUP_ID),
            vec![WidgetEvent::CommandInvoked {
                surface_id: StableId(crate::widgets_table::ROOT_GROUP_ID),
                command_id: DRAG_SETTLE_COMMAND.to_string(),
                target_ids: vec![StableId(24)],
            }],
        );
        assert_eq!(state.drag_settles, 0);
        assert_eq!(state.drag_unresolved, 1);
        assert!(state.pending_moves.is_empty());

        // A real settle, then the wire turns hostile (the coded reject).
        state.handle_events(
            StableId(crate::widgets_table::ROOT_GROUP_ID),
            vec![WidgetEvent::CommandInvoked {
                surface_id: StableId(crate::widgets_table::ROOT_GROUP_ID),
                command_id: DRAG_SETTLE_COMMAND.to_string(),
                target_ids: vec![StableId(DRAG_ELEMENT_ID)],
            }],
        );
        let stored = host.scene().expect("scene");
        let rejecter: Arc<dyn PluginRequestSender> = Arc::new(CodedReject {
            head: stored.revision.0,
        });
        let (moved, clean) = flush_moves(&rejecter, &mut state);
        assert_eq!((moved, clean), (0, false));
        // (a) the card never moved on the host;
        assert_eq!(host.scene().expect("scene"), stored, "zero mutation");
        // (b) the move is STILL owed (the flush restored it);
        assert_eq!(state.pending_moves, vec![StableId(DRAG_ELEMENT_ID)]);
        // (c) the structured reject is recorded with its recovery fact;
        assert_eq!(
            state.last_reject,
            Some((PatchRejectCode::StaleBase, Some(stored.revision)))
        );
        // (d) THE HEAL: the retry from the realigned frontier applies.
        let (moved, clean) = flush_moves(&sender, &mut state);
        assert_eq!((moved, clean), (1, true));
        assert_ne!(host.scene().expect("scene"), stored, "the retry moved the card");
        assert_eq!(state.drag_moves_applied, 1);
        assert!(state.pending_moves.is_empty());
    }
}
