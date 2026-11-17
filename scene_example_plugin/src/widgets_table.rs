//! The typed-table widget demo: a [`WidgetSurface`] whose root is a vertical
//! group holding a Tree (with one `lazy_children` branch), an IconButton
//! armed with [`ConfirmPolicy::Prompt`] (the host asks before invoking),
//! and a Table reference into the surface's typed-table registry — driven
//! with a cell-level [`WidgetOp::TableCell`] patch (one cell = one patch
//! fact) and answering `TablePageRequested` / `PanelStoreDemand` with the
//! documented snapshot-cycle publish that regains the registry window.
//!
//! Id spaces (per the widgets module's WGT-ID law):
//!
//! ```text
//! widget namespace:      group 90, tree 100, icon button 110, table ref 120
//! tree rows:             1, 2 (→1), 3, 4 (→3), 12 (lazy branch)
//! lazy backing children: 130..132 (parent 12; published on demand)
//! table registry:        table 200; columns 201/202/203/204
//! row keys:              300..=339 (40 backing rows; the first window
//!                        publishes 8)
//! ```

use std::collections::BTreeMap;
use std::sync::Arc;

use ocs_plugin_api::host::PluginRequestSender;
use ocs_plugin_api::ipc::protocol::PanelStoreKind;
use ocs_plugin_api::scene::{SceneRevision, StableId};
use ocs_plugin_api::widgets::{
    Axis, BadgeTone, CellKind, ConfirmPolicy, DomainRef, IconActionDef, IconRef, MAX_WIDGET_ROWS_INITIAL,
    PanelTableDef,
    TableCell, TableColumnDef, TableRef, TableRowDef, TreeCapabilities, TreeRowDef,
    WidgetCellValue, WidgetError, WidgetGroup, WidgetNode, WidgetOp, WidgetRange, WidgetSurface,
    WidgetSurfacePatch, WidgetTreeDef,
};

use crate::host_wire;
use crate::scene_graph::PANEL_ID;
use crate::state::{DemoState, FrameOutcome};

pub const ROOT_GROUP_ID: u64 = 90;
pub const TREE_WIDGET_ID: u64 = 100;
pub const DELETE_BUTTON_ID: u64 = 110;
pub const TABLE_REF_WIDGET_ID: u64 = 120;
pub const TABLE_ID: u64 = 200;
pub const COL_ITEM: u64 = 201;
pub const COL_LENGTH: u64 = 202;
pub const COL_LOCKED: u64 = 203;
pub const COL_STATE: u64 = 204;
pub const FIRST_ROW_KEY: u64 = 300;
/// The backing set's size (the registry's `total_rows` — the self-declared
/// total the host pages against).
pub const TABLE_TOTAL_ROWS: u64 = 40;
/// The initially published window (`rows` on the def; further windows ride
/// `TablePageRequested` / `PanelStoreDemand` → the snapshot-cycle publish).
pub const TABLE_WINDOW_ROWS: usize = 8;
pub const LAZY_BRANCH_ROW: u64 = 12;
pub const LAZY_CHILDREN_BASE: u64 = 130;
pub const LAZY_CHILDREN_COUNT: usize = 3;

/// Semantic command the icon button invokes (the host confirms through the
/// `ConfirmPolicy::Prompt` dialog first, then reports the invocation).
pub const CMD_DELETE_NODE: &str = "scn_delete_node";

/// The cell the demo's single-cell patch addresses (see [module docs]).
pub const DEMO_PATCH_ROW_KEY: u64 = FIRST_ROW_KEY + 5; // 305
pub const DEMO_PATCH_LENGTH: f64 = 9.75;

/// The paged BACKING row `index` (materialized on demand — pure, stable
/// keys). The demo table's truth: `key = FIRST_ROW_KEY + index`, cells
/// typed to their columns by POSITION.
pub fn backing_table_row(index: usize) -> TableRowDef {
    let key = FIRST_ROW_KEY + index as u64;
    let locked = index % 2 == 0;
    TableRowDef {
        key: StableId(key),
        cells: vec![
            TableCell::Text(format!("material {index:03}")),
            TableCell::Number(index as f64 / 2.0),
            TableCell::Bool(locked),
            TableCell::Badge {
                label: if locked { "locked" } else { "draft" }.to_string(),
                tone: if locked {
                    BadgeTone::Success
                } else {
                    BadgeTone::Info
                },
            },
        ],
        domain_ref: Some(DomainRef {
            kind: "material".to_string(),
            id: key,
        }),
    }
}

/// The backed row key at a 0-based offset (the `PanelStoreDemand` mapping's
/// keyed-window start).
pub fn backing_row_key(index: usize) -> StableId {
    StableId(FIRST_ROW_KEY + index as u64)
}

/// The 0-based offset of a backed row key.
pub fn backing_row_index(key: StableId) -> Option<usize> {
    (key.0 >= FIRST_ROW_KEY && key.0 < FIRST_ROW_KEY + TABLE_TOTAL_ROWS)
        .then_some((key.0 - FIRST_ROW_KEY) as usize)
}

/// The LAZY branch's real children (the plugin owns these rows; the host
/// sees them only after the demand is answered — the store never names a
/// row it has not been published).
pub fn backing_tree_children(parent: StableId) -> Vec<TreeRowDef> {
    if parent.0 != LAZY_BRANCH_ROW {
        return Vec::new();
    }
    (0..LAZY_CHILDREN_COUNT as u64)
        .map(|index| TreeRowDef {
            id: StableId(LAZY_CHILDREN_BASE + index),
            parent: Some(parent),
            label: format!("Surface {:02}", index + 1),
            icon: None,
            status: Some("measured".to_string()),
            expandable: false,
            enabled: true,
            lazy_children: false,
            domain_ref: Some(DomainRef {
                kind: "surface".to_string(),
                id: LAZY_CHILDREN_BASE + index,
            }),
        })
        .collect()
}

/// The tree's initial rows: two expandable roots (one with a static child),
/// and the lazy branch (expandable + `lazy_children` — the documented pair
/// `validate()` demands).
pub fn demo_tree_rows() -> Vec<TreeRowDef> {
    vec![
        TreeRowDef {
            id: StableId(1),
            parent: None,
            label: "Alignment".to_string(),
            icon: None,
            status: None,
            expandable: true,
            enabled: true,
            lazy_children: false,
            domain_ref: None,
        },
        TreeRowDef {
            id: StableId(2),
            parent: Some(StableId(1)),
            label: "Profile A".to_string(),
            icon: None,
            status: None,
            expandable: false,
            enabled: true,
            lazy_children: false,
            domain_ref: Some(DomainRef {
                kind: "alignment".to_string(),
                id: 42,
            }),
        },
        TreeRowDef {
            id: StableId(3),
            parent: None,
            label: "Corridors".to_string(),
            icon: None,
            status: None,
            expandable: true,
            enabled: true,
            lazy_children: false,
            domain_ref: None,
        },
        TreeRowDef {
            id: StableId(4),
            parent: Some(StableId(3)),
            label: "Baseline RD-1".to_string(),
            icon: None,
            status: Some("stationed".to_string()),
            expandable: false,
            enabled: true,
            lazy_children: false,
            domain_ref: None,
        },
        TreeRowDef {
            id: StableId(LAZY_BRANCH_ROW),
            parent: None,
            label: "Surfaces (lazy)".to_string(),
            icon: None,
            status: None,
            expandable: true,
            enabled: true,
            lazy_children: true,
            domain_ref: None,
        },
    ]
}

/// Column vocabulary of the demo table (the addresses cell patches name;
/// cells shadow these 1:1 by position).
pub fn demo_table_columns() -> Vec<TableColumnDef> {
    vec![
        TableColumnDef {
            id: StableId(COL_ITEM),
            label: "item".to_string(),
            kind: CellKind::Text,
            width_hint: None,
            stretch: true,
        },
        TableColumnDef {
            id: StableId(COL_LENGTH),
            label: "length".to_string(),
            kind: CellKind::Number,
            width_hint: Some(64.0),
            stretch: false,
        },
        TableColumnDef {
            id: StableId(COL_LOCKED),
            label: "locked".to_string(),
            kind: CellKind::Bool,
            width_hint: Some(48.0),
            stretch: false,
        },
        TableColumnDef {
            id: StableId(COL_STATE),
            label: "state".to_string(),
            kind: CellKind::Badge,
            width_hint: Some(72.0),
            stretch: false,
        },
    ]
}

/// The demo's widget surface: the composed root, the typed-table registry
/// with its first window, all validated.
pub fn demo_widget_surface(revision: u64) -> Result<WidgetSurface, WidgetError> {
    let surface = WidgetSurface {
        revision: SceneRevision(revision),
        roots: vec![WidgetNode::Group(WidgetGroup {
            id: StableId(ROOT_GROUP_ID),
            name: Some("Scene example panel".to_string()),
            enabled: true,
            visible: true,
            direction: Axis::Vertical,
            children: vec![
                WidgetNode::Tree(WidgetTreeDef {
                    id: StableId(TREE_WIDGET_ID),
                    name: Some("Model catalog".to_string()),
                    enabled: true,
                    visible: true,
                    label: Some("Layers of the model".to_string()),
                    capabilities: TreeCapabilities {
                        multi_select: true,
                        filterable: false,
                        lazy_children: true,
                    },
                    rows: demo_tree_rows(),
                }),
                WidgetNode::IconButton(IconActionDef {
                    id: StableId(DELETE_BUTTON_ID),
                    accessible_name: "Delete selected node".to_string(),
                    tooltip: Some("Asks to confirm first".to_string()),
                    icon: IconRef {
                        uri: "host://icons/scn-delete.svg".to_string(),
                        size: 16.0,
                    },
                    enabled: true,
                    visible: true,
                    active: false,
                    command: CMD_DELETE_NODE.to_string(),
                    confirmation: Some(ConfirmPolicy::Prompt {
                        message: "Remove the selected node and its edges? \
                                  This cannot be undone from the panel."
                            .to_string(),
                    }),
                }),
                WidgetNode::Table(TableRef {
                    id: StableId(TABLE_REF_WIDGET_ID),
                    name: Some("Materials (typed)".to_string()),
                    enabled: true,
                    visible: true,
                    table_id: StableId(TABLE_ID),
                }),
            ],
            split_ratio: None,
        })],
        tables: BTreeMap::from([(
            StableId(TABLE_ID),
            PanelTableDef {
                id_table: StableId(TABLE_ID),
                columns: demo_table_columns(),
                total_rows: Some(TABLE_TOTAL_ROWS),
                rows: (0..TABLE_WINDOW_ROWS).map(backing_table_row).collect(),
            },
        )]),
    };
    surface.validate()?;
    Ok(surface)
}

/// THE single-cell patch: one [`WidgetOp::TableCell`] fact on the `length`
/// column — the wire shape the size tests prove ("one cell edit transfers
/// only its cell-level patch"; the api crate's own acceptance is ≤300 B).
pub fn cell_length_patch(base: u64, revision: u64, key: u64, value: f64) -> WidgetSurfacePatch {
    WidgetSurfacePatch {
        base_revision: SceneRevision(base),
        revision: SceneRevision(revision),
        ops: vec![WidgetOp::TableCell {
            table: StableId(TABLE_ID),
            key: StableId(key),
            column: StableId(COL_LENGTH),
            value: WidgetCellValue::Number(value),
        }],
    }
}

/// The model's tree def (the demo nests it under the single root group).
pub(crate) fn find_tree_mut(surface: &mut WidgetSurface) -> Option<&mut WidgetTreeDef> {
    surface.roots.iter_mut().find_map(|root| match root {
        WidgetNode::Group(group) => group.children.iter_mut().find_map(|node| match node {
            WidgetNode::Tree(tree) if tree.id.0 == TREE_WIDGET_ID => Some(tree),
            _ => None,
        }),
        _ => None,
    })
}

/// The model's tree def (shared read).
pub(crate) fn model_tree(state: &DemoState) -> Option<&WidgetTreeDef> {
    surface_tree(state.widgets.as_ref()?)
}

/// The surface's demo tree def (shared read — the read twin of
/// [`find_tree_mut`]).
pub(crate) fn surface_tree(surface: &WidgetSurface) -> Option<&WidgetTreeDef> {
    surface.roots.iter().find_map(|root| match root {
        WidgetNode::Group(group) => group.children.iter().find_map(|node| match node {
            WidgetNode::Tree(tree) if tree.id.0 == TREE_WIDGET_ID => Some(tree),
            _ => None,
        }),
        _ => None,
    })
}

/// `true` while the model's surface holds the WIDGET NODE id (groups
/// descend; tree ROW ids live in the same namespace but need
/// [`tree_holds_row`]).
pub(crate) fn holds_widget_id(state: &DemoState, id: u64) -> bool {
    let Some(surface) = state.widgets.as_ref() else {
        return false;
    };
    let mut walker: Vec<&WidgetNode> = surface.roots.iter().collect();
    while let Some(node) = walker.pop() {
        if node.widget_id().0 == id {
            return true;
        }
        if let WidgetNode::Group(group) = node {
            walker.extend(group.children.iter());
        }
    }
    false
}

/// `true` while the model's tree already carries the ROW id (the
/// surface-wide namespace covers tree rows too).
pub(crate) fn tree_holds_row(state: &DemoState, row: u64) -> bool {
    model_tree(state).is_some_and(|tree| tree.rows.iter().any(|entry| entry.id.0 == row))
}

/// Advance the model's widget revision and publish the snapshot-cycle
/// frame: a full [`WidgetSurface`] publish whose event leg acks everything
/// merged since the last publish. This is the documented answer shape for
/// table-page / lazy-children demands ("the plugin answers by publishing
/// the window — a snapshot cycle regains the registry").
///
/// The refusal arm mirrors the store's own contract exactly ("a rejection
/// leaves everything exactly as it was"): the model's revision claim is
/// ROLLED BACK to the revision the leg still holds, the taken ack batch is
/// restored (the acknowledgment only rode an applied frame), and the
/// structured reject is recorded with the report's head — which RAISES the
/// claim frontier above the store's truth (the realignment in
/// [`DemoState::note_reject`]), so the next cycle's publish escapes the
/// wedged lane instead of re-colliding with the store head forever.
pub fn publish_widget_cycle(sender: &Arc<dyn PluginRequestSender>, state: &mut DemoState) -> bool {
    // The successor claims the cadence FIRST (the revision law the next
    // publish must satisfy), then the model takes it.
    let successor = state.next_revision();
    let prior_revision = state.widgets.as_ref().map(|surface| surface.revision);
    let Some(surface) = state.widgets.as_mut() else {
        // The claim made no frame at all (nothing is published to cycle):
        // roll it back so the claim ledger keeps matching real frames.
        state.head -= 1;
        return false;
    };
    surface.revision = successor;
    let published = surface.clone();
    let acks = std::mem::take(&mut state.pending_ack);
    state.sent += 1;
    match host_wire::send_widget_publish(sender, PANEL_ID, published, acks.clone()) {
        host_wire::WireOutcome::Applied { revision, .. } => {
            let leg_head = state.widgets.as_ref().map(|surface| surface.revision);
            debug_assert_eq!(
                Some(revision),
                leg_head,
                "the cycle's successor is the model's new leg head"
            );
            state.applied += 1;
            state.note_frame(
                "widget publish",
                None,
                Some(successor.0),
                FrameOutcome::Applied,
            );
            true
        }
        host_wire::WireOutcome::Rejected { code, report } => {
            // Zero mutation on the wire: the model never adopted the claim
            // (the store's own contract, mirrored — this arm used to keep
            // the claimed revision in the model, a model-vs-store
            // divergence waiting for the next patch's base).
            state.pending_ack = acks;
            if let Some(prior) = prior_revision {
                if let Some(surface) = state.widgets.as_mut() {
                    surface.revision = prior;
                }
            }
            state.note_reject(
                "widget publish",
                None,
                Some(successor.0),
                code,
                report.current_revision,
            );
            false
        }
        host_wire::WireOutcome::Unserviceable(message) => {
            state.pending_ack = acks;
            if let Some(prior) = prior_revision {
                if let Some(surface) = state.widgets.as_mut() {
                    surface.revision = prior;
                }
            }
            state.note_frame("widget publish", None, Some(successor.0), FrameOutcome::Unserviceable(message));
            false
        }
        host_wire::WireOutcome::Failed(error) => {
            state.pending_ack = acks;
            if let Some(prior) = prior_revision {
                if let Some(surface) = state.widgets.as_mut() {
                    surface.revision = prior;
                }
            }
            state.note_frame("widget publish", None, Some(successor.0), FrameOutcome::SendFailed(error));
            false
        }
        host_wire::WireOutcome::Unexpected => {
            state.pending_ack = acks;
            if let Some(prior) = prior_revision {
                if let Some(surface) = state.widgets.as_mut() {
                    surface.revision = prior;
                }
            }
            state.note_frame("widget publish", None, Some(successor.0), FrameOutcome::Unexpected);
            false
        }
    }
}

/// Answer every queued demand with the snapshot cycle: the lazy branch's
/// children rows append to the model (preserving the parent-before-child
/// row rule), the table window grows from the held rows onward (stable
/// keys: the held — possibly patched — cells stay exactly as they were).
/// Returns how many publishes went out.
///
/// The rollback law extends to the QUEUE: a publish that refused restores
/// its delivery at the FRONT of the pending queue (arrival order
/// preserved) — the answer is owed again, and the next opportunity retries
/// it from the realigned frontier. Each delivered window also writes the
/// panel-narration status line (the console journal stays the full record).
pub fn deliver_pending(sender: &Arc<dyn PluginRequestSender>, state: &mut DemoState) -> usize {
    let deliveries = std::mem::take(&mut state.pending_deliveries);
    let mut delivered = 0usize;
    let mut owed: Vec<crate::state::PendingDelivery> = Vec::new();
    for delivery in deliveries {
        // `(should publish, the narration line)` — the line lands ONLY on
        // an applied publish (a refused frame narrates nothing).
        let (progressed, line) = match &delivery {
            crate::state::PendingDelivery::TreeChildren { parent, range } => {
                answer_tree_children(sender, state, *parent, *range)
            }
            crate::state::PendingDelivery::TablePage {
                table,
                first_key,
                len,
            } => match extend_table_window(state, *table, *first_key, *len) {
                Some(last_key) => (
                    true,
                    Some(format!(
                        "table window delivered (keys {}..={})",
                        first_key.0, last_key.0
                    )),
                ),
                None => reconcile_table_window(sender, state, *table, *first_key, *len),
            },
        };
        if progressed {
            if publish_widget_cycle(sender, state) {
                state.pages_delivered += 1;
                delivered += 1;
                if let Some(line) = line {
                    state.note_status(line);
                }
            } else {
                // The publish refused: the answer is owed again —
                // front-first so the arrival order survives. The model's
                // already-appended rows self-heal on the retry (the
                // reconciliation below republishes the held window).
                owed.push(delivery);
            }
        }
    }
    if !owed.is_empty() {
        let mut restored = owed;
        restored.extend(std::mem::take(&mut state.pending_deliveries));
        state.pending_deliveries = restored;
    }
    delivered
}

/// The model already covers the asked table window: dedup ONLY while the
/// host's surface holds it too. A host whose window is short (a FRESH
/// DRAWING — the store emptied under a carried plugin state) gets the
/// window re-answered, the model's rows republishing through the ordinary
/// snapshot cycle. Returns `(should publish, the narration line)`.
fn reconcile_table_window(
    sender: &Arc<dyn PluginRequestSender>,
    state: &mut DemoState,
    table: StableId,
    first_key: StableId,
    len: usize,
) -> (bool, Option<String>) {
    if table.0 != TABLE_ID {
        return (false, None);
    }
    let Some(start_index) = backing_row_index(first_key) else {
        return (false, None);
    };
    let end = (start_index + len).min(TABLE_TOTAL_ROWS as usize);
    // ONE bounded read of the host's held window (the same page-miss probe
    // the host itself runs) decides the dedup.
    let host_holds = matches!(
        host_wire::send_store_read(
            sender,
            PANEL_ID,
            PanelStoreKind::TableRows,
            Some(table),
            WidgetRange {
                start: 0,
                len: MAX_WIDGET_ROWS_INITIAL,
            },
        ),
        Some(window) if window.rows.len() >= end
    );
    if host_holds {
        return (false, None);
    }
    state.journal_note(format!(
        "dedup reconciled: the host's window lacks keys up to {} the model holds \
         — re-answering (fresh drawing?)",
        first_key.0 + end as u64 - 1
    ));
    (
        true,
        Some(format!(
            "table window re-answered (the host's surface lacked keys to {})",
            first_key.0 + end as u64 - 1
        )),
    )
}

/// The tree-children answer for ONE ask: the model append when fresh rows
/// exist; on a model-satisfied ask the HOST decides the dedup — the
/// reconciliation law. Returns `(should publish, the narration line)`.
fn answer_tree_children(
    sender: &Arc<dyn PluginRequestSender>,
    state: &mut DemoState,
    parent: Option<StableId>,
    range: WidgetRange,
) -> (bool, Option<String>) {
    let Some(parent) = parent else {
        // Root rows: the demo tree's roots are fully published; nothing
        // exists beyond them — an honest no-op.
        return (false, None);
    };
    match crate::tree_lazy::answer_branch_children(state, parent, range) {
        crate::tree_lazy::ChildrenAnswer::Delivered { first, last } => (
            true,
            Some(format!(
                "lazy page {} delivered (rows {}..={})",
                state.pages_delivered + 1,
                first,
                last
            )),
        ),
        crate::tree_lazy::ChildrenAnswer::UnknownBranch => {
            state.journal_note(format!(
                "children demand refused: the model names no lazy branch at row {} \
                 (asked window {}..={})",
                parent.0,
                range.start,
                range.start + range.len
            ));
            (false, None)
        }
        crate::tree_lazy::ChildrenAnswer::ModelSatisfied => {
            // THE DEDUP, keyed on the host state the plugin last saw — not
            // on a durable answered-once ledger: while the host's current
            // surface still holds the branch's rows the ask is genuinely
            // answered-once (no re-send); a host that lacks them (a FRESH
            // DRAWING — the store emptied under a carried plugin state)
            // gets the window re-answered, the model's rows republishing
            // through the ordinary snapshot cycle.
            if host_holds_children(sender, parent) {
                (false, None)
            } else {
                state.journal_note(format!(
                    "dedup reconciled: the host's surface lacks branch {} rows the \
                     model holds — re-answering (fresh drawing?)",
                    parent.0
                ));
                (
                    true,
                    Some(format!(
                        "lazy branch {} re-answered (the host's surface lacked its rows)",
                        parent.0
                    )),
                )
            }
        }
    }
}

/// Does the HOST's current surface hold any children under `parent`? ONE
/// bounded store read (the wire's existing read form, one row) — the same
/// page-miss probe the host itself runs. A read that answers nothing
/// usable counts as NOT holding (the retry lands on the publish, whose
/// own refusal laws restore the queue).
fn host_holds_children(sender: &Arc<dyn PluginRequestSender>, parent: StableId) -> bool {
    matches!(
        host_wire::send_store_read(
            sender,
            PANEL_ID,
            PanelStoreKind::TreeChildren,
            Some(parent),
            WidgetRange {
                start: 0,
                len: 1
            },
        ),
        Some(window) if !window.rows.is_empty()
    )
}

/// Append the backing children of one lazy branch into the model's tree
/// (a repeat demand is a no-op — the rows are already published). The
/// FIRST lazy branch (row 12) answers its whole small backing set; the
/// PAGED branch (row 13, [`crate::tree_lazy`]) answers the ASKED WINDOW
/// only — the next bounded page, never a re-answer for a satisfied one.
/// Grow the published typed-table window to cover `first_key .. first_key +
/// len` (holding the keys the registry already published — including any
/// cell-patched values — and appending the fresh backing range). Returns
/// the LAST appended key (`None` = honest no-op — the window already
/// covers the ask, or the address is not this demo's registry table).
fn extend_table_window(
    state: &mut DemoState,
    table: StableId,
    first_key: StableId,
    len: usize,
) -> Option<StableId> {
    if table.0 != TABLE_ID {
        // The demo owns exactly one registry table; other asks are honest
        // no-ops (the store itself answers them with the empty page).
        return None;
    }
    let Some(start_index) = backing_row_index(first_key) else {
        return None;
    };
    let end = (start_index + len).min(TABLE_TOTAL_ROWS as usize);
    let Some(surface) = state.widgets.as_mut() else {
        return None;
    };
    let Some(def) = surface.tables.get_mut(&table) else {
        return None;
    };
    if def.rows.len() >= end {
        return None;
    }
    // Append the missing rows in key order; the held rows (and their
    // patched cells) stay byte-identical.
    let mut last = first_key;
    while def.rows.len() < end {
        let row = backing_table_row(def.rows.len());
        last = row.key;
        def.rows.push(row);
    }
    Some(last)
}

/// The snapshot leg of the widget demo (the publish that goes LIVE on the
/// host's mount). This is ALSO the ensure-base step the widget-leg
/// features call (only when the model holds no surface yet — a fresh
/// fixture publish replaces the model, so callers gate it on
/// `state.widgets.is_none()`). The refusal arm mirrors the store's own
/// contract exactly ("a rejection leaves everything exactly as it was"):
/// the model's revision claim is ROLLED BACK to the revision the leg still
/// holds, the taken ack batch is restored (the acknowledgment only rode an
/// applied frame), and the structured reject is recorded with the report's
/// head — which RAISES the claim frontier above the store's truth (the
/// realignment in [`DemoState::note_reject`]), so the next cycle's publish
/// escapes the wedged lane instead of re-colliding with the store head.
pub(crate) fn publish_widget_base(
    sender: &Arc<dyn PluginRequestSender>,
    state: &mut DemoState,
) -> bool {
    // Snapshot leg of the widget surface (the publish that goes LIVE
    // on the host's mount).
    let surface = match demo_widget_surface(state.next_revision().0) {
        Ok(surface) => surface,
        Err(_) => return false,
    };
    let claimed = surface.revision.0;
    let acks = std::mem::take(&mut state.pending_ack);
    state.sent += 1;
    match host_wire::send_widget_publish(sender, PANEL_ID, surface.clone(), acks.clone()) {
        host_wire::WireOutcome::Applied { revision, .. } => {
            debug_assert_eq!(revision, surface.revision);
            state.applied += 1;
            state.note_frame(
                "widget publish",
                None,
                Some(claimed),
                FrameOutcome::Applied,
            );
            state.widgets = Some(surface);
            true
        }
        host_wire::WireOutcome::Rejected { code, report } => {
            state.pending_ack = acks;
            state.note_reject(
                "widget publish",
                None,
                Some(claimed),
                code,
                report.current_revision,
            );
            false
        }
        host_wire::WireOutcome::Unserviceable(message) => {
            state.pending_ack = acks;
            state.note_frame("widget publish", None, Some(claimed), FrameOutcome::Unserviceable(message));
            false
        }
        host_wire::WireOutcome::Failed(error) => {
            state.pending_ack = acks;
            state.note_frame("widget publish", None, Some(claimed), FrameOutcome::SendFailed(error));
            false
        }
        host_wire::WireOutcome::Unexpected => {
            state.pending_ack = acks;
            state.note_frame("widget publish", None, Some(claimed), FrameOutcome::Unexpected);
            false
        }
    }
}

/// The widgets demo: publish the surface, then drive ONE cell-level patch
/// (the single-cell wire fact). Returns whether every frame applied. Every
/// exchange records its [`FrameFact`]; a non-applied publish restores the
/// taken ack batch (the acknowledgment only rode an applied frame).
pub fn run_widgets_demo(sender: &Arc<dyn PluginRequestSender>, state: &mut DemoState) -> bool {
    // (1) Snapshot leg of the widget surface (the publish that goes LIVE
    // on the host's mount).
    if !publish_widget_base(sender, state) {
        return false;
    }

    // (2) The one-cell patch: the `length` cell of row 305 — keyed, minimal.
    let Some(base) = state.widgets_head() else {
        return false;
    };
    let successor = state.next_revision();
    let patch = cell_length_patch(base.0, successor.0, DEMO_PATCH_ROW_KEY, DEMO_PATCH_LENGTH);
    let base_revision = patch.base_revision.0;
    let claimed = patch.revision.0;
    state.sent += 1;
    match host_wire::send_widget_patch(sender, PANEL_ID, patch.clone()) {
        host_wire::WireOutcome::Applied { revision, .. } => {
            state.applied += 1;
            let Some(applied) = state.widgets.as_mut() else {
                return false;
            };
            if applied.apply_patch(patch).is_err() {
                state.note_frame(
                    "widget patch",
                    Some(base_revision),
                    Some(claimed),
                    FrameOutcome::AppliedModelLagged(revision.0),
                );
                return false;
            }
            debug_assert_eq!(applied.revision, revision, "model and host agree");
            state.note_frame(
                "widget patch",
                Some(base_revision),
                Some(claimed),
                FrameOutcome::Applied,
            );
        }
        host_wire::WireOutcome::Rejected { code, report } => {
            state.note_reject(
                "widget patch",
                Some(base_revision),
                Some(claimed),
                code,
                report.current_revision,
            );
            return false;
        }
        host_wire::WireOutcome::Unserviceable(message) => {
            state.note_frame("widget patch", Some(base_revision), Some(claimed), FrameOutcome::Unserviceable(message));
            return false;
        }
        host_wire::WireOutcome::Failed(error) => {
            state.note_frame("widget patch", Some(base_revision), Some(claimed), FrameOutcome::SendFailed(error));
            return false;
        }
        host_wire::WireOutcome::Unexpected => {
            state.note_frame("widget patch", Some(base_revision), Some(claimed), FrameOutcome::Unexpected);
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use ocs_plugin_api::host::{BuiltinPlugin, HostNotification};
    use ocs_plugin_api::ipc::protocol::{PanelStoreKind, PatchRejectCode};
    use ocs_plugin_api::scene::SceneRevision;
    use ocs_plugin_api::widgets::{WidgetCellValue, WidgetEvent, WidgetRange};

    use crate::fake_host::FakeWireHost;
    use crate::state::PendingDelivery;

    use super::*;

    /// Read a state fact through the plugin's whole lock (no guard is held
    /// across a call — the on_notification path locks the same mutex).
    macro_rules! fact {
        ($plugin:expr, $field:ident) => {{
            let state = $plugin.state();
            state.$field.clone()
        }};
    }

    /// The happy path end to end, hostless: publish + one-cell patch on the
    /// wire, events merged through the plugin's own `on_notification` (ack
    /// legs + state machine), a typed-table page demand answered by the
    /// documented snapshot cycle, and store reads served from the fake's
    /// held store.
    #[test]
    fn widgets_demo_full_flow_headless() {
        let host = Arc::new(FakeWireHost::new());
        let sender: Arc<dyn PluginRequestSender> =
            Arc::clone(&host) as Arc<dyn PluginRequestSender>;
        let mut plugin = crate::SceneExamplePlugin::new();
        // The request line is deliberately NOT cached on the plugin here:
        // this test pins the merge + manual-cycle discipline (the queues
        // hold until an outbound opportunity runs); the auto-flush itself
        // is proven by its own tests (the flush rides a CACHED sender's
        // next notification).

        // (1) Publish + one-cell patch apply.
        {
            let mut state = plugin.state();
            assert!(super::run_widgets_demo(&sender, &mut state));
        }
        {
            let state = plugin.state();
            assert_eq!((state.sent, state.applied), (2, 2));
            assert_eq!(state.head, 2);
        }
        // The fake's held surface equals the plugin's model (the same
        // patched cell), and the cell write was surgical.
        let stored = host.widgets().expect("published").clone();
        assert_eq!(stored.revision, SceneRevision(2));
        let def = &stored.tables[&StableId(TABLE_ID)];
        assert_eq!(def.rows.len(), TABLE_WINDOW_ROWS);
        assert_eq!(
            def.rows[5].cells[1],
            TableCell::Number(DEMO_PATCH_LENGTH),
            "the one patched cell"
        );
        assert_eq!(
            def.rows[4].cells[1],
            TableCell::Number(2.0),
            "a neighbouring cell stayed exactly as published"
        );
        assert_eq!(
            def.rows[5].cells[3],
            TableCell::Badge {
                label: "draft".to_string(),
                tone: BadgeTone::Info
            },
            "the badge column stayed exactly as published"
        );

        // (2) The runner feeds the plugin the v24 event batch (the host's
        // drained widget events): selection/command/activation facts merge;
        // the batch queues on the ack leg; the page demand queues a
        // delivery.
        let surface_id = fact!(plugin, widgets)
            .as_ref()
            .and_then(|surface| surface.roots.first().map(|root| root.widget_id()))
            .expect("published surface");
        assert_eq!(
            surface_id,
            StableId(ROOT_GROUP_ID),
            "the store freezes the FIRST root's id"
        );
        plugin.on_notification(
            None,
            HostNotification::PanelSceneEvents {
                panel_id: PANEL_ID.to_owned(),
                surface_id,
                events: vec![
                    WidgetEvent::SelectionChanged {
                        surface_id,
                        selected_ids: vec![StableId(2)],
                    },
                    WidgetEvent::ItemActivated {
                        surface_id,
                        item_id: StableId(2),
                    },
                    WidgetEvent::CommandInvoked {
                        surface_id,
                        command_id: CMD_DELETE_NODE.to_string(),
                        target_ids: vec![StableId(2)],
                    },
                    WidgetEvent::TablePageRequested {
                        surface_id,
                        table: StableId(TABLE_ID),
                        first_key: StableId(308),
                        len: 8,
                    },
                    WidgetEvent::TableCellChanged {
                        surface_id,
                        row_key: StableId(DEMO_PATCH_ROW_KEY),
                        column_id: StableId(COL_LENGTH),
                        value: WidgetCellValue::Number(DEMO_PATCH_LENGTH),
                    },
                ],
            },
        );
        {
            let state = plugin.state();
            assert_eq!(state.selected, vec![StableId(2)]);
            assert_eq!(state.activations, vec![StableId(2)]);
            assert_eq!(
                state.commands_invoked,
                vec![(CMD_DELETE_NODE.to_string(), vec![StableId(2)])]
            );
            assert_eq!(state.pending_ack.len(), 5, "the whole batch merged");
            assert_eq!(
                state.pending_deliveries,
                vec![PendingDelivery::TablePage {
                    table: StableId(TABLE_ID),
                    first_key: StableId(308),
                    len: 8,
                }]
            );
        }

        // (3) The queued demand is answered by the snapshot cycle.
        {
            let mut state = plugin.state();
            assert_eq!(super::deliver_pending(&sender, &mut state), 1);
        }
        {
            let state = plugin.state();
            assert_eq!(state.pages_delivered, 1);
            assert_eq!(state.pending_ack.len(), 0, "the ack leg rode the cycle");
            assert_eq!(state.head, 3);
        }
        let def = &host.widgets().expect("cycle published").tables[&StableId(TABLE_ID)];
        assert_eq!(
            def.rows.len(),
            16,
            "the window grew 8 → 16 (keys 300..=315)"
        );
        assert_eq!(
            def.rows[0].cells[1],
            TableCell::Number(0.0),
            "stable keys: row 300 kept its cells"
        );
        assert_eq!(
            def.rows[5].cells[1],
            TableCell::Number(DEMO_PATCH_LENGTH),
            "the patched cell survived the window growth"
        );
        assert_eq!(def.total_rows, Some(TABLE_TOTAL_ROWS));

        // (4) A typed-table read served from the store: keyed paging, the
        // def's own total as the hint.
        let page = host_wire::send_store_read(
            &sender,
            PANEL_ID,
            PanelStoreKind::TableRows,
            Some(StableId(TABLE_ID)),
            WidgetRange { start: 0, len: 32 },
        )
        .expect("a page answered");
        assert_eq!(page.rows.len(), 16, "the held window");
        assert_eq!(
            page.total_hint, TABLE_TOTAL_ROWS,
            "the def's declared total"
        );

        // (5) The lazy branch as the host sees it BEFORE its children are
        // answered: the store has no rows under it yet (the plugin owns
        // them), so the page is empty.
        let page = host_wire::send_store_read(
            &sender,
            PANEL_ID,
            PanelStoreKind::TreeChildren,
            Some(StableId(LAZY_BRANCH_ROW)),
            WidgetRange { start: 0, len: 8 },
        )
        .expect("a page answered");
        assert!(page.rows.is_empty());
        assert_eq!(page.total_hint, 0);

        // (6) The demand path as the notification carries it: the host asks
        // for the lazy branch's children — the plugin answers with the
        // snapshot cycle and the then-held rows page.
        plugin.on_notification(
            None,
            HostNotification::PanelStoreDemand {
                panel_id: PANEL_ID.to_owned(),
                surface_id,
                parent_id: Some(StableId(LAZY_BRANCH_ROW)),
                range: WidgetRange { start: 0, len: 8 },
            },
        );
        {
            let mut state = plugin.state();
            assert_eq!(super::deliver_pending(&sender, &mut state), 1);
        }
        {
            let state = plugin.state();
            assert_eq!(state.pages_delivered, 2);
            let surface = state.widgets.as_ref().expect("model kept");
            let tree = surface
                .roots
                .iter()
                .find_map(|root| match root {
                    WidgetNode::Group(group) => group.children.iter().find_map(|node| match node {
                        WidgetNode::Tree(tree) if tree.id.0 == TREE_WIDGET_ID => Some(tree),
                        _ => None,
                    }),
                    _ => None,
                })
                .expect("the demo tree");
            assert_eq!(
                tree.rows.len(),
                demo_tree_rows().len() + LAZY_CHILDREN_COUNT
            );
            // Parent-before-child holds for the appended page.
            for row in tree.rows.iter().skip(demo_tree_rows().len()) {
                assert_eq!(row.parent, Some(StableId(LAZY_BRANCH_ROW)));
            }
        }
        let page = host_wire::send_store_read(
            &sender,
            PANEL_ID,
            PanelStoreKind::TreeChildren,
            Some(StableId(LAZY_BRANCH_ROW)),
            WidgetRange { start: 0, len: 8 },
        )
        .expect("a page answered");
        assert_eq!(
            page.rows.len(),
            LAZY_CHILDREN_COUNT,
            "the three backing children"
        );
        assert_eq!(page.total_hint, LAZY_CHILDREN_COUNT as u64);
    }

    /// The minimality proof on the demo's own patch: ONE cell fact on the
    /// wire stays under the api crate's own ≥300 B acceptance, while the
    /// full-table snapshot it replaces undercuts nothing — the fair
    /// contrast frame is the whole def (an order of magnitude bigger).
    #[test]
    fn table_cell_patch_minimality() {
        let one = cell_length_patch(2, 3, DEMO_PATCH_ROW_KEY, DEMO_PATCH_LENGTH);
        let bytes = bincode::serialize(&one).unwrap();
        assert!(
            bytes.len() <= 300,
            "1-cell patch payload = {} bytes (the single-cell acceptance ~200B)",
            bytes.len()
        );
        // The contrast: the demo surface's registry (the same 8-row window
        // a snapshot retransmits) is orders larger per byte.
        let surface = demo_widget_surface(1).unwrap();
        let registry_bytes = bincode::serialize(&surface).unwrap();
        assert!(
            registry_bytes.len() > bytes.len() * 20,
            "one cell fact = {} B; the full surface snapshot cycle = {} B (~{:0.0}x)",
            bytes.len(),
            registry_bytes.len(),
            registry_bytes.len() as f64 / bytes.len() as f64
        );
    }

    /// A widget patch frame round-trips serde exactly (the wire's own
    /// encoding discipline).
    #[test]
    fn widget_patch_frame_roundtrip_serde() {
        let patch = cell_length_patch(2, 3, DEMO_PATCH_ROW_KEY, DEMO_PATCH_LENGTH);
        let round: WidgetSurfacePatch =
            bincode::deserialize(&bincode::serialize(&patch).unwrap()).unwrap();
        assert_eq!(round, patch);
    }

    /// THE model-vs-store contract, mirrored plugin-side: a REFUSED
    /// snapshot cycle leaves everything exactly as it was — the model's
    /// revision claim rolls back to the leg the store STILL holds, the
    /// taken ack batch is restored (the acknowledgment only rode an
    /// applied frame), and the structured reject's report head raises the
    /// claim frontier so the NEXT cycle escapes the wedged lane. The
    /// wedge drive is the real lost-response shape: the plugin's claim
    /// frontier rewound BELOW the leg the host holds.
    #[test]
    fn refused_widget_cycle_restores_model_ack_leg_and_realigns() {
        let host = Arc::new(FakeWireHost::new());
        let sender: Arc<dyn PluginRequestSender> =
            Arc::clone(&host) as Arc<dyn PluginRequestSender>;
        let mut state = DemoState::default();
        assert!(super::run_widgets_demo(&sender, &mut state));
        // The surface id both the events and the store address.
        let surface_id = state.widgets_surface_id().expect("published");
        assert_eq!(surface_id, StableId(ROOT_GROUP_ID));

        // A merged event batch + a pending demand (the live-loop state).
        state.handle_events(
            surface_id,
            vec![WidgetEvent::SelectionChanged {
                surface_id,
                selected_ids: vec![StableId(1)],
            }],
        );
        state.on_store_demand(Some(StableId(TABLE_ID)), WidgetRange { start: 8, len: 8 });
        assert_eq!(state.pending_ack.len(), 1);
        assert_eq!(state.pending_deliveries.len(), 1);

        // THE WEDGE: the claim frontier rewound below the held leg —
        // exactly what a lost response (or a foreign-tab refusal) leaves
        // behind when the plugin's tallies were the only witness.
        state.head = 0;

        // The cycle through the plugin's real deliver path (the demand
        // answer's publish refuses — revision 1 ≤ the held head 2).
        assert_eq!(deliver_pending(&sender, &mut state), 0);

        // (a) the model never adopted the refused claim;
        assert_eq!(
            state.widgets.as_ref().expect("model kept").revision,
            SceneRevision(2),
            "the refused claim rolled back to the leg the store holds"
        );
        // (b) the ack batch is owed again (the publish that refused did
        // not carry it);
        assert_eq!(state.pending_ack.len(), 1, "the ack batch restored");
        // (c) the report's head raised the frontier above the store's
        // truth — the ONE-cadence law holds without rewinding a leg;
        assert_eq!(state.head, 2, "realigned to the report's head");
        assert_eq!(
            state.last_reject,
            Some((PatchRejectCode::NonMonotonicRevision, Some(SceneRevision(2))))
        );
        // (d) THE HEAL: the next demand's cycle publishes from the
        // realigned frontier — the store agrees. The queue-restore law
        // (SCN-034) also re-delivers the demand the refused pass owed: the
        // heal answers BOTH windows (the restored one via the fresh-
        // drawing reconciliation, the new one via the ordinary extension).
        state.on_store_demand(Some(StableId(TABLE_ID)), WidgetRange { start: 16, len: 8 });
        assert_eq!(deliver_pending(&sender, &mut state), 2);
        assert_eq!(state.head, 4);
        let stored = host.widgets().expect("published").clone();
        assert_eq!(stored.revision, SceneRevision(4));
        assert_eq!(
            stored.tables[&StableId(TABLE_ID)].rows.len(),
            24,
            "the window grew past the wedged lane"
        );
    }

    /// The rollback law extends to the DELIVERY QUEUE: a refused flush
    /// frame restores the delivery EXACTLY (front-first), and the retry
    /// reconciles — the model's already-appended rows republish and the
    /// host's window grows to match (the fresh-drawing reconciliation
    /// doubles as the heal).
    #[test]
    fn refused_delivery_flush_restores_the_queue_and_the_retry_reconciles() {
        let host = Arc::new(FakeWireHost::new());
        let sender: Arc<dyn PluginRequestSender> =
            Arc::clone(&host) as Arc<dyn PluginRequestSender>;
        let mut state = DemoState::default();
        assert!(super::run_widgets_demo(&sender, &mut state));

        // One queued window demand; the flush wedged (the frontier rewound
        // below the held leg — the lost-response shape).
        state.on_store_demand(Some(StableId(TABLE_ID)), WidgetRange { start: 8, len: 8 });
        state.head = 0;
        assert_eq!(deliver_pending(&sender, &mut state), 0);
        // (a) the delivery is owed again — the queue restored EXACTLY;
        assert_eq!(
            state.pending_deliveries,
            vec![PendingDelivery::TablePage {
                table: StableId(TABLE_ID),
                first_key: StableId(308),
                len: 8,
            }]
        );
        // (b) the reject's report realigned the frontier;
        assert_eq!(state.head, state.widgets_head().expect("leg").0);
        // (c) THE HEAL: the retry's publish applies and the host's window
        // grows (the model's already-appended rows reconcile into it).
        assert_eq!(deliver_pending(&sender, &mut state), 1);
        assert!(state.pending_deliveries.is_empty());
        let stored = host.widgets().expect("published").clone();
        assert_eq!(
            stored.tables[&StableId(TABLE_ID)].rows.len(),
            16,
            "the host regained the window the model held"
        );
        // The model and the host agree again.
        assert_eq!(
            state.widgets.as_ref().expect("model").tables[&StableId(TABLE_ID)].rows.len(),
            16
        );
        // The reconcile is legible in the journal.
        assert!(
            state
                .journal
                .iter()
                .any(|line| line.contains("dedup reconciled")),
            "{:?}",
            state.journal
        );
        // The status line narrated the reconciled delivery.
        assert!(
            state
                .status_line
                .as_deref()
                .is_some_and(|line| line.contains("table window re-answered")),
            "{:?}",
            state.status_line
        );
    }
}
