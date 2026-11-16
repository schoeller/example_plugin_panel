//! The owner-law demo: the FIRST publisher owns a panel's v24 surface
//! store (ownership is transport-authenticated — the panel id in a frame
//! is a name, not a credential), and publishing NEVER transfers it. A
//! SECOND publisher's publish of the live panel id is refused
//! **`PublisherOwnedByOther`** — structured, bounded, zero mutation: the
//! owning plugin keeps the publish/patch contract.
//!
//! The demo does NOT fight the real host: this plugin IS the owner, so
//! its own probe ([`run_owner_probe`]) is an ordinary snapshot-cycle
//! publish of the model — the exact frame a second publisher would send.
//! The REFUSAL half is proven headless against a wire double that mirrors
//! the multi-plugin host verbatim ([`OwnedByOtherHost`], in the tests):
//! the panel's store already attached to a different publisher, so every
//! mutating frame answers the structured code with the host's head as the
//! recovery fact — and the plugin's refusal laws do the rest (the ack leg
//! is restored, the revision claim rolls back, the report's head raises
//! the claim frontier above the store's truth).

use std::sync::Arc;

use ocs_plugin_api::host::PluginRequestSender;

use crate::state::DemoState;
use crate::widgets_table;

/// The owner-law feature's flow: ensure the widget leg exists (the probe
/// publishes the model, never a fresh fixture over live state), then ONE
/// snapshot-cycle publish of the CURRENT model — on the live host this
/// applies (we are the owner); against a second-publisher wire it refuses
/// `PublisherOwnedByOther` and the refusal laws restore everything.
pub fn run_owner_probe(sender: &Arc<dyn PluginRequestSender>, state: &mut DemoState) -> bool {
    if state.widgets.is_none() && !widgets_table::publish_widget_base(sender, state) {
        return false;
    }
    widgets_table::publish_widget_cycle(sender, state)
}

#[cfg(test)]
mod tests {
    use ocs_plugin_api::host::{PluginRequestError, PluginRequestSender};
    use ocs_plugin_api::ipc::protocol::{
        PatchRejectCode, PatchRejectReport, PluginRequest, PluginResponse,
    };
    use ocs_plugin_api::scene::SceneRevision;
    use ocs_plugin_api::widgets::WidgetEvent;

    use crate::fake_host::FakeWireHost;
    use crate::state::DemoState;
    use crate::widgets_table;

    use super::*;

    /// The multi-plugin host, mirrored: the panel's v24 store is ALREADY
    /// attached to a DIFFERENT transport-authenticated plugin (the first
    /// owner, holding the leg at `head`), so every mutating frame from
    /// THIS side answers the structured
    /// [`PatchRejectCode::PublisherOwnedByOther`] with the host's head as
    /// the recovery fact — zero mutation, exactly the host's documented
    /// arm. Reads stay servable (they are not ownership-bearing).
    struct OwnedByOtherHost {
        head: u64,
        served: std::sync::Mutex<Vec<&'static str>>,
    }

    impl OwnedByOtherHost {
        fn served(&self) -> Vec<&'static str> {
            self.served
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone()
        }
    }

    impl PluginRequestSender for OwnedByOtherHost {
        fn request(
            &self,
            req: PluginRequest,
        ) -> Result<PluginResponse, PluginRequestError> {
            match req {
                PluginRequest::PanelScenePublish { .. } => {
                    self.served
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .push("PanelScenePublish");
                    Ok(PluginResponse::PatchRejected {
                        code: PatchRejectCode::PublisherOwnedByOther,
                        report: PatchRejectReport {
                            current_revision: Some(SceneRevision(self.head)),
                            failed_op: None,
                        },
                    })
                }
                PluginRequest::PanelScenePatch { .. } => {
                    self.served
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .push("PanelScenePatch");
                    Ok(PluginResponse::PatchRejected {
                        code: PatchRejectCode::PublisherOwnedByOther,
                        report: PatchRejectReport {
                            current_revision: Some(SceneRevision(self.head)),
                            failed_op: None,
                        },
                    })
                }
                PluginRequest::PanelStoreRead { .. } | PluginRequest::PanelSceneSnapshot2 { .. } => {
                    Ok(PluginResponse::StorePage {
                        rows: Vec::new(),
                        total_hint: 0,
                    })
                }
                _ => Ok(PluginResponse::Ok),
            }
        }
    }

    /// THE owner-law demo, headless: the second publisher's publish of
    /// the live panel id refuses with the STRUCTURED code, the host's
    /// head rides the report as the recovery fact, and the plugin's
    /// refusal laws leave everything exactly as it was — the ack leg
    /// restored, the revision claim rolled back, the claim frontier
    /// realigned ABOVE the store's truth. Zero mutation on either side.
    #[test]
    fn second_publisher_refusal_is_structured_and_zero_mutation() {
        // The first owner's store: the demo surface published at head 2.
        let owner_store = Arc::new(FakeWireHost::new());
        let owner_sender: Arc<dyn PluginRequestSender> =
            Arc::clone(&owner_store) as Arc<dyn PluginRequestSender>;
        let mut state = DemoState::default();
        assert!(widgets_table::publish_widget_base(&owner_sender, &mut state));
        assert_eq!(state.widgets_head(), Some(SceneRevision(1)));

        // The live loop's pending state: one merged event batch (its
        // acknowledgment rides the NEXT publish — which is about to be
        // refused).
        let surface_id = state.widgets_surface_id().expect("published");
        state.handle_events(
            surface_id,
            vec![WidgetEvent::SelectionChanged {
                surface_id,
                selected_ids: vec![ocs_plugin_api::scene::StableId(1)],
            }],
        );
        assert_eq!(state.pending_ack.len(), 1);

        // THE SECOND PUBLISHER: the same panel id, a different
        // transport-authenticated side. The multi-plugin host mirrors:
        let mirror = Arc::new(OwnedByOtherHost {
            head: 1,
            served: std::sync::Mutex::new(Vec::new()),
        });
        let second: Arc<dyn PluginRequestSender> = Arc::clone(&mirror) as Arc<dyn PluginRequestSender>;

        // The probe publish — the exact frame a second publisher sends —
        // refuses with the structured code.
        assert!(!run_owner_probe(&second, &mut state));
        assert_eq!(
            state.last_reject,
            Some((PatchRejectCode::PublisherOwnedByOther, Some(SceneRevision(1)))),
            "the refusal names the code and the host's head (the recovery fact)"
        );
        // (a) the ack leg is owed again (the acknowledgment only rode an
        //     applied frame);
        assert_eq!(state.pending_ack.len(), 1, "the ack batch restored");
        // (b) the revision claim rolled back to the leg the store holds;
        assert_eq!(state.widgets.as_ref().expect("model kept").revision, SceneRevision(1));
        // (c) the frontier realigned above the store's truth (the burned
        //     claim is never reused — the report head is the floor);
        assert_eq!(state.head, 2);
        // (d) the first owner's store saw NOTHING.
        assert_eq!(mirror.served(), vec!["PanelScenePublish"]);

        // THE refusal report names the law verbatim (the user-log
        // grammar).
        let report = crate::refused_report(&state, &crate::Flow::owner());
        assert_eq!(
            report,
            "Scene Example: Flow::Owner refused (PublisherOwnedByOther; host head 1; \
             frame base —); frame = widget publish (next 2)"
        );

        // The HEAL is the ordinary discipline: back on the owner's wire,
        // the next snapshot cycle publishes cleanly from the realigned
        // frontier (carrying the owed ack leg).
        assert!(widgets_table::publish_widget_cycle(&owner_sender, &mut state));
        assert_eq!(state.head, 3);
        assert!(state.pending_ack.is_empty(), "the ack leg rode the heal");
    }
}
