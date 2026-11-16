//! The lazy-children tree demo: an expandable branch with NO static
//! children (`expandable: true` + `lazy_children: true` — the documented
//! pair `validate()` demands) patched onto the EXISTING demo tree. When
//! the host expands it, the `ChildrenRequested` ask is answered with the
//! NEXT BOUNDED PAGE — never the whole backing set, and never a
//! re-answer for an already-satisfied window.
//!
//! The base surface's first lazy branch (row 12, children 130..=132) is
//! small enough to answer in one page; THIS branch (row 13) carries TEN
//! backing children so the windowed answer actually pages: every ask is
//! answered with at most the asked window's fresh rows (4 at the demo's
//! page width), appended parent-before-child into the model and published
//! through the documented snapshot cycle ([`crate::widgets_table::
//! deliver_pending`]).
//!
//! Id space (the surface-wide namespace): branch row **13**, its backing
//! children **140..=149** (past the base surface's rows 1..=12 and the
//! first branch's children 130..=132).

use std::sync::Arc;

use ocs_plugin_api::host::PluginRequestSender;
use ocs_plugin_api::scene::StableId;
use ocs_plugin_api::widgets::{DomainRef, TreeRowDef, WidgetOp, WidgetNode, WidgetRange};

use crate::host_wire;
use crate::scene_graph::PANEL_ID;
use crate::state::{DemoState, FrameOutcome};
use crate::widgets_table::{self, TREE_WIDGET_ID};

/// The paged lazy branch's row id.
pub const PAGED_BRANCH_ROW: u64 = 13;
/// The branch's backing children (stable keys, materialized on demand).
pub const PAGED_CHILDREN_BASE: u64 = 140;
pub const PAGED_CHILDREN_COUNT: usize = 10;
/// The page width the demo answers with (the bounded window — a wider
/// ask still answers at most this many fresh rows per publish).
pub const PAGED_PAGE_LEN: usize = 4;

/// The branch row: expandable + lazy, NO static children.
pub fn paged_branch_row() -> TreeRowDef {
    TreeRowDef {
        id: StableId(PAGED_BRANCH_ROW),
        parent: None,
        label: "Sheets (lazy)".to_string(),
        icon: None,
        status: None,
        expandable: true,
        enabled: true,
        lazy_children: true,
        domain_ref: None,
    }
}

/// The backing child at `index` (pure — stable key, stable label).
pub fn paged_child(index: usize) -> TreeRowDef {
    TreeRowDef {
        id: StableId(PAGED_CHILDREN_BASE + index as u64),
        parent: Some(StableId(PAGED_BRANCH_ROW)),
        label: format!("Sheet {:02}", index + 1),
        icon: None,
        status: Some("paged".to_string()),
        expandable: false,
        enabled: true,
        lazy_children: false,
        domain_ref: Some(DomainRef {
            kind: "sheet".to_string(),
            id: PAGED_CHILDREN_BASE + index as u64,
        }),
    }
}

/// The paged branch's answer to ONE asked window: the fresh (never yet
/// published) backing rows inside `[range.start, range.start +
/// range.len)` — bounded by the page width, clipped to the backing set,
/// appended parent-before-child into the model's tree. A repeat ask for a
/// satisfied window is an honest no-op (`false` — nothing to publish).
pub(crate) fn append_windowed_children(
    state: &mut DemoState,
    parent: StableId,
    range: WidgetRange,
) -> bool {
    if parent.0 != PAGED_BRANCH_ROW {
        return false;
    }
    let Some(surface) = state.widgets.as_mut() else {
        return false;
    };
    let Some(tree) = widgets_table::find_tree_mut(surface) else {
        return false;
    };
    // Parent-before-child: the branch row must already be published (the
    // host can only ask for children of a row it has seen).
    if !tree.rows.iter().any(|row| row.id.0 == PAGED_BRANCH_ROW) {
        return false;
    }
    let published: std::collections::HashSet<u64> =
        tree.rows.iter().map(|row| row.id.0).collect();
    let start = range.start.min(PAGED_CHILDREN_COUNT);
    let end = range
        .start
        .saturating_add(range.len)
        .min(PAGED_CHILDREN_COUNT)
        .max(start);
    let fresh: Vec<TreeRowDef> = (start..end)
        .map(paged_child)
        .filter(|child| !published.contains(&child.id.0))
        .take(PAGED_PAGE_LEN)
        .collect();
    if fresh.is_empty() {
        return false;
    }
    tree.rows.extend(fresh);
    true
}

/// The lazy-tree feature's flow: ensure the widget leg exists, then ONE
/// `Update` patch adds the branch to the EXISTING tree (the whole payload
/// swaps; the model's current rows — including already-delivered children
/// of the first branch — are preserved verbatim). Idempotent: a second
/// run sends no frame.
pub fn run_tree_lazy_demo(
    sender: &Arc<dyn PluginRequestSender>,
    state: &mut DemoState,
) -> bool {
    // Ensure-base: a fresh fixture publish REPLACES the model, so it only
    // runs when the model holds no surface.
    if state.widgets.is_none() && !widgets_table::publish_widget_base(sender, state) {
        return false;
    }
    if widgets_table::tree_holds_row(state, PAGED_BRANCH_ROW) {
        return true;
    }
    let Some(base) = state.widgets_head() else {
        return false;
    };
    // The replacement tree payload is built from the MODEL's current rows
    // (the branch appends to whatever rows are live right now) — but the
    // model itself is NOT touched until the wire applies (a rejected
    // frame leaves everything exactly as it was).
    let Some(content_tree) = widgets_table::model_tree(state).map(|tree| {
        let mut tree = tree.clone();
        tree.rows.push(paged_branch_row());
        tree
    }) else {
        return false;
    };
    let content = WidgetNode::Tree(content_tree);
    let successor = state.next_revision();
    let patch = ocs_plugin_api::widgets::WidgetSurfacePatch {
        base_revision: base,
        revision: successor,
        ops: vec![WidgetOp::Update {
            id: StableId(TREE_WIDGET_ID),
            content,
        }],
    };
    let base_revision = patch.base_revision.0;
    let claimed = patch.revision.0;
    state.sent += 1;
    match host_wire::send_widget_patch(sender, PANEL_ID, patch.clone()) {
        host_wire::WireOutcome::Applied { revision, .. } => {
            state.applied += 1;
            let Some(surface) = state.widgets.as_mut() else {
                return false;
            };
            if surface.apply_patch(patch).is_err() {
                state.note_frame(
                    "widget patch",
                    Some(base_revision),
                    Some(claimed),
                    FrameOutcome::AppliedModelLagged(revision.0),
                );
                return false;
            }
            debug_assert_eq!(surface.revision, revision, "model and host agree");
            state.note_frame(
                "widget patch",
                Some(base_revision),
                Some(claimed),
                FrameOutcome::Applied,
            );
            state.journal_note(format!(
                "lazy tree: branch {} armed ({} backing children, pages of {}) — expand \
                 it in the host; every ChildrenRequested ask gets the next bounded page",
                PAGED_BRANCH_ROW, PAGED_CHILDREN_COUNT, PAGED_PAGE_LEN
            ));
            true
        }
        host_wire::WireOutcome::Rejected { code, report } => {
            state.note_reject(
                "widget patch",
                Some(base_revision),
                Some(claimed),
                code,
                report.current_revision,
            );
            false
        }
        host_wire::WireOutcome::Unserviceable(message) => {
            state.note_frame("widget patch", Some(base_revision), Some(claimed), FrameOutcome::Unserviceable(message));
            false
        }
        host_wire::WireOutcome::Failed(error) => {
            state.note_frame("widget patch", Some(base_revision), Some(claimed), FrameOutcome::SendFailed(error));
            false
        }
        host_wire::WireOutcome::Unexpected => {
            state.note_frame("widget patch", Some(base_revision), Some(claimed), FrameOutcome::Unexpected);
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use ocs_plugin_api::ipc::protocol::PanelStoreKind;
    use ocs_plugin_api::widgets::{WidgetEvent, WidgetRange, WidgetSurfacePatch};

    use crate::fake_host::FakeWireHost;
    use crate::host_wire::send_store_read;
    use crate::scene_graph::PANEL_ID;
    use crate::state::DemoState;

    use super::*;

    fn branch_row_count(state: &DemoState) -> usize {
        state
            .widgets
            .as_ref()
            .map(|surface| {
                surface
                    .roots
                    .iter()
                    .find_map(|root| match root {
                        WidgetNode::Group(group) => group.children.iter().find_map(|node| match node {
                            WidgetNode::Tree(tree) if tree.id.0 == TREE_WIDGET_ID => {
                                Some(tree.rows.len())
                            }
                            _ => None,
                        }),
                        _ => None,
                    })
                    .expect("the demo tree")
            })
            .expect("the surface")
    }

    /// The windowed answer: pages of 4 over the 10 backing children, no
    /// duplicate fetch for satisfied windows, and the host's store ends
    /// with exactly the 10 children under the branch.
    #[test]
    fn paged_lazy_branch_answers_windows_without_duplicates() {
        let host = Arc::new(FakeWireHost::new());
        let sender: Arc<dyn PluginRequestSender> =
            Arc::clone(&host) as Arc<dyn PluginRequestSender>;
        let mut state = DemoState::default();

        // Arm the branch (one Update patch over the base surface).
        assert!(run_tree_lazy_demo(&sender, &mut state));
        let branch = host
            .widgets()
            .expect("surface")
            .roots
            .iter()
            .find_map(|root| match root {
                WidgetNode::Group(group) => group.children.iter().find_map(|node| match node {
                    WidgetNode::Tree(tree) if tree.id.0 == TREE_WIDGET_ID => {
                        tree.rows.iter().find(|row| row.id.0 == PAGED_BRANCH_ROW).cloned()
                    }
                    _ => None,
                }),
                _ => None,
            })
            .expect("the branch is live");
        assert!(branch.lazy_children && branch.expandable);
        assert_eq!(branch.parent, None, "no static children: the row is a root");
        assert_eq!(
            host.widgets()
                .expect("surface")
                .roots
                .iter()
                .filter_map(|root| match root {
                    WidgetNode::Group(group) => Some(group.children.iter().filter_map(|node| match node {
                        WidgetNode::Tree(tree) if tree.id.0 == TREE_WIDGET_ID => Some(tree.rows.iter().filter(|row| row.parent == Some(StableId(PAGED_BRANCH_ROW))).count()),
                        _ => None,
                    })),
                    _ => None,
                })
                .flatten()
                .sum::<usize>(),
            0,
            "the branch ships NO static children"
        );

        // Idempotent: a second run sends no frame.
        let served = host.served().len();
        assert!(run_tree_lazy_demo(&sender, &mut state));
        assert_eq!(host.served().len(), served);

        // The host asks window by window; every answer is ONE bounded
        // snapshot cycle of fresh rows.
        let surface_id = state.widgets_surface_id().expect("published");
        let ask = |state: &mut DemoState, start: usize, len: usize| {
            state.handle_events(
                surface_id,
                vec![WidgetEvent::ChildrenRequested {
                    surface_id,
                    parent_id: StableId(PAGED_BRANCH_ROW),
                    range: WidgetRange { start, len },
                }],
            );
        };
        ask(&mut state, 0, 8);
        assert_eq!(crate::widgets_table::deliver_pending(&sender, &mut state), 1);
        assert_eq!(branch_row_count(&state), 6 + PAGED_PAGE_LEN, "rows 140..=143");

        ask(&mut state, 4, 8);
        assert_eq!(crate::widgets_table::deliver_pending(&sender, &mut state), 1);
        assert_eq!(branch_row_count(&state), 6 + 2 * PAGED_PAGE_LEN, "rows 144..=147");

        // A REPEAT ask for a satisfied window: no fresh rows → no publish.
        let served = host.served().len();
        ask(&mut state, 0, 4);
        assert_eq!(crate::widgets_table::deliver_pending(&sender, &mut state), 0);
        assert_eq!(host.served().len(), served, "no duplicate fetch");
        assert_eq!(branch_row_count(&state), 6 + 2 * PAGED_PAGE_LEN);

        // The last page: rows 148..=149.
        ask(&mut state, 8, 8);
        assert_eq!(crate::widgets_table::deliver_pending(&sender, &mut state), 1);
        assert_eq!(branch_row_count(&state), 6 + PAGED_CHILDREN_COUNT);

        // The host's store now holds all ten children under the branch,
        // and a read answers the full set with the honest total.
        let page = send_store_read(
            &sender,
            PANEL_ID,
            PanelStoreKind::TreeChildren,
            Some(StableId(PAGED_BRANCH_ROW)),
            WidgetRange { start: 0, len: 32 },
        )
        .expect("a page answered");
        assert_eq!(page.rows.len(), PAGED_CHILDREN_COUNT);
        assert_eq!(page.total_hint, PAGED_CHILDREN_COUNT as u64);
        // Parent-before-child held for every appended page.
        for row in page.rows {
            let ocs_plugin_api::ipc::protocol::StoreRow::Tree(tree_row) = row else {
                panic!("tree children page");
            };
            assert_eq!(tree_row.parent, Some(StableId(PAGED_BRANCH_ROW)));
        }
    }

    /// The arm patch round-trips serde exactly and a refused arm leaves
    /// the surface exactly as it was.
    #[test]
    fn arm_patch_shape_and_refusal_rollback() {
        use ocs_plugin_api::ipc::protocol::PatchRejectCode;

        // The patch is one Update keyed by the tree's own id.
        let surface = widgets_table::demo_widget_surface(1).unwrap();
        let patch = {
            let mut tree = surface
                .roots
                .iter()
                .find_map(|root| match root {
                    WidgetNode::Group(group) => group.children.iter().find_map(|node| match node {
                        WidgetNode::Tree(tree) if tree.id.0 == TREE_WIDGET_ID => Some(tree.clone()),
                        _ => None,
                    }),
                    _ => None,
                })
                .unwrap();
            tree.rows.push(paged_branch_row());
            WidgetSurfacePatch {
                base_revision: ocs_plugin_api::scene::SceneRevision(1),
                revision: ocs_plugin_api::scene::SceneRevision(2),
                ops: vec![WidgetOp::Update {
                    id: StableId(TREE_WIDGET_ID),
                    content: WidgetNode::Tree(tree),
                }],
            }
        };
        let round: WidgetSurfacePatch =
            bincode::deserialize(&bincode::serialize(&patch).unwrap()).unwrap();
        assert_eq!(round, patch);

        // THE refusal: a wedged claim frontier → the arm refuses, the
        // model keeps its rows (no branch), the report realigns. The seed
        // is the base surface ONLY (no branch yet — the arm is what the
        // wedged run attempts).
        let host = Arc::new(FakeWireHost::new());
        let sender: Arc<dyn PluginRequestSender> =
            Arc::clone(&host) as Arc<dyn PluginRequestSender>;
        let mut state = DemoState::default();
        assert!(crate::widgets_table::publish_widget_base(&sender, &mut state));
        // Wedge: rewind the frontier below the held widget leg.
        state.head = 0;
        assert!(!run_tree_lazy_demo(&sender, &mut state));
        assert_eq!(
            state.last_reject,
            Some((PatchRejectCode::NonMonotonicRevision, state.widgets_head())),
            "the structured refusal named the widget leg's head"
        );
        assert_eq!(state.head, state.widgets_head().expect("leg").0, "realigned");
    }
}
