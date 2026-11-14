//! Open CAD Studio add-on example: a dockable side panel that is fully
//! interactive with the host, both directions.
//!
//! Host → plugin: the runner streams notifications (`SelectionChangedDigest`
//! — v22 O(1) selection facts; `PanelAction`; document region events). The
//! plugin keeps only NEWEST state and refreshes its panel via the
//! thread-safe host request line (`plugin_request_sender()` →
//! `PluginRequest::UpdatePanel`) behind a 16 ms gate — no worker thread, no
//! per-notification request.
//!
//! Plugin → host: `EPL_APPLY_SELECTION` (declared button/command) reads the
//! selection (dispatch-time `selection()` + `entities_batch()`), journals
//! the originals, and mutates the entities' stroke colour through
//! `UpdateEntitiesTransaction` inside one undo group; `EPL_REVERT_SELECTION`
//! restores the journaled originals — but only for handles whose current
//! state still matches what Apply wrote (external edits/deletes are reported
//! as conflicts, never clobbered) — and reselects the restored handles via
//! `set_selection`. Both paths are exactly one undo step, and host-native
//! Undo/Redo streams back as document events the plugin reflects without
//! touching the host again. `EPL_INSPECT` reports a kind breakdown of the
//! selection (first 256).
//!
//! Build with `cargo build --release`; ship the cdylib plus a filled
//! `plugin.toml` (see README.md).

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use ocs_plugin_api::host::acadrust::entities::Entity;
use ocs_plugin_api::host::acadrust::{Color, EntityType};
use ocs_plugin_api::dialog::{DialogControl, DialogValue, HorizontalAlign, PanelDef, VerticalAlign};
use ocs_plugin_api::host::{BuiltinPlugin, Handle, HostApi, HostNotification, PluginRequestSender};
use ocs_plugin_api::ipc::protocol::{PluginRequest, PluginResponse};
use ocs_plugin_api::manifest::{ApiVersion, PluginManifest};
use ocs_plugin_api::ribbon::{CadModule, IconKind, ModuleEvent, RibbonGroup, RibbonItem, ToolDef};

// Keep these fields in sync with `plugin.toml`.
static MANIFEST: PluginManifest = PluginManifest {
    id: "schoeller.example_panel",
    name: "Panel Example",
    version: "0.3.0",
    description: "Reference dynamically-loaded add-on: a dockable side panel, fully interactive both ways.",
    api_version: ApiVersion::CURRENT,
    ribbon_order: 110,
    xdata_apps: &[],
    command_prefixes: &["EPL_"],
};

/// Panel identity. Host-facing calls (`toggle_panel` / `update_panel`) also
/// accept the panel title case- and punctuation-insensitively.
const PANEL_ID: &str = "epl_panel";

const CMD_OPEN: &str = "EPL_OPEN";
const CMD_APPLY: &str = "EPL_APPLY";
const CMD_APPLY_SELECTION: &str = "EPL_APPLY_SELECTION";
const CMD_REVERT_SELECTION: &str = "EPL_REVERT_SELECTION";
const CMD_INSPECT: &str = "EPL_INSPECT";

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
const BTN_APPLY_SEL: &str = "epl_button_apply_selection";
const BTN_REVERT: &str = "epl_button_revert_selection";
const BTN_INSPECT: &str = "epl_button_inspect";

/// Live-preview gate: at most one panel refresh per this many milliseconds;
/// explicit actions bypass it.
const PANEL_WRITE_MIN_GAP: Duration = Duration::from_millis(16);

/// Static panel definition. The plugin never re-registers with different
/// content, so the host keeps drafts/values across reloads.
fn demo_panel() -> PanelDef {
    PanelDef::new(PANEL_ID, "Panel Example")
        .with_command(CMD_OPEN)
        .with_width(320.0)
        .with_control(DialogControl::InfoText {
            text: "Interactive demo: selection/value summaries stream in; Apply/Revert write the host reversibly."
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
                    action: Some(CMD_APPLY.into()),
                    align_h: Some(HorizontalAlign::Left),
                    width: None,
                },
                DialogControl::Button {
                    id: BTN_PING.into(),
                    label: "Ping".into(),
                    // No action string: the host broadcasts a `click`
                    // PanelAction; MCP-testable via the `panel_action` op.
                    action: None,
                    align_h: Some(HorizontalAlign::Left),
                    width: None,
                },
                DialogControl::Button {
                    id: BTN_HIDE.into(),
                    label: "Hide".into(),
                    // Built-in keyword: the host hides the panel itself.
                    action: Some("close".into()),
                    align_h: Some(HorizontalAlign::Right),
                    width: None,
                },
            ],
            align_h: None,
            align_v: Some(VerticalAlign::Middle),
        })
        .with_control(DialogControl::Row {
            controls: vec![
                DialogControl::Button {
                    id: BTN_APPLY_SEL.into(),
                    label: "Apply to Sel".into(),
                    action: Some(CMD_APPLY_SELECTION.into()),
                    align_h: Some(HorizontalAlign::Left),
                    width: None,
                },
                DialogControl::Button {
                    id: BTN_REVERT.into(),
                    label: "Revert".into(),
                    action: Some(CMD_REVERT_SELECTION.into()),
                    align_h: Some(HorizontalAlign::Left),
                    width: None,
                },
                DialogControl::Button {
                    id: BTN_INSPECT.into(),
                    label: "Inspect".into(),
                    action: Some(CMD_INSPECT.into()),
                    align_h: Some(HorizontalAlign::Right),
                    width: None,
                },
            ],
            align_h: None,
            align_v: Some(VerticalAlign::Middle),
        })
}

/// The ribbon tab (one group, two tools) so the panel is discoverable from
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
                tools: vec![
                    RibbonItem::LargeTool(ToolDef {
                        id: CMD_OPEN,
                        label: "Panel",
                        icon: IconKind::Glyph("▤"),
                        event: ModuleEvent::Command(CMD_OPEN.to_string()),
                    }),
                    RibbonItem::LabeledTool(ToolDef {
                        id: CMD_APPLY_SELECTION,
                        label: "Apply Sel",
                        icon: IconKind::Glyph("◀"),
                        event: ModuleEvent::Command(CMD_APPLY_SELECTION.to_string()),
                    }),
                ],
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

/// Panel draft summary line for the tracked controls.
fn summary(values: &HashMap<String, DialogValue>, extra: &str) -> String {
    let mut parts: Vec<String> = Vec::new();
    for id in [TXT_SPECIMEN, NUM_LENGTH, CHO_STYLE, CHK_INFO, CLR_STROKE] {
        if let Some(value) = values.get(id) {
            parts.push(value_to_string(value));
        }
    }
    if !extra.is_empty() {
        parts.push(extra.to_string());
    }
    if parts.is_empty() {
        "Waiting for input…".to_string()
    } else {
        parts.join(" · ")
    }
}

/// Genuine recolor support per entity variant. A variant without an
/// `Entity`-backed common colour is skipped and reported, never guessed.
fn recolor(entity: EntityType, color: Color) -> (EntityType, bool) {
    match entity {
        EntityType::Point(mut e) => { e.set_color(color); (EntityType::Point(e), true) }
        EntityType::Line(mut e) => { e.set_color(color); (EntityType::Line(e), true) }
        EntityType::Circle(mut e) => { e.set_color(color); (EntityType::Circle(e), true) }
        EntityType::Arc(mut e) => { e.set_color(color); (EntityType::Arc(e), true) }
        EntityType::Ellipse(mut e) => { e.set_color(color); (EntityType::Ellipse(e), true) }
        EntityType::LwPolyline(mut e) => { e.set_color(color); (EntityType::LwPolyline(e), true) }
        EntityType::Polyline(mut e) => { e.set_color(color); (EntityType::Polyline(e), true) }
        EntityType::Text(mut e) => { e.set_color(color); (EntityType::Text(e), true) }
        EntityType::MText(mut e) => { e.set_color(color); (EntityType::MText(e), true) }
        EntityType::Spline(mut e) => { e.set_color(color); (EntityType::Spline(e), true) }
        EntityType::Ray(mut e) => { e.set_color(color); (EntityType::Ray(e), true) }
        EntityType::XLine(mut e) => { e.set_color(color); (EntityType::XLine(e), true) }
        EntityType::Solid(mut e) => { e.set_color(color); (EntityType::Solid(e), true) }
        EntityType::Insert(mut e) => { e.set_color(color); (EntityType::Insert(e), true) }
        EntityType::Mesh(mut e) => { e.set_color(color); (EntityType::Mesh(e), true) }
        EntityType::Face3D(mut e) => { e.set_color(color); (EntityType::Face3D(e), true) }
        EntityType::MLine(mut e) => { e.set_color(color); (EntityType::MLine(e), true) }
        other => (other, false),
    }
}

/// Current colour of an entity (`None` = unsupported variant).
fn entity_color(entity: &EntityType) -> Option<Color> {
    match entity {
        EntityType::Point(e) => Some(e.color()),
        EntityType::Line(e) => Some(e.color()),
        EntityType::Circle(e) => Some(e.color()),
        EntityType::Arc(e) => Some(e.color()),
        EntityType::Ellipse(e) => Some(e.color()),
        EntityType::LwPolyline(e) => Some(e.color()),
        EntityType::Polyline(e) => Some(e.color()),
        EntityType::Text(e) => Some(e.color()),
        EntityType::MText(e) => Some(e.color()),
        EntityType::Spline(e) => Some(e.color()),
        EntityType::Ray(e) => Some(e.color()),
        EntityType::XLine(e) => Some(e.color()),
        EntityType::Solid(e) => Some(e.color()),
        EntityType::Insert(e) => Some(e.color()),
        EntityType::Mesh(e) => Some(e.color()),
        EntityType::Face3D(e) => Some(e.color()),
        EntityType::MLine(e) => Some(e.color()),
        _ => None,
    }
}

/// Push ONE panel refresh for the summary label through the host request
/// line; gated (explicit actions bypass). `Ok` replies are best-effort.
fn send_summary(
    sender: &Option<Arc<dyn PluginRequestSender>>,
    values: &HashMap<String, DialogValue>,
    extra: &str,
    force: bool,
    last: &mut Option<Instant>,
) -> bool {
    let ready = force || last.is_none_or(|at| at.elapsed() >= PANEL_WRITE_MIN_GAP);
    if !ready {
        return false;
    }
    *last = Some(Instant::now());
    let Some(sender) = sender else { return false };
    let text = summary(values, extra);
    let values = HashMap::from([(LBL_SUMMARY.to_string(), DialogValue::Text(text))]);
    matches!(
        sender.request(PluginRequest::UpdatePanel {
            panel_id: PANEL_ID.to_string(),
            values,
        }),
        Ok(PluginResponse::Ok) | Err(_)
    )
}

/// One reversible journal entry per document tab.
struct Journal {
    /// Selection order at apply time.
    handles: Vec<Handle>,
    /// Original entities by the same order.
    originals: Vec<EntityType>,
    /// What Apply wrote (the Revert conflict test compares exact state).
    applied: Vec<EntityType>,
}

#[derive(Default)]
struct Inner {
    /// Latest known selection facts (from digest / legacy notifications).
    count: u64,
    revision: u64,
    /// Draft values the panel has shown us (kept current by PanelAction).
    values: HashMap<String, DialogValue>,
    last_panel_write: Option<Instant>,
    /// One journal per tab (plugin process lifetime).
    journals: HashMap<u64, Journal>,
    /// Thread-safe host request line (v4 runner).
    sender: Option<Arc<dyn PluginRequestSender>>,
    /// Total pings (surfaced by Apply's info line).
    pings: AtomicU64,
}

/// The plugin entry point.
struct ExamplePanelPlugin {
    inner: Mutex<Inner>,
}

impl ExamplePanelPlugin {
    fn new() -> Self {
        Self {
            inner: Mutex::new(Inner::default()),
        }
    }

    fn inner(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Refresh the summary label through the host request line (gated).
    fn refresh_summary(&self, extra: &str, force: bool) -> bool {
        let mut inner = self.inner();
        let sender = inner.sender.as_ref().map(|s| Arc::clone(s));
        let values = inner.values.clone();
        let Inner { last_panel_write, .. } = &mut *inner;
        send_summary(&sender, &values, extra, force, last_panel_write)
    }

    /// Apply: journal originals -> recolor -> one labeled transaction.
    fn apply_selection(&self, host: &mut dyn HostApi) {
        let handles = host.selection();
        if handles.is_empty() {
            host.push_info("Panel Example: nothing selected — select entities first.");
            return;
        }
        // The exact stroke draft the panel currently shows (v18 snapshot);
        // defaulting to the panel declaration's green.
        let color = match host.panel_snapshot(PANEL_ID) {
            Ok(snapshot) => match snapshot.values.get(CLR_STROKE) {
                Some(DialogValue::Color([r, g, b])) => Color::from_rgb(*r, *g, *b),
                _ => Color::from_rgb(46, 125, 50),
            },
            Err(_) => Color::from_rgb(46, 125, 50),
        };
        let Ok(entities) = host.entities_batch(&handles) else {
            host.push_info("Panel Example: entity read refused by the host.");
            return;
        };
        let mut journals_handles = Vec::with_capacity(handles.len());
        let mut originals = Vec::with_capacity(handles.len());
        let mut applied = Vec::with_capacity(handles.len());
        let mut skipped = 0usize;
        for (handle, entity) in handles.iter().cloned().zip(entities.into_iter()) {
            let Some(entity) = entity else {
                skipped += 1;
                continue;
            };
            if entity_color(&entity).is_none() {
                skipped += 1;
                continue;
            }
            let recolored = recolor(entity.clone(), color).0;
            if recolored == entity {
                // Already exactly the target colour: nothing to journal.
                skipped += 1;
                continue;
            }
            journals_handles.push(handle);
            originals.push(entity);
            applied.push(recolored);
        }
        if applied.is_empty() {
            host.push_info(&format!(
                "Panel Example: nothing to apply ({skipped} skipped) — already at target colour or unsupported kinds."
            ));
            return;
        }
        host.begin_undo_group("Panel Example: apply colour");
        match host.update_entities_transaction(
            "Panel Example: apply colour",
            applied.clone(),
        ) {
            Ok(()) => {
                host.end_undo_group();
                let applied_len = applied.len();
                let pings = self.inner().pings.load(Ordering::SeqCst);
                let pings_text = if pings > 0 {
                    format!(" ({pings} pings)")
                } else {
                    String::new()
                };
                self.inner().journals.insert(
                    host.tab_id(),
                    Journal {
                        handles: journals_handles,
                        originals,
                        applied,
                    },
                );
                host.push_info(&format!(
                    "Panel Example: recoloured {} entities ({skipped} skipped){pings_text}. Revert restores the originals.",
                    applied_len
                ));
                self.refresh_summary("applied ✓", true);
            }
            Err(error) => {
                host.end_undo_group();
                host.push_info(&format!(
                    "Panel Example: apply refused ({error}); nothing changed."
                ));
            }
        }
    }

    /// Revert: restore journaled originals where the host still shows what
    /// Apply wrote; report conflicts instead of overwriting newer edits.
    fn revert_selection(&self, host: &mut dyn HostApi) {
        let tab = host.tab_id();
        let journal = match self.inner().journals.remove(&tab) {
            Some(journal) => journal,
            None => {
                host.push_info("Panel Example: nothing to revert (apply first).");
                return;
            }
        };
        let Ok(current) = host.entities_batch(&journal.handles) else {
            self.inner().journals.insert(tab, journal);
            host.push_info("Panel Example: entity read refused by the host.");
            return;
        };
        let mut restored = Vec::with_capacity(journal.applied.len());
        let mut conflicts = 0usize;
        let mut deleted = 0usize;
        for index in 0..journal.applied.len() {
            match &current[index] {
                None => deleted += 1,
                Some(entity) if *entity == journal.applied[index] => {
                    restored.push(journal.originals[index].clone());
                }
                Some(_) => conflicts += 1,
            }
        }
        if restored.is_empty() {
            host.push_info(&format!(
                "Panel Example: nothing restored ({conflicts} conflicted, {deleted} deleted were left untouched); journal kept for retry."
            ));
            self.inner().journals.insert(tab, journal);
            return;
        }
        host.begin_undo_group("Panel Example: revert colour");
        match host.update_entities_transaction(
            "Panel Example: revert colour",
            restored.clone(),
        ) {
            Ok(()) => {
                host.end_undo_group();
                let _ = host.set_selection(&journal.handles);
                let detail = if conflicts + deleted > 0 {
                    format!(" ({conflicts} conflicted, {deleted} deleted were left untouched)")
                } else {
                    String::from(" — all restored")
                };
                host.push_info(&format!(
                    "Panel Example: reverted {} entities{detail}.",
                    restored.len()
                ));
                self.refresh_summary("reverted ✓", true);
            }
            Err(error) => {
                host.end_undo_group();
                self.inner().journals.insert(tab, journal);
                host.push_info(&format!(
                    "Panel Example: revert refused ({error}); nothing changed."
                ));
            }
        }
    }

    /// Inspect: batch-read the selection (first 256) and report kinds.
    fn inspect_selection(&self, host: &mut dyn HostApi) {
        let handles = host.selection();
        if handles.is_empty() {
            host.push_info("Panel Example: nothing selected.");
            return;
        }
        let sample = &handles[..handles.len().min(256)];
        let Ok(entities) = host.entities_batch(sample) else {
            host.push_info("Panel Example: entity read refused by the host.");
            return;
        };
        let mut kinds: HashMap<&str, u64> = HashMap::new();
        for entity in entities.iter().flatten() {
            let kind = match entity {
                EntityType::Point(_) => "Point",
                EntityType::Line(_) => "Line",
                EntityType::Circle(_) => "Circle",
                EntityType::Arc(_) => "Arc",
                EntityType::Ellipse(_) => "Ellipse",
                EntityType::LwPolyline(_)
                | EntityType::Polyline(_)
                | EntityType::Polyline2D(_)
                | EntityType::Polyline3D(_) => "Polyline",
                EntityType::Text(_) => "Text",
                EntityType::MText(_) => "MText",
                EntityType::Insert(_) => "Insert",
                EntityType::Spline(_) => "Spline",
                EntityType::Hatch(_) => "Hatch",
                _ => "Other",
            };
            *kinds.entry(kind).or_insert(0) += 1;
        }
        let mut kinds: Vec<(&str, u64)> = kinds.into_iter().collect();
        kinds.sort();
        let detail = kinds
            .iter()
            .map(|(kind, count)| format!("{kind}={count}"))
            .collect::<Vec<_>>()
            .join(", ");
        host.push_info(&format!(
            "Panel Example: selection {}/{} ({detail})",
            sample.len(),
            handles.len()
        ));
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
        // Cache the thread-safe request line (v4 runner): the live digest
        // preview pushes its gated WritePanel through it.
        self.inner().sender = host.plugin_request_sender().map(Arc::from);
        // Seed tracked values from the panel defaults, then register.
        let echo = {
            let mut inner = self.inner();
            inner.values.clear();
            for control in demo_panel().controls {
                match control {
                    DialogControl::Text { id, default, .. } => {
                        inner.values.insert(id, DialogValue::Text(default));
                    }
                    DialogControl::Number { id, default, .. } => {
                        inner.values.insert(id, DialogValue::Number(default));
                    }
                    DialogControl::Checkbox { id, default, .. } => {
                        inner.values.insert(id, DialogValue::Bool(default));
                    }
                    DialogControl::Choice {
                        id,
                        options,
                        default_index,
                        ..
                    } => {
                        inner.values.insert(
                            id,
                            DialogValue::Choice {
                                index: default_index,
                                text: options.into_iter().nth(default_index).unwrap_or_default(),
                            },
                        );
                    }
                    DialogControl::Color { id, default, .. } => {
                        inner.values.insert(id, DialogValue::Color(default));
                    }
                    _ => {}
                }
            }
            summary(&inner.values, "")
        };
        if host.register_panel(demo_panel()) {
            // The label is in the definition already; this write shows the
            // update path as the first thing the plugin does.
            let update = HashMap::from([(LBL_SUMMARY.to_string(), DialogValue::Text(echo))]);
            let _ = host.update_panel(PANEL_ID, update);
            host.push_info("Panel Example loaded — type EPL_OPEN or use the ribbon tool.");
        } else {
            host.push_info("Panel Example loaded, but its panel was refused.");
        }
    }

    fn dispatch(&self, host: &mut dyn HostApi, cmd: &str) -> bool {
        match cmd {
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
                    let inner = self.inner();
                    let info = matches!(inner.values.get(CHK_INFO), Some(DialogValue::Bool(true)));
                    let echo = summary(&inner.values, "");
                    (echo, info)
                };
                let update = HashMap::from([(LBL_SUMMARY.to_string(), DialogValue::Text(echo.clone()))]);
                let _ = host.update_panel(PANEL_ID, update);
                if info {
                    host.push_info(&format!("Applied: {echo}"));
                }
                true
            }
            CMD_APPLY_SELECTION => {
                self.apply_selection(host);
                true
            }
            CMD_REVERT_SELECTION => {
                self.revert_selection(host);
                true
            }
            CMD_INSPECT => {
                self.inspect_selection(host);
                true
            }
            _ => false,
        }
    }

    fn on_notification(&mut self, _command_id: Option<u64>, notification: HostNotification) {
        match notification {
            // v22 digest: O(1), newest state wins, one gated write.
            HostNotification::SelectionChangedDigest {
                tab_id: _,
                revision,
                count,
            } => {
                let mut inner = self.inner();
                if inner.revision == revision && inner.count == count {
                    // The same selection facts twice: nothing to do.
                    return;
                }
                inner.revision = revision;
                inner.count = count;
                let sender = inner.sender.as_ref().map(|s| Arc::clone(s));
                let values = inner.values.clone();
                let Inner { last_panel_write, .. } = &mut *inner;
                send_summary(&sender, &values, &format!("sel {count} rev {revision}"), true, last_panel_write);
            }
            // Legacy fallback (a v21 host cannot load this v22 plugin, but the
            // defensive belt is cheap): track the biggest fact only.
            HostNotification::SelectionChangedV4 { tab_id: _, handles } => {
                let mut inner = self.inner();
                let count = handles.len() as u64;
                if inner.count != count {
                    inner.count = count;
                    let sender = inner.sender.as_ref().map(|s| Arc::clone(s));
                    let values = inner.values.clone();
                    let Inner { last_panel_write, .. } = &mut *inner;
                    send_summary(
                        &sender,
                        &values,
                        &format!("sel {count} (legacy)"),
                        true,
                        last_panel_write,
                    );
                }
            }
            // Host panel/widget events: track drafts and push gated refreshes.
            HostNotification::PanelAction {
                panel_id,
                control_id,
                action,
                value,
            } => {
                if panel_id != PANEL_ID {
                    return;
                }
                let mut inner = self.inner();
                match &action[..] {
                    "click" => {
                        if control_id == BTN_PING {
                            inner.pings.fetch_add(1, Ordering::SeqCst);
                        }
                        return;
                    }
                    "change" => {}
                    _ => return,
                }
                if let Some(value) = value {
                    inner.values.insert(control_id, value);
                }
                let sender = inner.sender.as_ref().map(|s| Arc::clone(s));
                let values = inner.values.clone();
                let Inner { last_panel_write, .. } = &mut *inner;
                send_summary(&sender, &values, "", false, last_panel_write);
            }
            // External document changes: nothing to do — the panel is
            // host-passive and Revert's conflict handling runs at dispatch
            // time.
            _ => {}
        }
    }
}

// Emit the C-ABI symbols the host loader looks for.
ocs_plugin_api::export_plugin!(ExamplePanelPlugin::new());

#[cfg(test)]
mod tests {
    use super::*;
    use ocs_plugin_api::host::acadrust::entities::Point;

    /// A request sender that records nothing and always answers ok — tests
    /// exercise the gate/lw logic without a host.
    struct NoopSender;
    impl PluginRequestSender for NoopSender {
        fn request(&self, _request: PluginRequest) -> Result<PluginResponse, ocs_plugin_api::host::PluginRequestError> {
            Ok(PluginResponse::Ok)
        }
    }

    fn drafts() -> HashMap<String, DialogValue> {
        HashMap::from([
            (TXT_SPECIMEN.to_string(), DialogValue::Text("ABC-123".into())),
            (NUM_LENGTH.to_string(), DialogValue::Number(7.5)),
            (CHK_INFO.to_string(), DialogValue::Bool(true)),
            (CLR_STROKE.to_string(), DialogValue::Color([46, 125, 50])),
        ])
    }

    #[test]
    fn point_recolors_and_reports() {
        let mut point = Point::from_coords(1.0, 2.0, 0.0);
        point.set_color(Color::ByLayer);
        let entity = EntityType::Point(point);
        let (recolored, supported) = recolor(entity.clone(), Color::from_rgb(1, 2, 3));
        assert!(supported);
        assert_eq!(entity_color(&recolored), Some(Color::from_rgb(1, 2, 3)));
        assert_ne!(recolored, entity, "the recolor must be visible to Revert");
        // Revert's restore = recolor back to the journalled original colour.
        let original = entity_color(&entity).unwrap();
        let (restored, supported) = recolor(recolored, original);
        assert!(supported);
        assert_eq!(restored, entity, "restore is exact");
    }

    #[test]
    fn summary_tracks_drafts() {
        let text = summary(&drafts(), "sel 2 rev 4");
        assert!(text.contains("sel 2 rev 4"));
        assert!(text.contains("#2E7D32"));
    }

    #[test]
    fn summary_gate_and_latest_wins() {
        let values = drafts();
        let sender: Option<Arc<dyn PluginRequestSender>> = Some(Arc::new(NoopSender));
        let mut last = None;
        // First write: enqueued and marked.
        assert!(send_summary(&sender, &values, "sel 1 rev 1", false, &mut last));
        // The gate holds an immediate second.
        assert!(!send_summary(&sender, &values, "sel 2 rev 2", false, &mut last));
        // Force (an explicit action) passes at once.
        assert!(send_summary(&sender, &values, "sel 2 rev 2", true, &mut last));
    }

    #[test]
    fn unsupported_kind_is_skipped_not_guessed() {
        // Seqend carries no common colour: both directions report skips.
        let seqend = EntityType::Seqend(ocs_plugin_api::host::acadrust::entities::Seqend::default());
        assert!(entity_color(&seqend).is_none());
        let (same, supported) = recolor(seqend, Color::from_rgb(1, 2, 3));
        assert!(!supported);
        assert!(matches!(same, EntityType::Seqend(_)));
    }
}
