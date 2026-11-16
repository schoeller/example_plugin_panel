//! The AssetRef + theming demo: ONE patch adds a swatch group to the
//! scene with
//!
//! - a host-managed **asset**: a [`SceneElement::Asset`] whose `uri` is
//!   an opaque reference (`host://icons/scn-mark.svg` — the host loads
//!   and caches it against its own per-panel asset loader; NO bytes ride
//!   the scene channel, at most [`MAX_ASSET_URI_BYTES`] of uri);
//! - a **theme-token swatch**: a Rect filled with
//!   `SceneColor::Theme(ThemeColor::Accent)` — it FOLLOWS the host theme
//!   (re-theme the app and it re-colors);
//! - a **fixed-RGBA swatch**: a Rect filled with an explicit
//!   [`Rgba8`] — it NEVER changes, whatever the theme does.
//!
//! The pair is the whole theming contract in one glance: tokens resolve
//! host-side, explicit values are exact.
//!
//! Id space (scene namespace, past the graph demo's 0..80, the recovery's
//! 24 and the drag demo's 600): the swatch group is **500** with children
//! 501 (asset), 502/503 (theme swatch + label), 504/505 (RGBA swatch +
//! label).

use std::sync::Arc;

use ocs_plugin_api::host::PluginRequestSender;
use ocs_plugin_api::scene::{
    AssetKind, Rgba8, Rect2D, SceneColor, SceneElement, StableId, ThemeColor, Vec2,
};
use ocs_plugin_api::scene_builder::PatchBuilder;

use crate::host_wire;
use crate::scene_graph::{self, PANEL_ID};
use crate::state::{DemoState, FrameOutcome};

pub const ASSET_GROUP_ID: u64 = 500;
pub const ASSET_ELEMENT_ID: u64 = 501;
pub const THEME_SWATCH_ID: u64 = 502;
pub const THEME_LABEL_ID: u64 = 503;
pub const RGBA_SWATCH_ID: u64 = 504;
pub const RGBA_LABEL_ID: u64 = 505;

/// The opaque asset reference (the host resolves it; the plugin never
/// sends bytes).
pub const ASSET_URI: &str = "host://icons/scn-mark.svg";
/// The exact RGBA the fixed swatch wears (never theme-resolved).
pub const RGBA_FIXED: Rgba8 = Rgba8([155, 89, 182, 255]);

const TEXT_SIZE: f32 = 13.0;
const SWATCH_SIZE: [f64; 2] = [48.0, 24.0];

/// The swatch group: the asset, the theme-token swatch, the fixed-RGBA
/// swatch, one label each (a literal, the legend-group precedent).
pub fn swatch_group() -> SceneElement {
    SceneElement::Group {
        id: StableId(ASSET_GROUP_ID),
        name: Some("Theme & asset swatches".to_string()),
        actions: Vec::new(),
        drag: None,
        children: vec![
            SceneElement::Asset {
                id: StableId(ASSET_ELEMENT_ID),
                name: Some("Host-managed mark".to_string()),
                actions: Vec::new(),
                drag: None,
                kind: AssetKind::Svg,
                uri: ASSET_URI.to_string(),
                frame: Rect2D {
                    pos: Vec2([40.0, 400.0]),
                    size: Vec2([24.0, 24.0]),
                },
            },
            SceneElement::Rect {
                id: StableId(THEME_SWATCH_ID),
                name: Some("Theme accent swatch".to_string()),
                actions: Vec::new(),
                drag: None,
                pos: Vec2([80.0, 400.0]),
                size: Vec2(SWATCH_SIZE),
                fill: Some(SceneColor::Theme(ThemeColor::Accent)),
                stroke: None,
            },
            SceneElement::Text {
                id: StableId(THEME_LABEL_ID),
                name: None,
                actions: Vec::new(),
                drag: None,
                content: "theme accent — follows the host theme".to_string(),
                pos: Vec2([136.0, 416.0]),
                size: TEXT_SIZE,
                color: SceneColor::Theme(ThemeColor::Muted),
            },
            SceneElement::Rect {
                id: StableId(RGBA_SWATCH_ID),
                name: Some("Fixed RGBA swatch".to_string()),
                actions: Vec::new(),
                drag: None,
                pos: Vec2([80.0, 432.0]),
                size: Vec2(SWATCH_SIZE),
                fill: Some(SceneColor::Rgba(RGBA_FIXED)),
                stroke: None,
            },
            SceneElement::Text {
                id: StableId(RGBA_LABEL_ID),
                name: None,
                actions: Vec::new(),
                drag: None,
                content: "fixed RGBA(155,89,182) — never changes".to_string(),
                pos: Vec2([136.0, 448.0]),
                size: TEXT_SIZE,
                color: SceneColor::Theme(ThemeColor::Muted),
            },
        ],
        clip: None,
    }
}

/// `true` while the model's scene holds the element id.
fn holds_element(state: &DemoState, id: StableId) -> bool {
    state
        .scene
        .as_ref()
        .is_some_and(|scene| scene.elements.iter().any(|element| element.stable_id() == id))
}

/// The asset + theming feature's flow: ensure the scene leg exists, then
/// ONE `Add` patch mounts the swatch group (skipped when the model
/// already holds it — the command is idempotent).
pub fn run_asset_theme_demo(
    sender: &Arc<dyn PluginRequestSender>,
    state: &mut DemoState,
) -> bool {
    // Ensure-base: a fresh fixture publish REPLACES the model, so it only
    // runs when the model holds no scene.
    if state.scene.is_none() && !scene_graph::publish_scene_base(sender, state) {
        return false;
    }
    if holds_element(state, StableId(ASSET_GROUP_ID)) {
        return true;
    }
    let Some(base) = state.scene_head() else {
        return false;
    };
    let successor = state.next_revision();
    let Ok(patch) = PatchBuilder::new(base.0)
        .add(None, swatch_group())
        .finish(successor.0)
    else {
        state.head -= 1;
        return false;
    };
    let base_revision = patch.base_revision.0;
    let claimed = patch.revision.0;
    state.sent += 1;
    match host_wire::send_scene_patch(sender, PANEL_ID, patch.clone()) {
        host_wire::WireOutcome::Applied { revision, .. } => {
            state.applied += 1;
            let Some(scene) = state.scene.as_mut() else {
                return false;
            };
            if scene.apply_patch(patch).is_err() {
                state.note_frame(
                    "scene patch",
                    Some(base_revision),
                    Some(claimed),
                    FrameOutcome::AppliedModelLagged(revision.0),
                );
                return false;
            }
            debug_assert_eq!(scene.revision, revision, "model and host agree");
            state.note_frame(
                "scene patch",
                Some(base_revision),
                Some(claimed),
                FrameOutcome::Applied,
            );
            state.journal_note(format!(
                "asset+theme: swatch group {} armed — asset {ASSET_URI} (host-managed), \
                 a Theme(Accent) swatch beside a fixed RGBA({}) swatch",
                ASSET_GROUP_ID,
                RGBA_FIXED.0.iter().map(|b| b.to_string()).collect::<Vec<_>>().join(",")
            ));
            true
        }
        host_wire::WireOutcome::Rejected { code, report } => {
            state.note_reject(
                "scene patch",
                Some(base_revision),
                Some(claimed),
                code,
                report.current_revision,
            );
            false
        }
        host_wire::WireOutcome::Unserviceable(message) => {
            state.note_frame("scene patch", Some(base_revision), Some(claimed), FrameOutcome::Unserviceable(message));
            false
        }
        host_wire::WireOutcome::Failed(error) => {
            state.note_frame("scene patch", Some(base_revision), Some(claimed), FrameOutcome::SendFailed(error));
            false
        }
        host_wire::WireOutcome::Unexpected => {
            state.note_frame("scene patch", Some(base_revision), Some(claimed), FrameOutcome::Unexpected);
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::fake_host::FakeWireHost;
    use crate::state::DemoState;

    use super::*;

    /// The swatch group validates (the builder-independent literal goes
    /// through the patch, whose finish() runs the validator) and is
    /// byte-deterministic.
    #[test]
    fn swatch_group_validates_and_is_byte_deterministic() {
        let a = bincode::serialize(&swatch_group()).unwrap();
        let b = bincode::serialize(&swatch_group()).unwrap();
        assert_eq!(a, b);
        // The Add op's payload validator: a sub-scope finish() accepting
        // the group proves it structural (the same gate the real patch
        // runs pre-send).
        let mut builder = ocs_plugin_api::scene_builder::SceneBuilder::new(0);
        builder.element(swatch_group());
        builder.finish().expect("the swatch group is structural");
    }

    /// The feature arms the asset + the theme/RGBA pair with ONE patch;
    /// the host holds the asset ref verbatim; a second run is idempotent.
    #[test]
    fn asset_and_theme_patch_headless() {
        let host = Arc::new(FakeWireHost::new());
        let sender: Arc<dyn PluginRequestSender> =
            Arc::clone(&host) as Arc<dyn PluginRequestSender>;
        let mut state = DemoState::default();

        assert!(run_asset_theme_demo(&sender, &mut state));
        let scene = host.scene().expect("scene published");
        let find = |id: u64| {
            scene
                .elements
                .iter()
                .filter_map(|element| match element {
                    SceneElement::Group { children, .. } => {
                        children.iter().find(|child| child.stable_id() == StableId(id))
                    }
                    other if other.stable_id() == StableId(id) => Some(other),
                    _ => None,
                })
                .cloned()
                .next()
        };
        // The asset: an opaque host-managed ref (no bytes anywhere).
        let Some(SceneElement::Asset { uri, kind, .. }) = find(ASSET_ELEMENT_ID) else {
            panic!("the asset is live");
        };
        assert_eq!(uri, ASSET_URI);
        assert!(matches!(kind, AssetKind::Svg));
        // The pair: theme token vs fixed RGBA.
        let Some(SceneElement::Rect { fill, .. }) = find(THEME_SWATCH_ID) else {
            panic!("the theme swatch is live");
        };
        assert_eq!(fill, Some(SceneColor::Theme(ThemeColor::Accent)));
        let Some(SceneElement::Rect { fill, .. }) = find(RGBA_SWATCH_ID) else {
            panic!("the RGBA swatch is live");
        };
        assert_eq!(fill, Some(SceneColor::Rgba(RGBA_FIXED)));

        // Idempotent: a second run sends no frame.
        let served = host.served().len();
        assert!(run_asset_theme_demo(&sender, &mut state));
        assert_eq!(host.served().len(), served);
        assert_eq!(host.scene().expect("scene"), scene, "still the same scene");
    }
}
