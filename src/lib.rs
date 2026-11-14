//! Open CAD Studio add-on example: a dockable side panel.
//!
//! Demonstrates the API v7+ panel surface end to end:
//!   * `register_panel` at load — a palette with every basic control kind,
//!     docked on the right (the panel id and its `command` both become
//!     dynamic command-line completions);
//!   * `HostNotification::PanelAction` events — the host streams to every
//!     loaded plugin a `change` event on each text/checkbox/choice/color
//!     edit and a `click` event for buttons without an `action` string;
//!     `on_notification` carries no host, so the plugin only tracks state;
//!   * host writes from `dispatch` — the panel's Apply button sends the
//!     `EPL_APPLY` CAD command back through the dispatcher, and that path
//!     runs `update_panel` (rewrite the summary label) and `push_info`;
//!   * a built-in button keyword (`action: "close"`) the host handles
//!     itself, plus `toggle_panel` from a command (`EPL_OPEN` / `EPL_PANEL`)
//!     and a ribbon tool.
//!
//! Build with `cargo build --release`; ship the cdylib plus a filled
//! `plugin.toml` (see README.md).

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};

use ocs_plugin_api::dialog::{DialogControl, DialogValue, HorizontalAlign, PanelDef, VerticalAlign};
use ocs_plugin_api::host::{BuiltinPlugin, HostApi, HostNotification};
use ocs_plugin_api::manifest::{ApiVersion, PluginManifest};
use ocs_plugin_api::ribbon::{CadModule, IconKind, ModuleEvent, RibbonGroup, RibbonItem, ToolDef};

/// Ribbon/QAT art for the tool button: OCS pixel-grid SVG (16 px viewBox,
/// 1 px strokes on half-pixel coordinates, whole-pixel fills, `#ffffff`
/// palette colours — the host recolours it for the active theme; see
/// "Plugin icons" in OpenCADStudio/docs/plugin-architecture.md). A glyph
/// fallback like U+25A4 renders as nothing in the host's ribbon font stack.
static PANEL_ICON: &[u8] = include_bytes!("../panel_icon.svg");

// Keep these fields in sync with `plugin.toml`.
static MANIFEST: PluginManifest = PluginManifest {
    id: "schoeller.example_panel",
    name: "Panel Example",
    version: "0.1.0",
    description: "Reference dynamically-loaded add-on: a dockable side panel with live control events.",
    api_version: ApiVersion::CURRENT,
    ribbon_order: 110,
    xdata_apps: &[],
    command_prefixes: &["EPL_"],
};

/// Panel identity. Host-facing calls (`toggle_panel` / `update_panel`) also
/// accept the panel title case- and punctuation-insensitively.
const PANEL_ID: &str = "epl_panel";

/// Open/toggle command, registered both on the panel (`PanelDef::command`)
/// and through the manifest's `command_prefixes`.
const CMD_OPEN: &str = "EPL_OPEN";
/// Re-render command, wired to the panel's Apply button.
const CMD_APPLY: &str = "EPL_APPLY";

// Control ids (panel-relative; the host matches them as-is).
const TXT_SPECIMEN: &str = "epl_text_specimen";
const NUM_LENGTH: &str = "epl_number_length";
const CHK_INFO: &str = "epl_checkbox_info";
const CHO_STYLE: &str = "epl_choice_style";
const CLR_STROKE: &str = "epl_color_stroke";
const LBL_SUMMARY: &str = "epl_label_summary";
const BTN_APPLY: &str = "epl_button_apply";
const BTN_PING: &str = "epl_button_ping";
const BTN_HIDE: &str = "epl_button_hide";

/// Static panel definition: one control of every basic kind, a summary label
/// the plugin rewrites with `update_panel`, and two action buttons. The host
/// treats an unchanged re-registration idempotently (mid-edit values, focus
/// and layout survive), so loading this plugin twice is safe.
fn demo_panel() -> PanelDef {
    PanelDef::new(PANEL_ID, "Panel Example")
        .with_command(CMD_OPEN)
        .with_width(320.0)
        .with_control(DialogControl::InfoText {
            text: "Panel API demo: the plugin tracks your edits and applies them on demand."
                .into(),
            align_h: Some(HorizontalAlign::Left),
        })
        .with_control(DialogControl::Text {
            id: TXT_SPECIMEN.into(),
            label: "Specimen".into(),
            default: "ABC-123".into(),
        })
        .with_control(DialogControl::Number {
            id: NUM_LENGTH.into(),
            label: "Length".into(),
            default: 7.5,
            min: Some(0.0),
            max: Some(1_000.0),
            step: Some(0.5),
        })
        .with_control(DialogControl::Checkbox {
            id: CHK_INFO.into(),
            label: "Print summary to info bar".into(),
            default: true,
        })
        .with_control(DialogControl::Choice {
            id: CHO_STYLE.into(),
            label: "Style".into(),
            options: vec!["Outlined".into(), "Shaded".into(), "Survey".into()],
            default_index: 0,
        })
        .with_control(DialogControl::Color {
            id: CLR_STROKE.into(),
            label: "Stroke".into(),
            default: [46, 125, 50],
        })
        .with_control(DialogControl::Label {
            id: LBL_SUMMARY.into(),
            text: "Waiting for input…".into(),
            align_h: Some(HorizontalAlign::Left),
        })
        .with_control(DialogControl::Row {
            controls: vec![
                DialogControl::Button {
                    id: BTN_APPLY.into(),
                    label: "Apply".into(),
                    // Routes through the host command dispatcher to
                    // dispatch(EPL_APPLY), which can run update_panel.
                    action: Some(CMD_APPLY.into()),
                    align_h: Some(HorizontalAlign::Left),
                    width: None,
                },
                DialogControl::Button {
                    id: BTN_PING.into(),
                    label: "Ping".into(),
                    // No action string: the host broadcasts a "click"
                    // PanelAction; the plugin counts it (see EPL_APPLY).
                    action: None,
                    align_h: Some(HorizontalAlign::Left),
                    width: None,
                },
                DialogControl::Button {
                    id: BTN_HIDE.into(),
                    label: "Hide".into(),
                    // Built-in keyword: the host hides the panel itself,
                    // no command and no plugin event.
                    action: Some("close".into()),
                    align_h: Some(HorizontalAlign::Right),
                    width: None,
                },
            ],
            align_h: None,
            align_v: Some(VerticalAlign::Middle),
        })
}

/// The ribbon tab (one group, one tool) so the panel is discoverable from
/// the ribbon as well as the command prompt.
struct PanelModule;

impl CadModule for PanelModule {
    fn id(&self) -> &'static str {
        "example_panel"
    }
    fn title(&self) -> &'static str {
        "Panel Example"
    }
    fn ribbon_groups(&self) -> &[RibbonGroup] {
        static GROUPS: std::sync::OnceLock<Vec<RibbonGroup>> = std::sync::OnceLock::new();
        GROUPS.get_or_init(|| {
            vec![RibbonGroup {
                title: "Panels",
                tools: vec![RibbonItem::LargeTool(ToolDef {
                    id: CMD_OPEN,
                    label: "Panel",
                    icon: IconKind::Svg(PANEL_ICON),
                    event: ModuleEvent::Command(CMD_OPEN.to_string()),
                })],
            }]
        })
    }
}

/// Render a `DialogValue` the way the summary shows it.
fn value_to_string(value: &DialogValue) -> String {
    match value {
        DialogValue::Text(text) => format!("\"{text}\""),
        DialogValue::Number(number) => format!("{number}"),
        DialogValue::Bool(on) => if *on { "on" } else { "off" }.to_string(),
        DialogValue::Choice { index, text } => format!("{text} [{index}]"),
        DialogValue::Color([r, g, b]) => format!("#{r:02X}{g:02X}{b:02X}"),
        DialogValue::FilePath(path) => format!("\"{path}\""),
        // Plugin-sent dynamic values and container values never appear in
        // incoming panel events.
        DialogValue::Fields(_)
        | DialogValue::Table(_)
        | DialogValue::ChoiceOptions { .. }
        | DialogValue::Enabled(_)
        | DialogValue::Visible(_) => "<n/a>".to_string(),
    }
}

/// One line summarizing the controls the plugin currently tracks.
fn summary(values: &HashMap<String, DialogValue>, pings: u64) -> String {
    let mut parts: Vec<String> = Vec::new();
    for id in [TXT_SPECIMEN, NUM_LENGTH, CHO_STYLE, CHK_INFO, CLR_STROKE] {
        if let Some(value) = values.get(id) {
            parts.push(value_to_string(value));
        }
    }
    if pings > 0 {
        parts.push(format!("{pings} pings"));
    }
    if parts.is_empty() {
        "Waiting for input…".to_string()
    } else {
        parts.join(" · ")
    }
}

/// The plugin entry point.
struct ExamplePanelPlugin {
    /// Latest values observed through `PanelAction` events, keyed by control
    /// id. `dispatch` runs on the UI thread while notifications arrive over
    /// the same runner channel, so one plain `Mutex` serializes both ends.
    values: Mutex<HashMap<String, DialogValue>>,
    /// "Ping" button clicks seen since the last Apply (test hook).
    pings: AtomicU64,
}

impl ExamplePanelPlugin {
    /// The cdylib entry point (`export_plugin!` takes a constructor).
    fn new() -> Self {
        Self {
            values: Mutex::new(HashMap::new()),
            pings: AtomicU64::new(0),
        }
    }

    fn values(&self) -> MutexGuard<'_, HashMap<String, DialogValue>> {
        self.values.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl BuiltinPlugin for ExamplePanelPlugin {
    fn manifest(&self) -> &'static PluginManifest {
        &MANIFEST
    }

    fn ribbon(&self) -> Box<dyn CadModule> {
        Box::new(PanelModule)
    }

    fn on_load(&mut self, host: &mut dyn HostApi) {
        // Seed tracked values from the panel defaults, then register.
        let mut values = self.values();
        values.clear();
        for control in demo_panel().controls {
            match control {
                DialogControl::Text { id, default, .. } => {
                    values.insert(id, DialogValue::Text(default));
                }
                DialogControl::Number { id, default, .. } => {
                    values.insert(id, DialogValue::Number(default));
                }
                DialogControl::Checkbox { id, default, .. } => {
                    values.insert(id, DialogValue::Bool(default));
                }
                DialogControl::Choice {
                    id,
                    options,
                    default_index,
                    ..
                } => {
                    values.insert(
                        id,
                        DialogValue::Choice {
                            index: default_index,
                            text: options.into_iter().nth(default_index).unwrap_or_default(),
                        },
                    );
                }
                DialogControl::Color { id, default, .. } => {
                    values.insert(id, DialogValue::Color(default));
                }
                _ => {}
            }
        }
        let echo = summary(&values, 0);
        drop(values);

        if host.register_panel(demo_panel()) {
            // The label is in the definition already, but this shows the
            // write path as the first thing the plugin does.
            host.update_panel(
                PANEL_ID,
                HashMap::from([(LBL_SUMMARY.to_string(), DialogValue::Text(echo))]),
            );
            host.push_info("Panel Example loaded — type EPL_OPEN or use the ribbon tool.");
        } else {
            host.push_info("Panel Example loaded, but its panel was refused.");
        }
    }

    fn dispatch(&self, host: &mut dyn HostApi, cmd: &str) -> bool {
        match cmd {
            // Two spellings, both dynamic commands: the panel's `command`
            // string (EPL_OPEN) and the panel id (EPL_PANEL).
            CMD_OPEN | "EPL_PANEL" => {
                let shown = host.toggle_panel(PANEL_ID, None);
                host.push_info(if shown {
                    "Panel Example panel toggled."
                } else {
                    "Panel Example panel toggle failed."
                });
                true
            }
            CMD_APPLY => {
                let (echo, info) = {
                    let values = self.values();
                    let info = matches!(values.get(CHK_INFO), Some(DialogValue::Bool(true)));
                    let echo = summary(&values, self.pings.swap(0, Ordering::SeqCst));
                    (echo, info)
                };
                // Rewrite the summary label in the open panel.
                if host.update_panel(
                    PANEL_ID,
                    HashMap::from([(LBL_SUMMARY.to_string(), DialogValue::Text(echo.clone()))]),
                ) && info
                {
                    host.push_info(&format!("Applied: {echo}"));
                }
                true
            }
            _ => false,
        }
    }

    fn on_notification(&mut self, _command_id: Option<u64>, notification: HostNotification) {
        let HostNotification::PanelAction {
            panel_id,
            control_id,
            action,
            value,
        } = notification
        else {
            return;
        };
        // Host-owned panel events are broadcast to every plugin: ignore the
        // panels of other plugins.
        if panel_id != PANEL_ID {
            return;
        }
        if action == "click" {
            if control_id == BTN_PING {
                self.pings.fetch_add(1, Ordering::SeqCst);
            }
            return;
        }
        if let Some(value) = value {
            self.values().insert(control_id, value);
        }
    }
}

// Emit the C-ABI symbols the host loader looks for.
ocs_plugin_api::export_plugin!(ExamplePanelPlugin::new());
