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
use ocs_plugin_api::scene::{SceneRevision, StableId};
use ocs_plugin_api::widgets::{
    Axis, BadgeTone, CellKind, ConfirmPolicy, DomainRef, IconActionDef, IconRef, PanelTableDef,
    TableCell, TableColumnDef, TableRef, TableRowDef, TreeCapabilities, TreeRowDef,
    WidgetCellValue, WidgetError, WidgetGroup, WidgetNode, WidgetOp, WidgetSurface,
    WidgetSurfacePatch, WidgetTreeDef,
};

use crate::host_wire;
use crate::scene_graph::PANEL_ID;
use crate::state::DemoState;

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
fn find_tree_mut(surface: &mut WidgetSurface) -> Option<&mut WidgetTreeDef> {
    surface.roots.iter_mut().find_map(|root| match root {
        WidgetNode::Group(group) => group.children.iter_mut().find_map(|node| match node {
            WidgetNode::Tree(tree) if tree.id.0 == TREE_WIDGET_ID => Some(tree),
            _ => None,
        }),
        _ => None,
    })
}

/// Advance the model's widget revision and publish the snapshot-cycle
/// frame: a full [`WidgetSurface`] publish whose event leg acks everything
/// merged since the last publish. This is the documented answer shape for
/// table-page / lazy-children demands ("the plugin answers by publishing
/// the window — a snapshot cycle regains the registry").
pub fn publish_widget_cycle(sender: &Arc<dyn PluginRequestSender>, state: &mut DemoState) -> bool {
    // The successor claims the cadence FIRST (the revision law the next
    // publish must satisfy), then the model takes it.
    let successor = state.next_revision();
    let Some(surface) = state.widgets.as_mut() else {
        return false;
    };
    surface.revision = successor;
    let published = surface.clone();
    state.sent += 1;
    match host_wire::send_widget_publish(
        sender,
        PANEL_ID,
        published,
        std::mem::take(&mut state.pending_ack),
    ) {
        host_wire::WireOutcome::Applied { revision, .. } => {
            let leg_head = state.widgets.as_ref().map(|surface| surface.revision);
            debug_assert_eq!(
                Some(revision),
                leg_head,
                "the cycle's successor is the model's new leg head"
            );
            state.applied += 1;
            true
        }
        host_wire::WireOutcome::Rejected { code, report } => {
            state.rejected += 1;
            state.last_reject = Some((code, report.current_revision));
            false
        }
        _ => false,
    }
}

/// Answer every queued demand with the snapshot cycle: the lazy branch's
/// children rows append to the model (preserving the parent-before-child
/// row rule), the table window grows from the held rows onward (stable
/// keys: the held — possibly patched — cells stay exactly as they were).
/// Returns how many publishes went out.
pub fn deliver_pending(sender: &Arc<dyn PluginRequestSender>, state: &mut DemoState) -> usize {
    let deliveries = std::mem::take(&mut state.pending_deliveries);
    let mut delivered = 0usize;
    for delivery in deliveries {
        let progressed = match delivery {
            crate::state::PendingDelivery::TreeChildren { parent, range: _ } => {
                append_lazy_children(state, parent)
            }
            crate::state::PendingDelivery::TablePage {
                table,
                first_key,
                len,
            } => extend_table_window(state, table, first_key, len),
        };
        if progressed && publish_widget_cycle(sender, state) {
            state.pages_delivered += 1;
            delivered += 1;
        }
    }
    delivered
}

/// Append the backing children of one lazy branch into the model's tree
/// (a repeat demand is a no-op — the rows are already published).
fn append_lazy_children(state: &mut DemoState, parent: Option<StableId>) -> bool {
    let Some(parent) = parent else {
        // Root rows: the demo tree's roots are fully published; nothing
        // exists beyond them — an honest no-op.
        return false;
    };
    let children = backing_tree_children(parent);
    if children.is_empty() {
        return false;
    }
    let Some(surface) = state.widgets.as_mut() else {
        return false;
    };
    let Some(tree) = find_tree_mut(surface) else {
        return false;
    };
    // One delivery per row set: an already-published child skips the work.
    let published: std::collections::HashSet<StableId> =
        tree.rows.iter().map(|row| row.id).collect();
    let fresh: Vec<TreeRowDef> = children
        .into_iter()
        .filter(|child| !published.contains(&child.id))
        .collect();
    if fresh.is_empty() {
        return false;
    }
    // Parent bindings: the branch row precedes its children in the row vec
    // (the payload-validation rule — appending keeps that).
    tree.rows.extend(fresh);
    true
}

/// Grow the published typed-table window to cover `first_key .. first_key +
/// len` (holding the keys the registry already published — including any
/// cell-patched values — and appending the fresh backing range).
fn extend_table_window(
    state: &mut DemoState,
    table: StableId,
    first_key: StableId,
    len: usize,
) -> bool {
    if table.0 != TABLE_ID {
        // The demo owns exactly one registry table; other asks are honest
        // no-ops (the store itself answers them with the empty page).
        return false;
    }
    let Some(start_index) = backing_row_index(first_key) else {
        return false;
    };
    let end = (start_index + len).min(TABLE_TOTAL_ROWS as usize);
    let Some(surface) = state.widgets.as_mut() else {
        return false;
    };
    let Some(def) = surface.tables.get_mut(&table) else {
        return false;
    };
    if def.rows.len() >= end {
        return false;
    }
    // Append the missing rows in key order; the held rows (and their
    // patched cells) stay byte-identical.
    while def.rows.len() < end {
        def.rows.push(backing_table_row(def.rows.len()));
    }
    true
}

/// The widgets demo: publish the surface, then drive ONE cell-level patch
/// (the single-cell wire fact). Returns whether every frame applied.
pub fn run_widgets_demo(sender: &Arc<dyn PluginRequestSender>, state: &mut DemoState) -> bool {
    // (1) Snapshot leg of the widget surface (the publish that goes LIVE
    // on the host's mount).
    let surface = match demo_widget_surface(state.next_revision().0) {
        Ok(surface) => surface,
        Err(_) => return false,
    };
    state.sent += 1;
    match host_wire::send_widget_publish(
        sender,
        PANEL_ID,
        surface.clone(),
        std::mem::take(&mut state.pending_ack),
    ) {
        host_wire::WireOutcome::Applied { revision, .. } => {
            debug_assert_eq!(revision, surface.revision);
            state.applied += 1;
            state.widgets = Some(surface);
        }
        host_wire::WireOutcome::Rejected { code, report } => {
            state.rejected += 1;
            state.last_reject = Some((code, report.current_revision));
            return false;
        }
        _ => return false,
    }

    // (2) The one-cell patch: the `length` cell of row 305 — keyed, minimal.
    let Some(base) = state.widgets_head() else {
        return false;
    };
    let successor = state.next_revision();
    let patch = cell_length_patch(base.0, successor.0, DEMO_PATCH_ROW_KEY, DEMO_PATCH_LENGTH);
    state.sent += 1;
    match host_wire::send_widget_patch(sender, PANEL_ID, patch.clone()) {
        host_wire::WireOutcome::Applied { revision, .. } => {
            state.applied += 1;
            let Some(applied) = state.widgets.as_mut() else {
                return false;
            };
            if applied.apply_patch(patch).is_err() {
                return false;
            }
            debug_assert_eq!(applied.revision, revision, "model and host agree");
        }
        host_wire::WireOutcome::Rejected { code, report } => {
            state.rejected += 1;
            state.last_reject = Some((code, report.current_revision));
            return false;
        }
        _ => return false,
    }
    true
}

#[cfg(test)]
mod tests {
    use ocs_plugin_api::host::{BuiltinPlugin, HostNotification};
    use ocs_plugin_api::ipc::protocol::PanelStoreKind;
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
        plugin.state().sender = Some(Arc::clone(&sender));

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
}
