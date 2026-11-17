//! The lazy-children tree demo: an expandable branch with NO static
//! children (`expandable: true` + `lazy_children: true` — the documented
//! pair `validate()` demands) patched onto the EXISTING demo tree. When
//! the host expands it, the `ChildrenRequested` ask is answered with the
//! NEXT BOUNDED PAGE — never the whole backing set, and never a
//! re-answer for an already-satisfied window (while the host state the
//! plugin last saw still holds the rows — the dedup reconciles after a
//! fresh host drawing).
//!
//! The base surface's first lazy branch (row 12, children 130..=132) is
//! small enough to answer in one page; THIS branch (row 13) carries TEN
//! backing children so the windowed answer actually pages: every ask is
//! answered with at most the asked window's fresh rows (4 at the demo's
//! page width), appended parent-before-child into the model and published
//! through the documented snapshot cycle ([`crate::widgets_table::
//! deliver_pending`]).
//!
//! # The per-branch answer-strategy table
//!
//! EVERY lazy branch the model knows is answered — [`strategy_for`] maps
//! each known branch row to its answer strategy (whole-set vs paged), so
//! the demand path refuses only addresses the model genuinely does not
//! know (and the refusal names the branch per the journal grammar).
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

/// The answer strategy one known lazy branch uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BranchStrategy {
    /// The branch's whole small backing set in ONE bounded answer (the
    /// base surface's branch 12 — three rows).
    WholeSet,
    /// The asked window, at most [`PAGED_PAGE_LEN`] fresh rows per answer
    /// (branch 13's ten backing children actually page).
    Paged,
}

/// The per-branch answer-strategy table: EVERY lazy branch the model
/// knows. An unlisted branch is not answered — the caller journals the
/// named refusal (the model never guesses children it does not own).
pub(crate) fn strategy_for(parent: u64) -> Option<BranchStrategy> {
    match parent {
        crate::widgets_table::LAZY_BRANCH_ROW => Some(BranchStrategy::WholeSet),
        PAGED_BRANCH_ROW => Some(BranchStrategy::Paged),
        _ => None,
    }
}

/// The outcome of ONE children ask against the model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChildrenAnswer {
    /// Fresh rows were appended into the model (parent-before-child held)
    /// — the caller publishes the window.
    Delivered {
        /// The first appended row's stable id.
        first: u64,
        /// The last appended row's stable id.
        last: u64,
    },
    /// The model already holds the asked rows — an HONEST no-op ONLY while
    /// the host's current surface still holds them; the caller reconciles
    /// the dedup against the host state the plugin last saw.
    ModelSatisfied,
    /// The model names no lazy branch at this address — the caller
    /// journals the named refusal and sends nothing.
    UnknownBranch,
}

/// The strategy-table answer to ONE children ask: the fresh (never yet
/// published) backing rows for the branch's strategy — whole-set or the
/// asked window (bounded by the page width, clipped to the backing set) —
/// appended parent-before-child into the model's tree.
pub(crate) fn answer_branch_children(
    state: &mut DemoState,
    parent: StableId,
    range: WidgetRange,
) -> ChildrenAnswer {
    let Some(strategy) = strategy_for(parent.0) else {
        return ChildrenAnswer::UnknownBranch;
    };
    let Some(surface) = state.widgets.as_mut() else {
        return ChildrenAnswer::UnknownBranch;
    };
    let Some(tree) = widgets_table::find_tree_mut(surface) else {
        return ChildrenAnswer::UnknownBranch;
    };
    // Parent-before-child: the branch row must already be published (the
    // host can only ask for children of a row it has seen).
    if !tree.rows.iter().any(|row| row.id.0 == parent.0) {
        return ChildrenAnswer::UnknownBranch;
    }
    let published: std::collections::HashSet<u64> =
        tree.rows.iter().map(|row| row.id.0).collect();
    // The branch's fresh backing rows under its strategy.
    let fresh: Vec<TreeRowDef> = match strategy {
        BranchStrategy::WholeSet => widgets_table::backing_tree_children(parent)
            .into_iter()
            .filter(|child| !published.contains(&child.id.0))
            .collect(),
        BranchStrategy::Paged => {
            let start = range.start.min(PAGED_CHILDREN_COUNT);
            let end = range
                .start
                .saturating_add(range.len)
                .min(PAGED_CHILDREN_COUNT)
                .max(start);
            (start..end)
                .map(paged_child)
                .filter(|child| !published.contains(&child.id.0))
                .take(PAGED_PAGE_LEN)
                .collect()
        }
    };
    if fresh.is_empty() {
        return ChildrenAnswer::ModelSatisfied;
    }
    let first = fresh.first().map(|row| row.id.0).unwrap_or(0);
    let last = fresh.last().map(|row| row.id.0).unwrap_or(0);
    tree.rows.extend(fresh);
    ChildrenAnswer::Delivered { first, last }
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
        // (The dedup is keyed on the HOST state the plugin last saw, so the
        // check costs ONE bounded PanelStoreRead — the no-duplicate-PUBLISH
        // law is what the count pins: exactly one extra frame, the read.)
        let served = host.served().len();
        ask(&mut state, 0, 4);
        assert_eq!(crate::widgets_table::deliver_pending(&sender, &mut state), 0);
        assert_eq!(host.served().len(), served + 1, "no duplicate publish — the one extra frame is the dedup's host read");
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

    /// A helper: the count of rows under `parent` in the addressed HOST's
    /// store (the bounded read the tests take directly).
    fn host_children(sender: &Arc<dyn PluginRequestSender>, parent: u64) -> usize {
        host_wire::send_store_read(
            sender,
            PANEL_ID,
            PanelStoreKind::TreeChildren,
            Some(StableId(parent)),
            WidgetRange { start: 0, len: 32 },
        )
        .expect("a page answered")
        .rows
        .len()
    }

    /// Branch 12 ("Surfaces (lazy)") IS answered — its whole small backing
    /// set (130..=132) in ONE bounded answer, rows in the model AND
    /// published; a repeat ask on the SAME host state stays deduped; an
    /// unknown branch refuses BY NAME (the journal grammar).
    #[test]
    fn branch_12_answered_whole_set_unknown_branch_refused_by_name() {
        let host = Arc::new(FakeWireHost::new());
        let sender: Arc<dyn PluginRequestSender> =
            Arc::clone(&host) as Arc<dyn PluginRequestSender>;
        let mut state = DemoState::default();
        assert!(crate::widgets_table::publish_widget_base(&sender, &mut state));

        // The ask for branch 12 rides the ordinary delivery path.
        state.on_store_demand(
            Some(StableId(crate::widgets_table::LAZY_BRANCH_ROW)),
            WidgetRange { start: 0, len: 8 },
        );
        assert_eq!(crate::widgets_table::deliver_pending(&sender, &mut state), 1);
        // The rows are in the model AND in the host's store.
        assert!(crate::widgets_table::tree_holds_row(
            &state,
            crate::widgets_table::LAZY_CHILDREN_BASE
        ));
        assert_eq!(
            host_children(&sender, crate::widgets_table::LAZY_BRANCH_ROW),
            crate::widgets_table::LAZY_CHILDREN_COUNT
        );
        // The panel narrates the page.
        assert_eq!(
            state.status_line.as_deref(),
            Some("lazy page 1 delivered (rows 130..=132)")
        );

        // A repeat ask on the SAME host state: answered-once — no publish
        // (the dedup's host read is the one extra frame).
        let served = host.served().len();
        state.on_store_demand(
            Some(StableId(crate::widgets_table::LAZY_BRANCH_ROW)),
            WidgetRange { start: 0, len: 8 },
        );
        assert_eq!(crate::widgets_table::deliver_pending(&sender, &mut state), 0);
        assert_eq!(host.served().len(), served + 1, "no duplicate publish");

        // An UNKNOWN branch refuses BY NAME: nothing is sent at all (no
        // read, no publish) and the journal names the address.
        let served = host.served().len();
        state.on_store_demand(Some(StableId(1)), WidgetRange { start: 0, len: 8 });
        assert_eq!(crate::widgets_table::deliver_pending(&sender, &mut state), 0);
        assert_eq!(host.served().len(), served, "a refusal sends nothing");
        assert!(
            state
                .journal
                .iter()
                .any(|line| line.contains("no lazy branch at row 1")),
            "the refusal names the branch: {:?}",
            state.journal
        );
    }

    /// THE DEDUP RECONCILIATION (the fresh-drawing law): the answered-
    /// window dedup is keyed on the HOST state the plugin last saw, not on
    /// a durable answered-once ledger. On a fresh host store (a new
    /// drawing — the store emptied under the carried plugin state) the
    /// repeat ask IS re-answered: the model's rows republish through the
    /// ordinary snapshot cycle and the fresh host holds them. The true
    /// dedup stays pinned: once the fresh host holds the rows, the same
    /// ask answers nothing again.
    #[test]
    fn dedup_reconciles_after_a_fresh_host_drawing() {
        // Host A: arm the paged branch, answer two windows.
        let host_a = Arc::new(FakeWireHost::new());
        let sender_a: Arc<dyn PluginRequestSender> =
            Arc::clone(&host_a) as Arc<dyn PluginRequestSender>;
        let mut state = DemoState::default();
        assert!(crate::widgets_table::publish_widget_base(&sender_a, &mut state));
        assert!(run_tree_lazy_demo(&sender_a, &mut state));
        let surface_id = state.widgets_surface_id().expect("published");
        for start in [0usize, 4] {
            state.handle_events(
                surface_id,
                vec![WidgetEvent::ChildrenRequested {
                    surface_id,
                    parent_id: StableId(PAGED_BRANCH_ROW),
                    range: WidgetRange { start, len: 8 },
                }],
            );
            assert_eq!(crate::widgets_table::deliver_pending(&sender_a, &mut state), 1);
        }
        assert_eq!(host_children(&sender_a, PAGED_BRANCH_ROW), 8);

        // THE FRESH DRAWING: a brand-new host store; the plugin state
        // (model, cadence, journal) is carried over verbatim.
        let host_b = Arc::new(FakeWireHost::new());
        let sender_b: Arc<dyn PluginRequestSender> =
            Arc::clone(&host_b) as Arc<dyn PluginRequestSender>;
        assert_eq!(host_children(&sender_b, PAGED_BRANCH_ROW), 0);

        // The repeat ask on the fresh host: RE-ANSWERED.
        state.handle_events(
            surface_id,
            vec![WidgetEvent::ChildrenRequested {
                surface_id,
                parent_id: StableId(PAGED_BRANCH_ROW),
                range: WidgetRange { start: 0, len: 4 },
            }],
        );
        assert_eq!(
            crate::widgets_table::deliver_pending(&sender_b, &mut state),
            1,
            "a fresh drawing re-answers the ask"
        );
        assert!(
            state
                .journal
                .iter()
                .any(|line| line.contains("dedup reconciled")),
            "the reconciliation is legible: {:?}",
            state.journal
        );
        // The fresh host holds the branch rows again (the snapshot cycle
        // regained the registry), parent-before-child intact.
        let page = host_wire::send_store_read(
            &sender_b,
            PANEL_ID,
            PanelStoreKind::TreeChildren,
            Some(StableId(PAGED_BRANCH_ROW)),
            WidgetRange { start: 0, len: 32 },
        )
        .expect("a page answered");
        assert_eq!(page.rows.len(), 8);
        for row in &page.rows {
            let ocs_plugin_api::ipc::protocol::StoreRow::Tree(tree_row) = row else {
                panic!("tree children page");
            };
            assert_eq!(tree_row.parent, Some(StableId(PAGED_BRANCH_ROW)));
        }

        // THE TRUE DEDUP, pinned on the fresh host too: the same ask now
        // answers nothing (the host state the plugin saw holds the rows).
        let served = host_b.served().len();
        state.handle_events(
            surface_id,
            vec![WidgetEvent::ChildrenRequested {
                surface_id,
                parent_id: StableId(PAGED_BRANCH_ROW),
                range: WidgetRange { start: 0, len: 4 },
            }],
        );
        assert_eq!(crate::widgets_table::deliver_pending(&sender_b, &mut state), 0);
        assert_eq!(host_b.served().len(), served + 1, "one read, no publish");
    }
}
