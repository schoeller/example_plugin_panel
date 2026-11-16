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
            .finish()
    }
}

impl DemoState {
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
