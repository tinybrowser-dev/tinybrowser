use std::cell::{Cell, Ref, RefCell, RefMut};
use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt::Write as _;
use std::hash::Hash;
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::time::{Duration, Instant};

use blitz_traits::node_id::NodeId as BlitzNodeId;
use net::ReferrerPolicy;
use rquickjs::{Constructor, Object, Persistent, Value, class::Trace, function::Function};
use url::Url;

use crate::documents::DocumentStore;
use crate::documents::JournalEntry;
use crate::dom::tree::{NodeId, attr, is_child_navigable_container, is_connected, is_html_tag};
use crate::dom_string::DomString;

use crate::js::FrameRuntime;
use crate::js::canvas::{Canvas2D, CanvasId, Gradient, Pattern, SourceImage};
use crate::messaging::{MAX_FRAMES, SharedHandle};
use crate::protocol::{
    FrameId, SandboxFlag, SandboxFlags, StorageChange, StorageError, StorageKey, StorageKind,
    TargetName,
};

pub(crate) use crate::protocol::PolicyContainer;
use crate::storage::PendingStorageEvent;
use crate::{Parsed, ReadyState};

#[derive(Default)]
pub(crate) struct RealmRegistry {
    pub(crate) realm_contexts: Vec<usize>,
    pub(crate) attributes: AttributeRegistry,
    pub(crate) observers: super::observers::MutationObservers,
    pub(crate) reactions: super::reactions::CustomElementReactions,
    pub(crate) private_slots: Option<PrivateSlots>,
    budget: Rc<RefCell<ResourceBudget>>,
    documents: HashMap<u32, Weak<RefCell<World>>>,
    frames: HashMap<FrameId, Weak<RefCell<World>>>,
    wrappers: WrapperCache<NodeId>,
    same_object: WrapperCache<(NodeId, SameObject)>,
    frame_documents: HashMap<NodeId, NodeId>,
    // https://dom.spec.whatwg.org/#concept-node-adopt
    // https://github.com/whatwg/dom/issues/977
    creation_realms: HashMap<NodeId, Weak<RefCell<World>>>,
    // https://dom.spec.whatwg.org/#concept-node-adopt
    adopted_ids: HashMap<NodeId, NodeId>,
    next_remote: u64,
    canvases: HashMap<CanvasId, CanvasSlot>,
    canvas_nodes: HashMap<NodeId, CanvasId>,
    next_canvas: u64,
    next_realm: u64,
    bitmaps: HashMap<u64, BitmapPixels>,
    pub(crate) svg_rasters: super::canvas::SvgRasterCache,
    next_bitmap: u64,
    next_decoder: u64,
    // https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#textFieldSelection
    selections: HashMap<NodeId, (u32, u32, u8)>,
    // https://w3c.github.io/selection-api/#selection-interface
    document_selections: HashMap<u32, DocumentSelection>,
    // https://html.spec.whatwg.org/multipage/interaction.html#focusable-area
    active_elements: HashMap<u32, NodeId>,
}

// https://w3c.github.io/selection-api/#selection-interface
#[derive(Clone, Default)]
pub(crate) struct DocumentSelection {
    pub(crate) start: Option<(NodeId, u32)>,
    pub(crate) end: Option<(NodeId, u32)>,
    pub(crate) direction: u8,
    pub(crate) version: u32,
}

pub(crate) struct PrivateSlots {
    pub(crate) factory: Persistent<Function<'static>>,
    pub(crate) realms: HashSet<usize>,
}

pub(crate) struct WrapperCache<K> {
    entries: HashMap<K, Persistent<Value<'static>>>,
}

impl<K> Default for WrapperCache<K> {
    fn default() -> Self {
        Self {
            entries: HashMap::new(),
        }
    }
}

impl<K: Eq + Hash> WrapperCache<K> {
    pub(crate) fn get(&self, key: &K) -> Option<Persistent<Value<'static>>> {
        self.entries.get(key).cloned()
    }

    pub(crate) fn insert(&mut self, key: K, value: Persistent<Value<'static>>) {
        self.entries.insert(key, value);
    }

    // https://dom.spec.whatwg.org/#concept-node-adopt
    pub(crate) fn rekey(&mut self, from: &K, to: K) {
        if let Some(value) = self.entries.remove(from) {
            self.entries.insert(to, value);
        }
    }

    pub(crate) fn retain(&mut self, mut keep: impl FnMut(&K) -> bool) {
        self.entries.retain(|key, _| keep(key));
    }

    pub(crate) fn clear(&mut self) {
        self.entries.clear();
    }

    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum SameObject {
    ClassList,
    Blocking,
    Sandbox,
    Attributes,
    Style,
    Dataset,
    ChildNodes,
    Scripts,
    Links,
    Images,
    Forms,
    Embeds,
    Canvas2d,
    CanvasBitmap,
}

impl SameObject {
    pub(crate) const ALL: [SameObject; 14] = [
        SameObject::ClassList,
        SameObject::Blocking,
        SameObject::Sandbox,
        SameObject::Attributes,
        SameObject::Style,
        SameObject::Dataset,
        SameObject::ChildNodes,
        SameObject::Scripts,
        SameObject::Links,
        SameObject::Images,
        SameObject::Forms,
        SameObject::Embeds,
        SameObject::Canvas2d,
        SameObject::CanvasBitmap,
    ];
}

pub(crate) const MAX_CANVAS_BYTES: usize = 64 * 1024 * 1024;
const MAX_TOTAL_BITMAP_BYTES: usize = 256 * 1024 * 1024;

pub(crate) fn canvas_bytes(width: u32, height: u32) -> Option<usize> {
    usize::try_from(width)
        .ok()?
        .checked_mul(usize::try_from(height).ok()?)?
        .checked_mul(4)
        .filter(|bytes| *bytes <= MAX_CANVAS_BYTES)
}

pub(crate) struct BitmapPixels {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) pixels: Arc<[u8]>,
    pub(crate) origin_clean: bool,
}

// https://html.spec.whatwg.org/multipage/canvas.html#offscreencanvas
#[derive(Clone, Copy, Debug)]
pub(crate) enum CanvasOwner {
    Element(NodeId),
    Offscreen { realm: u64, document: Option<u32> },
}

// https://html.spec.whatwg.org/multipage/canvas.html#concept-canvas-context-mode
// https://html.spec.whatwg.org/multipage/canvas.html#offscreencanvas-context-mode
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CanvasMode {
    None,
    TwoD,
    Bitmap,
    Detached,
}

pub(crate) struct CanvasSlot {
    pub(crate) owner: CanvasOwner,
    pub(crate) mode: CanvasMode,
    pub(crate) width: u32,
    pub(crate) height: u32,
    canvas: Option<Canvas2D>,
}

impl RealmRegistry {
    pub(crate) fn insert_bitmap(
        &mut self,
        width: u32,
        height: u32,
        pixels: Arc<[u8]>,
        origin_clean: bool,
    ) -> u64 {
        let incoming = pixels.len();
        let mut total: usize = self.bitmaps.values().map(|b| b.pixels.len()).sum();
        while total.saturating_add(incoming) > MAX_TOTAL_BITMAP_BYTES {
            let Some(&oldest) = self.bitmaps.keys().min() else {
                break;
            };
            if let Some(removed) = self.bitmaps.remove(&oldest) {
                total = total.saturating_sub(removed.pixels.len());
            }
        }
        self.next_bitmap = self.next_bitmap.saturating_add(1);
        let id = self.next_bitmap;
        self.bitmaps.insert(
            id,
            BitmapPixels {
                width,
                height,
                pixels,
                origin_clean,
            },
        );
        id
    }

    pub(crate) fn bitmap(&self, id: u64) -> Option<SourceImage> {
        let bitmap = self.bitmaps.get(&id)?;
        Some(SourceImage {
            width: bitmap.width,
            height: bitmap.height,
            pixels: bitmap.pixels.clone(),
            origin_clean: bitmap.origin_clean,
        })
    }

    pub(crate) fn remove_bitmap(&mut self, id: u64) {
        self.bitmaps.remove(&id);
    }

    fn allocate_canvas(&mut self) -> CanvasId {
        self.next_canvas = self.next_canvas.saturating_add(1);
        CanvasId(self.next_canvas)
    }

    pub(crate) fn allocate_realm(&mut self) -> u64 {
        self.next_realm = self.next_realm.saturating_add(1);
        self.next_realm
    }

    // https://html.spec.whatwg.org/multipage/canvas.html#dom-canvas-getcontext
    pub(crate) fn element_canvas(&mut self, node: NodeId, width: u32, height: u32) -> CanvasId {
        if let Some(id) = self.canvas_nodes.get(&node) {
            return *id;
        }
        let id = self.allocate_canvas();
        self.canvas_nodes.insert(node, id);
        self.canvases.insert(
            id,
            CanvasSlot {
                owner: CanvasOwner::Element(node),
                mode: CanvasMode::None,
                width,
                height,
                canvas: None,
            },
        );
        id
    }

    // https://html.spec.whatwg.org/multipage/canvas.html#offscreencanvas
    pub(crate) fn offscreen_canvas(
        &mut self,
        realm: u64,
        document: Option<u32>,
        width: u32,
        height: u32,
    ) -> CanvasId {
        let id = self.allocate_canvas();
        self.canvases.insert(
            id,
            CanvasSlot {
                owner: CanvasOwner::Offscreen { realm, document },
                mode: CanvasMode::None,
                width,
                height,
                canvas: None,
            },
        );
        id
    }

    pub(crate) fn canvas_id_for_node(&self, node: NodeId) -> Option<CanvasId> {
        self.canvas_nodes.get(&node).copied()
    }

    pub(crate) fn owns_canvas(&self, id: CanvasId, world: &World) -> bool {
        let Some(slot) = self.canvases.get(&id) else {
            return false;
        };
        match slot.owner {
            CanvasOwner::Element(node) => {
                world.owns_node(node)
                    && world
                        .with_document(node, |parsed| {
                            parsed
                                .document
                                .base
                                .get_node(node.node)
                                .is_some_and(|node| is_html_tag(Some(&node.data), "canvas"))
                        })
                        .unwrap_or(false)
            }
            CanvasOwner::Offscreen { realm, .. } => realm == world.realm_id(),
        }
    }

    pub(crate) fn canvas_mode(&self, id: CanvasId) -> Option<CanvasMode> {
        self.canvases.get(&id).map(|slot| slot.mode)
    }

    // https://html.spec.whatwg.org/multipage/canvas.html#concept-canvas-context-mode
    pub(crate) fn canvas_mode_for_node(&self, node: NodeId) -> Option<CanvasMode> {
        let id = self.canvas_nodes.get(&node)?;
        self.canvases.get(id).map(|slot| slot.mode)
    }

    pub(crate) fn forget_realm(&mut self, realm: u64) {
        self.canvases.retain(|_, slot| {
            !matches!(slot.owner, CanvasOwner::Offscreen { realm: owner, .. } if owner == realm)
        });
    }

    pub(crate) fn set_canvas_mode(&mut self, id: CanvasId, mode: CanvasMode) {
        if let Some(slot) = self.canvases.get_mut(&id) {
            slot.mode = mode;
        }
    }

    pub(crate) fn canvas_node(&self, id: CanvasId) -> Option<NodeId> {
        match self.canvases.get(&id)?.owner {
            CanvasOwner::Element(node) => Some(node),
            CanvasOwner::Offscreen { .. } => None,
        }
    }

    pub(crate) fn canvas_dimensions(&self, id: CanvasId) -> Option<(u32, u32)> {
        self.canvases.get(&id).map(|slot| (slot.width, slot.height))
    }

    // https://html.spec.whatwg.org/multipage/canvas.html#dom-canvas-width
    pub(crate) fn reset_canvas(&mut self, id: NodeId, width: u32, height: u32) {
        let Some(canvas_id) = self.canvas_nodes.get(&id).copied() else {
            return;
        };
        self.resize_canvas(canvas_id, width, height);
    }

    pub(crate) fn resize_canvas(&mut self, id: CanvasId, width: u32, height: u32) {
        let Some(slot) = self.canvases.get_mut(&id) else {
            return;
        };
        slot.width = width;
        slot.height = height;
        if let Some(canvas) = &mut slot.canvas {
            canvas.reset_for_resize(width, height);
        }
    }

    pub(crate) fn detach_canvas(&mut self, id: CanvasId) {
        if let Some(slot) = self.canvases.get_mut(&id) {
            slot.mode = CanvasMode::Detached;
            slot.width = 0;
            slot.height = 0;
            slot.canvas = None;
        }
    }

    pub(crate) fn canvas_mut(&mut self, id: CanvasId) -> Option<&mut Canvas2D> {
        let slot = self.canvases.get_mut(&id)?;
        if slot.mode == CanvasMode::Detached {
            return None;
        }
        let (width, height) = (slot.width, slot.height);
        Some(
            slot.canvas
                .get_or_insert_with(|| Canvas2D::new(width, height)),
        )
    }

    pub(crate) fn canvas_has_open_layer(&self, id: CanvasId) -> bool {
        self.canvases
            .get(&id)
            .and_then(|slot| slot.canvas.as_ref())
            .is_some_and(Canvas2D::has_open_layer)
    }

    pub(crate) fn canvas_origin_clean(&self, id: CanvasId) -> Option<bool> {
        self.canvases
            .get(&id)
            .and_then(|slot| slot.canvas.as_ref())
            .map(Canvas2D::origin_clean)
    }

    pub(crate) fn canvas_png(&self, id: CanvasId) -> Option<Vec<u8>> {
        self.canvases.get(&id)?.canvas.as_ref()?.to_png()
    }

    pub(crate) fn canvas_jpeg(
        &self,
        id: CanvasId,
        quality: crate::js::canvas::JpegQuality,
    ) -> Option<Vec<u8>> {
        self.canvases.get(&id)?.canvas.as_ref()?.to_jpeg(quality)
    }

    pub(crate) fn clear_canvas_bitmap(&mut self, id: CanvasId) -> bool {
        let Some(slot) = self.canvases.get_mut(&id) else {
            return false;
        };
        let Some(canvas) = &mut slot.canvas else {
            return false;
        };
        canvas.clear_bitmap();
        true
    }

    pub(crate) fn canvas_snapshot(&self, id: CanvasId) -> Option<SourceImage> {
        let canvas = self.canvases.get(&id)?.canvas.as_ref()?;
        Some(SourceImage {
            width: canvas.width,
            height: canvas.height,
            pixels: canvas
                .read_pixels(0, 0, canvas.width, canvas.height)?
                .into(),
            origin_clean: canvas.origin_clean(),
        })
    }

    pub(crate) fn document_canvas_snapshots(
        &self,
        doc_id: u32,
    ) -> Vec<(crate::dom::tree::BlitzId, u32, u32, Vec<u8>)> {
        let mut snapshots = Vec::new();
        for (&node, &canvas_id) in &self.canvas_nodes {
            if node.document_id() != doc_id {
                continue;
            }
            let Some(canvas) = self
                .canvases
                .get(&canvas_id)
                .and_then(|s| s.canvas.as_ref())
            else {
                continue;
            };
            if canvas.width == 0 || canvas.height == 0 {
                continue;
            }
            if let Some(pixels) = canvas.read_pixels(0, 0, canvas.width, canvas.height) {
                snapshots.push((node.node, canvas.width, canvas.height, pixels));
            }
        }
        snapshots
    }

    // https://html.spec.whatwg.org/multipage/canvas.html#canvasgradient
    pub(crate) fn canvas_gradient(&self, id: CanvasId, resource: u32) -> Option<Gradient> {
        self.canvases
            .get(&id)?
            .canvas
            .as_ref()?
            .gradient_definition(resource)
    }

    // https://html.spec.whatwg.org/multipage/canvas.html#canvaspattern
    pub(crate) fn canvas_pattern(&self, id: CanvasId, resource: u32) -> Option<Pattern> {
        self.canvases
            .get(&id)?
            .canvas
            .as_ref()?
            .pattern_definition(resource)
    }

    // https://tc39.es/ecma262/#sec-number.max_safe_integer
    pub(crate) fn allocate_remote(&mut self) -> u64 {
        self.next_remote = self.next_remote.saturating_add(1);
        self.next_remote
    }

    pub(crate) fn allocate_decoder(&mut self) -> u64 {
        self.next_decoder = self.next_decoder.saturating_add(1);
        self.next_decoder
    }
}

#[derive(Default)]
struct ResourceBudget {
    pending_stream_bytes: usize,
    object_url_bytes: usize,
}

pub(crate) struct StreamReservation {
    budget: Rc<RefCell<ResourceBudget>>,
    bytes: usize,
}

impl Drop for StreamReservation {
    fn drop(&mut self) {
        let mut budget = self.budget.borrow_mut();
        budget.pending_stream_bytes = budget.pending_stream_bytes.saturating_sub(self.bytes);
    }
}

pub(crate) struct HtmlWrite {
    pub(crate) html: String,
    _reservation: StreamReservation,
}

const MAX_PENDING_STREAM_BYTES: usize = 8 * 1024 * 1024;
const MAX_OBJECT_URL_BYTES: usize = 8 * 1024 * 1024;

// https://w3c.github.io/FileAPI/#dfn-createObjectURL
fn object_url_id() -> Option<String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).ok()?;
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let mut id = String::with_capacity(36);
    for (index, byte) in bytes.iter().enumerate() {
        if matches!(index, 4 | 6 | 8 | 10) {
            id.push('-');
        }
        let _ = write!(id, "{byte:02x}");
    }
    Some(id)
}

impl RealmRegistry {
    // https://w3c.github.io/selection-api/#selection-interface
    pub(crate) fn document_selection(&self, document: u32) -> Option<&DocumentSelection> {
        self.document_selections.get(&document)
    }

    // https://w3c.github.io/selection-api/#selection-interface
    pub(crate) fn update_document_selection(
        &mut self,
        document: u32,
        start: Option<(NodeId, u32)>,
        end: Option<(NodeId, u32)>,
        direction: u8,
    ) -> u32 {
        let selection = self.document_selections.entry(document).or_default();
        selection.version = selection.version.wrapping_add(1);
        selection.start = start;
        selection.end = end;
        selection.direction = direction;
        selection.version
    }

    pub(crate) fn insert_document(&mut self, id: u32, world: &Rc<RefCell<World>>) {
        self.documents.insert(id, Rc::downgrade(world));
    }

    pub(crate) fn insert_frame(&mut self, frame: FrameId, world: &Rc<RefCell<World>>) {
        self.frames.insert(frame, Rc::downgrade(world));
    }

    pub(crate) fn frame_world(&self, frame: FrameId) -> Option<Rc<RefCell<World>>> {
        self.frames.get(&frame).and_then(Weak::upgrade)
    }

    pub(crate) fn forget_frame_world(&mut self, frame: FrameId) {
        self.frames.remove(&frame);
    }

    // https://cookiestore.spec.whatwg.org/#process-cookie-changes
    pub(crate) fn live_worlds(&self) -> Vec<Rc<RefCell<World>>> {
        self.frames.values().filter_map(Weak::upgrade).collect()
    }

    pub(crate) fn owner_world(&self, id: NodeId) -> Option<Rc<RefCell<World>>> {
        self.documents
            .get(&id.document_id())
            .and_then(Weak::upgrade)
    }

    pub(crate) fn forget_document(&mut self, id: u32) {
        self.attributes.forget_document(id);
        self.documents.remove(&id);
        self.wrappers.retain(|node| node.document_id() != id);
        self.same_object
            .retain(|(node, _)| node.document_id() != id);
        self.canvas_nodes.retain(|node, _| node.document_id() != id);
        self.canvases.retain(|_, slot| match slot.owner {
            CanvasOwner::Element(node) => node.document_id() != id,
            CanvasOwner::Offscreen { document, .. } => document != Some(id),
        });
        self.frame_documents
            .retain(|_, document| document.document_id() != id);
        self.creation_realms
            .retain(|node, _| node.document_id() != id);
        self.adopted_ids
            .retain(|from, to| from.document_id() != id && to.document_id() != id);
        self.selections.retain(|node, _| node.document_id() != id);
        self.document_selections.remove(&id);
        self.active_elements.remove(&id);
    }

    // https://html.spec.whatwg.org/multipage/interaction.html#focused-area-of-the-document
    pub(crate) fn active_element(&self, document: u32) -> Option<NodeId> {
        self.active_elements.get(&document).copied()
    }

    pub(crate) fn set_active_element(&mut self, document: u32, node: Option<NodeId>) {
        match node {
            Some(node) => {
                self.active_elements.insert(document, node);
            }
            None => {
                self.active_elements.remove(&document);
            }
        }
    }

    // https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#textFieldSelection
    pub(crate) fn selection_of(&self, id: NodeId) -> (u32, u32, u8) {
        self.selections.get(&id).copied().unwrap_or((0, 0, 0))
    }

    pub(crate) fn set_selection(&mut self, id: NodeId, start: u32, end: u32, direction: u8) {
        self.selections.insert(id, (start, end, direction));
    }

    pub(crate) fn prune_dead_wrappers(&mut self, documents: &DocumentStore) {
        if self.wrappers.len() <= 256 {
            return;
        }
        self.wrappers.retain(|node| documents.contains_node(*node));
        self.same_object
            .retain(|(node, _)| documents.contains_node(*node));
        self.creation_realms
            .retain(|node, _| documents.contains_node(*node));
        self.canvas_nodes
            .retain(|node, _| documents.contains_node(*node));
        self.canvases.retain(|_, slot| match slot.owner {
            CanvasOwner::Element(node) => documents.contains_node(node),
            CanvasOwner::Offscreen { .. } => true,
        });
        self.adopted_ids
            .retain(|_, to| documents.contains_node(*to));
    }

    pub(crate) fn wrapper(&self, id: NodeId) -> Option<Persistent<Value<'static>>> {
        self.wrappers.get(&id)
    }

    pub(crate) fn same_object(
        &self,
        id: NodeId,
        member: SameObject,
    ) -> Option<Persistent<Value<'static>>> {
        self.same_object.get(&(id, member))
    }

    pub(crate) fn intern_same_object(
        &mut self,
        id: NodeId,
        member: SameObject,
        value: Persistent<Value<'static>>,
    ) {
        self.same_object.insert((id, member), value);
    }

    pub(crate) fn intern_wrapper(&mut self, id: NodeId, value: Persistent<Value<'static>>) {
        self.wrappers.insert(id, value);
    }

    // https://dom.spec.whatwg.org/#concept-node-adopt
    pub(crate) fn rekey_wrapper(&mut self, from: NodeId, to: NodeId) {
        if from != to {
            self.adopted_ids.insert(from, to);
        }
        self.wrappers.rekey(&from, to);
        for member in SameObject::ALL {
            self.same_object.rekey(&(from, member), (to, member));
        }
        if let Some(world) = self.creation_realms.remove(&from) {
            self.creation_realms.insert(to, world);
        }
        if let Some(canvas_id) = self.canvas_nodes.remove(&from) {
            self.canvas_nodes.insert(to, canvas_id);
            if let Some(slot) = self.canvases.get_mut(&canvas_id)
                && let CanvasOwner::Element(node) = &mut slot.owner
            {
                *node = to;
            }
        }
        for selection in self.document_selections.values_mut() {
            if let Some((node, _)) = &mut selection.start
                && *node == from
            {
                *node = to;
            }
            if let Some((node, _)) = &mut selection.end
                && *node == from
            {
                *node = to;
            }
        }
    }

    pub(crate) fn stamp_creation_realm(&mut self, id: NodeId, world: &Rc<RefCell<World>>) {
        self.creation_realms
            .entry(id)
            .or_insert_with(|| Rc::downgrade(world));
    }

    pub(crate) fn creation_realm(&self, id: NodeId) -> Option<Rc<RefCell<World>>> {
        self.creation_realms.get(&id).and_then(Weak::upgrade)
    }

    pub(crate) fn live_node_id(&mut self, mut id: NodeId) -> NodeId {
        let mut path = Vec::new();
        let mut cyclic = false;
        while let Some(&next) = self.adopted_ids.get(&id) {
            if path.contains(&id) {
                cyclic = true;
                break;
            }
            path.push(id);
            id = next;
        }
        if !cyclic {
            for stale in path {
                self.adopted_ids.insert(stale, id);
            }
        }
        id
    }

    pub(crate) fn frame_document(&self, container: NodeId) -> Option<NodeId> {
        self.frame_documents.get(&container).copied()
    }

    pub(crate) fn set_frame_document(&mut self, container: NodeId, document: NodeId) {
        self.frame_documents.insert(container, document);
    }

    pub(crate) fn forget_frame(&mut self, container: NodeId) {
        self.frame_documents.remove(&container);
    }

    pub(crate) fn clear(&mut self) {
        super::bindings::forget_registry_contexts(&self.realm_contexts);
        self.realm_contexts.clear();
        self.attributes.clear();
        self.reactions.clear();
        self.private_slots = None;
        self.documents.clear();
        self.frames.clear();
        self.wrappers.clear();
        self.same_object.clear();
        self.frame_documents.clear();
        self.creation_realms.clear();
        self.adopted_ids.clear();
        self.canvases.clear();
        self.canvas_nodes.clear();
        self.bitmaps.clear();
        self.selections.clear();
        self.document_selections.clear();
        self.active_elements.clear();
    }

    fn budget(&self) -> Rc<RefCell<ResourceBudget>> {
        Rc::clone(&self.budget)
    }
}

#[derive(Clone)]
pub(crate) enum NavigationTarget {
    Container(NodeId),
    Frame(FrameId),
    Target(crate::protocol::TargetName),
}

// https://html.spec.whatwg.org/multipage/nav-history-apis.html#dom-open
#[derive(Clone, Copy, Debug)]
pub(crate) enum NavigationReferrer {
    Client {
        // https://html.spec.whatwg.org/multipage/links.html#hyperlink-referrer-policy
        policy: Option<ReferrerPolicy>,
    },
    NoReferrer,
}

impl Default for NavigationReferrer {
    fn default() -> Self {
        Self::Client { policy: None }
    }
}

#[derive(Clone)]
pub(crate) struct FrameNavigation {
    pub(crate) target: NavigationTarget,
    pub(crate) spec: String,
    // https://html.spec.whatwg.org/multipage/iframe-embed-object.html#attr-iframe-srcdoc
    pub(crate) srcdoc: Option<String>,
    pub(crate) method: net::Method,
    pub(crate) body: Vec<u8>,
    pub(crate) content_type: Option<String>,
    pub(crate) referrer: NavigationReferrer,
    // https://html.spec.whatwg.org/multipage/browsing-the-web.html#determine-navigation-params
    pub(crate) sandbox: SandboxFlags,
    // https://html.spec.whatwg.org/multipage/nav-history-apis.html#dom-location-href
    pub(crate) source: Option<FrameId>,
}

impl FrameNavigation {
    pub(crate) fn get(target: NavigationTarget, spec: String) -> Self {
        Self {
            target,
            spec,
            srcdoc: None,
            method: net::Method::GET,
            body: Vec::new(),
            content_type: None,
            referrer: NavigationReferrer::default(),
            sandbox: SandboxFlags::default(),
            source: None,
        }
    }

    // https://html.spec.whatwg.org/multipage/iframe-embed-object.html#attr-iframe-srcdoc
    pub(crate) fn srcdoc(target: NavigationTarget, markup: String) -> Self {
        let mut navigation = Self::get(target, String::new());
        navigation.srcdoc = Some(markup);
        navigation
    }

    pub(crate) fn with_referrer_policy(mut self, policy: Option<ReferrerPolicy>) -> Self {
        if let NavigationReferrer::Client { policy: current } = &mut self.referrer {
            *current = policy;
        }
        self
    }

    pub(crate) fn with_sandbox(mut self, sandbox: SandboxFlags) -> Self {
        self.sandbox = sandbox;
        self
    }

    pub(crate) fn without_referrer(mut self) -> Self {
        self.referrer = NavigationReferrer::NoReferrer;
        self
    }

    pub(crate) fn with_source(mut self, source: FrameId) -> Self {
        self.source = Some(source);
        self
    }
}

#[derive(Clone, Copy)]
pub(crate) enum DocumentStreamCommand {
    Open,
    Close,
}

#[derive(Clone, Copy, rquickjs::JsLifetime)]
pub(crate) struct Handle(pub(crate) NodeId);

impl<'js> Trace<'js> for Handle {
    fn trace<'a>(&self, _tracer: rquickjs::class::Tracer<'a, 'js>) {}
}

#[expect(
    clippy::struct_excessive_bools,
    reason = "each bool is a MutationObserverInit dictionary member"
)]
#[derive(Clone, Default)]
pub(crate) struct ObserverOptions {
    pub child_list: bool,
    pub attributes: bool,
    pub character_data: bool,
    pub subtree: bool,
    pub attribute_old_value: bool,
    pub character_data_old_value: bool,
    pub attribute_filter: Option<Vec<Vec<u16>>>,
}

#[derive(Clone)]
pub(crate) struct Observation {
    pub target: NodeReference,
    pub options: ObserverOptions,
    pub order: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum NodeReference {
    Tree(NodeId),
    Attribute { scope: NodeId, id: u64 },
}

impl NodeReference {
    pub(crate) fn tree(self) -> Option<NodeId> {
        match self {
            Self::Tree(id) => Some(id),
            Self::Attribute { .. } => None,
        }
    }

    pub(crate) fn scope(self) -> NodeId {
        match self {
            Self::Tree(id) | Self::Attribute { scope: id, .. } => id,
        }
    }
}

#[derive(Clone, rquickjs::JsLifetime, Trace)]
pub(crate) struct RecordData {
    pub typ: String,
    pub target: Handle,
    pub added: Vec<Handle>,
    pub removed: Vec<Handle>,
    pub previous: Option<Handle>,
    pub next: Option<Handle>,
    pub attribute_name: Option<String>,
    pub attribute_namespace: Option<String>,
    #[qjs(skip_trace)]
    pub old_value: Option<crate::dom_string::DomString>,
}

impl RecordData {
    pub(crate) fn new(typ: &str, target: Handle) -> Self {
        Self {
            typ: typ.to_owned(),
            target,
            added: Vec::new(),
            removed: Vec::new(),
            previous: None,
            next: None,
            attribute_name: None,
            attribute_namespace: None,
            old_value: None,
        }
    }
}

pub(crate) struct ObserverState {
    pub owner: FrameId,
    pub callback: Persistent<Function<'static>>,
    // https://dom.spec.whatwg.org/#notify-mutation-observers
    pub object: Option<Persistent<Object<'static>>>,
    pub observations: Vec<Observation>,
    // https://dom.spec.whatwg.org/#mutation-observers
    pub transient: Vec<Observation>,
    pub queue: Vec<RecordData>,
}

pub(crate) struct ReadyObserver {
    pub callback: Persistent<Function<'static>>,
    pub object: Persistent<Object<'static>>,
    pub records: Vec<RecordData>,
}

pub(crate) struct Listener {
    pub typ: String,
    pub callback: Option<Persistent<Value<'static>>>,
    pub capture: bool,
    pub once: bool,
    pub passive: bool,
    pub signal: Option<Persistent<Object<'static>>>,
    // https://dom.spec.whatwg.org/#concept-event-listener
    pub removed: Cell<bool>,
    // https://html.spec.whatwg.org/multipage/webappapis.html#the-event-handler-processing-algorithm
    pub handler_name: Option<String>,
    // https://chromedevtools.github.io/devtools-protocol/tot/Page/#method-createIsolatedWorld
    pub realm: u64,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum EventTargetKey {
    Window,
    Node(NodeId),
    Attribute { scope: NodeId, id: u64 },
    Standalone(u64),
}

impl From<Option<NodeId>> for EventTargetKey {
    fn from(node: Option<NodeId>) -> Self {
        node.map_or(EventTargetKey::Window, EventTargetKey::Node)
    }
}

// https://w3c.github.io/FileAPI/#blob-url
// https://w3c.github.io/FileAPI/#blob-url-store
pub(crate) struct ObjectUrlEntry {
    pub(crate) contents: Arc<str>,
    pub(crate) content_type: Arc<str>,
    pub(crate) origin: url::Origin,
    frame: FrameId,
    document: Option<u32>,
}

// https://w3c.github.io/FileAPI/#blob-url-store
pub(crate) fn object_url_key(url: &str) -> &str {
    url.split_once('#').map_or(url, |(head, _)| head)
}

pub(crate) fn same_document_url(document_url: &Url, url: &Url) -> bool {
    let mut own = document_url.clone();
    let mut other = url.clone();
    own.set_fragment(None);
    other.set_fragment(None);
    own == other
}

fn opaque_url_matches(
    document_url: &Url,
    captured: &url::Origin,
    url: &Url,
    blob_origin: Option<&url::Origin>,
) -> bool {
    if same_document_url(document_url, url) {
        return true;
    }
    url.scheme() == "blob" && blob_origin.is_some_and(|origin| origin == captured)
}

#[cfg(test)]
mod origin_match_tests {
    use super::{Url, opaque_url_matches};

    #[test]
    fn opaque_about_blank_rejects_data_and_file() {
        let document_url = Url::parse("about:blank").expect("about:blank");
        let origin = document_url.origin();
        assert!(!opaque_url_matches(
            &document_url,
            &origin,
            &Url::parse("data:text/html,x").expect("data"),
            None,
        ));
        assert!(!opaque_url_matches(
            &document_url,
            &origin,
            &Url::parse("file:///tmp/x").expect("file"),
            None,
        ));
        assert!(opaque_url_matches(
            &document_url,
            &origin,
            &Url::parse("about:blank").expect("about:blank"),
            None,
        ));
        assert!(opaque_url_matches(
            &document_url,
            &origin,
            &Url::parse("about:blank#frag").expect("fragment"),
            None,
        ));
        let blob = Url::parse("blob:null/00000000-0000-4000-8000-000000000000").expect("blob");
        assert!(!opaque_url_matches(&document_url, &origin, &blob, None));
        assert!(opaque_url_matches(
            &document_url,
            &origin,
            &blob,
            Some(&origin),
        ));
    }
}

pub(crate) struct WeakReferences {
    pub(crate) constructor: Persistent<rquickjs::function::Constructor<'static>>,
    pub(crate) deref: Persistent<Function<'static>>,
}

pub(crate) type ChildFrameHost = Rc<dyn Fn(&Rc<RefCell<World>>, FrameId, NodeId)>;

pub(crate) type AuxiliaryFrameHost =
    Rc<dyn Fn(&Rc<RefCell<World>>, FrameId, crate::protocol::OpenerPolicy)>;

#[derive(Clone)]
pub(crate) struct FrameHost {
    pub(crate) child: ChildFrameHost,
    pub(crate) auxiliary: AuxiliaryFrameHost,
}

pub(crate) type FontFaceKey = (
    parley::fontique::FontWidth,
    parley::fontique::FontStyle,
    parley::fontique::FontWeight,
);

pub(crate) type FontFamilyEntry = (parley::fontique::FamilyId, Vec<FontFaceKey>);

pub(crate) struct FontRegistration {
    pub(crate) document: Option<u32>,
    pub(crate) document_fonts: Vec<FontFamilyEntry>,
    pub(crate) runtime_fonts: Vec<FontFamilyEntry>,
}

// https://html.spec.whatwg.org/multipage/urls-and-fetching.html#cors-settings-attributes
// https://html.spec.whatwg.org/multipage/urls-and-fetching.html#referrer-policy-attributes
#[derive(Default)]
pub(crate) struct SubresourceOptions {
    pub(crate) crossorigin: Option<blitz_traits::net::CorsSetting>,
    pub(crate) referrer_policy: Option<blitz_traits::net::ReferrerPolicy>,
}

// https://html.spec.whatwg.org/multipage/workers.html#dedicatedworkerglobalscope
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum GlobalKind {
    Document,
    DedicatedWorker {
        id: crate::worker::WorkerId,
        parent: FrameId,
        name: String,
        // https://html.spec.whatwg.org/multipage/workers.html#dom-workerglobalscope-importscripts
        module: bool,
    },
    // https://html.spec.whatwg.org/multipage/workers.html#shared-workers-and-the-sharedworkerglobalscope-interface
    SharedWorker {
        id: crate::worker::WorkerId,
        name: String,
        module: bool,
    },
}

#[derive(Clone, Debug)]
pub(crate) struct ParentContext {
    pub(crate) origin: url::Origin,
    pub(crate) first_party: bool,
}

// https://html.spec.whatwg.org/multipage/window-object.html#windowproxy-set
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProxyReplay {
    // https://html.spec.whatwg.org/multipage/browsing-the-web.html#navigate
    Owed,
    Done,
}

// https://chromedevtools.github.io/devtools-protocol/tot/Page/#method-createIsolatedWorld
pub(crate) struct IsolatedRealm {
    pub(crate) name: String,
    // https://w3c.github.io/webappsec-csp/#csp-list
    pub(crate) csp: Option<crate::csp::List>,
}

#[derive(Default)]
pub(crate) struct RealmCaches {
    // https://html.spec.whatwg.org/multipage/common-dom-interfaces.html#reflecting-content-attributes-in-idl-attributes:element
    pub(crate) element_reference_cache: HashMap<(NodeId, String), ElementReferenceCache>,
    // https://html.spec.whatwg.org/multipage/dom.html#dom-document-nameditem
    pub(crate) document_named: HashMap<(NodeId, String), Persistent<Value<'static>>>,
    pub(crate) implementations: HashMap<u32, Persistent<Value<'static>>>,
    // https://html.spec.whatwg.org/multipage/forms.html#dom-form-nameditem
    pub(crate) form_named_lists: HashMap<(NodeId, String), Persistent<Value<'static>>>,
}

impl RealmCaches {
    pub(crate) fn clear(&mut self) {
        self.element_reference_cache.clear();
        self.document_named.clear();
        self.implementations.clear();
        self.form_named_lists.clear();
    }

    pub(crate) fn forget_document(&mut self, document: u32) {
        self.element_reference_cache
            .retain(|(node, _), _| node.document != document);
        self.document_named
            .retain(|(node, _), _| node.document != document);
        self.implementations.remove(&document);
        self.form_named_lists
            .retain(|(node, _), _| node.document != document);
    }
}

// https://html.spec.whatwg.org/multipage/dom.html#document
pub(crate) struct DocumentData {
    pub(crate) document: Option<u32>,
    pub(crate) owned: HashSet<u32>,
    pub(crate) pending_frames: Vec<(FrameId, NodeId)>,
    pub(crate) deferred_scripts: Vec<NodeId>,
    pub document_url: Url,
    // https://cookiestore.spec.whatwg.org/#dom-cookiestore-get
    pub(crate) creation_url: Url,
    // https://html.spec.whatwg.org/multipage/dom.html#dom-document-cookie
    pub(crate) inherited_cookie_url: Option<Url>,
    // https://w3c.github.io/webappsec-referrer-policy/#determine-requests-referrer
    pub(crate) referrer_url: Url,
    // https://html.spec.whatwg.org/multipage/document-lifecycle.html#creating-and-initializing-a-document-object
    pub(crate) document_referrer: Option<Url>,
    // https://html.spec.whatwg.org/multipage/urls-and-fetching.html#fallback-base-url
    pub(crate) base_url: Option<Url>,
    // https://html.spec.whatwg.org/multipage/browsers.html#concept-origin
    pub(crate) origin: url::Origin,
    // https://html.spec.whatwg.org/multipage/browsers.html#relaxing-the-same-origin-restriction
    pub(crate) domain: Rc<RefCell<Option<String>>>,
    // https://html.spec.whatwg.org/multipage/webappapis.html#concept-environment-top-level-origin
    pub(crate) top_level: Option<url::Origin>,
    // https://privacycg.github.io/storage-access/#first-party-site-context
    pub(crate) first_party: bool,
    pub(crate) parent_context: Option<ParentContext>,
    // https://privacycg.github.io/saa-non-cookie-storage/#shared-worker
    pub(crate) same_site_cookie_policy: net::SameSiteCookiePolicy,
    pub(crate) policy: PolicyContainer,
    // https://html.spec.whatwg.org/multipage/webappapis.html#import-maps
    pub(crate) import_map: crate::js::modules::ImportMap,
    // https://html.spec.whatwg.org/multipage/browsers.html#sandboxing-flag-set
    pub(crate) sandbox: SandboxFlags,
    pub(crate) history: crate::protocol::HistorySnapshot,
    // https://html.spec.whatwg.org/multipage/input.html#dom-input-files
    pub(crate) input_files: HashMap<NodeId, Vec<Persistent<Value<'static>>>>,
    pub(crate) input_file_list: HashMap<NodeId, Persistent<Value<'static>>>,
    pub(crate) font_registrations: HashMap<u32, FontRegistration>,
    pub(crate) next_font_registration: u32,
    pub parser_active: bool,
    pub(crate) state: DocumentState,
    // https://html.spec.whatwg.org/multipage/iframe-embed-object.html#the-iframe-element:html-element-insertion-steps
    pub(crate) insertion_load_fired: HashSet<NodeId>,
}

impl DocumentData {
    // https://privacycg.github.io/storage-access/#first-party-site-context
    fn refresh_first_party(&mut self) {
        self.first_party = self.parent_context.as_ref().is_none_or(|parent| {
            parent.first_party && net::origins_same_site(&self.origin, &parent.origin)
        });
    }
}

// https://html.spec.whatwg.org/multipage/nav-history-apis.html#window
pub(crate) struct WindowData {
    pub(crate) frame: FrameId,
    pub(crate) own_window: Option<crate::protocol::WindowId>,
    // https://chromedevtools.github.io/devtools-protocol/tot/Page/#method-addScriptToEvaluateOnNewDocument
    pub(crate) init_scripts: Rc<RefCell<Vec<(u64, String)>>>,
    // https://chromedevtools.github.io/devtools-protocol/tot/Runtime/#method-addBinding
    pub(crate) bindings: Rc<RefCell<Vec<String>>>,
    pub(crate) viewport_size: Cell<(u32, u32)>,
    pub(crate) frame_host: Option<FrameHost>,
    // https://chromedevtools.github.io/devtools-protocol/tot/Runtime/#method-evaluate — replies live outside the realm: a document.open() step replaces it mid-evaluation.
    pub(crate) evaluation_replies: Rc<RefCell<HashMap<String, String>>>,
    pub(crate) events: EventTargetState,
    // https://html.spec.whatwg.org/multipage/interaction.html#dom-click
    pub(crate) clicks_in_progress: HashSet<NodeId>,
    // https://html.spec.whatwg.org/multipage/interaction.html#transient-activation
    pub(crate) transient_activation: Option<Instant>,
    // https://html.spec.whatwg.org/multipage/interaction.html#dom-useractivation-hasbeenactive
    pub(crate) sticky_activation: bool,
    // https://dom.spec.whatwg.org/#dom-event-timestamp
    pub(crate) event_clock: (Instant, f64),
}

pub(crate) struct World {
    pub(crate) runtime: FrameRuntime,
    pub(crate) kind: GlobalKind,
    realm: u64,
    pub(crate) doc: Rc<RefCell<DocumentData>>,
    pub(crate) win: Rc<RefCell<WindowData>>,
    pub(crate) realm_context: Option<rquickjs::Context>,
    // https://html.spec.whatwg.org/multipage/workers.html#dom-dedicatedworkerglobalscope-close
    pub(crate) worker_close_requested: Rc<Cell<bool>>,
    // https://w3c.github.io/webappsec-csp/#can-compile-strings
    pub(crate) compiling_handler: Cell<bool>,
    // https://chromedevtools.github.io/devtools-protocol/tot/Runtime/#method-evaluate
    pub(crate) browser_eval_depth: Cell<usize>,
    // https://chromedevtools.github.io/devtools-protocol/tot/Page/#method-createIsolatedWorld
    pub(crate) isolated: Option<IsolatedRealm>,
    isolated_wrappers: RefCell<HashMap<NodeId, Persistent<Value<'static>>>>,
    // https://html.spec.whatwg.org/multipage/timers-and-user-prompts.html#timer-initialisation-steps
    pub(crate) timer_string: Cell<bool>,
    pub(crate) outbox: Outbox,
    budget: Rc<RefCell<ResourceBudget>>,
    window_object: Option<Persistent<Object<'static>>>,
    standalone_targets: HashMap<u64, Persistent<Object<'static>>>,
    standalone_listeners: HashMap<u64, Vec<Rc<Listener>>>,
    next_standalone_target: u64,
    // https://html.spec.whatwg.org/multipage/window-object.html#windowproxy-set
    proxy_replay: ProxyReplay,
    pub(crate) caches: RealmCaches,
    brands: HashMap<String, Persistent<Object<'static>>>,
    pub(crate) bridge: Option<Persistent<Object<'static>>>,
    pub(crate) location_object: Option<Persistent<Object<'static>>>,
    // https://dom.spec.whatwg.org/#concept-event-listener-inner-invoke
    pub(crate) current_event: Option<Persistent<Value<'static>>>,
    pub(crate) pristine_number: Option<Persistent<Function<'static>>>,
    pub(crate) pristine_boolean: Option<Persistent<Function<'static>>>,
    pub(crate) pristine_reflect_set: Option<Persistent<Function<'static>>>,
    pub(crate) pristine_queue_microtask: Option<Persistent<Function<'static>>>,
    pub(crate) pristine_eval_error: Option<Persistent<Function<'static>>>,
    pub(crate) pristine_blob: Option<Persistent<Constructor<'static>>>,
    pub(crate) pristine_set_timeout: Option<Persistent<Function<'static>>>,
    pub(crate) event_ctors: HashMap<String, Persistent<Object<'static>>>,
    pub(crate) weak_references: Option<WeakReferences>,
    pub(crate) deliver_mutations_fn: Option<Persistent<Function<'static>>>,
}

// https://html.spec.whatwg.org/multipage/webappapis.html#event-handler-value
#[derive(Clone)]
pub(crate) enum HandlerValue {
    Null,
    Value(Persistent<Value<'static>>),
    Raw(String),
}

// https://html.spec.whatwg.org/multipage/webappapis.html#event-handler-map
#[derive(Clone)]
pub(crate) struct EventHandlerSlot {
    pub(crate) value: HandlerValue,
    pub(crate) listener: Option<Rc<Listener>>,
}

// https://dom.spec.whatwg.org/#interface-eventtarget
// https://html.spec.whatwg.org/multipage/webappapis.html#event-handlers
#[derive(Default)]
pub(crate) struct EventTargetState {
    listeners: HashMap<EventTargetKey, Vec<Rc<Listener>>>,
    // https://html.spec.whatwg.org/multipage/webappapis.html#event-handlers
    handlers: HashMap<(Option<NodeId>, String), EventHandlerSlot>,
}

impl EventTargetState {
    // https://html.spec.whatwg.org/multipage/webappapis.html#event-handlers
    fn reset_for_navigation(&mut self) {
        self.listeners.clear();
        self.handlers.retain(|(node, _), slot| {
            if node.is_none() {
                slot.listener = None;
                true
            } else {
                false
            }
        });
    }

    fn forget_document(&mut self, document: u32) {
        self.handlers
            .retain(|(node, _), _| node.is_none_or(|node| node.document_id() != document));
    }

    fn forget_realm(&mut self, realm: u64) {
        for list in self.listeners.values_mut() {
            list.retain(|listener| listener.realm != realm);
        }
        self.listeners.retain(|_, list| !list.is_empty());
    }

    fn clear_realm(&mut self) {
        self.listeners.clear();
        self.handlers.clear();
    }
}

pub(crate) enum RealmRequest {
    Timer(super::TimeoutRequest),
    ClearTimer(super::ClearTimeoutRequest),
    Fetch(Box<super::JsFetchRequest>),
    AbortFetch(super::AbortFetchRequest),
    Script(NodeId),
    HistoryPush {
        url: String,
        state: String,
        handling: crate::protocol::HistoryHandling,
    },
    HistoryTraverse {
        delta: i32,
    },
    // https://html.spec.whatwg.org/multipage/browsing-the-web.html#navigate-to-a-fragment
    FragmentNavigation {
        old_url: String,
        url: Url,
        handling: crate::protocol::HistoryHandling,
    },
    // https://cookiestore.spec.whatwg.org/#process-cookie-changes
    CookieChanges,
    SelectionChanged(NodeId),
    // https://html.spec.whatwg.org/multipage/interactive-elements.html#queue-a-details-toggle-event-task
    // https://html.spec.whatwg.org/multipage/interactive-elements.html#queue-a-dialog-toggle-event-task
    ToggleEvent {
        id: NodeId,
        old_state: String,
        new_state: String,
    },
    // https://html.spec.whatwg.org/multipage/interactive-elements.html#the-dialog-element:close-event
    DialogClose(NodeId),
    // https://w3c.github.io/webappsec-csp/#report-violation
    CspViolation(Option<NodeId>, Vec<crate::csp::Violation>),
    // https://w3c.github.io/webappsec-csp/#should-block-request
    // https://html.spec.whatwg.org/multipage/workers.html#worker-processing-model
    WorkerBlocked(crate::worker::WorkerId, Vec<crate::csp::Violation>),
    // https://html.spec.whatwg.org/multipage/semantics.html#attr-meta-http-equiv-refresh
    DeclarativeRefresh(String),
    HtmlWrite(HtmlWrite),
    DocumentStream(DocumentStreamCommand),
    // https://chromedevtools.github.io/devtools-protocol/tot/Runtime/#event-bindingCalled
    Binding {
        name: String,
        payload: String,
    },
    // https://chromedevtools.github.io/devtools-protocol/tot/Runtime/#event-consoleAPICalled
    Console {
        level: crate::protocol::ConsoleLevel,
        args: String,
    },
    // https://chromedevtools.github.io/devtools-protocol/tot/Runtime/#event-exceptionThrown
    ExceptionThrown {
        text: String,
        filename: String,
        line: u32,
        column: u32,
    },
    // https://html.spec.whatwg.org/multipage/webappapis.html#notify-about-rejected-promises
    RejectionTasks,
}

pub(crate) type Outbox = Rc<RefCell<VecDeque<RealmRequest>>>;

// https://html.spec.whatwg.org/multipage/common-dom-interfaces.html#reflecting-content-attributes-in-idl-attributes:element
pub(crate) struct ElementReferenceCache {
    pub(crate) nodes: Vec<NodeId>,
    pub(crate) object: Persistent<Value<'static>>,
}

#[derive(Default)]
pub(crate) struct DocumentState {
    pub(crate) applied_csp_metas: HashSet<NodeId>,
    // https://w3c.github.io/webappsec-csp/#is-base-allowed-for-document
    pub(crate) blocked_bases: HashSet<NodeId>,
    // https://html.spec.whatwg.org/multipage/semantics.html#the-base-element
    pub(crate) base_href_seen: HashMap<NodeId, Option<String>>,
    base_results: HashMap<NodeId, Option<Url>>,
    frame_navigations: Vec<FrameNavigation>,
    // https://html.spec.whatwg.org/multipage/scripting.html#parser-inserted
    pub(crate) parser_inserted: HashSet<NodeId>,
    // https://html.spec.whatwg.org/multipage/scripting.html#script-force-async
    pub(crate) script_force_async_cleared: HashSet<NodeId>,
    // https://encoding.spec.whatwg.org/#dom-textdecoder-decode
    pub(crate) decoders: HashMap<u64, DecoderSession>,
    // https://html.spec.whatwg.org/multipage/dom.html#dom-document-currentscript
    pub current_script: Option<NodeId>,
    // https://html.spec.whatwg.org/multipage/webappapis.html#fetch-the-descendants-of-and-link-a-module-script
    pub(crate) module_fetch_root: Option<NodeId>,
    // https://tc39.es/ecma262/#sec-hostresolveimportedmodule
    pub(crate) current_script_url: Option<Url>,
    // https://html.spec.whatwg.org/multipage/workers.html#worker-processing-model
    pub(crate) module_client_origin: Option<url::Origin>,
    // https://html.spec.whatwg.org/multipage/webappapis.html#fetch-a-single-module-script
    pub(crate) module_credentials: Option<net::cors::CredentialsMode>,
    // https://html.spec.whatwg.org/multipage/webappapis.html#fetch-a-single-module-script
    pub(crate) declaring_module: bool,
    // https://w3c.github.io/webappsec-referrer-policy/#integration-with-html
    pub(crate) worker_referrer_policy: Option<net::ReferrerPolicy>,
    // https://html.spec.whatwg.org/multipage/browsers.html#initialize-a-policy-container-from-a-fetch-response
    pub(crate) worker_module_csp: Option<crate::csp::List>,
    // https://html.spec.whatwg.org/multipage/webappapis.html#fetch-a-single-module-script
    // https://html.spec.whatwg.org/multipage/workers.html#worker-processing-model
    pub(crate) worker_owner_csp: Option<crate::csp::List>,

    pub(crate) custom_construction: Vec<NodeId>,
    remote_ids: HashMap<NodeId, u64>,
    remote_nodes: HashMap<u64, NodeId>,
    // https://html.spec.whatwg.org/multipage/urls-and-fetching.html#nonce-attributes
    pub(crate) nonce_slots: HashMap<NodeId, String>,
    // https://html.spec.whatwg.org/multipage/interactive-elements.html#the-dialog-element:dom-dialog-returnvalue
    pub(crate) dialog_return_values: HashMap<NodeId, crate::dom_string::DomString>,
    // https://html.spec.whatwg.org/multipage/interactive-elements.html#the-dialog-element:previously-focused-element
    pub(crate) dialog_previously_focused: HashMap<NodeId, Option<NodeId>>,
    // https://html.spec.whatwg.org/multipage/forms.html#past-names-map
    pub(crate) form_past_names: std::cell::RefCell<HashMap<NodeId, Vec<(String, NodeId)>>>,
    // https://w3c.github.io/webappsec-csp/#create-violation-for-global
    pub(crate) creator_stacks: HashMap<NodeId, CreatorInfo>,
    // https://w3c.github.io/webappsec-csp/#report-violation
    pub(crate) reported_handlers: HashMap<(NodeId, String), String>,
    pub(crate) controls: HashMap<NodeId, ControlState>,
}

// https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#the-constraint-validation-api
#[derive(Clone, Debug, Default)]
pub(crate) struct ControlState {
    pub(crate) user_edited: bool,
    pub(crate) bad_input: bool,
    pub(crate) custom_validity: Vec<u16>,
    pub(crate) number_buffer: Option<String>,
    pub(crate) caret: Option<u32>,
    pub(crate) output_value_mode: bool,
    pub(crate) output_default_value: Option<Vec<u16>>,
}

impl ControlState {
    pub(crate) fn clear_edit_state(&mut self) {
        self.user_edited = false;
        self.bad_input = false;
        self.number_buffer = None;
        self.caret = None;
    }

    pub(crate) fn clear_edit_position(&mut self) {
        self.number_buffer = None;
        self.caret = None;
    }
}

impl DocumentState {
    fn reset(&mut self) {
        self.applied_csp_metas.clear();
        self.blocked_bases.clear();
        self.base_href_seen.clear();
        self.base_results.clear();
        self.frame_navigations.clear();
        self.parser_inserted.clear();
        self.script_force_async_cleared.clear();
        self.decoders.clear();
        self.current_script = None;
        self.module_fetch_root = None;
        self.current_script_url = None;
        self.module_client_origin = None;
        self.module_credentials = None;
        self.declaring_module = false;
        self.worker_referrer_policy = None;
        self.worker_module_csp = None;
        self.worker_owner_csp = None;
        self.custom_construction.clear();
        self.remote_ids.clear();
        self.remote_nodes.clear();
        self.dialog_return_values.clear();
        self.dialog_previously_focused.clear();
        self.form_past_names.borrow_mut().clear();
    }

    fn forget_document(&mut self, document: u32) {
        self.blocked_bases.retain(|node| node.document != document);
        self.base_href_seen
            .retain(|node, _| node.document != document);
        self.base_results
            .retain(|node, _| node.document != document);
        self.nonce_slots.retain(|node, _| node.document != document);
        self.creator_stacks
            .retain(|node, _| node.document != document);
        self.reported_handlers
            .retain(|(node, _), _| node.document != document);
        self.controls.retain(|node, _| node.document != document);
        self.dialog_return_values
            .retain(|node, _| node.document != document);
        self.dialog_previously_focused
            .retain(|node, _| node.document != document);
        self.form_past_names
            .borrow_mut()
            .retain(|node, _| node.document != document);
    }
}

#[derive(Clone, Debug)]
pub(crate) struct CreatorInfo {
    pub url: String,
    pub line: u32,
    pub column: u32,
}

pub(crate) struct DecoderSession {
    pub(crate) decoder: encoding_rs::Decoder,
    pub(crate) encoding: &'static encoding_rs::Encoding,
    pub(crate) ignore_bom: bool,
    pub(crate) carry: Vec<u8>,
}

impl DecoderSession {
    pub(crate) fn fresh(encoding: &'static encoding_rs::Encoding, ignore_bom: bool) -> Self {
        let decoder = if ignore_bom {
            encoding.new_decoder_without_bom_handling()
        } else {
            encoding.new_decoder_with_bom_removal()
        };
        Self {
            decoder,
            encoding,
            ignore_bom,
            carry: Vec::new(),
        }
    }
}

impl Drop for World {
    fn drop(&mut self) {
        if self.isolated.is_none() {
            self.unload_object_urls_for_frame();
            self.clear_outbox();
        }
    }
}

// https://html.spec.whatwg.org/multipage/nav-history-apis.html#window-open-steps
pub(crate) struct WindowOpenPlan {
    pub(crate) url: Option<Url>,
    pub(crate) opener: crate::protocol::OpenerPolicy,
    // https://html.spec.whatwg.org/multipage/nav-history-apis.html#window-open-steps
    pub(crate) withholds: bool,
    pub(crate) source: crate::protocol::NavigationSource,
}

impl World {
    pub(crate) fn new(
        document_url: Url,
        frame: FrameId,
        runtime: &FrameRuntime,
        init_scripts: Rc<RefCell<Vec<(u64, String)>>>,
        bindings: Rc<RefCell<Vec<String>>>,
    ) -> Self {
        let secure_context = secure_context::is_secure_environment(&document_url, None);
        let realm = runtime.registry.borrow_mut().allocate_realm();
        let origin = document_url.origin();
        let doc = Rc::new(RefCell::new(DocumentData {
            document: None,
            owned: HashSet::new(),
            pending_frames: Vec::new(),
            deferred_scripts: Vec::new(),
            referrer_url: document_url.clone(),
            document_referrer: None,
            creation_url: document_url.clone(),
            inherited_cookie_url: None,
            document_url,
            base_url: None,
            origin,
            domain: Rc::new(RefCell::new(None)),
            top_level: None,
            first_party: true,
            parent_context: None,
            same_site_cookie_policy: net::SameSiteCookiePolicy::All,
            policy: PolicyContainer {
                secure_context,
                ..PolicyContainer::default()
            },
            import_map: crate::js::modules::ImportMap::default(),
            sandbox: SandboxFlags::default(),
            history: crate::protocol::HistorySnapshot::default(),
            input_files: HashMap::new(),
            input_file_list: HashMap::new(),
            font_registrations: HashMap::new(),
            next_font_registration: 1,
            parser_active: false,
            state: DocumentState::default(),
            insertion_load_fired: HashSet::new(),
        }));
        let win = Rc::new(RefCell::new(WindowData {
            frame,
            own_window: None,
            init_scripts,
            bindings,
            viewport_size: Cell::new(crate::engine::DEFAULT_VIEWPORT),
            frame_host: None,
            evaluation_replies: Rc::new(RefCell::new(HashMap::new())),
            events: EventTargetState::default(),
            clicks_in_progress: HashSet::new(),
            transient_activation: None,
            sticky_activation: false,
            event_clock: (Instant::now(), 0.0),
        }));
        Self {
            runtime: runtime.clone(),
            kind: GlobalKind::Document,
            realm,
            doc,
            win,
            realm_context: None,
            worker_close_requested: Rc::new(Cell::new(false)),
            compiling_handler: Cell::new(false),
            browser_eval_depth: Cell::new(0),
            isolated: None,
            isolated_wrappers: RefCell::new(HashMap::new()),
            timer_string: Cell::new(false),
            outbox: Rc::new(RefCell::new(VecDeque::new())),
            budget: runtime.registry.borrow().budget(),
            window_object: None,
            standalone_targets: HashMap::new(),
            standalone_listeners: HashMap::new(),
            next_standalone_target: 0,
            proxy_replay: ProxyReplay::Owed,
            caches: RealmCaches::default(),
            brands: HashMap::new(),
            bridge: None,
            location_object: None,
            current_event: None,
            pristine_number: None,
            pristine_boolean: None,
            pristine_reflect_set: None,
            pristine_queue_microtask: None,
            pristine_eval_error: None,
            pristine_blob: None,
            pristine_set_timeout: None,
            event_ctors: HashMap::new(),
            weak_references: None,
            deliver_mutations_fn: None,
        }
    }

    // https://chromedevtools.github.io/devtools-protocol/tot/Page/#method-createIsolatedWorld
    pub(crate) fn new_isolated(main: &Self, name: String) -> Self {
        let realm = main.runtime.registry.borrow_mut().allocate_realm();
        Self {
            runtime: main.runtime.clone(),
            kind: GlobalKind::Document,
            realm,
            doc: Rc::clone(&main.doc),
            win: Rc::clone(&main.win),
            realm_context: None,
            worker_close_requested: Rc::new(Cell::new(false)),
            compiling_handler: Cell::new(false),
            browser_eval_depth: Cell::new(0),
            isolated: Some(IsolatedRealm { name, csp: None }),
            isolated_wrappers: RefCell::new(HashMap::new()),
            timer_string: Cell::new(false),
            outbox: Rc::clone(&main.outbox),
            budget: Rc::clone(&main.budget),
            window_object: None,
            standalone_targets: HashMap::new(),
            standalone_listeners: HashMap::new(),
            next_standalone_target: 0,
            proxy_replay: ProxyReplay::Done,
            caches: RealmCaches::default(),
            brands: HashMap::new(),
            bridge: None,
            location_object: None,
            current_event: None,
            pristine_number: None,
            pristine_boolean: None,
            pristine_reflect_set: None,
            pristine_queue_microtask: None,
            pristine_eval_error: None,
            pristine_blob: None,
            pristine_set_timeout: None,
            event_ctors: HashMap::new(),
            weak_references: None,
            deliver_mutations_fn: None,
        }
    }

    pub(crate) fn doc(&self) -> Ref<'_, DocumentData> {
        self.doc.borrow()
    }

    pub(crate) fn doc_mut(&self) -> RefMut<'_, DocumentData> {
        self.doc.borrow_mut()
    }

    pub(crate) fn win(&self) -> Ref<'_, WindowData> {
        self.win.borrow()
    }

    pub(crate) fn win_mut(&self) -> RefMut<'_, WindowData> {
        self.win.borrow_mut()
    }

    pub(crate) fn is_isolated(&self) -> bool {
        self.isolated.is_some()
    }

    // https://w3c.github.io/webappsec-csp/#csp-list
    // https://chromedevtools.github.io/devtools-protocol/tot/Page/#method-createIsolatedWorld
    pub(crate) fn effective_csp(&self) -> crate::csp::List {
        match &self.isolated {
            Some(isolated) => isolated.csp.clone().unwrap_or_default(),
            None => self.doc.borrow().policy.csp.clone(),
        }
    }

    // https://html.spec.whatwg.org/multipage/workers.html#dedicated-workers-and-the-dedicatedworkerglobalscope-interface
    pub(crate) fn new_worker(
        document_url: Url,
        frame: FrameId,
        parent: FrameId,
        runtime: &FrameRuntime,
        id: crate::worker::WorkerId,
        name: String,
        module: bool,
    ) -> Self {
        let mut world = Self::new(
            document_url,
            frame,
            runtime,
            Rc::new(RefCell::new(Vec::new())),
            Rc::new(RefCell::new(Vec::new())),
        );
        world.kind = GlobalKind::DedicatedWorker {
            id,
            parent,
            name,
            module,
        };
        world
    }

    // https://html.spec.whatwg.org/multipage/workers.html#shared-workers-and-the-sharedworkerglobalscope-interface
    pub(crate) fn new_shared_worker(
        document_url: Url,
        frame: FrameId,
        runtime: &FrameRuntime,
        id: crate::worker::WorkerId,
        name: String,
        module: bool,
    ) -> Self {
        let mut world = Self::new(
            document_url,
            frame,
            runtime,
            Rc::new(RefCell::new(Vec::new())),
            Rc::new(RefCell::new(Vec::new())),
        );
        world.kind = GlobalKind::SharedWorker { id, name, module };
        world
    }

    pub(crate) fn is_shared_worker(&self) -> bool {
        matches!(self.kind, GlobalKind::SharedWorker { .. })
    }

    pub(crate) fn worker_is_module(&self) -> bool {
        match &self.kind {
            GlobalKind::Document => false,
            GlobalKind::DedicatedWorker { module, .. }
            | GlobalKind::SharedWorker { module, .. } => *module,
        }
    }

    pub(crate) fn worker_name(&self) -> Option<&str> {
        match &self.kind {
            GlobalKind::Document => None,
            GlobalKind::DedicatedWorker { name, .. } | GlobalKind::SharedWorker { name, .. } => {
                Some(name)
            }
        }
    }

    pub(crate) fn worker_id(&self) -> Option<crate::worker::WorkerId> {
        match &self.kind {
            GlobalKind::Document => None,
            GlobalKind::DedicatedWorker { id, .. } | GlobalKind::SharedWorker { id, .. } => {
                Some(*id)
            }
        }
    }

    pub(crate) fn worker_parent(&self) -> Option<FrameId> {
        match &self.kind {
            GlobalKind::Document | GlobalKind::SharedWorker { .. } => None,
            GlobalKind::DedicatedWorker { parent, .. } => Some(*parent),
        }
    }

    pub(crate) fn clear_observers(&mut self) {
        let frame = self.frame();
        self.runtime
            .registry
            .borrow_mut()
            .observers
            .forget_frame(frame, &mut self.runtime.documents.borrow_mut());
    }

    pub(crate) fn replace_document(&mut self, parsed: Parsed) -> u32 {
        self.drop_active_document();
        let id = self.runtime.documents.borrow_mut().insert(parsed);
        {
            let mut doc = self.doc.borrow_mut();
            doc.document = Some(id);
            doc.owned.insert(id);
            doc.state.reset();
        }
        self.caches.clear();
        self.clear_outbox();
        // https://html.spec.whatwg.org/multipage/webappapis.html#event-handlers
        self.win.borrow_mut().events.reset_for_navigation();
        // https://html.spec.whatwg.org/multipage/webappapis.html#event-handlers
        crate::js::events::reactivate_window_handlers(self);
        self.clear_observers();
        id
    }

    pub(crate) fn set_document(&mut self, parsed: Parsed) -> u32 {
        let id = self.runtime.documents.borrow_mut().insert(parsed);
        if self.doc.borrow().document != Some(id) {
            self.drop_active_document();
            let mut doc = self.doc.borrow_mut();
            doc.document = Some(id);
            doc.owned.insert(id);
        }
        let mut doc = self.doc.borrow_mut();
        doc.state.current_script = None;
        doc.state.module_fetch_root = None;
        doc.state.current_script_url = None;
        doc.state.declaring_module = false;
        id
    }

    fn drop_active_document(&mut self) {
        let old = {
            let mut doc = self.doc.borrow_mut();
            let Some(old) = doc.document.take() else {
                return;
            };
            doc.owned.remove(&old);
            doc.input_files.retain(|node, _| node.document_id() != old);
            doc.input_file_list
                .retain(|node, _| node.document_id() != old);
            doc.state.forget_document(old);
            old
        };
        self.caches.forget_document(old);
        self.unload_object_urls_for_document(old);
        self.runtime.documents.borrow_mut().remove(old);
        self.runtime.registry.borrow_mut().forget_document(old);
    }

    pub(crate) fn forget_owned_documents(&mut self) {
        if self.isolated.is_none() {
            let ids: Vec<u32> = self.doc.borrow_mut().owned.drain().collect();
            for id in ids {
                self.unload_object_urls_for_document(id);
                {
                    let mut doc = self.doc.borrow_mut();
                    doc.input_files.retain(|node, _| node.document_id() != id);
                    doc.input_file_list
                        .retain(|node, _| node.document_id() != id);
                    doc.state.forget_document(id);
                }
                self.caches.forget_document(id);
                self.runtime.documents.borrow_mut().remove(id);
                self.runtime.registry.borrow_mut().forget_document(id);
                self.win.borrow_mut().events.forget_document(id);
            }
            self.doc.borrow_mut().document = None;
        }
        self.runtime
            .registry
            .borrow_mut()
            .forget_realm(self.realm_id());
    }

    pub(crate) fn prune_dead_wrappers(&self) {
        let documents = self.runtime.documents.borrow();
        self.runtime
            .registry
            .borrow_mut()
            .prune_dead_wrappers(&documents);
    }

    pub(crate) fn registry(&self) -> Rc<RefCell<RealmRegistry>> {
        Rc::clone(&self.runtime.registry)
    }

    pub(crate) fn owner_world(&self, id: NodeId) -> Option<Rc<RefCell<World>>> {
        self.runtime.registry.borrow().owner_world(id)
    }

    pub(crate) fn shared_wrapper(&self, id: NodeId) -> Option<Persistent<Value<'static>>> {
        if self.isolated.is_some() {
            return self.isolated_wrappers.borrow().get(&id).cloned();
        }
        self.runtime.registry.borrow().wrapper(id)
    }

    pub(crate) fn intern_shared_wrapper(&self, id: NodeId, value: Persistent<Value<'static>>) {
        if self.isolated.is_some() {
            self.isolated_wrappers.borrow_mut().insert(id, value);
            return;
        }
        self.runtime.registry.borrow_mut().intern_wrapper(id, value);
    }

    pub(crate) fn main_document(&self) -> Option<Ref<'_, Parsed>> {
        let id = self.doc.borrow().document?;
        Ref::filter_map(self.runtime.documents.borrow(), |store| store.get(id)).ok()
    }

    pub(crate) fn main_document_mut(&self) -> Option<RefMut<'_, Parsed>> {
        let id = self.doc.borrow().document?;
        RefMut::filter_map(self.runtime.documents.borrow_mut(), |store| {
            store.get_mut(id)
        })
        .ok()
    }

    pub(crate) fn implementation(&self, id: NodeId) -> Option<Persistent<Value<'static>>> {
        self.caches.implementations.get(&id.document_id()).cloned()
    }

    pub(crate) fn intern_implementation(&mut self, id: NodeId, value: Persistent<Value<'static>>) {
        self.caches.implementations.insert(id.document_id(), value);
    }

    pub(crate) fn document(&self, id: NodeId) -> Option<Ref<'_, Parsed>> {
        Ref::filter_map(self.runtime.documents.borrow(), |store| {
            store.get(id.document_id())
        })
        .ok()
    }

    // https://html.spec.whatwg.org/multipage/urls-and-fetching.html#cors-settings-attributes
    // https://html.spec.whatwg.org/multipage/urls-and-fetching.html#referrer-policy-attribute
    pub(crate) fn subresource_options(&self, element: NodeId) -> SubresourceOptions {
        let Some(parsed) = self.document(element) else {
            return SubresourceOptions::default();
        };
        let base = &parsed.document.base;
        SubresourceOptions {
            crossorigin: crate::dom::tree::attr(base, element.node, "crossorigin")
                .map(blitz_traits::net::CorsSetting::parse),
            referrer_policy: crate::dom::tree::attr(base, element.node, "referrerpolicy")
                .and_then(blitz_traits::net::ReferrerPolicy::parse),
        }
    }

    pub(crate) fn store(&self) -> Ref<'_, DocumentStore> {
        self.runtime.documents.borrow()
    }

    pub(crate) fn owns_node(&self, id: NodeId) -> bool {
        self.doc.borrow().owned.contains(&id.document)
    }

    pub(crate) fn with_document<R>(
        &self,
        id: NodeId,
        reader: impl FnOnce(&Parsed) -> R,
    ) -> Option<R> {
        let store = self.runtime.documents.borrow();
        store.get(id.document_id()).map(reader)
    }

    pub(crate) fn with_main_document<R>(&self, reader: impl FnOnce(&Parsed) -> R) -> Option<R> {
        let id = self.doc.borrow().document?;
        let store = self.runtime.documents.borrow();
        store.get(id).map(reader)
    }

    pub(crate) fn document_mut(&self, id: NodeId) -> Option<RefMut<'_, Parsed>> {
        RefMut::filter_map(self.runtime.documents.borrow_mut(), |store| {
            store.get_mut(id.document_id())
        })
        .ok()
    }

    pub(crate) fn document_mut_by_id(&self, document: u32) -> Option<RefMut<'_, Parsed>> {
        RefMut::filter_map(self.runtime.documents.borrow_mut(), |store| {
            store.get_mut(document)
        })
        .ok()
    }

    // https://dom.spec.whatwg.org/#concept-range
    pub(crate) fn try_free_range(&self, document: u32, slot: usize) {
        let Ok(mut documents) = self.runtime.documents.try_borrow_mut() else {
            return;
        };
        if let Some(parsed) = documents.get_mut(document) {
            parsed.document.side.ranges.free(slot);
        }
    }

    pub(crate) fn add_document(&mut self, parsed: Parsed) -> NodeId {
        let node = parsed.document.base.root_node().id;
        let document = self.runtime.documents.borrow_mut().insert(parsed);
        self.doc.borrow_mut().owned.insert(document);
        NodeId { document, node }
    }

    pub(crate) fn frame_document(&self, container: NodeId) -> Option<NodeId> {
        self.runtime.registry.borrow().frame_document(container)
    }

    pub(crate) fn realm_id(&self) -> u64 {
        self.realm
    }

    // https://chromedevtools.github.io/devtools-protocol/tot/Page/#method-createIsolatedWorld
    pub(crate) fn scoped_handle(&self, handle: String) -> String {
        #[cfg(not(target_family = "wasm"))]
        let pid = std::process::id();
        #[cfg(target_family = "wasm")]
        let pid = 0u32;
        let scoped = format!("r{pid}-{}-{handle}", self.realm);
        match &self.isolated {
            Some(isolated) => format!("iw:{}:{scoped}", isolated.name),
            None => scoped,
        }
    }

    pub(crate) fn main_document_id(&self) -> Option<u32> {
        self.doc.borrow().document
    }

    pub(crate) fn frame(&self) -> FrameId {
        self.win.borrow().frame
    }

    // https://html.spec.whatwg.org/multipage/document-sequences.html#nav-container
    pub(crate) fn container(&self) -> Option<NodeId> {
        self.runtime.shared.borrow().tree.container(self.frame())
    }

    // https://html.spec.whatwg.org/multipage/nav-history-apis.html#dom-location-href
    pub(crate) fn has_browsing_context(&self) -> bool {
        self.runtime.shared.borrow().tree.is_alive(self.frame())
    }

    // https://html.spec.whatwg.org/multipage/webappapis.html#concept-environment-top-level-origin
    pub(crate) fn top_level_origin(&self) -> url::Origin {
        let doc = self.doc.borrow();
        doc.top_level.clone().unwrap_or_else(|| doc.origin.clone())
    }

    // https://privacycg.github.io/storage-access/#first-party-site-context
    pub(crate) fn set_parent_context(&mut self, parent: &Self) {
        let (origin, first_party) = {
            let parent_doc = parent.doc.borrow();
            (parent_doc.origin.clone(), parent_doc.first_party)
        };
        let mut doc = self.doc.borrow_mut();
        doc.parent_context = Some(ParentContext {
            origin,
            first_party,
        });
        doc.refresh_first_party();
    }

    // https://github.com/privacycg/CHIPS#computing-the-cookie-partition-key
    pub(crate) fn partition_key(&self) -> Option<String> {
        net::site_for_origin(&self.top_level_origin())
    }

    pub(crate) fn is_secure_context(&self) -> bool {
        self.doc.borrow().policy.secure_context
    }

    // https://html.spec.whatwg.org/multipage/webappapis.html#concept-settings-object-cross-origin-isolated-capability
    pub(crate) fn is_cross_origin_isolated(&self) -> bool {
        self.doc.borrow().policy.cross_origin_isolated
    }

    // https://w3c.github.io/hr-time/#dfn-current-high-resolution-time
    pub(crate) fn time_resolution_micros(&self) -> u32 {
        if self.is_cross_origin_isolated() {
            5
        } else {
            100
        }
    }

    pub(crate) fn is_attached(&self) -> bool {
        let frame = self.frame();
        if frame == FrameId::MAIN {
            return true;
        }
        let container = self.runtime.shared.borrow().tree.container(frame);
        let Some(container) = container else {
            return self.runtime.shared.borrow().tree.contains(frame);
        };
        self.owner_world(container).is_some_and(|owner| {
            owner
                .borrow()
                .main_document()
                .is_some_and(|parsed| is_connected(&parsed.document.base, container.node))
        })
    }

    pub(crate) fn shared(&self) -> SharedHandle {
        self.runtime.shared.clone()
    }

    pub(crate) fn register_pending_frames(&mut self, before: Option<NodeId>) -> Vec<NodeId> {
        let mut created = Vec::new();
        let containers = self.iframe_containers_before(before);
        for container in containers {
            if self.register_frame_for_container(container) {
                created.push(container);
            }
        }
        created
    }

    // https://html.spec.whatwg.org/multipage/iframe-embed-object.html#the-iframe-element:html-element-insertion-steps
    pub(crate) fn register_frame_for_container(&mut self, container: NodeId) -> bool {
        if self
            .runtime
            .shared
            .borrow()
            .tree
            .frame_for_container(container)
            .is_some()
        {
            return false;
        }
        let eligible = self
            .with_document(container, |parsed| {
                let base = &parsed.document.base;
                is_child_navigable_container(base, container.node)
                    && is_connected(base, container.node)
            })
            .unwrap_or(false);
        if !eligible || self.runtime.shared.borrow().tree.len() >= MAX_FRAMES {
            return false;
        }
        let name = self
            .with_document(container, |parsed| {
                attr(&parsed.document.base, container.node, "name")
                    .unwrap_or("")
                    .to_owned()
            })
            .unwrap_or_default();
        let before = self.frame_sort_before(container);
        let frame = {
            let mut shared = self.runtime.shared.borrow_mut();
            let frame = shared.allocate_frame();
            shared
                .tree
                .add_container(self.frame(), frame, container, before);
            if !name.is_empty() {
                shared.tree.set_name(frame, name);
            }
            frame
        };
        self.doc
            .borrow_mut()
            .pending_frames
            .push((frame, container));
        true
    }

    // https://html.spec.whatwg.org/multipage/document-sequences.html#document-tree-child-navigable
    pub(crate) fn frame_sort_before(&self, container: NodeId) -> Option<FrameId> {
        self.with_document(container, |parsed| {
            let base = &parsed.document.base;
            let mut stack = vec![base.root_node().id];
            let mut seen = false;
            while let Some(id) = stack.pop() {
                if id == container.node {
                    seen = true;
                } else if seen
                    && is_child_navigable_container(base, id)
                    && is_connected(base, id)
                    && let Some(frame) = self.frame_for_container(NodeId {
                        document: container.document,
                        node: id,
                    })
                {
                    return Some(frame);
                }
                if let Some(node) = base.get_node(id) {
                    stack.extend(node.children.iter().rev().copied());
                }
            }
            None
        })?
    }

    // https://html.spec.whatwg.org/multipage/iframe-embed-object.html#the-iframe-element:html-element-insertion-steps
    pub(crate) fn materialize_iframe(world_rc: &Rc<RefCell<World>>, container: NodeId) -> bool {
        world_rc
            .borrow_mut()
            .register_frame_for_container(container);
        let pending = {
            let world = world_rc.borrow();
            let mut doc = world.doc.borrow_mut();
            doc.pending_frames
                .iter()
                .position(|(_, existing)| *existing == container)
                .map(|index| doc.pending_frames.remove(index))
        };
        let Some((frame, container)) = pending else {
            return world_rc
                .borrow()
                .runtime
                .registry
                .borrow()
                .frame_document(container)
                .is_some();
        };
        let host = world_rc.borrow().win.borrow().frame_host.clone();
        let Some(host) = host else {
            world_rc
                .borrow()
                .doc
                .borrow_mut()
                .pending_frames
                .push((frame, container));
            return false;
        };
        (host.child)(world_rc, frame, container);
        true
    }

    pub(crate) fn set_frame_host(&mut self, host: FrameHost) {
        self.win.borrow_mut().frame_host = Some(host);
    }

    // https://html.spec.whatwg.org/multipage/browsers.html#determining-the-creation-sandboxing-flags
    // https://html.spec.whatwg.org/multipage/browsers.html#popup-sandboxing-flag-set
    pub(crate) fn install_sandbox(&mut self, created: SandboxFlags) {
        let popup = self
            .runtime
            .shared
            .borrow()
            .tree
            .popup_sandbox(self.frame());
        self.doc.borrow_mut().sandbox = created.union(popup);
    }

    // https://html.spec.whatwg.org/multipage/browsers.html#sandboxed-auxiliary-navigation-browsing-context-flag
    pub(crate) fn allows_auxiliary_navigation(&self) -> bool {
        !self
            .doc
            .borrow()
            .sandbox
            .contains(SandboxFlag::AuxiliaryNavigation)
    }

    // https://html.spec.whatwg.org/multipage/nav-history-apis.html#window-open-steps
    // https://html.spec.whatwg.org/multipage/nav-history-apis.html#get-noopener-for-window-open
    pub(crate) fn plan_window_open(
        &self,
        url: &str,
        target: &TargetName,
        features: &str,
    ) -> Option<WindowOpenPlan> {
        // https://html.spec.whatwg.org/multipage/nav-history-apis.html#window-open-steps
        let url = if url.is_empty() {
            None
        } else {
            Some(self.resolve_relative(url)?)
        };
        let blob_origin = url
            .as_ref()
            .and_then(|url| self.object_url_origin(url.as_str()));
        let opener = crate::window_features::WindowFeatures::parse(features)
            .opener_policy(blob_origin.as_ref(), &self.top_level_origin());
        let doc = self.doc.borrow();
        Some(WindowOpenPlan {
            url,
            opener,
            withholds: opener != crate::protocol::OpenerPolicy::Keep && target.noopener_withholds(),
            source: crate::protocol::NavigationSource {
                origin: crate::protocol::SerializedOrigin::from(&doc.origin),
                referrer: doc.referrer_url.clone(),
                referrer_policy: doc.policy.referrer_policy.unwrap_or_default(),
                frame: self.frame(),
                secure_context: doc.policy.secure_context,
            },
        })
    }

    // https://html.spec.whatwg.org/multipage/nav-history-apis.html#window-open-steps
    pub(crate) fn navigate_selected(
        world_rc: &Rc<RefCell<World>>,
        frame: FrameId,
        url: Option<&Url>,
        opener: crate::protocol::OpenerPolicy,
    ) {
        let Some(url) = url else {
            return;
        };
        let mut navigation = FrameNavigation::get(NavigationTarget::Frame(frame), url.to_string());
        if opener == crate::protocol::OpenerPolicy::NoReferrer {
            navigation = navigation.without_referrer();
        }
        world_rc.borrow_mut().queue_frame_navigation(navigation);
    }

    // https://html.spec.whatwg.org/multipage/nav-history-apis.html#window-open-steps
    pub(crate) fn create_auxiliary(
        world_rc: &Rc<RefCell<World>>,
        url: Option<&Url>,
        target: &TargetName,
        opener: crate::protocol::OpenerPolicy,
        allow_cross_origin: bool,
    ) -> Option<FrameId> {
        let current = world_rc.borrow().frame();
        if !world_rc.borrow().allows_auxiliary_navigation()
            || !world_rc
                .borrow()
                .may_host_auxiliary(url, allow_cross_origin)
        {
            return None;
        }
        let (frame, host) = {
            let world = world_rc.borrow();
            let shared = world.runtime.shared.clone();
            let mut shared = shared.borrow_mut();
            if shared.tree.len() >= MAX_FRAMES {
                return None;
            }
            let frame = shared.allocate_frame();
            let sandbox = world.doc.borrow().sandbox.auxiliary_flags();
            shared.tree.add_auxiliary(
                current,
                frame,
                opener == crate::protocol::OpenerPolicy::Keep,
                sandbox,
            );
            if let Some(name) = target.new_context_name() {
                shared.tree.set_name(frame, name.as_str().to_owned());
            }
            (frame, world.win.borrow().frame_host.clone())
        };
        let Some(host) = host else {
            world_rc
                .borrow()
                .runtime
                .shared
                .borrow_mut()
                .tree
                .remove(frame);
            return None;
        };
        (host.auxiliary)(world_rc, frame, opener);
        World::navigate_selected(world_rc, frame, url, opener);
        Some(frame)
    }

    // https://html.spec.whatwg.org/multipage/browsers.html#concept-origin
    fn may_host_auxiliary(&self, url: Option<&Url>, allow_cross_origin: bool) -> bool {
        if allow_cross_origin {
            return true;
        }
        url.is_none_or(|url| {
            if url.scheme() == "about" {
                return true;
            }
            if url.scheme() == "blob" {
                return self.object_url_origin(url.as_str()).is_some();
            }
            url.origin() == self.doc.borrow().origin
        })
    }

    pub(crate) fn queue_deferred_script(&mut self, id: NodeId) {
        self.doc.borrow_mut().deferred_scripts.push(id);
    }

    pub(crate) fn take_deferred_scripts(&mut self) -> Vec<NodeId> {
        std::mem::take(&mut self.doc.borrow_mut().deferred_scripts)
    }

    pub(crate) fn take_pending_frames(&mut self) -> Vec<(FrameId, NodeId)> {
        std::mem::take(&mut self.doc.borrow_mut().pending_frames)
    }

    fn iframe_containers_before(&self, before: Option<NodeId>) -> Vec<NodeId> {
        self.with_main_document(|parsed| {
            let base = &parsed.document.base;
            let document = parsed.id;
            let stop = before
                .filter(|node| node.document == document)
                .map(|node| node.node);
            let mut containers = Vec::new();
            let mut stack = vec![base.root_node().id];
            while let Some(id) = stack.pop() {
                if stop == Some(id) {
                    break;
                }
                if is_child_navigable_container(base, id) && is_connected(base, id) {
                    containers.push(NodeId { document, node: id });
                }
                if let Some(node) = base.get_node(id) {
                    stack.extend(node.children.iter().rev().copied());
                }
            }
            containers
        })
        .unwrap_or_default()
    }

    pub(crate) fn frame_world(&self, frame: FrameId) -> Option<Rc<RefCell<World>>> {
        self.runtime.registry.borrow().frame_world(frame)
    }

    // https://html.spec.whatwg.org/multipage/iframe-embed-object.html#dom-iframe-contentwindow
    pub(crate) fn frame_for_container(&self, container: NodeId) -> Option<FrameId> {
        self.runtime
            .shared
            .borrow()
            .tree
            .frame_for_container(container)
    }

    // https://html.spec.whatwg.org/multipage/iframe-embed-object.html#the-iframe-element:html-element-removing-steps
    // https://github.com/whatwg/html/issues/10194
    pub(crate) fn destroy_child_navigables(&self, containers: &[NodeId]) {
        for container in containers {
            let Some(frame) = self.frame_for_container(*container) else {
                continue;
            };
            self.fire_frame_unload(frame);
            let mut shared = self.runtime.shared.borrow_mut();
            if shared.tree.frame_for_container(*container) == Some(frame) {
                shared.remove_child_navigable(frame, *container);
            }
        }
    }

    // https://html.spec.whatwg.org/multipage/document-sequences.html#definitely-close-a-top-level-traversable
    pub(crate) fn close_auxiliary(&self, frame: FrameId) {
        let victims = {
            let shared = self.runtime.shared.borrow();
            if shared.tree.opener(frame).is_none() {
                return;
            }
            let mut victims = vec![frame];
            let mut pending: Vec<FrameId> = shared.tree.children(frame).to_vec();
            while let Some(child) = pending.pop() {
                pending.extend(shared.tree.children(child).iter().copied());
                victims.push(child);
            }
            victims
        };
        for frame in victims {
            self.fire_frame_unload(frame);
        }
        self.runtime.shared.borrow_mut().close_auxiliary(frame);
    }

    // https://html.spec.whatwg.org/multipage/browsing-the-web.html#unload
    pub(crate) fn fire_frame_unload(&self, frame: FrameId) {
        let Some(world) = self.runtime.registry.borrow().frame_world(frame) else {
            return;
        };
        let Some(context) = world.borrow().realm_context.clone() else {
            return;
        };
        context.with(|realm_ctx| {
            if let Err(error) = crate::js::events::fire_document_unload(&realm_ctx) {
                crate::js::bindings::host::report_callback_error(&realm_ctx, &error);
            }
        });
    }

    // https://html.spec.whatwg.org/multipage/browsers.html#concept-origin
    pub(crate) fn origin_string(&self) -> String {
        self.doc.borrow().origin.ascii_serialization()
    }

    pub(crate) fn set_url(&mut self, url: Url) {
        let origin = url.origin();
        self.set_url_with_origin(url, origin);
    }

    // https://html.spec.whatwg.org/multipage/browsers.html#determining-the-origin
    pub(crate) fn set_url_with_origin(&mut self, url: Url, origin: url::Origin) {
        let mut doc = self.doc.borrow_mut();
        doc.referrer_url = url.clone();
        doc.document_url = url;
        doc.origin = origin;
        // https://html.spec.whatwg.org/multipage/browsers.html#relaxing-the-same-origin-restriction
        doc.domain = Rc::new(RefCell::new(None));
        doc.refresh_first_party();
    }

    // https://html.spec.whatwg.org/multipage/browsers.html#concept-origin-effective-domain
    pub(crate) fn effective_domain(&self) -> Option<String> {
        let doc = self.doc.borrow();
        match &doc.origin {
            url::Origin::Tuple(_, host, _) => Some(
                doc.domain
                    .borrow()
                    .clone()
                    .unwrap_or_else(|| host.to_string()),
            ),
            url::Origin::Opaque(_) => None,
        }
    }

    // https://html.spec.whatwg.org/multipage/browsers.html#same-origin-domain
    pub(crate) fn same_origin_domain_with(&self, other: &Self) -> bool {
        let left_doc = self.doc.borrow();
        let right_doc = other.doc.borrow();
        match (&left_doc.origin, &right_doc.origin) {
            (url::Origin::Tuple(left_scheme, _, _), url::Origin::Tuple(right_scheme, _, _)) => {
                match (&*left_doc.domain.borrow(), &*right_doc.domain.borrow()) {
                    (Some(left), Some(right)) => left_scheme == right_scheme && left == right,
                    _ => {
                        left_doc.domain.borrow().is_none()
                            && right_doc.domain.borrow().is_none()
                            && left_doc.origin == right_doc.origin
                    }
                }
            }
            _ => left_doc.origin == right_doc.origin,
        }
    }

    // https://html.spec.whatwg.org/multipage/urls-and-fetching.html#fallback-base-url
    pub(crate) fn resolve_relative(&self, spec: &str) -> Option<Url> {
        let doc = self.doc.borrow();
        Url::parse(spec).ok().or_else(|| {
            doc.document_url
                .join(spec)
                .ok()
                .or_else(|| doc.base_url.as_ref().and_then(|base| base.join(spec).ok()))
        })
    }

    pub(crate) fn url_matches_origin(&self, url: &Url) -> bool {
        let doc = self.doc.borrow();
        match doc.origin {
            url::Origin::Tuple(..) => url.origin() == doc.origin,
            url::Origin::Opaque(_) => {
                let blob_origin = if url.scheme() == "blob" {
                    self.runtime.stored_object_url_origin(url.as_str())
                } else {
                    None
                };
                opaque_url_matches(&doc.document_url, &doc.origin, url, blob_origin.as_ref())
            }
        }
    }

    // https://html.spec.whatwg.org/multipage/webstorage.html#the-localstorage-attribute
    pub(crate) fn storage_origin(&self) -> Option<StorageKey> {
        StorageKey::from_origin(&self.doc.borrow().origin)
    }

    pub(crate) fn storage_get(&self, kind: StorageKind, key: &str) -> Option<String> {
        let origin = self.storage_origin()?;
        self.runtime.services.storage_get(kind, &origin, key)
    }

    pub(crate) fn storage_keys(&self, kind: StorageKind) -> Vec<String> {
        let Some(origin) = self.storage_origin() else {
            return Vec::new();
        };
        self.runtime.services.storage_keys(kind, &origin)
    }

    // https://html.spec.whatwg.org/multipage/webstorage.html#concept-storage-broadcast
    pub(crate) fn storage_set(
        &self,
        kind: StorageKind,
        key: &str,
        value: &str,
    ) -> Result<Option<StorageChange>, StorageError> {
        let Some(origin) = self.storage_origin() else {
            return Ok(None);
        };
        let document_url = self.doc.borrow().document_url.to_string();
        let change = self.runtime.services.storage_set(
            kind,
            &origin,
            &document_url,
            key,
            value,
            self.frame(),
        )?;
        if let Some(change) = &change {
            self.queue_storage_event(kind, &origin, change);
        }
        Ok(change)
    }

    pub(crate) fn storage_remove(&self, kind: StorageKind, key: &str) -> Option<StorageChange> {
        let origin = self.storage_origin()?;
        let document_url = self.doc.borrow().document_url.to_string();
        let change =
            self.runtime
                .services
                .storage_remove(kind, &origin, &document_url, key, self.frame());
        if let Some(change) = &change {
            self.queue_storage_event(kind, &origin, change);
        }
        change
    }

    pub(crate) fn storage_clear(&self, kind: StorageKind) -> Option<StorageChange> {
        let origin = self.storage_origin()?;
        let document_url = self.doc.borrow().document_url.to_string();
        let change =
            self.runtime
                .services
                .storage_clear(kind, &origin, &document_url, self.frame());
        if let Some(change) = &change {
            self.queue_storage_event(kind, &origin, change);
        }
        change
    }

    fn queue_storage_event(&self, kind: StorageKind, origin: &StorageKey, change: &StorageChange) {
        if kind != StorageKind::Session {
            return;
        }
        let url = self.doc.borrow().document_url.to_string();
        self.runtime
            .pending_storage
            .borrow_mut()
            .push(PendingStorageEvent {
                source: Some(self.frame()),
                origin: origin.clone(),
                kind,
                key: change.key.clone(),
                old_value: change.old_value.clone(),
                new_value: change.new_value.clone(),
                url,
            });
    }

    pub(crate) fn main_document_root(&self) -> Option<NodeId> {
        let document = self.doc.borrow().document?;
        self.main_document().map(|parsed| NodeId {
            document,
            node: parsed.document.base.root_node().id,
        })
    }

    // https://dom.spec.whatwg.org/#dom-document-url
    pub(crate) fn document_url_of(&self, id: NodeId) -> String {
        if let Some(url) = self.document(id).and_then(|parsed| parsed.url.clone()) {
            return url;
        }
        if self.is_main_document(id) {
            return self.doc.borrow().document_url.as_str().to_owned();
        }
        "about:blank".to_owned()
    }

    // https://html.spec.whatwg.org/multipage/semantics.html#the-base-element
    // https://w3c.github.io/webappsec-csp/#is-base-allowed-for-document
    pub(crate) fn apply_base_element(&mut self, id: NodeId) -> Option<Url> {
        let href = self.document(id).and_then(|parsed| {
            crate::dom::tree::attr(&parsed.document.base, id.node, "href").map(str::to_owned)
        });
        {
            let mut doc = self.doc.borrow_mut();
            if doc.state.base_href_seen.get(&id) == Some(&href) {
                return doc.state.base_results.get(&id).cloned().flatten();
            }
            doc.state.base_href_seen.insert(id, href.clone());
            if href.is_none() {
                doc.state.blocked_bases.remove(&id);
                doc.state.base_results.insert(id, None);
                return None;
            }
        }
        let href = href?;
        let fallback = self.document_base_url(id);
        // https://html.spec.whatwg.org/multipage/semantics.html#the-base-element
        let Some(url) = fallback
            .join(&href)
            .ok()
            .filter(|url| !matches!(url.scheme(), "data" | "javascript"))
        else {
            let mut doc = self.doc.borrow_mut();
            doc.state.blocked_bases.remove(&id);
            doc.state.base_results.insert(id, None);
            return None;
        };
        let document_url = Url::parse(&self.document_url_of(id))
            .unwrap_or_else(|_| self.doc.borrow().document_url.clone());
        let violations = {
            let doc = self.doc.borrow();
            doc.policy.csp.check_base(&doc.origin, &document_url, &url)
        };
        if !violations.is_empty() {
            self.queue_csp_violations(Some(id), violations.clone());
        }
        let mut doc = self.doc.borrow_mut();
        if crate::csp::blocks(&violations) {
            doc.state.blocked_bases.insert(id);
            doc.state.base_results.insert(id, None);
            None
        } else {
            doc.state.blocked_bases.remove(&id);
            doc.state.base_results.insert(id, Some(url.clone()));
            Some(url)
        }
    }

    // https://html.spec.whatwg.org/multipage/urls-and-fetching.html#document-base-url
    pub(crate) fn document_base_url_of(&self, id: NodeId) -> String {
        self.document_base_url(id).to_string()
    }

    // https://html.spec.whatwg.org/multipage/urls-and-fetching.html#document-base-url
    pub(crate) fn document_base_url(&self, id: NodeId) -> Url {
        let document_url = self.document_url_of(id);
        let parsed = Url::parse(&document_url).ok();
        // https://html.spec.whatwg.org/multipage/urls-and-fetching.html#fallback-base-url
        let fallback = self
            .is_main_document(id)
            .then(|| self.doc.borrow().base_url.clone())
            .flatten()
            .or(parsed)
            .unwrap_or_else(|| Url::parse("about:blank").expect("static URL parses"));
        let Some(parsed) = self.document(id) else {
            return fallback;
        };
        let Some(href) = self.frozen_base_href(&parsed) else {
            return fallback;
        };
        // https://html.spec.whatwg.org/multipage/semantics.html#the-base-element
        fallback
            .join(&href)
            .ok()
            .filter(|url| !matches!(url.scheme(), "data" | "javascript"))
            .unwrap_or(fallback)
    }

    // https://html.spec.whatwg.org/multipage/dom.html#dom-document-cookie
    pub(crate) fn cookie_url(&self) -> Url {
        let doc = self.doc.borrow();
        doc.inherited_cookie_url
            .clone()
            .unwrap_or_else(|| doc.creation_url.clone())
    }

    pub(crate) fn main_base_url(&self) -> Url {
        let fallback = {
            let doc = self.doc.borrow();
            doc.base_url
                .clone()
                .unwrap_or_else(|| doc.document_url.clone())
        };
        let Some(parsed) = self.main_document() else {
            return fallback;
        };
        let Some(href) = self.frozen_base_href(&parsed) else {
            return fallback;
        };
        fallback
            .join(&href)
            .ok()
            .filter(|url| !matches!(url.scheme(), "data" | "javascript"))
            .unwrap_or(fallback)
    }

    // https://html.spec.whatwg.org/multipage/urls-and-fetching.html#document-base-url
    // https://w3c.github.io/webappsec-csp/#is-base-allowed-for-document
    pub(crate) fn frozen_base_href(&self, parsed: &Parsed) -> Option<String> {
        let base = &parsed.document.base;
        let doc = self.doc.borrow();
        let mut stack = vec![base.root_node().id];
        while let Some(current) = stack.pop() {
            let Some(node) = base.get_node(current) else {
                continue;
            };
            if node.data.downcast_element().is_some_and(|element| {
                element.name.ns == crate::dom::tree::html_namespace()
                    && element.name.local.as_str() == "base"
            }) && let Some(value) = crate::dom::tree::attr(base, current, "href")
            {
                let id = NodeId {
                    document: parsed.id,
                    node: current,
                };
                if doc.state.blocked_bases.contains(&id) {
                    return None;
                }
                return Some(value.to_owned());
            }
            stack.extend(node.children.iter().rev().copied());
        }
        None
    }

    pub(crate) fn queue_frame_navigation(&mut self, navigation: FrameNavigation) {
        // https://html.spec.whatwg.org/multipage/browsing-the-web.html#navigate
        let target = match &navigation.target {
            NavigationTarget::Frame(frame) => Some(*frame),
            NavigationTarget::Container(container) => self.frame_for_container(*container),
            NavigationTarget::Target(_) => None,
        };
        if let Some(frame) = target
            && frame != self.frame()
            && let Some(world) = self.frame_world(frame)
        {
            world.borrow_mut().await_proxy_sets_replay();
        }
        self.doc
            .borrow_mut()
            .state
            .frame_navigations
            .push(navigation);
    }

    pub(crate) fn take_frame_navigations(&mut self) -> Vec<FrameNavigation> {
        std::mem::take(&mut self.doc.borrow_mut().state.frame_navigations)
    }

    // https://html.spec.whatwg.org/multipage/window-object.html#windowproxy-set
    pub(crate) fn needs_proxy_sets_replay(&self) -> bool {
        self.proxy_replay == ProxyReplay::Owed
    }

    pub(crate) fn mark_proxy_sets_replayed(&mut self) {
        self.proxy_replay = ProxyReplay::Done;
    }

    // https://html.spec.whatwg.org/multipage/browsing-the-web.html#navigate
    pub(crate) fn await_proxy_sets_replay(&mut self) {
        self.proxy_replay = ProxyReplay::Owed;
    }

    pub(crate) fn has_frame_navigations(&self) -> bool {
        !self.doc.borrow().state.frame_navigations.is_empty()
    }

    // https://cookiestore.spec.whatwg.org/#process-cookie-changes
    pub(crate) fn queue_cookie_changes(&self) {
        let mut outbox = self.outbox.borrow_mut();
        if matches!(outbox.back(), Some(RealmRequest::CookieChanges)) {
            return;
        }
        outbox.push_back(RealmRequest::CookieChanges);
    }

    // https://cookiestore.spec.whatwg.org/#process-cookie-changes
    pub(crate) fn notify_cookie_changes(&self) {
        let frame = self.frame();
        let worlds = self.runtime.registry.borrow().live_worlds();
        for world in worlds {
            if world.borrow().frame() == frame {
                continue;
            }
            world.borrow().queue_cookie_changes();
        }
    }

    pub(crate) fn queue_script(&mut self, element: NodeId) {
        self.outbox
            .borrow_mut()
            .push_back(RealmRequest::Script(element));
    }

    pub(crate) fn queue_declarative_refresh(&mut self, value: String) {
        self.outbox
            .borrow_mut()
            .push_back(RealmRequest::DeclarativeRefresh(value));
    }

    pub(crate) fn queue_selection_changed(&mut self, id: NodeId) {
        self.outbox
            .borrow_mut()
            .push_back(RealmRequest::SelectionChanged(id));
    }

    // https://html.spec.whatwg.org/multipage/interactive-elements.html#queue-a-details-toggle-event-task
    // https://html.spec.whatwg.org/multipage/interactive-elements.html#queue-a-dialog-toggle-event-task
    pub(crate) fn queue_toggle_task(&mut self, id: NodeId, old_state: &str, new_state: &str) {
        self.outbox
            .borrow_mut()
            .push_back(RealmRequest::ToggleEvent {
                id,
                old_state: old_state.to_owned(),
                new_state: new_state.to_owned(),
            });
    }

    // https://html.spec.whatwg.org/multipage/interactive-elements.html#the-dialog-element:close-event
    pub(crate) fn queue_dialog_close(&mut self, id: NodeId) {
        self.outbox
            .borrow_mut()
            .push_back(RealmRequest::DialogClose(id));
    }

    pub(crate) fn queue_csp_violations(
        &mut self,
        target: Option<NodeId>,
        violations: Vec<crate::csp::Violation>,
    ) {
        self.outbox
            .borrow_mut()
            .push_back(RealmRequest::CspViolation(target, violations));
    }

    // https://w3c.github.io/webappsec-csp/#report-violation
    pub(crate) fn queue_worker_blocked(
        &self,
        worker: crate::worker::WorkerId,
        violations: Vec<crate::csp::Violation>,
    ) {
        self.outbox
            .borrow_mut()
            .push_back(RealmRequest::WorkerBlocked(worker, violations));
    }

    pub(crate) fn queue_write(&mut self, html: String) -> std::result::Result<(), ()> {
        if !self.reserve_stream_bytes(html.len()) {
            return Err(());
        }
        let reservation = StreamReservation {
            budget: Rc::clone(&self.budget),
            bytes: html.len(),
        };
        self.outbox
            .borrow_mut()
            .push_back(RealmRequest::HtmlWrite(HtmlWrite {
                html,
                _reservation: reservation,
            }));
        Ok(())
    }

    pub(crate) fn pop_outbox(&self) -> Option<RealmRequest> {
        self.outbox.borrow_mut().pop_front()
    }

    // https://fetch.spec.whatwg.org/#keepalive-flag
    pub(crate) fn take_queued_keepalive_fetches(&self) -> Vec<RealmRequest> {
        let mut outbox = self.outbox.borrow_mut();
        let mut taken = Vec::new();
        let mut kept = VecDeque::with_capacity(outbox.len());
        for request in outbox.drain(..) {
            if matches!(&request, RealmRequest::Fetch(fetch) if fetch.options.keepalive) {
                taken.push(request);
            } else {
                kept.push_back(request);
            }
        }
        *outbox = kept;
        taken
    }

    pub(crate) fn has_outbox_work(&self) -> bool {
        !self.outbox.borrow().is_empty()
    }

    pub(crate) fn clear_outbox(&self) {
        let mut outbox = self.outbox.borrow_mut();
        outbox.retain(|request| {
            matches!(
                request,
                RealmRequest::ClearTimer(_)
                    | RealmRequest::AbortFetch(_)
                    | RealmRequest::HistoryPush { .. }
                    | RealmRequest::HistoryTraverse { .. }
                    | RealmRequest::Binding { .. }
                    | RealmRequest::Console { .. }
            )
        });
    }

    pub(crate) fn clear_outbox_for_open(&self) {
        let mut outbox = self.outbox.borrow_mut();
        outbox.retain(|request| {
            matches!(
                request,
                RealmRequest::HtmlWrite(_)
                    | RealmRequest::DocumentStream(_)
                    | RealmRequest::ClearTimer(_)
                    | RealmRequest::AbortFetch(_)
                    | RealmRequest::HistoryPush { .. }
                    | RealmRequest::HistoryTraverse { .. }
                    | RealmRequest::Binding { .. }
                    | RealmRequest::Console { .. }
            )
        });
    }

    // https://w3c.github.io/webappsec-csp/#meta-element
    pub(crate) fn apply_meta_policy(&mut self, id: NodeId) {
        if self.doc.borrow().state.applied_csp_metas.contains(&id) {
            return;
        }
        let content = self
            .document(id)
            .and_then(|parsed| crate::csp::meta_policy(&parsed.document.base, id.node));
        if let Some(content) = content {
            let mut doc = self.doc.borrow_mut();
            if doc.state.applied_csp_metas.insert(id) {
                doc.policy.csp.push_meta(&content);
            }
        }
    }

    // https://html.spec.whatwg.org/multipage/scripting.html#parser-inserted
    pub(crate) fn mark_parser_inserted(&mut self, id: NodeId) {
        let mut doc = self.doc.borrow_mut();
        doc.state.parser_inserted.insert(id);
        doc.state.script_force_async_cleared.insert(id);
    }

    // https://html.spec.whatwg.org/multipage/scripting.html#script-force-async
    pub(crate) fn clear_script_force_async(&mut self, id: NodeId) {
        self.doc
            .borrow_mut()
            .state
            .script_force_async_cleared
            .insert(id);
    }

    pub(crate) fn script_force_async(&self, id: NodeId) -> bool {
        !self
            .doc
            .borrow()
            .state
            .script_force_async_cleared
            .contains(&id)
    }

    pub(crate) fn clear_parser_inserted(&mut self, id: NodeId) {
        self.doc.borrow_mut().state.parser_inserted.remove(&id);
    }

    pub(crate) fn is_parser_inserted(&self, id: NodeId) -> bool {
        self.doc.borrow().state.parser_inserted.contains(&id)
    }

    pub(crate) fn set_input_files(&mut self, id: NodeId, files: Vec<Persistent<Value<'static>>>) {
        self.doc.borrow_mut().input_files.insert(id, files);
    }

    pub(crate) fn input_files(&self, id: NodeId) -> Option<Vec<Persistent<Value<'static>>>> {
        self.doc.borrow().input_files.get(&id).cloned()
    }

    pub(crate) fn rekey_input_files(&mut self, from: NodeId, to: NodeId) {
        let mut doc = self.doc.borrow_mut();
        if let Some(files) = doc.input_files.remove(&from) {
            doc.input_files.insert(to, files);
        }
        if let Some(list) = doc.input_file_list.remove(&from) {
            doc.input_file_list.insert(to, list);
        }
    }

    pub(crate) fn set_input_file_list(&mut self, id: NodeId, list: Persistent<Value<'static>>) {
        self.doc.borrow_mut().input_file_list.insert(id, list);
    }

    pub(crate) fn input_file_list(&self, id: NodeId) -> Option<Persistent<Value<'static>>> {
        self.doc.borrow().input_file_list.get(&id).cloned()
    }

    pub(crate) fn with_control_mut<R>(
        &self,
        id: NodeId,
        f: impl FnOnce(&mut ControlState) -> R,
    ) -> R {
        let mut doc = self.doc.borrow_mut();
        let control = doc.state.controls.entry(id).or_default();
        f(control)
    }

    pub(crate) fn with_control_state<R>(
        &self,
        id: NodeId,
        f: impl FnOnce(&ControlState) -> R,
    ) -> Option<R> {
        let doc = self.doc.borrow();
        doc.state.controls.get(&id).map(f)
    }

    pub(crate) fn clear_control_edit_state(&mut self, id: NodeId) {
        if let Some(control) = self.doc.borrow_mut().state.controls.get_mut(&id) {
            control.clear_edit_state();
        }
    }

    // https://html.spec.whatwg.org/multipage/images.html#updating-the-image-data
    pub(crate) fn image_request(&self, id: NodeId) -> Option<blitz_dom::ImageRequestState> {
        let parsed = self.document(id)?;
        parsed.document.base.image_request(id.node).cloned()
    }

    pub(crate) fn image_size(&self, id: NodeId) -> Option<(u32, u32)> {
        let parsed = self.document(id)?;
        let node = parsed.document.base.get_node(id.node)?;
        match node.element_data()?.image_data()? {
            blitz_dom::node::ImageData::Raster(raster) => Some((raster.width, raster.height)),
            blitz_dom::node::ImageData::Svg(svg) => Some(crate::render::svg_natural_size(svg)),
            blitz_dom::node::ImageData::None => None,
        }
    }

    pub(crate) fn queue_document_stream(&mut self, command: DocumentStreamCommand) {
        self.outbox
            .borrow_mut()
            .push_back(RealmRequest::DocumentStream(command));
    }

    pub(crate) fn reserve_stream_bytes(&self, bytes: usize) -> bool {
        let mut budget = self.budget.borrow_mut();
        let Some(total) = budget.pending_stream_bytes.checked_add(bytes) else {
            return false;
        };
        if total > MAX_PENDING_STREAM_BYTES {
            return false;
        }
        budget.pending_stream_bytes = total;
        true
    }

    pub(crate) fn create_object_url(
        &mut self,
        contents: String,
        content_type: String,
    ) -> Option<String> {
        let length = contents.len() + content_type.len();
        let mut budget = self.budget.borrow_mut();
        let total = budget.object_url_bytes.checked_add(length)?;
        if total > MAX_OBJECT_URL_BYTES {
            return None;
        }
        let id = object_url_id()?;
        let (origin, document) = {
            let doc = self.doc.borrow();
            (doc.origin.clone(), doc.document)
        };
        let url = format!("blob:{}/{id}", origin.ascii_serialization());
        self.runtime.insert_object_url(
            url.clone(),
            ObjectUrlEntry {
                contents: Arc::from(contents),
                content_type: Arc::from(content_type),
                origin,
                frame: self.frame(),
                document,
            },
        );
        budget.object_url_bytes = total;
        Some(url)
    }

    pub(crate) fn object_url_contents(&self, url: &str) -> Option<Arc<str>> {
        let origin = self.doc.borrow().origin.clone();
        self.runtime.object_url_contents(url, &origin)
    }

    pub(crate) fn object_url_type(&self, url: &str) -> Option<Arc<str>> {
        let origin = self.doc.borrow().origin.clone();
        self.runtime.object_url_type(url, &origin)
    }

    pub(crate) fn object_url_origin(&self, url: &str) -> Option<url::Origin> {
        let origin = self.doc.borrow().origin.clone();
        self.runtime.object_url_origin(url, &origin)
    }

    fn refund_object_url_bytes(&self, bytes: usize) {
        let mut budget = self.budget.borrow_mut();
        budget.object_url_bytes = budget.object_url_bytes.saturating_sub(bytes);
    }

    fn unload_object_urls_for_document(&mut self, document: u32) {
        let released = self
            .runtime
            .take_object_urls(|entry| entry.document == Some(document));
        self.refund_object_url_bytes(released);
    }

    fn unload_object_urls_for_frame(&mut self) {
        let frame = self.frame();
        let released = self.runtime.take_object_urls(|entry| entry.frame == frame);
        self.refund_object_url_bytes(released);
    }

    pub(crate) fn revoke_object_url(&mut self, url: &str) {
        // https://w3c.github.io/FileAPI/#dfn-revokeObjectURL
        let origin = self.doc.borrow().origin.clone();
        if self.runtime.object_url_origin(url, &origin).is_none() {
            return;
        }
        if let Some(released) = self.runtime.remove_object_url(url) {
            let mut budget = self.budget.borrow_mut();
            budget.object_url_bytes = budget.object_url_bytes.saturating_sub(released);
        }
    }

    pub(crate) fn add_listener(&mut self, target: EventTargetKey, listener: Rc<Listener>) {
        match target {
            EventTargetKey::Attribute { id, .. } => {
                if let Some(entry) = self
                    .runtime
                    .registry
                    .borrow_mut()
                    .attributes
                    .entries
                    .get_mut(&id)
                {
                    entry.listeners.push(listener);
                }
            }
            EventTargetKey::Standalone(id) => {
                self.standalone_listeners
                    .entry(id)
                    .or_default()
                    .push(listener);
            }
            EventTargetKey::Window | EventTargetKey::Node(_) => {
                self.win
                    .borrow_mut()
                    .events
                    .listeners
                    .entry(target)
                    .or_default()
                    .push(listener);
            }
        }
    }

    // https://dom.spec.whatwg.org/#concept-event-listener-invoke
    pub(crate) fn listener_snapshot(&self, target: EventTargetKey) -> Vec<Rc<Listener>> {
        match target {
            EventTargetKey::Attribute { id, .. } => self
                .runtime
                .registry
                .borrow()
                .attributes
                .entries
                .get(&id)
                .map_or_default(|entry| entry.listeners.clone()),
            EventTargetKey::Standalone(id) => self
                .standalone_listeners
                .get(&id)
                .cloned()
                .unwrap_or_default(),
            EventTargetKey::Window | EventTargetKey::Node(_) => self
                .win
                .borrow()
                .events
                .listeners
                .get(&target)
                .cloned()
                .unwrap_or_default(),
        }
    }

    pub(crate) fn remove_listener(&mut self, target: EventTargetKey, listener: &Rc<Listener>) {
        match target {
            EventTargetKey::Attribute { id, .. } => {
                if let Some(entry) = self
                    .runtime
                    .registry
                    .borrow_mut()
                    .attributes
                    .entries
                    .get_mut(&id)
                {
                    entry
                        .listeners
                        .retain(|existing| !Rc::ptr_eq(existing, listener));
                }
            }
            EventTargetKey::Standalone(id) => {
                if let Some(list) = self.standalone_listeners.get_mut(&id) {
                    list.retain(|existing| !Rc::ptr_eq(existing, listener));
                }
            }
            EventTargetKey::Window | EventTargetKey::Node(_) => {
                if let Some(list) = self.win.borrow_mut().events.listeners.get_mut(&target) {
                    list.retain(|existing| !Rc::ptr_eq(existing, listener));
                }
            }
        }
    }

    pub(crate) fn next_standalone_target(&mut self) -> u64 {
        let id = self.next_standalone_target;
        self.next_standalone_target = self.next_standalone_target.wrapping_add(1);
        id
    }

    pub(crate) fn intern_standalone_target(
        &mut self,
        id: u64,
        target: Persistent<Object<'static>>,
    ) {
        self.standalone_targets.insert(id, target);
    }

    pub(crate) fn standalone_target(&self, id: u64) -> Option<Persistent<Object<'static>>> {
        self.standalone_targets.get(&id).cloned()
    }

    pub(crate) fn set_window(&mut self, window: Persistent<Object<'static>>) {
        self.window_object = Some(window);
    }

    pub(crate) fn window_object(&self) -> Option<Persistent<Object<'static>>> {
        self.window_object.clone()
    }

    // https://html.spec.whatwg.org/multipage/dom.html#current-document-readiness
    pub(crate) fn main_ready_state(&self) -> ReadyState {
        self.main_document()
            .map_or(ReadyState::Complete, |parsed| parsed.ready_state)
    }

    pub(crate) fn set_main_ready_state(&mut self, state: ReadyState) {
        if let Some(mut parsed) = self.main_document_mut() {
            parsed.ready_state = state;
        }
    }

    pub(crate) fn node_parent(&self, id: NodeId) -> Option<NodeId> {
        self.document(id).and_then(|parsed| {
            parsed
                .document
                .base
                .get_node(id.node)
                .and_then(|node| node.parent)
                .map(|parent| NodeId {
                    document: id.document,
                    node: parent,
                })
        })
    }

    pub(crate) fn node_is_document(&self, id: NodeId) -> bool {
        self.document(id)
            .is_some_and(|parsed| parsed.document.base.root_node().id == id.node)
    }

    // https://dom.spec.whatwg.org/#concept-shadow-root
    pub(crate) fn shadow_root_host(&self, id: NodeId) -> Option<NodeId> {
        self.document(id).and_then(|parsed| {
            crate::dom::tree::shadow_root_host(&parsed.document.base, id.node).map(|host| NodeId {
                document: id.document,
                node: host,
            })
        })
    }

    pub(crate) fn clear_listeners(&mut self) {
        if self.isolated.is_some() {
            self.win.borrow_mut().events.forget_realm(self.realm);
        } else {
            self.win.borrow_mut().events.clear_realm();
            self.clear_observers();
            // https://w3c.github.io/FileAPI/#file
            let mut doc = self.doc.borrow_mut();
            doc.input_files.clear();
            doc.input_file_list.clear();
        }
        self.window_object = None;
        self.standalone_targets.clear();
        self.standalone_listeners.clear();
        self.next_standalone_target = 0;
        self.isolated_wrappers.borrow_mut().clear();
        self.caches.clear();
        self.brands.clear();
        self.current_event = None;
    }

    pub(crate) fn release_host_primitives(&mut self) {
        if self.isolated.is_none() {
            let frame = self.frame();
            self.runtime
                .registry
                .borrow_mut()
                .reactions
                .forget_frame(frame);
        }
        self.pristine_number = None;
        self.pristine_boolean = None;
        self.pristine_reflect_set = None;
        self.pristine_queue_microtask = None;
        self.pristine_eval_error = None;
        self.pristine_blob = None;
        self.pristine_set_timeout = None;
        self.event_ctors.clear();
        self.weak_references = None;
        self.deliver_mutations_fn = None;
        self.bridge = None;
        self.location_object = None;
        self.current_event = None;
    }

    pub(crate) fn same_object(
        &self,
        id: NodeId,
        member: SameObject,
    ) -> Option<Persistent<Value<'static>>> {
        self.runtime.registry.borrow().same_object(id, member)
    }

    pub(crate) fn intern_same_object(
        &mut self,
        id: NodeId,
        member: SameObject,
        value: Persistent<Value<'static>>,
    ) {
        self.runtime
            .registry
            .borrow_mut()
            .intern_same_object(id, member, value);
    }

    pub(crate) fn active_element(&self, document: u32) -> Option<NodeId> {
        self.runtime.registry.borrow().active_element(document)
    }

    pub(crate) fn selection_of(&self, id: NodeId) -> (u32, u32, u8) {
        self.runtime.registry.borrow().selection_of(id)
    }

    pub(crate) fn set_selection(&mut self, id: NodeId, start: u32, end: u32, direction: u8) {
        self.runtime
            .registry
            .borrow_mut()
            .set_selection(id, start, end, direction);
    }

    // https://w3c.github.io/webdriver/#elements
    pub(crate) fn remote_id(&mut self, node: NodeId) -> u64 {
        if let Some(&remote) = self.doc.borrow().state.remote_ids.get(&node) {
            return remote;
        }
        let remote = self.runtime.registry.borrow_mut().allocate_remote();
        let mut doc = self.doc.borrow_mut();
        doc.state.remote_ids.insert(node, remote);
        doc.state.remote_nodes.insert(remote, node);
        remote
    }

    pub(crate) fn node_for_remote(&self, remote: u64) -> Option<NodeId> {
        self.doc.borrow().state.remote_nodes.get(&remote).copied()
    }

    // https://html.spec.whatwg.org/multipage/nav-history-apis.html#dom-document-defaultview
    pub(crate) fn is_main_document(&self, node: NodeId) -> bool {
        self.doc.borrow().document == Some(node.document_id())
    }

    // https://html.spec.whatwg.org/multipage/webappapis.html#event-handler-map
    pub(crate) fn handler_slot(
        &self,
        node: Option<NodeId>,
        name: &str,
    ) -> Option<EventHandlerSlot> {
        self.win
            .borrow()
            .events
            .handlers
            .get(&(node, name.to_owned()))
            .cloned()
    }

    pub(crate) fn set_handler_value(
        &mut self,
        node: Option<NodeId>,
        name: &str,
        value: HandlerValue,
    ) {
        let mut win = self.win.borrow_mut();
        let entry = win
            .events
            .handlers
            .entry((node, name.to_owned()))
            .or_insert_with(|| EventHandlerSlot {
                value: HandlerValue::Null,
                listener: None,
            });
        entry.value = value;
    }

    // https://html.spec.whatwg.org/multipage/webappapis.html#activate-an-event-handler
    pub(crate) fn set_handler_listener(
        &mut self,
        node: Option<NodeId>,
        name: &str,
        listener: Option<Rc<Listener>>,
    ) {
        let mut win = self.win.borrow_mut();
        let entry = win
            .events
            .handlers
            .entry((node, name.to_owned()))
            .or_insert_with(|| EventHandlerSlot {
                value: HandlerValue::Null,
                listener: None,
            });
        entry.listener = listener;
    }

    pub(crate) fn handler_names(&self, node: Option<NodeId>) -> Vec<String> {
        self.win
            .borrow()
            .events
            .handlers
            .keys()
            .filter(|(key, _)| *key == node)
            .map(|(_, name)| name.clone())
            .collect()
    }

    // https://html.spec.whatwg.org/multipage/webappapis.html#deactivate-an-event-handler
    pub(crate) fn deactivate_handler_slot(&mut self, node: Option<NodeId>, name: &str) {
        let mut win = self.win.borrow_mut();
        if let Some(entry) = win.events.handlers.get_mut(&(node, name.to_owned())) {
            entry.value = HandlerValue::Null;
            entry.listener = None;
        } else {
            win.events.handlers.insert(
                (node, name.to_owned()),
                EventHandlerSlot {
                    value: HandlerValue::Null,
                    listener: None,
                },
            );
        }
    }

    pub(crate) fn take_handler(
        &mut self,
        node: Option<NodeId>,
        name: &str,
    ) -> Option<EventHandlerSlot> {
        self.win
            .borrow_mut()
            .events
            .handlers
            .remove(&(node, name.to_owned()))
    }

    pub(crate) fn set_active_element(&mut self, document: u32, node: Option<NodeId>) {
        self.runtime
            .registry
            .borrow_mut()
            .set_active_element(document, node);
        // https://drafts.csswg.org/selectors-4/#the-focus-pseudo
        if let Some(parsed) = self.runtime.documents.borrow_mut().get_mut(document) {
            match node {
                Some(focused) => {
                    parsed.document.base.set_focus_to(focused.node);
                }
                None => parsed.document.base.clear_focus(),
            }
        }
    }

    pub(crate) fn click_in_progress(&self, node: NodeId) -> bool {
        self.win.borrow().clicks_in_progress.contains(&node)
    }

    pub(crate) fn set_click_in_progress(&mut self, node: NodeId, running: bool) {
        let mut win = self.win.borrow_mut();
        if running {
            win.clicks_in_progress.insert(node);
        } else {
            win.clicks_in_progress.remove(&node);
        }
    }

    // https://html.spec.whatwg.org/multipage/interaction.html#activation-notification
    pub(crate) fn activate_user(&mut self) {
        let mut win = self.win.borrow_mut();
        win.transient_activation = Some(Instant::now() + Duration::from_secs(5));
        win.sticky_activation = true;
    }

    pub(crate) fn user_activation_active(&self) -> bool {
        self.win
            .borrow()
            .transient_activation
            .is_some_and(|expires| Instant::now() < expires)
    }

    pub(crate) fn user_activation_sticky(&self) -> bool {
        self.win.borrow().sticky_activation
    }

    // https://html.spec.whatwg.org/multipage/interaction.html#consume-user-activation
    pub(crate) fn consume_user_activation(&mut self) -> bool {
        if self.user_activation_active() {
            self.win.borrow_mut().transient_activation = None;
            true
        } else {
            false
        }
    }

    pub(crate) fn intern_brand(
        &mut self,
        name: impl Into<String>,
        proto: Persistent<Object<'static>>,
    ) {
        self.brands.insert(name.into(), proto);
    }

    pub(crate) fn brand(&self, name: &str) -> Option<Persistent<Object<'static>>> {
        self.brands.get(name).cloned()
    }
}

// https://w3c.github.io/hr-time/#dfn-current-high-resolution-time
pub(crate) fn clamp_time(millis: f64, resolution_micros: u32) -> f64 {
    if resolution_micros <= 1 {
        return millis;
    }
    let step = f64::from(resolution_micros);
    ((millis * 1000.0) / step).floor() * step / 1000.0
}

// https://dom.spec.whatwg.org/#concept-mo-queue
pub(super) fn match_observation(
    base: &blitz_dom::BaseDocument,
    document: u32,
    observer: &ObserverState,
    entry: &JournalEntry,
) -> Option<(usize, u64, RecordData)> {
    let (target, kind) = match entry {
        JournalEntry::ChildList { target, .. } => (*target, 0_u8),
        JournalEntry::Attributes { target, .. } => (*target, 1_u8),
        JournalEntry::CharacterData { target, .. } => (*target, 2_u8),
    };
    if target.document != document {
        return None;
    }
    let mut first_registration = None;
    let mut want_attribute_old_value = false;
    let mut want_character_data_old_value = false;
    for observation in observer.observations.iter().chain(&observer.transient) {
        let Some(root) = observation.target.tree() else {
            continue;
        };
        if root.document != document {
            continue;
        }
        let Some(depth) = ancestor_distance(base, root.node, target.node) else {
            continue;
        };
        if depth != 0 && !observation.options.subtree {
            continue;
        }
        let enabled = match kind {
            0 => observation.options.child_list,
            1 => {
                observation.options.attributes
                    && match (&observation.options.attribute_filter, entry) {
                        // https://dom.spec.whatwg.org/#queue-a-mutation-record
                        (
                            Some(filter),
                            JournalEntry::Attributes {
                                name, namespace, ..
                            },
                        ) => {
                            namespace.is_empty()
                                && filter
                                    .iter()
                                    .any(|wanted| wanted.iter().copied().eq(name.encode_utf16()))
                        }
                        _ => true,
                    }
            }
            _ => observation.options.character_data,
        };
        if !enabled {
            continue;
        }
        let position = (depth, observation.order);
        first_registration =
            Some(first_registration.map_or(position, |first: (usize, u64)| first.min(position)));
        want_attribute_old_value |= observation.options.attribute_old_value;
        want_character_data_old_value |= observation.options.character_data_old_value;
    }
    first_registration.map(|(depth, order)| {
        (
            depth,
            order,
            record(
                want_attribute_old_value,
                want_character_data_old_value,
                entry,
            ),
        )
    })
}

fn record(
    want_attribute_old_value: bool,
    want_character_data_old_value: bool,
    entry: &JournalEntry,
) -> RecordData {
    match entry {
        JournalEntry::ChildList {
            target,
            added,
            removed,
            previous,
            next,
        } => RecordData {
            added: added.iter().copied().map(Handle).collect(),
            removed: removed.iter().copied().map(Handle).collect(),
            previous: previous.map(Handle),
            next: next.map(Handle),
            ..RecordData::new("childList", Handle(*target))
        },
        JournalEntry::Attributes {
            target,
            name,
            namespace,
            old_value,
        } => RecordData {
            attribute_name: Some(name.clone()),
            attribute_namespace: (!namespace.is_empty()).then(|| namespace.clone()),
            old_value: want_attribute_old_value
                .then(|| old_value.clone())
                .flatten()
                .map(DomString::from),
            ..RecordData::new("attributes", Handle(*target))
        },
        JournalEntry::CharacterData { target, old_value } => RecordData {
            old_value: want_character_data_old_value.then(|| old_value.clone()),
            ..RecordData::new("characterData", Handle(*target))
        },
    }
}

pub(super) fn observes_region(
    base: &blitz_dom::BaseDocument,
    document: u32,
    observer: &ObserverState,
    entry: &JournalEntry,
) -> bool {
    let target = match entry {
        JournalEntry::ChildList { target, .. }
        | JournalEntry::Attributes { target, .. }
        | JournalEntry::CharacterData { target, .. } => *target,
    };
    if target.document != document {
        return false;
    }
    observer
        .observations
        .iter()
        .chain(&observer.transient)
        .any(|observation| {
            observation.target.tree().is_some_and(|root| {
                root.document == document
                    && ancestor_distance(base, root.node, target.node).is_some()
            })
        })
}

fn ancestor_distance(
    base: &blitz_dom::BaseDocument,
    ancestor: BlitzNodeId,
    node: BlitzNodeId,
) -> Option<usize> {
    let mut cursor = Some(node);
    let mut depth = 0;
    while let Some(id) = cursor {
        if id == ancestor {
            return Some(depth);
        }
        cursor = base.get_node(id).and_then(|node| node.parent);
        depth += 1;
    }
    None
}

#[derive(Clone)]
pub(crate) struct AttrState {
    pub scope: NodeId,
    pub document: NodeId,
    pub owner: Option<NodeId>,
    pub value: String,
    pub namespace: String,
    pub prefix: Option<String>,
    pub local: String,
    pub qualified: String,
}

// https://dom.spec.whatwg.org/#concept-element-attributes-append
#[derive(Default)]
pub(crate) struct AttributeRegistry {
    entries: HashMap<u64, AttrEntry>,
    attached: HashMap<(NodeId, String, String), u64>,
    next_id: u64,
}

struct AttrEntry {
    state: AttrState,
    wrapper: Option<Persistent<Value<'static>>>,
    listeners: Vec<Rc<Listener>>,
}

pub(crate) enum AttrAttachError {
    Stale,
    InUse,
    NotAnElement,
}

impl AttributeRegistry {
    pub(crate) fn create(&mut self, state: AttrState) -> Option<u64> {
        let id = self.next_id.checked_add(1)?;
        self.next_id = id;
        if let Some(owner) = state.owner {
            self.attached
                .insert((owner, state.namespace.clone(), state.local.clone()), id);
        }
        self.entries.insert(
            id,
            AttrEntry {
                state,
                wrapper: None,
                listeners: Vec::new(),
            },
        );
        Some(id)
    }

    pub(crate) fn state(&self, id: u64) -> Option<&AttrState> {
        self.entries.get(&id).map(|entry| &entry.state)
    }

    pub(crate) fn state_mut(&mut self, id: u64) -> Option<&mut AttrState> {
        self.entries.get_mut(&id).map(|entry| &mut entry.state)
    }

    pub(crate) fn wrapper(&self, id: u64) -> Option<Persistent<Value<'static>>> {
        self.entries
            .get(&id)
            .and_then(|entry| entry.wrapper.clone())
    }

    pub(crate) fn intern_wrapper(&mut self, id: u64, value: Persistent<Value<'static>>) {
        if let Some(entry) = self.entries.get_mut(&id) {
            entry.wrapper = Some(value);
        }
    }

    pub(crate) fn attached(&self, element: NodeId, namespace: &str, local: &str) -> Option<u64> {
        self.attached
            .get(&(element, namespace.to_owned(), local.to_owned()))
            .copied()
    }

    pub(crate) fn detach(&mut self, element: NodeId, namespace: &str, local: &str) {
        if let Some(id) = self
            .attached
            .remove(&(element, namespace.to_owned(), local.to_owned()))
            && let Some(state) = self.state_mut(id)
        {
            state.owner = None;
        }
    }

    pub(crate) fn touch(&mut self, element: NodeId, namespace: &str, local: &str, value: String) {
        if let Some(id) = self.attached(element, namespace, local)
            && let Some(state) = self.state_mut(id)
        {
            state.value = value;
        }
    }

    // https://dom.spec.whatwg.org/#concept-element-attributes-set
    pub(crate) fn attach(
        &mut self,
        id: u64,
        base: &mut blitz_dom::BaseDocument,
        element: NodeId,
        value: String,
    ) -> Result<Option<u64>, AttrAttachError> {
        let entry = self.entries.get_mut(&id).ok_or(AttrAttachError::Stale)?;
        let state = &mut entry.state;
        if state.owner.is_some_and(|owner| owner != element) {
            return Err(AttrAttachError::InUse);
        }
        let key = (element, state.namespace.clone(), state.local.clone());
        let previous = self.attached.get(&key).copied();
        if previous == Some(id) {
            return Ok(previous);
        }
        let Some(node) = base.get_node(element.node) else {
            return Err(AttrAttachError::Stale);
        };
        if node.data.downcast_element().is_none() {
            return Err(AttrAttachError::NotAnElement);
        }
        let name = markup5ever::QualName::new(
            state.prefix.clone().map(markup5ever::Prefix::from),
            markup5ever::Namespace::from(state.namespace.clone()),
            markup5ever::LocalName::from(state.local.clone()),
        );
        base.mutate().set_attribute(element.node, name, &value);
        state.owner = Some(element);
        state.document = NodeId {
            document: element.document,
            node: base.root_node().id,
        };
        state.value = value;
        if let Some(previous) = previous
            && let Some(state) = self.state_mut(previous)
        {
            state.owner = None;
        }
        self.attached.insert(key, id);
        Ok(previous)
    }

    fn forget_document(&mut self, document: u32) {
        self.entries
            .retain(|_, entry| entry.state.document.document_id() != document);
        self.attached
            .retain(|(element, _, _), _| element.document_id() != document);
    }

    fn clear(&mut self) {
        self.entries.clear();
        self.attached.clear();
    }
}

#[cfg(test)]
mod control_state_tests {
    use super::ControlState;

    #[test]
    fn clearing_an_edit_keeps_custom_validity_and_output_state() {
        let mut state = ControlState {
            user_edited: true,
            bad_input: true,
            custom_validity: vec![u16::from(b'x')],
            number_buffer: Some("1e".to_owned()),
            caret: Some(2),
            output_value_mode: true,
            output_default_value: Some("default".encode_utf16().collect()),
        };
        state.clear_edit_state();
        assert!(!state.user_edited);
        assert!(!state.bad_input);
        assert!(state.number_buffer.is_none());
        assert!(state.caret.is_none());
        assert_eq!(state.custom_validity, vec![u16::from(b'x')]);
        assert!(state.output_value_mode);
        assert_eq!(
            state.output_default_value,
            Some("default".encode_utf16().collect::<Vec<u16>>())
        );
    }
}
