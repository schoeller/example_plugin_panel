//! The stale-base conflict demo — the restore contract end to end, on the
//! real wire:
//!
//! 1. the plugin sends a patch built against a STALE base (one revision
//!    behind the head both sides presently agree on — the shape a lost or
//!    replayed frame produces);
//! 2. the host answers the STRUCTURED [`PatchRejectCode::StaleBase`]
//!    rejection (no mutation anywhere; the bounded report names the head —
//!    the recovery fact, never a payload dump);
//! 3. the plugin re-synchronizes through [`PluginRequest::PanelSceneSnapshot2`]:
//!    the answer LEADS with the [`StoreRow::State`] digest (the bounded
//!    summaries; the scene digest carries the authoritative revision) plus
//!    a leading element window;
//! 4. it locates its target IN THE SNAPSHOT (it patches what the host
//!    holds, not what it believes), rebuilds the patch from the digest
//!    revision, and re-patches — the same monotone cadence, one more
//!    revision up.
//!
//! Headless tests drive every step against the wire double.

use std::sync::Arc;

use ocs_plugin_api::host::PluginRequestSender;
use ocs_plugin_api::ipc::protocol::{PatchRejectCode, StoreRow};
use ocs_plugin_api::scene::{
    PanelScenePatch, SceneColor, SceneElement, SceneError, SceneRevision, StableId, ThemeColor,
    Vec2,
};
use ocs_plugin_api::scene_builder::PatchBuilder;

use crate::host_wire::{self, WireOutcome};
use crate::scene_graph::{PANEL_ID, RECOVERED_LABEL};
use crate::state::DemoState;

/// The label content the STALE write tried to install (the demo's marker
/// for "this text never reached the host").
pub const STALE_LABEL: &str = "Node B (stale write)";

/// The label the demo patches (the same element the scene flow replaces —
/// its id lives in the snapshot's leading window).
pub const TARGET_LABEL_ID: u64 = 24;

const TEXT_SIZE: f32 = 14.0;

/// A label element with the demo's id/content/shape (a whole replace
/// payload; a leaf, so the swap is exactly the new run).
fn text_element(id: u64, content: &str) -> SceneElement {
    SceneElement::Text {
        id: StableId(id),
        name: None,
        actions: Vec::new(),
        drag: None,
        content: content.to_string(),
        pos: Vec2([328.0, 58.0]),
        size: TEXT_SIZE,
        color: SceneColor::Theme(ThemeColor::Normal),
    }
}

/// The deliberately stale patch: base one revision behind `head`, one
/// Replace op. (The successor exceeds the DECLARED base — [`PatchBuilder`]
/// enforces that — while the host's check against ITS head refuses the
/// frame first.)
pub fn stale_patch(head: SceneRevision) -> Result<PanelScenePatch, SceneError> {
    let stale_base = head.0.saturating_sub(1);
    PatchBuilder::new(stale_base)
        .replace(text_element(TARGET_LABEL_ID, STALE_LABEL))
        .finish(stale_base + 1)
}

/// Locate the target label in a page of store rows (the snapshot window's
/// [`StoreRow::Scene`] entries carry whole elements).
pub fn located_label<'a>(rows: &'a [StoreRow], id: StableId) -> Option<&'a SceneElement> {
    rows.iter().find_map(|row| match row {
        StoreRow::Scene(element) if element.stable_id() == id => Some(element),
        _ => None,
    })
}

/// The snapshot element with ITS content pointed at `content` (id, shape
/// and accessories stay exactly what the host holds).
fn with_content(element: &SceneElement, content: &str) -> Option<SceneElement> {
    let mut replacement = element.clone();
    match &mut replacement {
        SceneElement::Text { content: run, .. } => *run = content.to_string(),
        _ => return None,
    }
    Some(replacement)
}

/// The whole conflict→recovery loop. Returns whether the re-patch applied.
pub fn run_stale_conflict(sender: &Arc<dyn PluginRequestSender>, state: &mut DemoState) -> bool {
    // (1) The stale send: built against a base we KNOW the host has moved
    // past in this demo (the honest stand-in for a lost/replayed frame).
    let Some(head) = state.scene_head() else {
        return false;
    };
    let Ok(stale) = stale_patch(head) else {
        return false;
    };
    state.sent += 1;
    let reported_head: Option<SceneRevision>;
    match host_wire::send_scene_patch(sender, PANEL_ID, stale) {
        WireOutcome::Rejected {
            code: PatchRejectCode::StaleBase,
            report,
        } => {
            state.rejected += 1;
            state.stale_rejections += 1;
            state.last_reject = Some((PatchRejectCode::StaleBase, report.current_revision));
            reported_head = report.current_revision;
        }
        // Anything else breaks the demo's premise (an applied stale patch
        // would be a wire defect worth noticing loudly).
        _ => return false,
    }

    // (2) The recovery read: the bounded snapshot (digest + leading
    // window). The digest's revision is the authoritative base — the same
    // head the rejection report named (both are the host's head; a
    // disagreement never blocks the recovery — the digest is the base
    // either way).
    let Some((digest, window)) = host_wire::send_scene_snapshot2(sender, PANEL_ID) else {
        return false;
    };
    state.sent += 1;
    let Some(scene_digest) = digest.scene else {
        return false;
    };
    let _same_head = reported_head == Some(scene_digest.revision);

    // (3) Patch what the SNAPSHOT holds: locate the target label's live
    // element in the leading window, then write the recovered content.
    let Some(element) = located_label(&window.rows, StableId(TARGET_LABEL_ID)) else {
        return false;
    };
    let Some(replacement) = with_content(element, RECOVERED_LABEL) else {
        return false;
    };
    let successor = state.next_revision();
    let Ok(rebuilt) = PatchBuilder::new(scene_digest.revision.0)
        .replace(replacement)
        .finish(successor.0)
    else {
        return false;
    };
    state.sent += 1;
    match host_wire::send_scene_patch(sender, PANEL_ID, rebuilt.clone()) {
        WireOutcome::Applied { revision, .. } => {
            state.applied += 1;
            state.recoveries += 1;
            let Some(scene) = state.scene.as_mut() else {
                return false;
            };
            if scene.apply_patch(rebuilt).is_err() {
                return false;
            }
            debug_assert_eq!(scene.revision, revision, "model and host agree");
            true
        }
        WireOutcome::Rejected { code, report } => {
            state.rejected += 1;
            state.last_reject = Some((code, report.current_revision));
            false
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use ocs_plugin_api::scene::SceneOp;

    use crate::fake_host::FakeWireHost;
    use crate::scene_graph::demo_graph_scene;

    use super::*;

    /// The unit shape: the stale patch names a stale base and the stale
    /// content run, as ONE Replace op (exactly the patch a second send of
    /// an already-applied frame produces).
    #[test]
    fn stale_patch_shape() {
        let patch = stale_patch(SceneRevision(4)).unwrap();
        assert_eq!(patch.base_revision, SceneRevision(3));
        assert_eq!(patch.revision, SceneRevision(4));
        assert_eq!(patch.ops.len(), 1);
        let Some(SceneOp::Replace { element }) = patch.ops.first() else {
            panic!("the stale write is one Replace");
        };
        assert_eq!(element.stable_id().0, TARGET_LABEL_ID);
        match element {
            SceneElement::Text { content, .. } => assert_eq!(content, STALE_LABEL),
            other => panic!("the stale write is the label Text: {other:?}"),
        }
    }

    /// The locate step reads elements back out of snapshot rows and the
    /// recovered payload keeps id/shape while pointing at the new content.
    #[test]
    fn locate_and_rebuild_from_window() {
        let scene = demo_graph_scene(1).unwrap();
        // A leading-window stand-in: the label element is what a snapshot2
        // window carries as a StoreRow::Scene row; the recovery driver
        // locates the demo label by its stable id.
        // The label sits inside node B's group — the same nested shape
        // the snapshot window's flatten carries.
        let label = scene
            .elements
            .iter()
            .find_map(|element| match element {
                SceneElement::Group { children, .. } => children
                    .iter()
                    .find(|child| child.stable_id() == StableId(TARGET_LABEL_ID))
                    .cloned(),
                other if other.stable_id() == StableId(TARGET_LABEL_ID) => Some(other.clone()),
                _ => None,
            })
            .expect("the demo label lives under node B's group");
        let rows = vec![
            StoreRow::Scene(label),
            StoreRow::Scene(text_element(99, "other")),
        ];
        assert!(located_label(&rows, StableId(TARGET_LABEL_ID)).is_some());
        assert!(located_label(&rows, StableId(99)).is_some());
        assert!(located_label(&rows, StableId(244)).is_none());
        let owned = with_content(
            located_label(&rows, StableId(TARGET_LABEL_ID)).unwrap(),
            RECOVERED_LABEL,
        )
        .expect("the located element is a Text");
        let SceneElement::Text { content, id, .. } = owned else {
            panic!("the located label stays a Text");
        };
        assert_eq!(id, StableId(TARGET_LABEL_ID));
        assert_eq!(content, RECOVERED_LABEL);
    }

    /// The whole loop headless, against the wire double: the stale frame
    /// moves NOTHING (the stored content survives the rejection); the
    /// re-patch moves exactly one revision and the content the snapshot
    /// located; the digest read-back then reports the recovered state.
    #[test]
    fn stale_conflict_recovery_end_to_end() {
        let host = Arc::new(FakeWireHost::new());
        let sender: Arc<dyn PluginRequestSender> =
            Arc::clone(&host) as Arc<dyn PluginRequestSender>;
        let mut state = DemoState::default();

        // Seed: publish the graph scene + one real patch (so a stale base
        // is meaningful).
        assert!(crate::scene_graph::run_scene_demo(&sender, &mut state));
        assert_eq!(
            host.label_content(TARGET_LABEL_ID).as_deref(),
            Some("Node B (second pass)")
        );
        assert_eq!(host.scene_head(), Some(SceneRevision(4)));

        // The loop: stale send -> StaleBase -> snapshot2 -> re-patch.
        assert!(run_stale_conflict(&sender, &mut state));

        // Wire tallies: one rejection, one recovery; the sent frames were
        // stale-patch + snapshot2 + re-patch.
        assert_eq!(state.sent, 4 + 3);
        assert_eq!(state.applied, 4 + 1);
        assert_eq!(state.rejected, 1);
        assert_eq!(state.stale_rejections, 1);
        assert_eq!(state.recoveries, 1);
        assert_eq!(
            state.last_reject,
            Some((PatchRejectCode::StaleBase, Some(SceneRevision(4)))),
            "the bounded report named the host head as the recovery fact"
        );

        // The host: exactly ONE revision past where the stale send found it
        // (the stale frame moved nothing; the re-patch moved exactly one) —
        // and the recovered content is what the host now holds.
        assert_eq!(host.scene_head(), Some(SceneRevision(5)));
        assert_eq!(
            host.label_content(TARGET_LABEL_ID).as_deref(),
            Some(RECOVERED_LABEL)
        );
        // The plugin's model mirrors the host.
        let scene = state.scene.as_ref().expect("model kept");
        assert_eq!(scene.revision, SceneRevision(5));

        // The same bounded recovery read a LATER client would take: the
        // digest row leads with the post-recovery revision, and the label
        // inside the window already carries the recovered content.
        let (digest, window) = host_wire::send_scene_snapshot2(&sender, PANEL_ID).expect("a page");
        assert_eq!(digest.scene.expect("scene held").revision, SceneRevision(5));
        let recovered = located_label(&window.rows, StableId(TARGET_LABEL_ID)).expect("in window");
        let SceneElement::Text { content, .. } = recovered else {
            panic!("the window carries the label");
        };
        assert_eq!(content, RECOVERED_LABEL);
    }
}
