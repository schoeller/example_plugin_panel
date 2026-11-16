//! The Menu + List demo: a [`MenuDef`] (submenu + keyboard shortcuts +
//! one [`ConfirmPolicy::Prompt`] item) and an [`ItemListDef`] of four
//! layer items, patched onto the EXISTING widgets surface as
//! [`WidgetNode`]s under the root group (the mount composes the Group
//! layouts). Invocations ride the ordinary `CommandInvoked` stream and
//! activations the `ItemActivated` stream — both merge into the plugin's
//! state exactly like every other semantic event.
//!
//! Id space (the surface-wide namespace — widget nodes + tree rows + list
//! items share it): menu **150**, its Zoom submenu **151**, list **160**,
//! list items **161..=164** (past the base surface's 90/100/110/120, the
//! tree rows 1..=12 and the lazy children 130..=132).
//!
//! Command strings live OUTSIDE the id namespace (menu items are
//! command-addressed): `scn_menu_fit`, `scn_menu_zoom_in` /
//! `scn_menu_zoom_out`, `scn_menu_export` (the confirmed one),
//! `scn_list_layer`.

use std::sync::Arc;

use ocs_plugin_api::host::PluginRequestSender;
use ocs_plugin_api::scene::{SceneRevision, StableId};
use ocs_plugin_api::widgets::{
    ConfirmPolicy, DomainRef, ItemDef, ItemListDef, MenuDef, MenuItemDef, WidgetNode,
    WidgetOp, WidgetSurfacePatch,
};

use crate::host_wire;
use crate::scene_graph::PANEL_ID;
use crate::state::{DemoState, FrameOutcome};
use crate::widgets_table::{self, ROOT_GROUP_ID};

pub const MENU_ID: u64 = 150;
pub const MENU_SUBMENU_ID: u64 = 151;
pub const LIST_ID: u64 = 160;
pub const LIST_FIRST_ITEM: u64 = 161;
pub const LIST_ITEM_COUNT: usize = 4;

/// The menu's semantic command ids (the host invokes them; the plugin
/// resolves what each names for the reported targets).
pub const CMD_MENU_FIT: &str = "scn_menu_fit";
/// The submenu parent's command id (the submenu opens host-side; the id
/// is required non-empty but never invoked).
pub const CMD_MENU_ZOOM: &str = "scn_menu_zoom";
pub const CMD_MENU_ZOOM_IN: &str = "scn_menu_zoom_in";
pub const CMD_MENU_ZOOM_OUT: &str = "scn_menu_zoom_out";
pub const CMD_MENU_EXPORT: &str = "scn_menu_export";
/// Every list item's activation command (the item's own id arrives as
/// the invocation's target).
pub const CMD_LIST_LAYER: &str = "scn_list_layer";

/// The demo menu: Fit (shortcut F), a Zoom submenu (+/- shortcuts), and
/// the confirmed Export item.
pub fn demo_menu() -> MenuDef {
    MenuDef {
        id: StableId(MENU_ID),
        name: Some("View menu".to_string()),
        enabled: true,
        visible: true,
        label: Some("View".to_string()),
        items: vec![
            MenuItemDef {
                command_id: CMD_MENU_FIT.to_string(),
                label: "Fit view".to_string(),
                icon: None,
                shortcut: Some("F".to_string()),
                enabled: true,
                visible: true,
                submenu: None,
                confirmation: None,
            },
            MenuItemDef {
                command_id: CMD_MENU_ZOOM.to_string(),
                label: "Zoom".to_string(),
                icon: None,
                shortcut: None,
                enabled: true,
                visible: true,
                submenu: Some(Box::new(MenuDef {
                    id: StableId(MENU_SUBMENU_ID),
                    name: Some("Zoom submenu".to_string()),
                    enabled: true,
                    visible: true,
                    label: Some("Zoom".to_string()),
                    items: vec![
                        MenuItemDef {
                            command_id: CMD_MENU_ZOOM_IN.to_string(),
                            label: "Zoom in".to_string(),
                            icon: None,
                            shortcut: Some("+".to_string()),
                            enabled: true,
                            visible: true,
                            submenu: None,
                            confirmation: None,
                        },
                        MenuItemDef {
                            command_id: CMD_MENU_ZOOM_OUT.to_string(),
                            label: "Zoom out".to_string(),
                            icon: None,
                            shortcut: Some("-".to_string()),
                            enabled: true,
                            visible: true,
                            submenu: None,
                            confirmation: None,
                        },
                    ],
                })),
                confirmation: None,
            },
            MenuItemDef {
                command_id: CMD_MENU_EXPORT.to_string(),
                label: "Export panel data…".to_string(),
                icon: None,
                shortcut: None,
                enabled: true,
                visible: true,
                submenu: None,
                confirmation: Some(ConfirmPolicy::Prompt {
                    message: "Export the panel's data next to the drawing? \
                              The host asks once before invoking."
                        .to_string(),
                }),
            },
        ],
    }
}

/// The demo list: four enabled layer items, each bound to the
/// `scn_list_layer` command (its own item id rides as the target).
pub fn demo_list() -> ItemListDef {
    let names = ["Walls", "Doors", "Roof", "Annotations"];
    ItemListDef {
        id: StableId(LIST_ID),
        name: Some("Layer list".to_string()),
        enabled: true,
        visible: true,
        label: Some("Layers".to_string()),
        items: (0..LIST_ITEM_COUNT)
            .map(|index| ItemDef {
                id: StableId(LIST_FIRST_ITEM + index as u64),
                label: names[index].to_string(),
                icon: None,
                enabled: true,
                visible: true,
                command: Some(CMD_LIST_LAYER.to_string()),
                domain_ref: Some(DomainRef {
                    kind: "layer".to_string(),
                    id: LIST_FIRST_ITEM + index as u64,
                }),
            })
            .collect(),
    }
}

/// The one patch that mounts both widgets: two `Add` ops into the root
/// group (a patch successor must strictly exceed the base — the same
/// discipline every widget frame here follows).
pub fn menu_list_patch(base: u64, revision: u64) -> WidgetSurfacePatch {
    WidgetSurfacePatch {
        base_revision: SceneRevision(base),
        revision: SceneRevision(revision),
        ops: vec![
            WidgetOp::Add {
                parent: Some(StableId(ROOT_GROUP_ID)),
                widget: WidgetNode::Menu(demo_menu()),
            },
            WidgetOp::Add {
                parent: Some(StableId(ROOT_GROUP_ID)),
                widget: WidgetNode::List(demo_list()),
            },
        ],
    }
}

/// The menu + list feature's flow: ensure the widget leg exists, then ONE
/// patch mounts both widgets (skipped when the model already holds them —
/// the command is idempotent). Every frame carries the piggybacked event
/// ack leg through the ordinary patch frame (a patch carries no events
/// field; the ack rides the next publish).
pub fn run_menu_list_demo(
    sender: &Arc<dyn PluginRequestSender>,
    state: &mut DemoState,
) -> bool {
    // Ensure-base: a fresh fixture publish REPLACES the model, so it only
    // runs when the model holds no surface.
    if state.widgets.is_none() && !widgets_table::publish_widget_base(sender, state) {
        return false;
    }
    if widgets_table::holds_widget_id(state, MENU_ID) && widgets_table::holds_widget_id(state, LIST_ID)
    {
        return true;
    }
    let Some(base) = state.widgets_head() else {
        return false;
    };
    let successor = state.next_revision();
    let patch = menu_list_patch(base.0, successor.0);
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
                "menu+list: menu {} (submenu {}, one Prompt item) and list {} ({} items) \
                 mounted with ONE patch (revision {})",
                MENU_ID, MENU_SUBMENU_ID, LIST_ID, LIST_ITEM_COUNT, claimed
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
    use ocs_plugin_api::widgets::WidgetEvent;

    use crate::fake_host::FakeWireHost;
    use crate::state::DemoState;

    use super::*;

    /// The fixtures validate as part of the composed surface and are
    /// byte-deterministic.
    #[test]
    fn menu_and_list_fixtures_validate_and_deterministic() {
        let mut surface = widgets_table::demo_widget_surface(1).unwrap();
        let patched = surface
            .apply_patch(menu_list_patch(1, 2))
            .expect("the menu+list patch composes onto the base surface");
        let _ = patched;
        // Byte determinism of both fixtures and of the patch frame.
        let a = bincode::serialize(&demo_menu()).unwrap();
        let b = bincode::serialize(&demo_menu()).unwrap();
        assert_eq!(a, b);
        let a = bincode::serialize(&demo_list()).unwrap();
        let b = bincode::serialize(&demo_list()).unwrap();
        assert_eq!(a, b);
        let a = bincode::serialize(&menu_list_patch(1, 2)).unwrap();
        let b = bincode::serialize(&menu_list_patch(1, 2)).unwrap();
        assert_eq!(a, b);
        // The confirm item is exactly the export entry.
        let menu = demo_menu();
        assert!(menu.items.iter().any(|item| {
            item.command_id == CMD_MENU_EXPORT
                && matches!(item.confirmation, Some(ConfirmPolicy::Prompt { .. }))
        }));
        assert!(menu
            .items
            .iter()
            .any(|item| item.submenu.as_ref().is_some_and(|sub| sub.id.0 == MENU_SUBMENU_ID)));
    }

    /// The feature mounts both widgets with ONE patch; the events the
    /// mount then produces (menu invocation incl. the confirmed item,
    /// list activation) merge into the plugin's state like every other
    /// semantic fact.
    #[test]
    fn menu_list_feature_patches_surface_and_events_merge() {
        let host = Arc::new(FakeWireHost::new());
        let sender: Arc<dyn PluginRequestSender> =
            Arc::clone(&host) as Arc<dyn PluginRequestSender>;
        let mut state = DemoState::default();

        assert!(run_menu_list_demo(&sender, &mut state));
        let stored = host.widgets().expect("surface published");
        // The widgets live UNDER the root group (the mount composes the
        // Group layouts) — walk the group tree.
        let holds = |id: u64| {
            let mut walker: Vec<&WidgetNode> = stored.roots.iter().collect();
            while let Some(node) = walker.pop() {
                if node.widget_id() == StableId(id) {
                    return true;
                }
                if let WidgetNode::Group(group) = node {
                    walker.extend(group.children.iter());
                }
            }
            false
        };
        assert!(holds(MENU_ID), "the menu is live");
        assert!(holds(LIST_ID), "the list is live");

        // Idempotent: a second run sends no frame.
        let served = host.served().len();
        assert!(run_menu_list_demo(&sender, &mut state));
        assert_eq!(host.served().len(), served);

        // The mount's events: a submenu invocation, the confirmed item's
        // invocation (the host asks once, then reports the invocation),
        // and a list activation.
        let surface_id = StableId(ROOT_GROUP_ID);
        state.handle_events(
            surface_id,
            vec![
                WidgetEvent::CommandInvoked {
                    surface_id,
                    command_id: CMD_MENU_ZOOM_IN.to_string(),
                    target_ids: vec![],
                },
                WidgetEvent::CommandInvoked {
                    surface_id,
                    command_id: CMD_MENU_EXPORT.to_string(),
                    target_ids: vec![],
                },
                WidgetEvent::ItemActivated {
                    surface_id,
                    item_id: StableId(LIST_FIRST_ITEM + 1),
                },
            ],
        );
        assert_eq!(
            state.commands_invoked.len(),
            2,
            "both invocations merged (the confirmed one included)"
        );
        assert_eq!(
            state.commands_invoked[1].0,
            CMD_MENU_EXPORT.to_string(),
            "the confirm-policy item's invocation arrives like any other"
        );
        assert_eq!(
            state.activations,
            vec![StableId(LIST_FIRST_ITEM + 1)]
        );
    }
}
