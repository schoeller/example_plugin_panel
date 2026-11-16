//! The wire double the headless tests drive: a
//! [`PluginRequestSender`] (the same seam the root example's `NoopSender`
//! tests use) that serves the four v24 tails exactly the way the host's
//! panel-surface store does — the attach contract encoded by the api
//! crate's own tests and the `HostApi` v24 defaults:
//!
//! - publish legs re-run the crate `validate()` gate (`InvalidOp`), then
//!   the per-leg revision law (an equal head is the refresh shape only for
//!   IDENTICAL content; anything else `NonMonotonicRevision` — the store's
//!   documented rejection shapes);
//! - patch legs apply the crate's atomic `apply_patch(_report)` over the
//!   stored leg and map the crate's error vocabulary to the structured
//!   codes (`StaleBase` / `NonMonotonicRevision` keep the head as the
//!   report's recovery fact; every structural other lands in `InvalidOp`);
//! - store reads page with checked window bounds against the held rows
//!   (`total_hint` = the held count; a table's self-declared total when
//!   its def names one), past-end windows empty — never a bigger page;
//! - the snapshot2 answer leads with the [`StoreRow::State`] digest plus
//!   the bounded leading scene window — never a full dump.
//!
//! The widget surface id freezes ONCE to the published surface's FIRST
//! root's widget id (the registration convention the mount pins — the
//! plugin built the surface, so it can name that id). Deliberate
//! simplifications, noted inline: ONE owner (the single-publisher demo;
//! the `PublisherOwnedByOther` transfer-refusal arm is the multi-plugin
//! host's), no demand leg (the host's attach issues `PanelStoreDemand`
//! from page misses; the demo tests inject those notifications directly),
//! and non-v24 requests answer `Ok`.

use std::sync::{Mutex, MutexGuard};

use ocs_plugin_api::host::{PluginRequestError, PluginRequestSender};
use ocs_plugin_api::ipc::protocol::{
    PanelStoreKind, PatchRejectCode, PatchRejectReport, PluginRequest, PluginResponse,
    SceneOrWidgetPatch, SceneOrWidgetSurface, StoreRow, StoreStateDigest,
};
use ocs_plugin_api::scene::{
    AppliedPatchReport, PanelScene, SceneElement, SceneError, SceneRevision, SceneSummary, StableId,
};
use ocs_plugin_api::widgets::{
    TreeRowDef, WidgetError, WidgetNode, WidgetOp, WidgetRange, WidgetSurface, WidgetTreeDef,
    MAX_WIDGET_ROWS_INITIAL,
};

/// The store's held legs + the frozen surface id.
#[derive(Default)]
struct Store {
    scene: Option<PanelScene>,
    widgets: Option<WidgetSurface>,
    surface_id: Option<StableId>,
}

/// The test-side host. Send paths are the plugin's own; the assertions
/// read back through the accessors below.
pub struct FakeWireHost {
    store: Mutex<Store>,
    /// Request labels served, in arrival order (the wire facts the tests
    /// name).
    served: Mutex<Vec<&'static str>>,
}

impl Default for FakeWireHost {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeWireHost {
    pub fn new() -> Self {
        Self {
            store: Mutex::new(Store::default()),
            served: Mutex::new(Vec::new()),
        }
    }

    fn store(&self) -> MutexGuard<'_, Store> {
        self.store
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn record(&self, label: &'static str) {
        self.served
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(label);
    }

    // -- test accessors (reads only; the plugin state stays its own view) --

    /// The stored scene leg (a clone).
    pub fn scene(&self) -> Option<PanelScene> {
        self.store().scene.clone()
    }

    /// The stored scene leg's head.
    pub fn scene_head(&self) -> Option<SceneRevision> {
        self.store().scene.as_ref().map(|scene| scene.revision)
    }

    /// The stored widget surface (a clone).
    pub fn widgets(&self) -> Option<WidgetSurface> {
        self.store().widgets.clone()
    }

    /// The stored scene's bounded summary (the recovery-digest shape the
    /// tests read).
    pub fn scene_summary(&self) -> SceneSummary {
        self.store()
            .scene
            .as_ref()
            .map(PanelScene::scene_summary)
            .expect("the test read the stored scene leg")
    }

    /// The digest row a `PanelSceneSnapshot2` leads with: the scene
    /// digest's revision is the authoritative base fact.
    pub fn snapshot_digest_scene_revision(&self) -> Option<SceneRevision> {
        let store = self.store();
        store.scene.as_ref().map(|scene| scene.revision)
    }

    /// The stored scene's `Text` element content by id (the read-back the
    /// conflict demo asserts).
    pub fn label_content(&self, id: u64) -> Option<String> {
        let store = self.store();
        let scene = store.scene.as_ref()?;
        let mut walker: Vec<&SceneElement> = scene.elements.iter().collect();
        while let Some(element) = walker.pop() {
            match element {
                SceneElement::Group { children, .. } => walker.extend(children.iter()),
                SceneElement::Text {
                    id: element_id,
                    content,
                    ..
                } if element_id.0 == id => {
                    return Some(content.clone());
                }
                _ => {}
            }
        }
        None
    }

    /// The v24 tail labels served so far (in arrival order).
    pub fn served(&self) -> Vec<&'static str> {
        self.served
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    // -- the v24 serving (the store's semantics, mirrored) ----------------

    /// The two rejection constructors (codes only; the report carries the
    /// head as the recovery fact where the code left it known).
    fn reject(code: PatchRejectCode, current_revision: Option<SceneRevision>) -> PluginResponse {
        PluginResponse::PatchRejected {
            code,
            report: PatchRejectReport {
                current_revision,
                failed_op: None,
            },
        }
    }

    /// The crate's scene error → the structured wire code (the attach's
    /// SCENE-leg mapping: `StaleBase` / `NonMonotonicRevision` keep the
    /// head; every structural other lands in `InvalidOp`).
    fn scene_reject(error: &SceneError, head: SceneRevision) -> PluginResponse {
        match error {
            SceneError::StaleBase { current, .. } => {
                Self::reject(PatchRejectCode::StaleBase, Some(*current))
            }
            SceneError::NonMonotonicRevision { current, .. } => {
                Self::reject(PatchRejectCode::NonMonotonicRevision, Some(*current))
            }
            _ => Self::reject(PatchRejectCode::InvalidOp, Some(head)),
        }
    }

    /// The widget mirror of the scene mapping.
    fn widgets_reject(error: &WidgetError, head: SceneRevision) -> PluginResponse {
        match error {
            WidgetError::StaleBase { current, .. } => {
                Self::reject(PatchRejectCode::StaleBase, Some(*current))
            }
            WidgetError::NonMonotonicRevision { current, .. } => {
                Self::reject(PatchRejectCode::NonMonotonicRevision, Some(*current))
            }
            _ => Self::reject(PatchRejectCode::InvalidOp, Some(head)),
        }
    }

    /// A publish leg's bounded report (the store's shape: the arriving
    /// root ids are the touched set, the replaced leg's roots the retired
    /// set; the summary is the scene's own — an absent scene leg reduces
    /// to a zeroed summary at the frame's revision).
    fn publication_report(
        revision: SceneRevision,
        arriving_root_ids: Vec<StableId>,
        retired_root_ids: Vec<StableId>,
        summary: Option<SceneSummary>,
    ) -> AppliedPatchReport {
        AppliedPatchReport {
            revision,
            touched_subtree_ids: arriving_root_ids,
            touched_truncated: 0,
            retired_ids: retired_root_ids,
            retired_truncated: 0,
            summary: summary.unwrap_or_else(|| SceneSummary {
                revision,
                nodes: 0,
                by_kind: Default::default(),
                root_ids: Vec::new(),
                root_ids_truncated: 0,
                overflow_roots: false,
            }),
        }
    }

    /// A widget-patch leg's bounded report (facts from the ops themselves,
    /// pre-application — the store's rule).
    fn widget_report(
        revision: SceneRevision,
        ops: &[WidgetOp],
        scene_summary: Option<SceneSummary>,
    ) -> AppliedPatchReport {
        let touched = ops
            .iter()
            .map(|op| match op {
                WidgetOp::Add { widget, .. } => widget.widget_id(),
                WidgetOp::Update { id, .. } | WidgetOp::Remove { id } => *id,
                WidgetOp::TableCell { key, .. } => *key,
            })
            .collect();
        let retired: Vec<StableId> = ops
            .iter()
            .filter_map(|op| match op {
                WidgetOp::Remove { id } => Some(*id),
                _ => None,
            })
            .collect();
        Self::publication_report(revision, touched, retired, scene_summary)
    }

    /// The snapshot leg (the store's publish): the structural gate, the
    /// per-leg revision law, then the store.
    fn publish(&self, surface: SceneOrWidgetSurface) -> PluginResponse {
        let structural = match &surface {
            SceneOrWidgetSurface::Scene(scene) => scene.validate().is_ok(),
            SceneOrWidgetSurface::Widgets(surface) => surface.validate().is_ok(),
        };
        if !structural {
            return Self::reject(PatchRejectCode::InvalidOp, None);
        }
        let mut store = self.store();
        match surface {
            SceneOrWidgetSurface::Scene(scene) => {
                if let Some(stored) = store.scene.as_ref() {
                    let head = stored.revision;
                    if scene.revision == head {
                        if *stored == scene {
                            // Identical at the head: the refresh shape
                            // (idempotent; the store keeps the stored leg).
                            let touched: Vec<StableId> =
                                scene.elements.iter().map(|e| e.stable_id()).collect();
                            let summary = stored.scene_summary();
                            return PluginResponse::PatchApplied(Self::publication_report(
                                head,
                                touched,
                                Vec::new(),
                                Some(summary),
                            ));
                        }
                        return Self::reject(PatchRejectCode::NonMonotonicRevision, Some(head));
                    }
                    if scene.revision < head {
                        return Self::reject(PatchRejectCode::NonMonotonicRevision, Some(head));
                    }
                }
                let touched: Vec<StableId> = scene.elements.iter().map(|e| e.stable_id()).collect();
                let retired: Vec<StableId> = store
                    .scene
                    .replace(scene.clone())
                    .map(|replaced| replaced.elements.iter().map(|e| e.stable_id()).collect())
                    .unwrap_or_default();
                let revision = scene.revision;
                let summary = scene.scene_summary();
                PluginResponse::PatchApplied(Self::publication_report(
                    revision,
                    touched,
                    retired,
                    Some(summary),
                ))
            }
            SceneOrWidgetSurface::Widgets(surface) => {
                // The frozen identity: assigned ONCE per panel (first root
                // convention); re-publications KEEP it.
                let frozen = match store.surface_id {
                    Some(frozen) => frozen,
                    None => surface
                        .roots
                        .first()
                        .map(|node| node.widget_id())
                        .unwrap_or(StableId(0)),
                };
                if let Some(stored) = store.widgets.as_ref() {
                    let head = stored.revision;
                    if surface.revision == head {
                        if *stored == surface {
                            // Identical resurface at the head: the refresh
                            // shape (the mount preserves its running
                            // machines — a store fact the frame contract
                            // only mirrors).
                            let touched: Vec<StableId> =
                                surface.roots.iter().map(|n| n.widget_id()).collect();
                            let summary = store.scene.as_ref().map(PanelScene::scene_summary);
                            return PluginResponse::PatchApplied(Self::publication_report(
                                head,
                                touched,
                                Vec::new(),
                                summary,
                            ));
                        }
                        return Self::reject(PatchRejectCode::NonMonotonicRevision, Some(head));
                    }
                    if surface.revision < head {
                        return Self::reject(PatchRejectCode::NonMonotonicRevision, Some(head));
                    }
                }
                let touched: Vec<StableId> = surface.roots.iter().map(|n| n.widget_id()).collect();
                let retired: Vec<StableId> = store
                    .widgets
                    .replace(surface.clone())
                    .map(|replaced| replaced.roots.iter().map(|n| n.widget_id()).collect())
                    .unwrap_or_default();
                let revision = surface.revision;
                let summary = store.scene.as_ref().map(PanelScene::scene_summary);
                store.surface_id = Some(frozen);
                PluginResponse::PatchApplied(Self::publication_report(
                    revision, touched, retired, summary,
                ))
            }
        }
    }

    /// The incremental leg (the store's patch): the crate's atomic
    /// `apply_patch(_report)` over the stored leg; rejection = structured,
    /// zero mutation.
    fn patch(&self, patch: SceneOrWidgetPatch) -> PluginResponse {
        let mut store = self.store();
        match patch {
            SceneOrWidgetPatch::Scene(scene_patch) => {
                let Some(scene) = store.scene.as_mut() else {
                    return Self::reject(PatchRejectCode::PublisherUnattached, None);
                };
                match scene.apply_patch_report(scene_patch) {
                    Ok(report) => PluginResponse::PatchApplied(report),
                    Err(error) => Self::scene_reject(&error, scene.revision),
                }
            }
            SceneOrWidgetPatch::Widgets(widget_patch) => {
                // The report's facts come from the ops and the SCENE leg's
                // summary — read before the widget leg takes its borrow.
                let scene_summary = store.scene.as_ref().map(PanelScene::scene_summary);
                let Some(surface) = store.widgets.as_mut() else {
                    return Self::reject(PatchRejectCode::PublisherUnattached, None);
                };
                let head = surface.revision;
                let report =
                    Self::widget_report(widget_patch.revision, &widget_patch.ops, scene_summary);
                match surface.apply_patch(widget_patch) {
                    Ok(()) => PluginResponse::PatchApplied(report),
                    Err(error) => Self::widgets_reject(&error, head),
                }
            }
        }
    }

    /// The page server: one window of the addressed store, checked bounds
    /// (`page_window_bounds` — in-range portions serve, past-end windows
    /// answer EMPTY, never a panic), the width never exceeding the request
    /// nor the page wire budget.
    fn read(
        &self,
        kind: PanelStoreKind,
        parent_id: Option<StableId>,
        range: WidgetRange,
    ) -> PluginResponse {
        let store = self.store();
        let attached = store.scene.is_some() || store.widgets.is_some();
        if !attached {
            return PluginResponse::StorePage {
                rows: Vec::new(),
                total_hint: 0,
            };
        }
        match kind {
            // The SCENE leg resolves; a region read is never a page-miss
            // demand (the host store is the whole scene truth).
            PanelStoreKind::SceneRegion => {
                let Some(scene) = store.scene.as_ref() else {
                    return PluginResponse::StorePage {
                        rows: Vec::new(),
                        total_hint: 0,
                    };
                };
                let held = scene_region_flatten(&scene.elements, parent_id);
                let (start, width) = page_window_bounds(range.start, range.len, held.len());
                let rows: Vec<StoreRow> = held[start..start + width]
                    .iter()
                    .map(|element| StoreRow::Scene((*element).clone()))
                    .collect();
                PluginResponse::StorePage {
                    rows,
                    total_hint: held.len() as u64,
                }
            }
            // A resolved row's children (rows declaring the parent) or the
            // surface's root rows (parent `None`).
            PanelStoreKind::TreeChildren => {
                let Some(widgets) = store.widgets.as_ref() else {
                    return PluginResponse::StorePage {
                        rows: Vec::new(),
                        total_hint: 0,
                    };
                };
                let held: Vec<&TreeRowDef> = widget_tree_defs(widgets)
                    .iter()
                    .flat_map(|tree| tree.rows.iter())
                    .filter(|row| row.parent == parent_id)
                    .collect();
                let (start, width) = page_window_bounds(range.start, range.len, held.len());
                let rows: Vec<StoreRow> = held[start..start + width]
                    .iter()
                    .map(|row| StoreRow::Tree((*row).clone()))
                    .collect();
                PluginResponse::StorePage {
                    rows,
                    total_hint: held.len() as u64,
                }
            }
            // A list window (the demo publishes no lists — an honest empty
            // page for the kinds it does not carry).
            PanelStoreKind::ListItems => PluginResponse::StorePage {
                rows: Vec::new(),
                total_hint: 0,
            },
            // The typed-table rows window; `total_hint` = the def's own
            // declared total when it names one (a windowing table), else
            // the held count.
            PanelStoreKind::TableRows => {
                let Some(widgets) = store.widgets.as_ref() else {
                    return PluginResponse::StorePage {
                        rows: Vec::new(),
                        total_hint: 0,
                    };
                };
                let Some(table_id) = parent_id else {
                    return PluginResponse::StorePage {
                        rows: Vec::new(),
                        total_hint: 0,
                    };
                };
                let Some(def) = widgets.tables.get(&table_id) else {
                    return PluginResponse::StorePage {
                        rows: Vec::new(),
                        total_hint: 0,
                    };
                };
                let held: Vec<&_> = def.rows.iter().collect();
                let (start, width) = page_window_bounds(range.start, range.len, held.len());
                let rows: Vec<StoreRow> = held[start..start + width]
                    .iter()
                    .map(|row| StoreRow::Table((*row).clone()))
                    .collect();
                let total_hint = def.total_rows.unwrap_or(held.len() as u64);
                PluginResponse::StorePage { rows, total_hint }
            }
        }
    }

    /// The recovery read (the store's `snapshot_page`): the digest row
    /// FIRST, then at most the page budget of the leading scene window
    /// (parent `None` — the same flatten the region continuation pages
    /// over). Never a full dump.
    fn snapshot2(&self) -> PluginResponse {
        let store = self.store();
        if store.scene.is_none() && store.widgets.is_none() {
            return PluginResponse::StorePage {
                rows: vec![StoreRow::State(StoreStateDigest {
                    scene: None,
                    widgets: None,
                })],
                total_hint: 0,
            };
        }
        let digest = StoreRow::State(StoreStateDigest {
            scene: store.scene.as_ref().map(PanelScene::scene_summary),
            widgets: store.widgets.as_ref().map(WidgetSurface::widget_summary),
        });
        let leading: Vec<&SceneElement> = match store.scene.as_ref() {
            Some(scene) => scene_region_flatten(&scene.elements, None),
            None => Vec::new(),
        };
        // The digest row shares the page's budget: the window reserves its
        // one row.
        let (start, width) =
            page_window_bounds(0, MAX_WIDGET_ROWS_INITIAL.saturating_sub(1), leading.len());
        let mut rows: Vec<StoreRow> = Vec::with_capacity(width + 1);
        rows.push(digest);
        for element in &leading[start..start + width] {
            rows.push(StoreRow::Scene((*element).clone()));
        }
        PluginResponse::StorePage {
            rows,
            total_hint: leading.len() as u64,
        }
    }
}

impl PluginRequestSender for FakeWireHost {
    fn request(&self, req: PluginRequest) -> Result<PluginResponse, PluginRequestError> {
        let response = match req {
            PluginRequest::PanelScenePublish {
                panel_id: _,
                surface,
                events,
            } => {
                self.record("PanelScenePublish");
                // The events leg is the plugin's merge acknowledgment of
                // host-observed events — absorbed, NEVER re-forwarded (the
                // exactly-once law).
                let _ = events;
                self.publish(surface)
            }
            PluginRequest::PanelScenePatch { panel_id: _, patch } => {
                self.record("PanelScenePatch");
                self.patch(patch)
            }
            PluginRequest::PanelStoreRead {
                panel_id: _,
                kind,
                parent_id,
                range,
            } => {
                self.record("PanelStoreRead");
                self.read(kind, parent_id, range)
            }
            PluginRequest::PanelSceneSnapshot2 { panel_id: _ } => {
                self.record("PanelSceneSnapshot2");
                self.snapshot2()
            }
            // The headless tests exercise the v24 tails; everything else is
            // not this demo's contract.
            _ => {
                self.record("other");
                PluginResponse::Ok
            }
        };
        Ok(response)
    }
}

// -- the store's private page helpers (mirrored shapes) ------------------

/// The page window: `(start, width)` with `width` bounded by the request,
/// the held rows and the page budget; a held-empty or past-end window is
/// EMPTY (`(0, 0)` — never a panic, never a bigger page).
fn page_window_bounds(start: usize, requested_len: usize, held_len: usize) -> (usize, usize) {
    if held_len == 0 || start >= held_len {
        return (0, 0);
    }
    (
        start,
        requested_len
            .min(held_len - start)
            .min(MAX_WIDGET_ROWS_INITIAL),
    )
}

/// Tree defs in document order (one stack walk, the crate's
/// iterative-with-`.rev()` discipline; groups descend).
fn widget_tree_defs(surface: &WidgetSurface) -> Vec<&WidgetTreeDef> {
    let mut defs: Vec<&WidgetTreeDef> = Vec::new();
    let mut walker: Vec<&WidgetNode> = surface.roots.iter().collect();
    while let Some(node) = walker.pop() {
        match node {
            WidgetNode::Tree(def) => defs.push(def),
            WidgetNode::Group(group) => walker.extend(group.children.iter().rev()),
            _ => {}
        }
    }
    defs
}

/// The scene subtree resolution for a region read: `None` → the whole
/// scene, pre-order, document order (the snapshot2 window and the
/// continuation `PanelStoreKind::SceneRegion` windows share THE one
/// flatten); `Some(root)` → that element's subtree, inclusive, pre-order
/// (an unresolvable root is an empty window — not an error).
fn scene_region_flatten(elements: &[SceneElement], root: Option<StableId>) -> Vec<&SceneElement> {
    if let Some(root_id) = root {
        let mut subtree: Vec<&SceneElement> = Vec::new();
        let mut walker: Vec<&SceneElement> = elements.iter().rev().collect();
        while let Some(element) = walker.pop() {
            if element.stable_id() == root_id {
                scene_subtree_preorder(element, &mut subtree);
                return subtree;
            }
            if let SceneElement::Group { children, .. } = element {
                walker.extend(children.iter().rev());
            }
        }
        return subtree;
    }
    let mut flat: Vec<&SceneElement> = Vec::new();
    let mut walker: Vec<&SceneElement> = elements.iter().rev().collect();
    while let Some(element) = walker.pop() {
        flat.push(element);
        if let SceneElement::Group { children, .. } = element {
            walker.extend(children.iter().rev());
        }
    }
    flat
}

/// Inclusive pre-order of one validated element's subtree (the store only
/// holds `validate()`-passed scenes: depth ≤ the crate's own cap, so a
/// bounded recursion matches the host's own rationale).
fn scene_subtree_preorder<'a>(element: &'a SceneElement, out: &mut Vec<&'a SceneElement>) {
    out.push(element);
    if let SceneElement::Group { children, .. } = element {
        for child in children {
            scene_subtree_preorder(child, out);
        }
    }
}
