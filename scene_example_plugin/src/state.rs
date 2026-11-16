//! The plugin-side demo state: the plugin's model of what the host holds,
//! its own revision cadence, and the two incoming streams it merges.
//!
//! The state machine is deliberately host-free (it never needs a `HostApi`
//! or a live host): the send paths live on the flow helpers in
//! [`crate::scene_graph`] / [`crate::widgets_table`] / [`crate::recovery`],
//! and the headless tests drive these methods directly — the way the root
//! example's tests drive its free functions with a `PluginRequestSender`
//! double.

use std::sync::Arc;

use ocs_plugin_api::host::PluginRequestSender;
use ocs_plugin_api::ipc::protocol::PatchRejectCode;
use ocs_plugin_api::scene::{PanelScene, SceneRevision, StableId};
use ocs_plugin_api::widgets::{WidgetEvent, WidgetRange, WidgetSurface};

// The typed-table backing constants live beside the table demo that owns
// them; the demand mapping below references them by path.
use crate::widgets_table;

/// How one outbound frame's exchange ended, as the refusal report names it.
/// This is the DIAGNOSTIC record (the state's tally of what the wire did),
/// not a control path: the flows' behavior is identical without it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FrameOutcome {
    /// `PatchApplied` — the host adopted the frame's revision.
    Applied,
    /// A structured `PatchRejected`: the code plus the report's head (the
    /// recovery fact — `None` when the code left the revision unknown).
    Rejected(PatchRejectCode, Option<u64>),
    /// The frame applied on the wire but the plugin's own model mirror
    /// refused — the model now LAGS the host (the run's report must name
    /// it: the host head is the truth the next frame has to rebuild from).
    AppliedModelLagged(u64),
    /// The request line itself answered an error — no host answer exists
    /// (the wire note carries the transport's text verbatim).
    SendFailed(String),
    /// An answer arrived that NAMES NO CODE: the wire's plain `Error`
    /// text — on the live host this is exactly the V4 nested-serving
    /// guard's refusal ("nested plugin request for tab … cannot be served
    /// inline while the host dispatches to …"). Recorded verbatim so the
    /// next run names the real cause instead of a generic line.
    Unserviceable(String),
    /// A decodable answer that is neither an ack nor a structured reject.
    Unexpected,
}

/// One outbound frame's journal fact (the refusal printout's material):
/// the leg the frame rode, the base/claimed revision it named, and how the
/// exchange ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FrameFact {
    /// "scene publish" / "scene patch" / "widget publish" / "widget patch"
    /// / "snapshot2" — the leg name the printout quotes.
    pub leg: &'static str,
    /// The frame's base revision when the frame names one (a publish or a
    /// read has none).
    pub base: Option<u64>,
    /// The frame's claimed successor revision when it claims one.
    pub next: Option<u64>,
    /// How the exchange ended.
    pub outcome: FrameOutcome,
}

/// One queued answer to a host store demand (the `PanelStoreDemand`
/// notification or its `WidgetEvent::ChildrenRequested` /
/// `TablePageRequested` shorthand). The plugin works through these by
/// publishing the page — the ANSWER never rides the notification channel.
#[derive(Debug, PartialEq, Eq)]
pub enum PendingDelivery {
    /// A lazy tree branch's children page (parent `None` = root rows).
    TreeChildren {
        /// The row the children hang under (`None` = the surface's root
        /// rows).
        parent: Option<StableId>,
        /// The asked window.
        range: WidgetRange,
    },
    /// A typed-table window: publish the backing rows starting at
    /// `first_key` (keyed windows — row keys are the stable identities).
    TablePage {
        /// The registry table the page belongs to.
        table: StableId,
        /// The row key the window STARTS at.
        first_key: StableId,
        /// Rows requested.
        len: usize,
    },
}

/// The plugin's whole demo state. Everything lives plugin-process-local
/// (host-side interaction state — expansion, scroll — is the host's).
#[derive(Default)]
pub struct DemoState {
    /// The thread-safe host request line cached at load (the same v4
    /// runner discipline the root example uses; the demo flows send
    /// through it).
    pub sender: Option<Arc<dyn PluginRequestSender>>,
    /// The plugin's model of the host-held scene leg (the base its next
    /// scene patch names).
    pub scene: Option<PanelScene>,
    /// The plugin's model of the host-held widget surface (the base its
    /// next widget patch names).
    pub widgets: Option<WidgetSurface>,
    /// Highest revision ever claimed for this panel — ONE cadence across
    /// both legs (the documented panel revision space: "one panel, one
    /// revision cadence"), so every claim strictly exceeds every leg's head
    /// without rewinding either.
    pub head: u64,
    /// The widget selection (newest `SelectionChanged` wins; newest-state
    /// discipline from the root example).
    pub selected: Vec<StableId>,
    /// Semantic commands observed (`CommandInvoked`), in arrival order.
    pub commands_invoked: Vec<(String, Vec<StableId>)>,
    /// Item activations observed (`ItemActivated`), in arrival order.
    pub activations: Vec<StableId>,
    /// Events consumed but not yet acknowledged: the merge ACKNOWLEDGMENT
    /// rides the next publish's event leg (the events-vs-patches split —
    /// the ack is on the publish tail, patches carry no events field).
    pub pending_ack: Vec<WidgetEvent>,
    /// Demand answers not yet published.
    pub pending_deliveries: Vec<PendingDelivery>,
    /// Wire tallies: what the plugin sent and how the host answered.
    pub sent: u64,
    pub applied: u64,
    pub rejected: u64,
    /// Publishes that answered a demand (the snapshot-cycle page
    /// deliveries).
    pub pages_delivered: u64,
    /// Structured `StaleBase` rejections seen in the conflict demo.
    pub stale_rejections: u64,
    /// `PanelSceneSnapshot2` re-synchronize successes (digest → re-patch).
    pub recoveries: u64,
    /// The latest structured rejection (the recovery report the tests
    /// read).
    pub last_reject: Option<(PatchRejectCode, Option<SceneRevision>)>,
    /// Diagnostic record of THE LAST outbound frame of the CURRENT
    /// exchange (`begin_flow` clears it; every send site in the flows
    /// re-records): the leg, base/claimed revision and wire outcome the
    /// refusal report quotes. Cleared per flow run so a print names THIS
    /// run's refusal, never a stale one.
    pub(crate) last_frame: Option<FrameFact>,
    /// Frames whose exchange ended at the REQUEST LINE (`SendFailed`).
    pub(crate) send_failures: u64,
    /// Answers that NAMED NO CODE (`Unserviceable` / `Unexpected`).
    pub(crate) unserved_answers: u64,
    /// The model mirror refused after a wire-apply (the model lags).
    pub(crate) mirror_lags: u64,
}

impl std::fmt::Debug for DemoState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The request-line handle stays out of the Debug shape (a trait
        // object has no Debug bound to derive from).
        f.debug_struct("DemoState")
            .field("scene", &self.scene)
            .field("widgets", &self.widgets)
            .field("head", &self.head)
            .field("selected", &self.selected)
            .field("commands_invoked", &self.commands_invoked)
            .field("activations", &self.activations)
            .field("pending_ack", &self.pending_ack)
            .field("pending_deliveries", &self.pending_deliveries)
            .field("sent", &self.sent)
            .field("applied", &self.applied)
            .field("rejected", &self.rejected)
            .field("pages_delivered", &self.pages_delivered)
            .field("stale_rejections", &self.stale_rejections)
            .field("recoveries", &self.recoveries)
            .field("last_reject", &self.last_reject)
            .field("last_frame", &self.last_frame)
            .field("send_failures", &self.send_failures)
            .field("unserved_answers", &self.unserved_answers)
            .field("mirror_lags", &self.mirror_lags)
            .finish()
    }
}

impl DemoState {
    /// Diagnostic hygiene at the START of one `run_command` exchange: the
    /// per-run frame record clears (a refusal print must name THIS run's
    /// exchange); the RUNNING tallies and the historical `last_reject`
    /// stay.
    pub(crate) fn begin_flow(&mut self) {
        self.last_frame = None;
    }

    /// Record ONE frame's exchange outcome (the refusal print's material)
    /// and keep the wire-level counters. Diagnostic-only: no flow branch
    /// reads it; the flows' boolean behavior is untouched by the record.
    pub(crate) fn note_frame(
        &mut self,
        leg: &'static str,
        base: Option<u64>,
        next: Option<u64>,
        outcome: FrameOutcome,
    ) {
        match &outcome {
            FrameOutcome::Applied => {}
            FrameOutcome::AppliedModelLagged(_) => self.mirror_lags += 1,
            FrameOutcome::Rejected(..) => {}
            FrameOutcome::SendFailed(_) => self.send_failures += 1,
            FrameOutcome::Unserviceable(_) | FrameOutcome::Unexpected => {
                self.unserved_answers += 1;
            }
        }
        self.last_frame = Some(FrameFact {
            leg,
            base,
            next,
            outcome,
        });
    }

    /// The structured refusal's shared recording (the flows' ONE mutation
    /// path — `last_reject` keeps the recovery fact the tests read), plus
    /// THE REALIGNMENT: a structured rejection proves zero mutation on the
    /// host, and the `PatchRejectReport` names the host's head — the claim
    /// frontier rises to that truth (and above both stored legs) so the
    /// NEXT frame leaves the wedged lane. Before this, a wedged lane (a
    /// loss whose frontier sat below a store head the plugin never
    /// observed) first re-clashed with the store head — equal-revision,
    /// different-content refusals — before climbing past it; with the
    /// report's head on record the next claim is strictly above every
    /// stored leg head, so NO retry is spent on a collision the plugin
    /// already knew about.
    pub(crate) fn note_reject(
        &mut self,
        leg: &'static str,
        base: Option<u64>,
        next: Option<u64>,
        code: PatchRejectCode,
        current: Option<SceneRevision>,
    ) {
        self.last_reject = Some((code, current));
        self.note_frame(leg, base, next, FrameOutcome::Rejected(code, current.map(|r| r.0)));
        let reported_floor = current.map(|r| r.0).unwrap_or(0);
        let stored_floor = self
            .scene_head()
            .map(|revision| revision.0)
            .unwrap_or(0)
            .max(self.widgets_head().map(|revision| revision.0).unwrap_or(0));
        let floor = reported_floor.max(stored_floor);
        if floor > self.head {
            self.head = floor;
        }
        self.rejected += 1;
    }

    /// The one-line `SCN_DEBUG` dump: the wire tallies, both legs' heads,
    ///the pending queues and the last recorded rejection — everything the
    /// refusal reports quote, in one passive line.
    pub fn debug_report(&self) -> String {
        let head = |summary: Option<SceneRevision>| match summary {
            Some(revision) => revision.0.to_string(),
            None => "—".to_string(),
        };
        let pending_ack_note = if self.pending_ack.is_empty() {
            "0".to_string()
        } else {
            self.pending_ack.len().to_string()
        };
        format!(
            "Scene Example debug: sent {}, applied {}, rejected {}, stale rejected {}, \
             recovered {}, pages delivered {}, model-lag {}, send fails {}, \
             codeless answers {} | head {} | scene head {}, widget head {} | \
             pending ack {}, pending deliver {} | last reject: {:?} | last frame: {:?}",
            self.sent,
            self.applied,
            self.rejected,
            self.stale_rejections,
            self.recoveries,
            self.pages_delivered,
            self.mirror_lags,
            self.send_failures,
            self.unserved_answers,
            self.head,
            head(self.scene_head()),
            head(self.widgets_head()),
            pending_ack_note,
            self.pending_deliveries.len(),
            self.last_reject,
            self.last_frame,
        )
    }

    /// The revision to submit next (strictly above `head` — the successor
    /// monotonicity every leg's apply demands, and above both legs because
    /// the panel shares ONE cadence).
    pub fn next_revision(&mut self) -> SceneRevision {
        self.head += 1;
        SceneRevision(self.head)
    }

    /// The scene leg's head (the base the next scene patch names) — `None`
    /// until the scene is published.
    pub fn scene_head(&self) -> Option<SceneRevision> {
        self.scene.as_ref().map(|scene| scene.revision)
    }

    /// The widget leg's head (the base the next widget patch names).
    pub fn widgets_head(&self) -> Option<SceneRevision> {
        self.widgets.as_ref().map(|surface| surface.revision)
    }

    /// The widget surface id the events stream names. The store freezes it
    /// ONCE to the published surface's FIRST root widget id — the plugin
    /// built the surface, so it can name that id (the registration
    /// convention the mount documents).
    pub fn widgets_surface_id(&self) -> Option<StableId> {
        self.widgets
            .as_ref()
            .and_then(|surface| surface.roots.first().map(|root| root.widget_id()))
    }

    /// Fold one `HostNotification::PanelSceneEvents` batch into the state:
    /// every event is queued for the next publish's ack leg (the merge
    /// acknowledgment), and the semantic ones update the state machine.
    /// Batches addressed to a different surface id are dropped (this demo
    /// publishes exactly one widget surface).
    pub fn handle_events(&mut self, surface_id: StableId, events: Vec<WidgetEvent>) {
        if self
            .widgets_surface_id()
            .is_none_or(|mine| mine != surface_id)
        {
            return;
        }
        for event in events {
            // The ack leg accumulates the whole batch (the merge
            // acknowledgment of what the host observed and this plugin
            // absorbed now).
            self.pending_ack.push(event.clone());
            match event {
                WidgetEvent::SelectionChanged { selected_ids, .. } => self.selected = selected_ids,
                WidgetEvent::CommandInvoked {
                    command_id,
                    target_ids,
                    ..
                } => self.commands_invoked.push((command_id, target_ids)),
                WidgetEvent::ItemActivated { item_id, .. } => self.activations.push(item_id),
                WidgetEvent::ChildrenRequested {
                    parent_id, range, ..
                } => {
                    self.pending_deliveries.push(PendingDelivery::TreeChildren {
                        parent: Some(parent_id),
                        range,
                    });
                }
                WidgetEvent::TablePageRequested {
                    table,
                    first_key,
                    len,
                    ..
                } => self.pending_deliveries.push(PendingDelivery::TablePage {
                    table,
                    first_key,
                    len,
                }),
                // A typed-table cell fact: the READ/notify shape of what
                // this plugin itself publishes as patch data (the
                // events-vs-patches split) — merge-nothing; the state's own
                // registry rows stay authoritative.
                WidgetEvent::TableCellChanged { .. } => {}
                // Expansion is host-local presentation state (the semantic
                // notification rides only on lazy rows, which this demo
                // works through `ChildrenRequested`).
                WidgetEvent::TreeExpanded { .. } => {}
            }
        }
    }

    /// Fold one `HostNotification::PanelStoreDemand` into the pending
    /// deliveries. The demand names a store by ADDRESS: a parent that names
    /// this plugin's registry table is a typed-table window; anything else
    /// addresses the tree (root rows when `None`).
    pub fn on_store_demand(&mut self, parent_id: Option<StableId>, range: WidgetRange) {
        let delivery = match parent_id {
            Some(id)
                if self
                    .widgets
                    .as_ref()
                    .is_some_and(|surface| surface.tables.contains_key(&id)) =>
            {
                PendingDelivery::TablePage {
                    table: id,
                    first_key: widgets_table::backing_row_key(range.start),
                    len: range.len,
                }
            }
            other => PendingDelivery::TreeChildren {
                parent: other,
                range,
            },
        };
        self.pending_deliveries.push(delivery);
    }
}
