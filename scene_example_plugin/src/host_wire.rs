//! The plugin's client-side send paths over the thread-safe host request
//! line (`plugin_request_sender()` → `PluginRequest` → `PluginResponse`),
//! with typed decoding of the API v24 scene/widget answers.
//!
//! This is exactly the root example's discipline (`send_summary` sends
//! `PluginRequest::UpdatePanel` through `sender.request(...)`) extended to
//! the v24 publish-or-patch tails: the vendor-independent send site is the
//! `Arc<dyn PluginRequestSender>` handle, and the answers decode into the
//! small result vocabulary below. Tests drive these helpers against a
//! `PluginRequestSender` double — no live host involved.

use std::sync::Arc;

use ocs_plugin_api::host::PluginRequestSender;
use ocs_plugin_api::ipc::protocol::{
    PatchRejectCode, PatchRejectReport, PluginRequest, PluginResponse, SceneOrWidgetPatch,
    SceneOrWidgetSurface, StoreRow, StoreStateDigest,
};
use ocs_plugin_api::scene::{PanelScene, PanelScenePatch, SceneRevision};
use ocs_plugin_api::widgets::{WidgetEvent, WidgetSurface, WidgetSurfacePatch};

// The read-side helpers the headless tests exercise directly (their kinds
// and range types ride the same gate).
#[cfg(test)]
use ocs_plugin_api::ipc::protocol::PanelStoreKind;
#[cfg(test)]
use ocs_plugin_api::widgets::WidgetRange;

/// Decoded answer of one v24 mutating frame (`PanelScenePublish` /
/// `PanelScenePatch`): the typed ack, the structured rejection, or a wire
/// failure. The `Unknown` passthrough of a future major decodes as
/// [`WireOutcome::Unexpected`] — an honest never-pretend-success shape.
#[derive(Debug, PartialEq)]
pub enum WireOutcome {
    /// `PatchApplied(AppliedPatchReport)` — the successor revision the host
    /// adopted plus the bounded invalidation plan.
    Applied {
        /// The report's revision (equals the post-patch head).
        revision: SceneRevision,
        /// Top ids of the subtrees the ops changed (bounded; the recovery
        /// facts the demo names in its docs).
        touched: Vec<ocs_plugin_api::scene::StableId>,
    },
    /// `PatchRejected{code, report}` — bounded, no mutation anywhere.
    Rejected {
        code: PatchRejectCode,
        report: PatchRejectReport,
    },
    /// The send itself failed (no request line, or an error result).
    Failed,
    /// A decodable answer that is not a v24 patch ack (`Ok`, labels, …):
    /// never treated as success.
    Unexpected,
}

/// One bounded store window (`StorePage`): the rows plus the host's held
/// count (`total_hint`; a table's self-declared total when its def names
/// one).
#[derive(Debug, PartialEq)]
pub struct StoreWindow {
    pub rows: Vec<StoreRow>,
    pub total_hint: u64,
}

/// Decode one answered v24 mutating frame (labels and a future major's
/// additions decode as `Unexpected` — never pretend-success).
fn decode_mutating(response: PluginResponse) -> WireOutcome {
    match response {
        PluginResponse::PatchApplied(report) => WireOutcome::Applied {
            revision: report.revision,
            touched: report.touched_subtree_ids,
        },
        PluginResponse::PatchRejected { code, report } => WireOutcome::Rejected { code, report },
        _ => WireOutcome::Unexpected,
    }
}

/// Send the snapshot leg of the publish-or-patch contract (a `PanelScene`):
/// the FIRST publish carries the full bounded scene; a later full publish
/// is the snapshot-cycle shape the store-page answers use.
pub fn send_scene_publish(
    sender: &Arc<dyn PluginRequestSender>,
    panel_id: &str,
    scene: PanelScene,
    events: Vec<WidgetEvent>,
) -> WireOutcome {
    sender
        .request(PluginRequest::PanelScenePublish {
            panel_id: panel_id.to_owned(),
            surface: SceneOrWidgetSurface::Scene(scene),
            events,
        })
        .map_or(WireOutcome::Failed, decode_mutating)
}

/// Send the snapshot leg for a `WidgetSurface` (its publish also goes LIVE
/// on the host's mount).
pub fn send_widget_publish(
    sender: &Arc<dyn PluginRequestSender>,
    panel_id: &str,
    surface: WidgetSurface,
    events: Vec<WidgetEvent>,
) -> WireOutcome {
    sender
        .request(PluginRequest::PanelScenePublish {
            panel_id: panel_id.to_owned(),
            surface: SceneOrWidgetSurface::Widgets(surface),
            events,
        })
        .map_or(WireOutcome::Failed, decode_mutating)
}

/// Send the incremental scene leg (`PanelScenePatch` / `SceneOrWidgetPatch::Scene`).
pub fn send_scene_patch(
    sender: &Arc<dyn PluginRequestSender>,
    panel_id: &str,
    patch: PanelScenePatch,
) -> WireOutcome {
    sender
        .request(PluginRequest::PanelScenePatch {
            panel_id: panel_id.to_owned(),
            patch: SceneOrWidgetPatch::Scene(patch),
        })
        .map_or(WireOutcome::Failed, decode_mutating)
}

/// Send the incremental widget leg (`PanelScenePatch` / `SceneOrWidgetPatch::Widgets`).
pub fn send_widget_patch(
    sender: &Arc<dyn PluginRequestSender>,
    panel_id: &str,
    patch: WidgetSurfacePatch,
) -> WireOutcome {
    sender
        .request(PluginRequest::PanelScenePatch {
            panel_id: panel_id.to_owned(),
            patch: SceneOrWidgetPatch::Widgets(patch),
        })
        .map_or(WireOutcome::Failed, decode_mutating)
}

/// One windowed store read (`PanelStoreRead` → `StorePage`). The live demo
/// derives its answers from events/demands; the headless tests exercise
/// the read path directly against the wire double.
#[cfg(test)]
pub fn send_store_read(
    sender: &Arc<dyn PluginRequestSender>,
    panel_id: &str,
    kind: PanelStoreKind,
    parent_id: Option<ocs_plugin_api::scene::StableId>,
    range: WidgetRange,
) -> Option<StoreWindow> {
    let response = sender.request(PluginRequest::PanelStoreRead {
        panel_id: panel_id.to_owned(),
        kind,
        parent_id,
        range,
    });
    match response {
        Ok(PluginResponse::StorePage { rows, total_hint }) => {
            Some(StoreWindow { rows, total_hint })
        }
        _ => None,
    }
}

/// The bounded recovery read (`PanelSceneSnapshot2` → `StorePage`): the
/// answer LEADS with a `StoreRow::State` digest row followed by the leading
/// scene window — never a full dump.
pub fn send_scene_snapshot2(
    sender: &Arc<dyn PluginRequestSender>,
    panel_id: &str,
) -> Option<(StoreStateDigest, StoreWindow)> {
    let response = sender.request(PluginRequest::PanelSceneSnapshot2 {
        panel_id: panel_id.to_owned(),
    });
    match response {
        Ok(PluginResponse::StorePage {
            rows: mut page_rows,
            total_hint,
        }) => {
            let digest = match page_rows.first() {
                Some(StoreRow::State(digest)) => digest.clone(),
                _ => return None,
            };
            page_rows.remove(0);
            Some((
                digest,
                StoreWindow {
                    rows: page_rows,
                    total_hint,
                },
            ))
        }
        _ => None,
    }
}
