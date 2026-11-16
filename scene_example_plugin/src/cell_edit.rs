//! THE GUI cell-edit closed loop (the headline): the user edits a cell IN
//! THE HOST (two-phase pointer click, or keyboard — Tab dives in, arrows
//! move the cell cursor, Enter commits, Escape cancels the draft), the
//! host crate-validates the commit and forwards ONE
//! `WidgetEvent::TableCellChanged` to the owning plugin; the plugin then
//!
//! 1. **validates** the fact against its OWN model (the row key must be
//!    inside the published window, the column's kind must match the
//!    skeleton value, and the demo's law: lengths are finite and
//!    non-negative, item text is non-empty, badge cells are read-only);
//! 2. **acks** with ONE [`WidgetOp::TableCell`] patch — the patch = data
//!    leg of the documented events-vs-patches split (the event is the
//!    notify leg; the typed write rides `TableCell` op data);
//! 3. mirrors the applied patch into its model — so model and host agree
//!    cell for cell, and a REFUSED ack leaves everything exactly as it
//!    was (the model is never touched before the wire answers; a rejected
//!    frame restores the queued acks and realigns the claim frontier).
//!
//! The loop prints legibly through the state's interaction journal (the
//! `SCN_DEBUG` dump quotes it) and its tallies: accepted / refused edits,
//! ack patch frames.
//!
//! A deliberately invalid value (a negative length, an unknown row key, a
//! badge write) is refused WITHOUT partial writes: nothing is queued,
//! nothing is sent, the model stands.

use std::sync::Arc;

use ocs_plugin_api::host::PluginRequestSender;
use ocs_plugin_api::scene::StableId;
use ocs_plugin_api::widgets::{
    CellKind, WidgetCellValue, WidgetOp, WidgetSurfacePatch,
};

use crate::host_wire;
use crate::scene_graph::PANEL_ID;
use crate::state::{DemoState, FrameOutcome};
use crate::widgets_table::{self, TABLE_ID};

/// The demo's validation ceiling for `length` cells (finite and
/// non-negative is the law; the ceiling keeps a typo from writing 1e300).
pub const MAX_LENGTH_VALUE: f64 = 10_000.0;

/// ONE validated cell edit awaiting its ack patch.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CellAck {
    pub row_key: StableId,
    pub column: StableId,
    pub value: WidgetCellValue,
}

/// The validation law, applied to ONE `TableCellChanged` fact against the
/// plugin's own model. `Ok(note)` names the accepted fact (the journal's
/// material); `Err(reason)` names the refusal.
fn validate(
    state: &DemoState,
    row_key: StableId,
    column_id: StableId,
    value: &WidgetCellValue,
) -> Result<String, String> {
    let Some(surface) = state.widgets.as_ref() else {
        return Err("no widget surface is published".to_string());
    };
    let Some(def) = surface.tables.get(&StableId(TABLE_ID)) else {
        return Err(format!("table {} is not published", TABLE_ID));
    };
    let Some(_row) = def.rows.iter().find(|row| row.key == row_key) else {
        return Err(format!(
            "row key {} is outside the published window (keys {}..={})",
            row_key.0,
            def.rows.first().map(|row| row.key.0).unwrap_or(0),
            def.rows.last().map(|row| row.key.0).unwrap_or(0)
        ));
    };
    let Some(column) = def.columns.iter().find(|column| column.id == column_id) else {
        return Err(format!("column {} is not in the registry", column_id.0));
    };
    // The cell's kind must match the skeleton value, and the demo's
    // per-column law applies.
    match (column.kind, value) {
        (CellKind::Number, WidgetCellValue::Number(number)) => {
            if !number.is_finite() || *number < 0.0 {
                return Err(format!(
                    "row {} length {number} is not a finite non-negative number",
                    row_key.0
                ));
            }
            if *number > MAX_LENGTH_VALUE {
                return Err(format!(
                    "row {} length {number} passes the ceiling {MAX_LENGTH_VALUE}",
                    row_key.0
                ));
            }
            Ok(format!(
                "row {} column {} = {number} (length)",
                row_key.0, column_id.0
            ))
        }
        (CellKind::Text, WidgetCellValue::Text(text)) => {
            if text.trim().is_empty() {
                return Err(format!(
                    "row {} item text is empty",
                    row_key.0
                ));
            }
            Ok(format!(
                "row {} column {} = {text:?} (item)",
                row_key.0, column_id.0
            ))
        }
        (CellKind::Bool, WidgetCellValue::Bool(flag)) => Ok(format!(
            "row {} column {} = {flag} (locked)",
            row_key.0, column_id.0
        )),
        (CellKind::Badge, _) => Err(format!(
            "column {} is a badge column — read-only to edits",
            column_id.0
        )),
        (kind, value) => Err(format!(
            "row {} column {}: the {kind:?} column cannot take {value:?}",
            row_key.0, column_id.0
        )),
    }
}

/// The host's GUI commit arrived (`TableCellChanged`): validate against
/// the model; an accepted fact queues its ack patch op, a refused fact
/// leaves everything exactly as it was (no partial writes).
pub(crate) fn note_cell_changed(
    state: &mut DemoState,
    row_key: StableId,
    column_id: StableId,
    value: WidgetCellValue,
) {
    match validate(state, row_key, column_id, &value) {
        Ok(note) => {
            state.cell_edits_accepted += 1;
            state.pending_cell_acks.push(CellAck {
                row_key,
                column: column_id,
                value,
            });
            state.journal_note(format!(
                "cell edit accepted: {note} — ack patch queued (SCN_DEMO_CELLEDIT \
                 flushes)"
            ));
        }
        Err(reason) => {
            state.cell_edits_refused += 1;
            state.journal_note(format!(
                "cell edit refused: {reason} — the model is untouched"
            ));
        }
    }
}

/// The cell-edit feature's flow: ensure the widget leg exists, then flush
/// the queued acks as ONE bounded cell patch (the op count stays under
/// the crate's patch budget; the surplus stays queued for the next run).
/// Every refusal law applies: the model is mirrored ONLY on an applied
/// frame; a rejected frame restores the acks and realigns the frontier.
pub fn run_cell_edit_demo(
    sender: &Arc<dyn PluginRequestSender>,
    state: &mut DemoState,
) -> bool {
    // Ensure-base: a fresh fixture publish REPLACES the model, so it only
    // runs when the model holds no surface.
    if state.widgets.is_none() && !widgets_table::publish_widget_base(sender, state) {
        return false;
    }
    if state.pending_cell_acks.is_empty() {
        return true;
    }
    let Some(base) = state.widgets_head() else {
        return false;
    };
    // ONE bounded patch: take at most MAX_PATCH_OPS acks; the rest stay
    // queued (no payload dumps, the crate's budgets respected).
    let take = state.pending_cell_acks.len().min(ocs_plugin_api::scene::MAX_PATCH_OPS);
    let acks: Vec<CellAck> = state.pending_cell_acks.drain(..take).collect();
    let ops: Vec<WidgetOp> = acks
        .iter()
        .map(|ack| WidgetOp::TableCell {
            table: StableId(TABLE_ID),
            key: ack.row_key,
            column: ack.column,
            value: ack.value.clone(),
        })
        .collect();
    let successor = state.next_revision();
    let patch = WidgetSurfacePatch {
        base_revision: base,
        revision: successor,
        ops,
    };
    let base_revision = patch.base_revision.0;
    let claimed = patch.revision.0;
    let ack_count = acks.len();
    state.sent += 1;
    match host_wire::send_widget_patch(sender, PANEL_ID, patch.clone()) {
        host_wire::WireOutcome::Applied { revision, .. } => {
            state.applied += 1;
            state.cell_patch_frames += 1;
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
                "cell-edit loop closed: host commit → validated → model applied → \
                 ONE cell patch acked {} op(s) at revision {}",
                ack_count, claimed
            ));
            true
        }
        host_wire::WireOutcome::Rejected { code, report } => {
            // The acknowledgment only rode an applied frame: the acks are
            // owed again, front-first (arrival order preserved).
            let mut restored = acks;
            restored.extend(state.pending_cell_acks.drain(..));
            state.pending_cell_acks = restored;
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
            let mut restored = acks;
            restored.extend(state.pending_cell_acks.drain(..));
            state.pending_cell_acks = restored;
            state.note_frame("widget patch", Some(base_revision), Some(claimed), FrameOutcome::Unserviceable(message));
            false
        }
        host_wire::WireOutcome::Failed(error) => {
            let mut restored = acks;
            restored.extend(state.pending_cell_acks.drain(..));
            state.pending_cell_acks = restored;
            state.note_frame("widget patch", Some(base_revision), Some(claimed), FrameOutcome::SendFailed(error));
            false
        }
        host_wire::WireOutcome::Unexpected => {
            let mut restored = acks;
            restored.extend(state.pending_cell_acks.drain(..));
            state.pending_cell_acks = restored;
            state.note_frame("widget patch", Some(base_revision), Some(claimed), FrameOutcome::Unexpected);
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use ocs_plugin_api::host::PluginRequestError;
    use ocs_plugin_api::ipc::protocol::{PatchRejectCode, PluginRequest, PluginResponse};
    use ocs_plugin_api::widgets::{TableCell, WidgetEvent};

    use crate::fake_host::FakeWireHost;
    use crate::state::DemoState;
    use crate::widgets_table::{COL_ITEM, COL_LENGTH, COL_LOCKED, COL_STATE};

    use super::*;

    /// The scripted coded-reject double (every mutating frame refused
    /// `StaleBase` naming `head`).
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
                    current_revision: Some(ocs_plugin_api::scene::SceneRevision(self.head)),
                    failed_op: None,
                },
            })
        }
    }

    /// Read a cell straight out of the model's registry.
    fn model_cell(state: &DemoState, row_key: u64, column: u64) -> TableCell {
        state
            .widgets
            .as_ref()
            .expect("surface")
            .tables
            .get(&StableId(TABLE_ID))
            .expect("table")
            .rows
            .iter()
            .find(|row| row.key.0 == row_key)
            .expect("row")
            .cells[match column {
                COL_ITEM => 0,
                COL_LENGTH => 1,
                COL_LOCKED => 2,
                COL_STATE => 3,
                _ => panic!("column"),
            }]
            .clone()
    }

    /// THE closed loop, headless: the host commits a cell edit (the
    /// plugin receives `TableCellChanged`), the plugin validates, applies
    /// to its model and acks with ONE cell patch; model and host agree.
    #[test]
    fn gui_cell_edit_closed_loop_headless() {
        let host = Arc::new(FakeWireHost::new());
        let sender: Arc<dyn PluginRequestSender> =
            Arc::clone(&host) as Arc<dyn PluginRequestSender>;
        let mut state = DemoState::default();
        assert!(widgets_table::publish_widget_base(&sender, &mut state));

        // The host's GUI commit: the user typed 3.5 into row 306's length.
        let surface_id = state.widgets_surface_id().expect("published");
        state.handle_events(
            surface_id,
            vec![WidgetEvent::TableCellChanged {
                surface_id,
                row_key: StableId(306),
                column_id: StableId(COL_LENGTH),
                value: WidgetCellValue::Number(3.5),
            }],
        );
        assert_eq!(state.cell_edits_accepted, 1);
        assert_eq!(state.pending_cell_acks.len(), 1);
        assert!(
            state
                .journal
                .iter()
                .any(|line| line.contains("cell edit accepted")),
            "the accept is legible: {:?}",
            state.journal
        );
        // The MODEL is untouched until the ack patch applies (the loop's
        // rollback discipline: no pre-writes).
        assert_eq!(
            model_cell(&state, 306, COL_LENGTH),
            TableCell::Number(3.0),
            "the model still holds the backing value"
        );

        // The flush: ONE cell patch acks the edit; model and host agree.
        let served = host.served().len();
        assert!(run_cell_edit_demo(&sender, &mut state));
        assert_eq!(host.served().len() - served, 1, "exactly one frame");
        assert_eq!(host.served().last().copied(), Some("PanelScenePatch"));
        assert_eq!(state.cell_patch_frames, 1);
        assert!(state.pending_cell_acks.is_empty(), "the ack drained");
        assert_eq!(
            model_cell(&state, 306, COL_LENGTH),
            TableCell::Number(3.5),
            "the model adopted the edit"
        );
        let def = &host.widgets().expect("published").tables[&StableId(TABLE_ID)];
        assert_eq!(
            def.rows.iter().find(|row| row.key.0 == 306).expect("row").cells[1],
            TableCell::Number(3.5),
            "the host holds the acked edit"
        );
        assert!(
            state
                .journal
                .iter()
                .any(|line| line.contains("cell-edit loop closed")),
            "the loop prints legibly: {:?}",
            state.journal
        );

        // A second edit with nothing to ack: no frame, applied=true.
        let served = host.served().len();
        assert!(run_cell_edit_demo(&sender, &mut state));
        assert_eq!(host.served().len(), served, "no frame without acks");
    }

    /// Invalid values are refused WITHOUT partial writes: a negative
    /// length, an unknown row key, a badge write, and a kind mismatch all
    /// refuse — nothing queued, nothing sent, the model untouched.
    #[test]
    fn invalid_cell_edit_refused_without_partial_writes() {
        let host = Arc::new(FakeWireHost::new());
        let sender: Arc<dyn PluginRequestSender> =
            Arc::clone(&host) as Arc<dyn PluginRequestSender>;
        let mut state = DemoState::default();
        assert!(widgets_table::publish_widget_base(&sender, &mut state));
        let surface_id = state.widgets_surface_id().expect("published");
        let served = host.served().len();
        let model_before = state.widgets.clone();

        state.handle_events(
            surface_id,
            vec![
                // A negative length — the demo's law refuses it.
                WidgetEvent::TableCellChanged {
                    surface_id,
                    row_key: StableId(305),
                    column_id: StableId(COL_LENGTH),
                    value: WidgetCellValue::Number(-1.0),
                },
                // A row key outside the published window (keys 300..=307).
                WidgetEvent::TableCellChanged {
                    surface_id,
                    row_key: StableId(340),
                    column_id: StableId(COL_LENGTH),
                    value: WidgetCellValue::Number(1.0),
                },
                // The badge column is read-only.
                WidgetEvent::TableCellChanged {
                    surface_id,
                    row_key: StableId(305),
                    column_id: StableId(COL_STATE),
                    value: WidgetCellValue::Text("locked".to_string()),
                },
                // A kind mismatch: text into the number column.
                WidgetEvent::TableCellChanged {
                    surface_id,
                    row_key: StableId(305),
                    column_id: StableId(COL_LENGTH),
                    value: WidgetCellValue::Text("wide".to_string()),
                },
            ],
        );
        assert_eq!(state.cell_edits_refused, 4);
        assert_eq!(state.cell_edits_accepted, 0);
        assert!(state.pending_cell_acks.is_empty(), "nothing queued");
        assert_eq!(host.served().len(), served, "nothing sent");
        assert_eq!(state.widgets, model_before, "the model is untouched");
        assert!(
            state
                .journal
                .iter()
                .any(|line| line.contains("cell edit refused: row 305 length -1")),
            "the refusal names its reason: {:?}",
            state.journal
        );
    }

    /// A REFUSED ack frame leaves everything exactly as it was: the model
    /// keeps its cells, the acks are owed again, and the reject's report
    /// head realigns the frontier so the next run's retry applies
    /// cleanly.
    #[test]
    fn cell_edit_refused_frame_restores_state() {
        let host = Arc::new(FakeWireHost::new());
        let sender: Arc<dyn PluginRequestSender> =
            Arc::clone(&host) as Arc<dyn PluginRequestSender>;
        let mut state = DemoState::default();
        assert!(widgets_table::publish_widget_base(&sender, &mut state));

        // One valid edit accepted and queued.
        let surface_id = state.widgets_surface_id().expect("published");
        state.handle_events(
            surface_id,
            vec![WidgetEvent::TableCellChanged {
                surface_id,
                row_key: StableId(305),
                column_id: StableId(COL_LENGTH),
                value: WidgetCellValue::Number(7.25),
            }],
        );
        let model_before = state.widgets.clone();

        // THE WEDGE: the frontier rewound below the held widget leg.
        state.head = 0;
        assert!(!run_cell_edit_demo(&sender, &mut state));
        // (a) the model never adopted the edit;
        assert_eq!(state.widgets, model_before, "zero mutation");
        // (b) the ack is owed again;
        assert_eq!(state.pending_cell_acks.len(), 1);
        // (c) the structured reject is recorded with its recovery fact;
        assert_eq!(
            state.last_reject,
            Some((PatchRejectCode::NonMonotonicRevision, model_before.as_ref().map(|surface| surface.revision)))
        );
        // (d) THE HEAL: the retry from the realigned frontier applies.
        assert!(run_cell_edit_demo(&sender, &mut state));
        assert_eq!(
            model_cell(&state, 305, COL_LENGTH),
            TableCell::Number(7.25),
            "the retry adopted the edit"
        );
        let def = &host.widgets().expect("published").tables[&StableId(TABLE_ID)];
        assert_eq!(
            def.rows.iter().find(|row| row.key.0 == 305).expect("row").cells[1],
            TableCell::Number(7.25)
        );
        assert_eq!(state.cell_patch_frames, 1);
        assert!(state.pending_cell_acks.is_empty());
    }

    /// Two accepted edits ack as ONE patch with TWO ops (bounded, minimal
    /// — one op per cell, never a table dump).
    #[test]
    fn two_edits_ack_as_one_two_op_patch() {
        let host = Arc::new(FakeWireHost::new());
        let sender: Arc<dyn PluginRequestSender> =
            Arc::clone(&host) as Arc<dyn PluginRequestSender>;
        let mut state = DemoState::default();
        assert!(widgets_table::publish_widget_base(&sender, &mut state));
        let surface_id = state.widgets_surface_id().expect("published");
        state.handle_events(
            surface_id,
            vec![
                WidgetEvent::TableCellChanged {
                    surface_id,
                    row_key: StableId(300),
                    column_id: StableId(COL_LENGTH),
                    value: WidgetCellValue::Number(0.5),
                },
                WidgetEvent::TableCellChanged {
                    surface_id,
                    row_key: StableId(301),
                    column_id: StableId(COL_ITEM),
                    value: WidgetCellValue::Text("renamed".to_string()),
                },
            ],
        );
        assert!(run_cell_edit_demo(&sender, &mut state));
        assert_eq!(state.cell_edits_accepted, 2);
        assert_eq!(state.cell_patch_frames, 1);
        assert_eq!(
            model_cell(&state, 300, COL_LENGTH),
            TableCell::Number(0.5)
        );
        assert_eq!(
            model_cell(&state, 301, COL_ITEM),
            TableCell::Text("renamed".to_string())
        );
        let def = &host.widgets().expect("published").tables[&StableId(TABLE_ID)];
        assert_eq!(def.rows[0].cells[1], TableCell::Number(0.5));
        assert_eq!(def.rows[1].cells[0], TableCell::Text("renamed".to_string()));
        // Neighbouring cells stayed exactly as published.
        assert_eq!(def.rows[0].cells[0], TableCell::Text("material 000".to_string()));
    }
}
