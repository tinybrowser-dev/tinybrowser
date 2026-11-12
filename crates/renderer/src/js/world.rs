//! Shared JS world for the renderer.

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

/// Renderer-process realm bookkeeping shared by every frame.
///
/// Owned by the page engine and dropped while its `QuickJS` runtime is still
/// alive, so cached wrappers never outlive the heap.
#[derive(Default)]
pub(crate) struct RealmRegistry {
    pub(crate) realm_contexts: Vec<usize>,
    pub(crate) attributes: AttributeRegistry,
    pub(crate) observers: super::observers::MutationObservers,
    pub(crate) reactions: super::reactions::CustomElementReactions,
    pub(crate) private_slots: Option<PrivateSlots>,
    budget: Rc<RefCell<ResourceBudget>>,
    /// The World that owns each document id, for wrapper realm resolution.
    documents: HashMap<u32, Weak<RefCell<World>>>,
    /// The World that owns each frame, for `window.parent` and `event.source`.
    frames: HashMap<FrameId, Weak<RefCell<World>>>,
    /// One wrapper per node, shared by every realm in this renderer process.
    /// The persistent holds a `WeakRef`, so an unreferenced wrapper can still
    /// be collected.
    wrappers: WrapperCache<NodeId>,
    /// One platform object per `[SameObject]` member, shared by every realm
    /// in this renderer process like the node wrappers themselves: the
    /// member object belongs to the node, not to the realm that first read
    /// it.
    same_object: WrapperCache<(NodeId, SameObject)>,
    /// The active child document for each connected iframe container.
    frame_documents: HashMap<NodeId, NodeId>,
    /// The realm that created each node. Adoption changes the node document
    /// but not this realm
    /// (<https://dom.spec.whatwg.org/#concept-node-adopt>,
    /// <https://github.com/whatwg/dom/issues/977>).
    creation_realms: HashMap<NodeId, Weak<RefCell<World>>>,
    /// Adopted node ids follow the destination copy so live collections still
    /// rooted at the pre-adopt handle walk the live tree
    /// (<https://dom.spec.whatwg.org/#concept-node-adopt>).
    adopted_ids: HashMap<NodeId, NodeId>,
    /// `WebDriver` element ids, allocated across every world and frame so a
    /// reference cannot alias between browsing contexts.
    next_remote: u64,
    /// Canvas bitmaps keyed by an opaque id. The id names the bitmap, not a
    /// DOM node, so an `OffscreenCanvas` needs no element.
    canvases: HashMap<CanvasId, CanvasSlot>,
    /// The bitmap a `<canvas>` element draws into, once it has a context.
    canvas_nodes: HashMap<NodeId, CanvasId>,
    next_canvas: u64,
    /// Identifies this realm among the worlds sharing the registry, so a
    /// documentless realm can own offscreen canvases.
    next_realm: u64,
    /// Decoded `ImageBitmap` pixels, keyed by an opaque id. The id is exposed
    /// on the JS object; the pixels are not, so a page cannot read a
    /// cross-origin image through the bitmap's internal storage.
    bitmaps: HashMap<u64, BitmapPixels>,
    /// Rasters of vector image sources, bounded by entry count and bytes.
    /// The entry keeps the parsed tree alive so a tree pointer cannot be
    /// reused by a later parse while its raster is cached.
    pub(crate) svg_rasters: super::canvas::SvgRasterCache,
    next_bitmap: u64,
    /// `TextDecoder` session ids, allocated across every world and frame so
    /// a freed id can never alias a live session, even if a finalizer runs
    /// under another realm.
    next_decoder: u64,
}

pub(crate) struct PrivateSlots {
    pub(crate) factory: Persistent<Function<'static>>,
    pub(crate) realms: HashSet<usize>,
}

/// Cached JS platform objects, keyed by whatever identifies the object: a
/// node for the shared wrapper, a `(node, member)` pair for the
/// lazily-created same-object members.
///
/// A cache entry holds a `WeakRef` (a `Persistent` to a weak wrapper), so an
/// unreferenced object can still be collected; callers dereference with
/// `deref_weak` and rebuild on a miss.
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

    /// Moves an entry to a new key; adoption retargets the caller's wrapper
    /// to the destination copy
    /// (<https://dom.spec.whatwg.org/#concept-node-adopt>).
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
}

/// A lazily-created member whose identity is one per node
/// (`[SameObject]` in IDL).
///
/// The cache is keyed by node and member, not by interface: `Document.children`
/// and `Document.scripts` are both `HTMLCollection`, and `classList`/`relList`
/// are both `DOMTokenList`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum SameObject {
    /// `Element.classList`.
    ClassList,
    /// `HTMLIFrameElement.sandbox`.
    Sandbox,
    /// `Element.attributes`.
    Attributes,
    /// `HTMLElement.style`.
    Style,
    /// `HTMLElement.dataset`.
    Dataset,
    /// `Node.childNodes`.
    ChildNodes,
    /// `Document.scripts`.
    Scripts,
    /// `Document.links`.
    Links,
    /// `HTMLCanvasElement.getContext("2d")`.
    Canvas2d,
    /// `HTMLCanvasElement.getContext("bitmaprenderer")`.
    CanvasBitmap,
}

impl SameObject {
    /// Every member, for rekeying the cache on adoption.
    pub(crate) const ALL: [SameObject; 10] = [
        SameObject::ClassList,
        SameObject::Sandbox,
        SameObject::Attributes,
        SameObject::Style,
        SameObject::Dataset,
        SameObject::ChildNodes,
        SameObject::Scripts,
        SameObject::Links,
        SameObject::Canvas2d,
        SameObject::CanvasBitmap,
    ];
}

/// Largest canvas backing store the renderer allocates. A page controls
/// `canvas.width` and `canvas.height`, so an unchecked `width * height * 4`
/// is a process abort, not a rendering bug.
pub(crate) const MAX_CANVAS_BYTES: usize = 64 * 1024 * 1024;

/// Byte length of a `width` x `height` RGBA bitmap, or `None` when it exceeds
/// [`MAX_CANVAS_BYTES`] or would overflow.
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

/// Which platform object owns one canvas bitmap.
///
/// An element slot follows its node through adoption and dies with the node's
/// document; an offscreen slot belongs to the realm that constructed it, even
/// when that realm has no document
/// (<https://html.spec.whatwg.org/multipage/canvas.html#offscreencanvas>).
#[derive(Clone, Copy, Debug)]
pub(crate) enum CanvasOwner {
    Element(NodeId),
    Offscreen { realm: u64, document: Option<u32> },
}

/// The context mode of a canvas bitmap. `OffscreenCanvas` uses the full set;
/// `<canvas>` only reaches `TwoD` or `Bitmap` through `getContext`
/// (<https://html.spec.whatwg.org/multipage/canvas.html#concept-canvas-context-mode>,
/// <https://html.spec.whatwg.org/multipage/canvas.html#offscreencanvas-context-mode>).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CanvasMode {
    None,
    TwoD,
    Bitmap,
    Detached,
}

/// One bitmap and the dimension of its backing store. `Canvas2D` is allocated
/// on first drawing, so creating a context neither reserves pixels nor fails
/// for a canvas that is never painted.
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

    /// The next bitmap id. Ids are opaque and never reused, so a stale
    /// `__tbCanvasId` cannot reach a later canvas.
    fn allocate_canvas(&mut self) -> CanvasId {
        self.next_canvas = self.next_canvas.saturating_add(1);
        CanvasId(self.next_canvas)
    }

    /// A realm id for a documentless `OffscreenCanvas` owner.
    pub(crate) fn allocate_realm(&mut self) -> u64 {
        self.next_realm = self.next_realm.saturating_add(1);
        self.next_realm
    }

    /// The bitmap a `<canvas>` element draws into, created on its first
    /// `getContext` call
    /// (<https://html.spec.whatwg.org/multipage/canvas.html#dom-canvas-getcontext>).
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

    /// A detached bitmap for `OffscreenCanvas`
    /// (<https://html.spec.whatwg.org/multipage/canvas.html#offscreencanvas>).
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

    /// The bitmap a `<canvas>` element already draws into, if any.
    pub(crate) fn canvas_id_for_node(&self, node: NodeId) -> Option<CanvasId> {
        self.canvas_nodes.get(&node).copied()
    }

    /// Whether this realm may reach `id`. Element slots follow the node they
    /// belong to; offscreen slots follow the realm that constructed them.
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

    /// The context mode already chosen for an element, even after its context
    /// object was collected. `getContext` must keep refusing a second mode
    /// (<https://html.spec.whatwg.org/multipage/canvas.html#concept-canvas-context-mode>).
    pub(crate) fn canvas_mode_for_node(&self, node: NodeId) -> Option<CanvasMode> {
        let id = self.canvas_nodes.get(&node)?;
        self.canvases.get(id).map(|slot| slot.mode)
    }

    /// Drops every offscreen canvas a realm owns. A realm with no document
    /// never reaches `forget_document`, so its bitmaps would otherwise stay
    /// for the life of the tab.
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

    /// The element backing a canvas, or `None` for an `OffscreenCanvas`.
    pub(crate) fn canvas_node(&self, id: CanvasId) -> Option<NodeId> {
        match self.canvases.get(&id)?.owner {
            CanvasOwner::Element(node) => Some(node),
            CanvasOwner::Offscreen { .. } => None,
        }
    }

    pub(crate) fn canvas_dimensions(&self, id: CanvasId) -> Option<(u32, u32)> {
        self.canvases.get(&id).map(|slot| (slot.width, slot.height))
    }

    /// Drops the bitmap and context state. Setting `canvas.width` or
    /// `canvas.height` resets the context even when the value is unchanged
    /// (<https://html.spec.whatwg.org/multipage/canvas.html#dom-canvas-width>).
    pub(crate) fn reset_canvas(&mut self, id: NodeId, width: u32, height: u32) {
        let Some(canvas_id) = self.canvas_nodes.get(&id).copied() else {
            return;
        };
        self.resize_canvas(canvas_id, width, height);
    }

    /// Replaces the bitmap dimensions and resets a live 2D context.
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

    /// The transfer steps leave the sender without a bitmap.
    pub(crate) fn detach_canvas(&mut self, id: CanvasId) {
        if let Some(slot) = self.canvases.get_mut(&id) {
            slot.mode = CanvasMode::Detached;
            slot.width = 0;
            slot.height = 0;
            slot.canvas = None;
        }
    }

    /// The 2D context state for `id`, allocated on first drawing.
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

    /// The origin-clean flag of a live canvas, or `None` when it has no
    /// context yet.
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

    /// Replaces the bitmap with a fresh transparent one without touching the
    /// drawing state.
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

    /// A straight-RGBA copy of a canvas bitmap, for `drawImage` and patterns.
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

    /// A gradient defined on another canvas, for cross-canvas styles
    /// (<https://html.spec.whatwg.org/multipage/canvas.html#canvasgradient>).
    pub(crate) fn canvas_gradient(&self, id: CanvasId, resource: u32) -> Option<Gradient> {
        self.canvases
            .get(&id)?
            .canvas
            .as_ref()?
            .gradient_definition(resource)
    }

    /// A pattern defined on another canvas, for cross-canvas styles
    /// (<https://html.spec.whatwg.org/multipage/canvas.html#canvaspattern>).
    pub(crate) fn canvas_pattern(&self, id: CanvasId, resource: u32) -> Option<Pattern> {
        self.canvases
            .get(&id)?
            .canvas
            .as_ref()?
            .pattern_definition(resource)
    }

    /// The next process-unique `WebDriver` element id.
    ///
    /// Ids keep counting past the range a JS number holds exactly, because
    /// [`crate::js::bridge::js_number`] turns such an id into `NaN` rather than
    /// a neighbouring one; the shim then answers no such element, so two nodes
    /// never share a page-side id. The `WebDriver` string form carries the id
    /// as decimal text and still addresses it
    /// (<https://tc39.es/ecma262/#sec-number.max_safe_integer>).
    pub(crate) fn allocate_remote(&mut self) -> u64 {
        self.next_remote = self.next_remote.saturating_add(1);
        self.next_remote
    }

    /// The next process-unique `TextDecoder` session id.
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

/// Releases the stream budget reserved for one queued `document.write`.
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

/// One queued `document.write` body and the budget it reserved.
pub(crate) struct HtmlWrite {
    pub(crate) html: String,
    _reservation: StreamReservation,
}

const MAX_PENDING_STREAM_BYTES: usize = 8 * 1024 * 1024;
const MAX_OBJECT_URL_BYTES: usize = 8 * 1024 * 1024;

/// A version-4 UUID for one object URL, so the URL keeps the File API's shape
/// and never repeats across realms
/// (<https://w3c.github.io/FileAPI/#dfn-createObjectURL>).
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
    /// Remembers that `world` owns the document `id`.
    pub(crate) fn insert_document(&mut self, id: u32, world: &Rc<RefCell<World>>) {
        self.documents.insert(id, Rc::downgrade(world));
    }

    /// Remembers that `world` is the realm of `frame`.
    pub(crate) fn insert_frame(&mut self, frame: FrameId, world: &Rc<RefCell<World>>) {
        self.frames.insert(frame, Rc::downgrade(world));
    }

    /// The realm of `frame`, while it is alive.
    pub(crate) fn frame_world(&self, frame: FrameId) -> Option<Rc<RefCell<World>>> {
        self.frames.get(&frame).and_then(Weak::upgrade)
    }

    /// Drops the realm association for a frame that is gone.
    pub(crate) fn forget_frame_world(&mut self, frame: FrameId) {
        self.frames.remove(&frame);
    }

    /// The realm that owns the document `id` points into, if still alive.
    pub(crate) fn owner_world(&self, id: NodeId) -> Option<Rc<RefCell<World>>> {
        self.documents
            .get(&id.document_id())
            .and_then(Weak::upgrade)
    }

    /// Drops every realm and wrapper association for a document that is gone.
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
    }

    /// The shared wrapper cache entry for `id`, when one exists.
    pub(crate) fn wrapper(&self, id: NodeId) -> Option<Persistent<Value<'static>>> {
        self.wrappers.get(&id)
    }

    /// The cached `[SameObject]` member for `(id, member)`, when one exists.
    pub(crate) fn same_object(
        &self,
        id: NodeId,
        member: SameObject,
    ) -> Option<Persistent<Value<'static>>> {
        self.same_object.get(&(id, member))
    }

    /// Publishes the cached `[SameObject]` member.
    pub(crate) fn intern_same_object(
        &mut self,
        id: NodeId,
        member: SameObject,
        value: Persistent<Value<'static>>,
    ) {
        self.same_object.insert((id, member), value);
    }

    /// Publishes the wrapper cache entry for `id`.
    pub(crate) fn intern_wrapper(&mut self, id: NodeId, value: Persistent<Value<'static>>) {
        self.wrappers.insert(id, value);
    }

    /// Moves the shared wrapper cache entry from `from` to `to`.
    ///
    /// Adoption copies the node into another tree and retargets the existing
    /// wrapper so the caller's object is the adopted node
    /// (<https://dom.spec.whatwg.org/#concept-node-adopt>).
    pub(crate) fn rekey_wrapper(&mut self, from: NodeId, to: NodeId) {
        if from != to {
            self.adopted_ids.insert(from, to);
        }
        self.wrappers.rekey(&from, to);
        // The member objects follow the node too, so a held `classList` stays
        // the element's `classList` across adoption.
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
    }

    /// Records the realm that created `id` the first time it is seen.
    pub(crate) fn stamp_creation_realm(&mut self, id: NodeId, world: &Rc<RefCell<World>>) {
        self.creation_realms
            .entry(id)
            .or_insert_with(|| Rc::downgrade(world));
    }

    /// The realm that created `id`, while that realm is alive.
    pub(crate) fn creation_realm(&self, id: NodeId) -> Option<Rc<RefCell<World>>> {
        self.creation_realms.get(&id).and_then(Weak::upgrade)
    }

    /// The current id of a node that may have been adopted since `id` was
    /// captured. Chains compress on read: repeated adopts of one wrapper
    /// would otherwise walk a link per generation on every live-collection
    /// query, and entries only shrink on document death. A cycle returns the
    /// entry point uncompressed (wrong id, loudly visible as a stale wrapper)
    /// rather than looping; rekey targets are freshly materialized ids, so
    /// cycles should not arise outside double-adopt races under test.
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

    /// Drops every cached wrapper; called while the runtime is still alive.
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
    }

    fn budget(&self) -> Rc<RefCell<ResourceBudget>> {
        Rc::clone(&self.budget)
    }
}

/// A child frame navigation queued from inside a script, applied after it
/// stops: the `iframe`'s `src` changed (or the element just connected).
/// Where a queued navigation lands.
#[derive(Clone)]
pub(crate) enum NavigationTarget {
    /// The child frame whose container is this node.
    Container(NodeId),
    /// The frame with this identity, chosen by `window.open` before the
    /// navigation was queued.
    Frame(FrameId),
    /// The `target` of a link or form, parsed once at the boundary. The
    /// engine applies the rules for choosing a navigable once the script
    /// stops, so a queued navigation never carries a second spelling of the
    /// same choice.
    Target(crate::protocol::TargetName),
}

/// Where a navigation's referrer comes from.
///
/// `noreferrer` cannot be combined with a policy, so no caller has to decide
/// which one wins
/// (<https://html.spec.whatwg.org/multipage/nav-history-apis.html#dom-open>).
#[derive(Clone, Copy, Debug)]
pub(crate) enum NavigationReferrer {
    /// Determine the referrer from the initiator document, under this
    /// element's policy when it names one.
    Client {
        /// The navigating element's `hyperlink referrer policy`, `None` when
        /// the initiator document's policy stays in force
        /// (<https://html.spec.whatwg.org/multipage/links.html#hyperlink-referrer-policy>).
        policy: Option<ReferrerPolicy>,
    },
    /// `noreferrer`: the navigation sends no referrer at all.
    NoReferrer,
}

impl Default for NavigationReferrer {
    fn default() -> Self {
        Self::Client { policy: None }
    }
}

#[derive(Clone)]
pub(crate) struct FrameNavigation {
    /// The frame to navigate.
    pub(crate) target: NavigationTarget,
    /// The spec to navigate to, absolute or resolvable against the queuing
    /// document.
    pub(crate) spec: String,
    /// The `srcdoc` markup that replaces the spec, navigating to
    /// `about:srcdoc`
    /// (<https://html.spec.whatwg.org/multipage/iframe-embed-object.html#attr-iframe-srcdoc>).
    pub(crate) srcdoc: Option<String>,
    /// HTTP method; `GET` for every navigation but a form submission.
    pub(crate) method: net::Method,
    /// Request body, empty for a GET.
    pub(crate) body: Vec<u8>,
    /// `Content-Type` for `body`, when there is one.
    pub(crate) content_type: Option<String>,
    /// Where this navigation's referrer comes from.
    pub(crate) referrer: NavigationReferrer,
    /// The sandboxing flags the target frame's container imposes on the
    /// document this navigation commits, carried here so a failed or replaced
    /// navigation never changes the current document's flags
    /// (<https://html.spec.whatwg.org/multipage/browsing-the-web.html#determine-navigation-params>).
    pub(crate) sandbox: SandboxFlags,
    /// The document that started the navigation when it is not the one the
    /// navigation was queued on: a cross-frame `Location` setter takes its
    /// referrer, policy container, and sandbox rules from the incumbent
    /// document, not from the target
    /// (<https://html.spec.whatwg.org/multipage/nav-history-apis.html#dom-location-href>).
    pub(crate) source: Option<FrameId>,
}

impl FrameNavigation {
    /// A GET navigation, the shape of every navigation but a form submission.
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

    /// A `srcdoc` navigation: the markup loads as the frame's
    /// `about:srcdoc` document
    /// (<https://html.spec.whatwg.org/multipage/iframe-embed-object.html#attr-iframe-srcdoc>).
    pub(crate) fn srcdoc(target: NavigationTarget, markup: String) -> Self {
        let mut navigation = Self::get(target, String::new());
        navigation.srcdoc = Some(markup);
        navigation
    }

    /// The policy this navigation's referrer is determined under, overriding
    /// the initiator document's for this navigation alone. A navigation
    /// already marked no-referrer keeps that decision.
    pub(crate) fn with_referrer_policy(mut self, policy: Option<ReferrerPolicy>) -> Self {
        if let NavigationReferrer::Client { policy: current } = &mut self.referrer {
            *current = policy;
        }
        self
    }

    /// The sandboxing flags the target frame's container imposes at commit.
    pub(crate) fn with_sandbox(mut self, sandbox: SandboxFlags) -> Self {
        self.sandbox = sandbox;
        self
    }

    /// Marks this navigation as sending no referrer (`window.open`'s
    /// `noreferrer` feature).
    pub(crate) fn without_referrer(mut self) -> Self {
        self.referrer = NavigationReferrer::NoReferrer;
        self
    }

    /// Records the document that started the navigation when it is not the
    /// document the navigation runs on.
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

/// One DOM node handle owned by the JS world.
#[derive(Clone, Copy, rquickjs::JsLifetime)]
pub(crate) struct Handle(pub(crate) NodeId);

impl<'js> Trace<'js> for Handle {
    fn trace<'a>(&self, _tracer: rquickjs::class::Tracer<'a, 'js>) {}
}

/// `MutationObserver` options, already normalized. The booleans mirror the
/// `MutationObserverInit` dictionary one-for-one.
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

/// Both native representations implementing Node.
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

/// One queued `MutationRecord`, ready to wrap for JS.
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
    /// Not traced: a `DomString` holds no JavaScript value.
    #[qjs(skip_trace)]
    pub old_value: Option<crate::dom_string::DomString>,
}

impl RecordData {
    /// A record of one mutation type on one target, with every payload
    /// collection empty.
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
    /// The observer platform object, for the callback's `this` value and
    /// second argument
    /// (<https://dom.spec.whatwg.org/#notify-mutation-observers>). Present
    /// while the observer is registered; cleared by `disconnect`.
    pub object: Option<Persistent<Object<'static>>>,
    pub observations: Vec<Observation>,
    pub queue: Vec<RecordData>,
}

/// One observer with queued records, ready for callback delivery.
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
    /// Cleared when the listener is removed; a clone taken for dispatch still
    /// sees the removal (<https://dom.spec.whatwg.org/#concept-event-listener>).
    pub removed: Cell<bool>,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum EventTargetKey {
    Window,
    Node(NodeId),
    /// Attr registry identity: immutable creation scope and agent-issued id.
    Attribute {
        scope: NodeId,
        id: u64,
    },
    /// A constructible `EventTarget`, numbered per world.
    Standalone(u64),
}

/// One `blob:` URL's data and the `Blob.type` it was created from, so a
/// fetch of the URL can carry a `Content-Type`. `origin` is the creator, and
/// a lookup from another origin is a miss
/// (<https://w3c.github.io/FileAPI/#blob-url>,
/// <https://w3c.github.io/FileAPI/#blob-url-store>).
pub(crate) struct ObjectUrlEntry {
    pub(crate) contents: Arc<str>,
    pub(crate) content_type: Arc<str>,
    pub(crate) origin: url::Origin,
    frame: FrameId,
    document: Option<u32>,
}

/// The Blob URL Store key: the URL without its fragment
/// (<https://w3c.github.io/FileAPI/#blob-url-store>).
pub(crate) fn object_url_key(url: &str) -> &str {
    url.split_once('#').map_or(url, |(head, _)| head)
}

fn same_document_url(document_url: &Url, url: &Url) -> bool {
    let mut own = document_url.clone();
    let mut other = url.clone();
    own.set_fragment(None);
    other.set_fragment(None);
    own == other
}

/// Opaque captured-origin match: never `Url::origin()`, which is a new
/// `Origin::Opaque` for `about:blank`, `data:`, and `file:`.
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

/// Builds a child navigable's initial document for one `iframe`.
pub(crate) type ChildFrameHost = Rc<dyn Fn(&Rc<RefCell<World>>, FrameId, NodeId)>;

/// Builds an auxiliary top-level context's initial document.
pub(crate) type AuxiliaryFrameHost =
    Rc<dyn Fn(&Rc<RefCell<World>>, FrameId, crate::protocol::OpenerPolicy)>;

/// Builds the documents the `document` layer owns when a realm asks for one.
/// Installed by that layer, so the realm asks for a browsing context's
/// document without naming a document type.
#[derive(Clone)]
pub(crate) struct FrameHost {
    pub(crate) child: ChildFrameHost,
    pub(crate) auxiliary: AuxiliaryFrameHost,
}

/// One face's width, style, and weight, as fontique identifies it.
pub(crate) type FontFaceKey = (
    parley::fontique::FontWidth,
    parley::fontique::FontStyle,
    parley::fontique::FontWeight,
);

/// Every face added for one family.
pub(crate) type FontFamilyEntry = (parley::fontique::FamilyId, Vec<FontFaceKey>);

/// One call to `__tbRegisterFont`, with the exact faces added to each
/// collection so `__tbUnregisterFont` can remove only those faces. The
/// document and runtime collections are separate clones, so their face lists
/// are kept apart: removing a key from one must never touch the other.
pub(crate) struct FontRegistration {
    pub(crate) document: Option<u32>,
    pub(crate) document_fonts: Vec<FontFamilyEntry>,
    pub(crate) runtime_fonts: Vec<FontFamilyEntry>,
}

/// The request inputs an element's own attributes supply: its `crossorigin`
/// CORS settings state and its `referrerpolicy` override
/// (<https://html.spec.whatwg.org/multipage/urls-and-fetching.html#cors-settings-attributes>,
/// <https://html.spec.whatwg.org/multipage/urls-and-fetching.html#referrer-policy-attributes>).
///
/// Both are the attribute's state as the engine parses it; the dial's wire
/// types are a translation at the call site.
#[derive(Default)]
pub(crate) struct SubresourceOptions {
    /// The `crossorigin` CORS settings state; `None` is the `No CORS` state.
    pub(crate) crossorigin: Option<blitz_traits::net::CorsSetting>,
    /// The element's `referrerpolicy`, when it names one.
    pub(crate) referrer_policy: Option<blitz_traits::net::ReferrerPolicy>,
}

/// Which global a realm exposes.
///
/// A document realm is the Window the page runs in. A dedicated worker realm
/// has no document, no browsing context, and a `DedicatedWorkerGlobalScope`
/// instead of a Window
/// (<https://html.spec.whatwg.org/multipage/workers.html#dedicatedworkerglobalscope>).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum GlobalKind {
    Document,
    DedicatedWorker {
        id: crate::worker::WorkerId,
        /// The document frame that constructed the worker: the worker's only
        /// message destination and the owner of its `Worker` object.
        parent: FrameId,
        name: String,
        /// Whether the worker was constructed with `{type: "module"}`. A
        /// module worker's `importScripts()` throws a `TypeError`
        /// (<https://html.spec.whatwg.org/multipage/workers.html#dom-workerglobalscope-importscripts>).
        module: bool,
    },
    /// A shared worker realm: no document, and a `SharedWorkerGlobalScope`.
    /// It has no single owner; every connection reaches it over its own
    /// routed port
    /// (<https://html.spec.whatwg.org/multipage/workers.html#shared-workers-and-the-sharedworkerglobalscope-interface>).
    SharedWorker {
        id: crate::worker::WorkerId,
        name: String,
        module: bool,
    },
}

/// The site context a child realm's parent provides: the parent's origin and
/// whether the parent itself is first-party.
///
/// A realm is first-party exactly when its parent is and its own site matches
/// the parent's, which folds the whole ancestor chain into one value.
#[derive(Clone, Debug)]
pub(crate) struct ParentContext {
    pub(crate) origin: url::Origin,
    pub(crate) first_party: bool,
}

pub(crate) struct World {
    /// The handles every frame of this renderer process shares: trees,
    /// registry and wrapper cache, ports, JS heap, wake handle, and stop flag.
    pub(crate) runtime: FrameRuntime,
    /// The frame this realm belongs to. A worker uses a synthetic id outside
    /// the frame namespace because it has no browsing context.
    frame: FrameId,
    /// The kind of global this realm exposes.
    pub(crate) kind: GlobalKind,
    /// The browser tab this document belongs to, when a browser owns it. A
    /// popup tab's `close()` names it; a child frame or an embedded carrier
    /// has none.
    pub(crate) own_window: Option<crate::protocol::WindowId>,
    /// This realm's id in the shared registry. It owns offscreen canvases even
    /// when the realm has no document.
    realm: u64,
    /// Page-scoped scripts registered through CDP to run in every new realm
    /// of this tab, shared by every frame's world, with their identifiers
    /// (<https://chromedevtools.github.io/devtools-protocol/tot/Page/#method-addScriptToEvaluateOnNewDocument>).
    pub(crate) init_scripts: Rc<RefCell<Vec<(u64, String)>>>,
    /// The frame's persistent viewport size in device pixels, readable
    /// without an active document.
    pub(crate) viewport_size: Cell<(u32, u32)>,
    /// Frames whose browsing context was registered inside a script but whose
    /// realm cannot be created until JS execution has stopped.
    pending_frames: Vec<(FrameId, NodeId)>,
    /// Classic inline scripts inserted into this document from another realm
    /// while this realm did not exist yet. The engine runs them through
    /// `prepare_classic_script` once the realm is created.
    deferred_scripts: Vec<NodeId>,
    /// Creates a child frame's document when an `iframe` connects, and an
    /// auxiliary context's initial document when `window.open` runs. The
    /// `document` layer installs this hook; the realm asks for a document and
    /// never names one.
    frame_host: Option<FrameHost>,
    /// The active document of the frame this realm belongs to.
    document: Option<u32>,
    /// The realm's own context, kept so a script inserted from another realm
    /// can run in this document's realm while the caller's script is still on
    /// the stack. Cleared when the realm is torn down, so it never keeps an
    /// old realm alive.
    pub(crate) realm_context: Option<rquickjs::Context>,
    /// `close()` was called inside this worker realm. The worker thread reads
    /// it after each task, because the realm cannot reach its own handle
    /// (<https://html.spec.whatwg.org/multipage/workers.html#dom-dedicatedworkerglobalscope-close>).
    pub(crate) worker_close_requested: Rc<Cell<bool>>,
    /// Set while the engine compiles an event handler content attribute into a
    /// function. That compilation is HTML's, and CSP governs the handler with
    /// its inline check rather than the `unsafe-eval` rule that applies to
    /// page script compiling strings
    /// (<https://w3c.github.io/webappsec-csp/#can-compile-strings>).
    ///
    /// A `Cell` because the string-compilation hook reads it from a shared
    /// borrow while the handler's constructor call is in flight.
    pub(crate) compiling_handler: Cell<bool>,
    /// Set for the one compilation a timer's string handler performs. The
    /// handler is checked once when the timer is created, not every time it
    /// fires, so the hook admits this compilation and then resumes normal
    /// checking for any `eval` the handler itself runs
    /// (<https://html.spec.whatwg.org/multipage/timers-and-user-prompts.html#timer-initialisation-steps>).
    pub(crate) timer_string: Cell<bool>,
    /// Document ids this realm created. Its logs feed every realm's
    /// observers through the union drain.
    owned: HashSet<u32>,
    pub document_url: Url,
    /// The URL the document was created with. A same-document history update
    /// changes [`Self::document_url`] but not this one, and `CookieStore`
    /// resolves its default path and its `url` option against it
    /// (<https://cookiestore.spec.whatwg.org/#dom-cookiestore-get>).
    pub(crate) creation_url: Url,
    /// The cookie URL a document that inherits its origin uses: the creator's
    /// cookie URL, captured when the `about:blank` or `about:srcdoc` document
    /// was installed. `None` for an ordinary document, whose own creation URL
    /// is its cookie URL. It never follows a later `<base href>`, which is
    /// what makes it different from the document base URL
    /// (<https://html.spec.whatwg.org/multipage/dom.html#dom-document-cookie>).
    pub(crate) inherited_cookie_url: Option<Url>,
    /// The URL the `"client"` referrer branch reads. It is the document URL,
    /// except for an `iframe srcdoc` document, where the spec walks to the
    /// container document's URL
    /// (<https://w3c.github.io/webappsec-referrer-policy/#determine-requests-referrer>).
    pub(crate) referrer_url: Url,
    /// The document's own referrer: the serialization of the navigation
    /// request's referrer once `Determine request's Referrer` ran, or the
    /// creator's URL for a document made locally. `document.referrer` reads
    /// it, and policy never un-redacts it
    /// (<https://html.spec.whatwg.org/multipage/document-lifecycle.html#creating-and-initializing-a-document-object>).
    pub(crate) document_referrer: Option<Url>,
    /// The creator document's base URL while this document is an `iframe
    /// srcdoc document`. Its own URL cannot be a base, so its fallback base
    /// URL — what relative URLs resolve against — is the creator's
    /// (<https://html.spec.whatwg.org/multipage/urls-and-fetching.html#fallback-base-url>).
    pub(crate) base_url: Option<Url>,
    /// Origin captured for this document. `Url::origin()` on `about:blank`,
    /// `data:`, and `file:` yields a fresh `Origin::Opaque` each call, so
    /// blob lookups and same-origin checks must reuse this value
    /// (<https://html.spec.whatwg.org/multipage/browsers.html#concept-origin>).
    pub(crate) origin: url::Origin,
    /// The top-level traversable's origin, recorded when a child realm is
    /// created; `None` for a top-level realm, which answers its own origin
    /// (<https://html.spec.whatwg.org/multipage/webappapis.html#concept-environment-top-level-origin>).
    pub(crate) top_level: Option<url::Origin>,
    /// Whether this document is in a first-party site context: its site is
    /// same-site with every ancestor's site, so no cross-site frame embeds it.
    ///
    /// The `SharedWorker` `sameSiteCookies` default and the `all` value both
    /// read this
    /// (<https://privacycg.github.io/storage-access/#first-party-site-context>).
    pub(crate) first_party: bool,
    /// The parent browsing context's site context, recorded when a child
    /// realm is created; `None` for a top-level context, which nothing embeds
    /// and which is therefore always first-party.
    ///
    /// Keeping the parent's origin and verdict lets [`Self::refresh_first_party`]
    /// answer for the whole ancestor chain, since every ancestor's verdict is
    /// already folded into its parent's.
    parent_context: Option<ParentContext>,
    /// Which `SameSite` cookies this realm's requests may send and store.
    ///
    /// Documents keep [`net::SameSiteCookiePolicy::All`]. A shared worker's
    /// `sameSiteCookies` option sets the whole global scope
    /// (<https://privacycg.github.io/saa-non-cookie-storage/#shared-worker>).
    pub(crate) same_site_cookie_policy: net::SameSiteCookiePolicy,
    /// This document's policy container.
    pub(crate) policy: PolicyContainer,
    /// The sandboxing flags this frame's documents obey, from the `iframe`
    /// `sandbox` attribute that created it
    /// (<https://html.spec.whatwg.org/multipage/browsers.html#sandboxing-flag-set>).
    pub(crate) sandbox: SandboxFlags,
    pub(crate) history: crate::protocol::HistorySnapshot,
    /// The realm's outgoing work, drained once per turn by the document.
    pub(crate) outbox: Outbox,
    /// Files a script set on an `input[type=file]` through `input.files`, so
    /// the form entry list reads them without depending on JS wrapper identity
    /// (<https://html.spec.whatwg.org/multipage/input.html#dom-input-files>).
    input_files: HashMap<NodeId, Vec<Persistent<Value<'static>>>>,
    /// Fonts registered by `FontFaceSet.add`, keyed by the token returned to
    /// the shim. `delete`/`clear` unregister exactly these faces.
    pub(crate) font_registrations: HashMap<u32, FontRegistration>,
    pub(crate) next_font_registration: u32,
    budget: Rc<RefCell<ResourceBudget>>,
    pub parser_active: bool,
    /// The realm's event-target graph: window object, listener registries,
    /// standalone targets, and event handler attributes.
    events: EventTargetState,
    /// State that belongs to the documents this realm owns.
    pub(crate) state: DocumentState,
    /// Elements whose `click()` is running, so a nested `click()` returns
    /// (<https://html.spec.whatwg.org/multipage/interaction.html#dom-click>).
    clicks_in_progress: HashSet<NodeId>,
    /// When the realm's transient activation expires, or `None` once it was
    /// consumed or never granted
    /// (<https://html.spec.whatwg.org/multipage/interaction.html#transient-activation>).
    transient_activation: Option<Instant>,
    /// Whether the realm was ever activated, exposed as
    /// `navigator.userActivation.hasBeenActive`
    /// (<https://html.spec.whatwg.org/multipage/interaction.html#dom-useractivation-hasbeenactive>).
    sticky_activation: bool,
    brands: HashMap<String, Persistent<Object<'static>>>,
    pub(crate) bridge: Option<Persistent<Object<'static>>>,
    /// The realm's `Location` object. `window.location` is
    /// `[LegacyUnforgeable]` and returns the same object on every read, but the
    /// global accessor's closure must not hold the object: a JS value captured
    /// by a Rust closure outlives the context teardown and trips `QuickJS`'s
    /// live-object assertion. A `Persistent` dies with the realm instead.
    pub(crate) location_object: Option<Persistent<Object<'static>>>,
    /// Containers whose insertion steps already fired `load` (src-less
    /// iframes). A later container navigation clears the entry when it
    /// starts, so that navigation's own `load` still fires; `finish_frame_load`
    /// consumes whatever remains instead of re-reading the current `src` and
    /// `srcdoc` attributes
    /// (<https://html.spec.whatwg.org/multipage/iframe-embed-object.html#the-iframe-element:html-element-insertion-steps>).
    pub(crate) insertion_load_fired: HashSet<NodeId>,
    /// Registered `MutationObserver`s, keyed by their platform id.
    /// Pristine intrinsics captured at install, before page script runs.
    /// `WebIDL` conversions and scheduling must use these, never
    /// `ctx.globals()`: a page that replaces `String`/`Number`/`Boolean` (or
    /// deletes `queueMicrotask`) must not change conversion behavior, which
    /// follows the realm's original intrinsics.
    pub(crate) pristine_number: Option<Persistent<Function<'static>>>,
    pub(crate) pristine_boolean: Option<Persistent<Function<'static>>>,
    pub(crate) pristine_reflect_set: Option<Persistent<Function<'static>>>,
    pub(crate) pristine_queue_microtask: Option<Persistent<Function<'static>>>,
    /// The pristine `EvalError` constructor, so a refused string compilation
    /// raises the realm's own exception: a page that swaps `EvalError` must
    /// neither run code inside the bindings nor substitute another value.
    pub(crate) pristine_eval_error: Option<Persistent<Function<'static>>>,
    /// The pristine `Blob` constructor, captured after the web bundle. Canvas
    /// serialization builds its Blob here so a page that replaces `Blob`
    /// cannot reroute `toBlob`/`convertToBlob`.
    pub(crate) pristine_blob: Option<Persistent<Constructor<'static>>>,
    /// The pristine `setTimeout`, captured with the web bundle. `toBlob` queues
    /// its callback through this value instead of a page-replaceable global.
    pub(crate) pristine_set_timeout: Option<Persistent<Function<'static>>>,
    /// The realm's own event-interface constructors, captured after the web
    /// bundle installs them and before page script runs. `createEvent` uses
    /// these, never `globals[interface]`: a page that replaces the global
    /// must not stamp its own prototype onto the event.
    pub(crate) event_ctors: HashMap<String, Persistent<Object<'static>>>,
    pub(crate) weak_references: Option<WeakReferences>,
    /// The realm's own mutation-delivery entry point, so scheduling never
    /// depends on a page-deletable global.
    pub(crate) deliver_mutations_fn: Option<Persistent<Function<'static>>>,
}

/// The realm's event-target graph.
///
/// The maps are keyed by document (`None` keys are the window), so the two
/// teardowns — a navigation drops the old document's registrations, a
/// forgotten document drops its elements' handlers — live here with them
/// (<https://dom.spec.whatwg.org/#interface-eventtarget>,
/// <https://html.spec.whatwg.org/multipage/webappapis.html#event-handlers>).
#[derive(Default)]
pub(crate) struct EventTargetState {
    /// The realm's window object, for event targets that belong to this
    /// world but are reached from another realm's call frame.
    window: Option<Persistent<Object<'static>>>,
    listeners: HashMap<EventTargetKey, Vec<Rc<Listener>>>,
    /// The `EventTarget` object behind each `EventTargetKey::Standalone`.
    standalone_targets: HashMap<u64, Persistent<Object<'static>>>,
    next_standalone_target: u64,
    /// Event handler properties (`element.onload`, `window.onmessage`) live
    /// here rather than on the wrapper, which may be collected while the node
    /// stays alive. Keyed by `(None, name)` for the window and
    /// `(Some(node), name)` for an element
    /// (<https://html.spec.whatwg.org/multipage/webappapis.html#event-handlers>).
    handler_attributes: HashMap<(Option<NodeId>, String), Persistent<Value<'static>>>,
    /// Handler properties explicitly set to null or undefined, so a dispatch
    /// does not resurrect the element's content attribute
    /// (<https://html.spec.whatwg.org/multipage/webappapis.html#event-handler-content-attributes>).
    cleared_handlers: HashSet<(Option<NodeId>, String)>,
}

impl EventTargetState {
    /// A navigation keeps the realm's window object, so window-scoped
    /// handlers survive it; the old document's element handlers must not
    /// fire in the new document, and neither must its listener
    /// registrations
    /// (<https://html.spec.whatwg.org/multipage/webappapis.html#event-handlers>).
    fn reset_for_navigation(&mut self) {
        self.listeners.clear();
        self.handler_attributes
            .retain(|(node, _), _| node.is_none());
        self.cleared_handlers.retain(|(node, _)| node.is_none());
    }

    /// Drops every handler of one document's elements.
    fn forget_document(&mut self, document: u32) {
        self.handler_attributes
            .retain(|(node, _), _| node.is_none_or(|node| node.document_id() != document));
        self.cleared_handlers
            .retain(|(node, _)| node.is_none_or(|node| node.document_id() != document));
    }

    /// Drops the whole graph, window object included; the realm is going.
    fn clear_realm(&mut self) {
        self.listeners.clear();
        self.standalone_targets.clear();
        self.window = None;
        self.next_standalone_target = 0;
        self.handler_attributes.clear();
        self.cleared_handlers.clear();
    }
}

/// One piece of realm work waiting for the document to apply it.
///
/// The realm does not run document tasks itself. Timers, fetch submissions and
/// aborts, script starts, history updates, CSP reports, and
/// `document.write` text land here in enqueue order. The document drains the
/// queue at the start of a turn. `document.open` and `document.close` stay on
/// the engine's stream queue because applying them republishes the frame.
pub(crate) enum RealmRequest {
    Timer(super::TimeoutRequest),
    ClearTimer(super::PageTimerId),
    Fetch(Box<super::JsFetchRequest>),
    AbortFetch(super::JsFetchId),
    Script(NodeId),
    HistoryPush {
        url: String,
        state: String,
        handling: crate::protocol::HistoryHandling,
    },
    HistoryTraverse {
        delta: i32,
    },
    CspViolation(NodeId, Vec<crate::csp::Violation>),
    /// A `meta http-equiv=refresh` value the document should run the shared
    /// declarative refresh steps on. The realm cannot schedule a navigation
    /// itself; the document owns the timer
    /// (<https://html.spec.whatwg.org/multipage/semantics.html#attr-meta-http-equiv-refresh>).
    DeclarativeRefresh(String),
    HtmlWrite(HtmlWrite),
    DocumentStream(DocumentStreamCommand),
}

/// The realm's outgoing work, shared with the host functions that push into it.
pub(crate) type Outbox = Rc<RefCell<VecDeque<RealmRequest>>>;

/// Realm state that belongs to the documents this realm owns.
///
/// A navigation installs a fresh document and a forgotten document takes its
/// entries with it, so the concern owns both teardowns: `reset` for the
/// wholesale replacement and `forget_document` for one document's id-keyed
/// maps. Every field here is dropped by one of the two, which is why the
/// call sites no longer list fields.
#[derive(Default)]
pub(crate) struct DocumentState {
    /// `meta` elements whose policy was already appended.
    pub(crate) applied_csp_metas: HashSet<NodeId>,
    /// `base` elements a `base-uri` policy refused. The document URL stays
    /// the base, and a later `base` element is not consulted: HTML freezes
    /// the first one that carries an `href`
    /// (<https://w3c.github.io/webappsec-csp/#is-base-allowed-for-document>).
    pub(crate) blocked_bases: HashSet<NodeId>,
    /// The `href` each `base` element was last checked with, so a resumed
    /// parse walk does not repeat a check that already ran for the same
    /// value (<https://html.spec.whatwg.org/multipage/semantics.html#the-base-element>).
    pub(crate) base_href_seen: HashMap<NodeId, Option<String>>,
    /// The base URL each `base` element may freeze, or `None` when its
    /// `href` is refused or unusable.
    base_results: HashMap<NodeId, Option<Url>>,
    frame_navigations: Vec<FrameNavigation>,
    /// Script elements the HTML parser inserted; script insertion clears the
    /// bit (<https://html.spec.whatwg.org/multipage/scripting.html#parser-inserted>).
    pub(crate) parser_inserted: HashSet<NodeId>,
    /// Streaming `TextDecoder` sessions, keyed by platform id. The decoder
    /// object buffers partial sequences internally across `decode()` calls,
    /// exactly as the Encoding Standard's streaming decoder does; the entry
    /// dies with the realm, and `FinalizationRegistry` frees it earlier when
    /// the engine runs the callback
    /// (<https://encoding.spec.whatwg.org/#dom-textdecoder-decode>).
    pub(crate) decoders: HashMap<u64, DecoderSession>,
    /// The script element whose code is running, which
    /// `document.currentScript` reports. A module script leaves it unset: the
    /// attribute is null while a module evaluates
    /// (<https://html.spec.whatwg.org/multipage/dom.html#dom-document-currentscript>).
    pub current_script: Option<NodeId>,
    /// The element that started the module graph being fetched. Every fetch in
    /// the graph reads its `crossorigin` and `referrerpolicy`, while
    /// `current_script` stays unset so `document.currentScript` is null
    /// (<https://html.spec.whatwg.org/multipage/webappapis.html#fetch-the-descendants-of-and-link-a-module-script>).
    pub(crate) module_fetch_root: Option<NodeId>,
    /// The URL of the script or module currently evaluating. `QuickJS` labels
    /// code it compiles from a string `<input>`, so a dynamic `import()` in
    /// `eval` resolves against this URL instead
    /// (<https://tc39.es/ecma262/#sec-hostresolveimportedmodule>).
    pub(crate) current_script_url: Option<Url>,
    /// The fetch client origin for a worker's top-level module graph. A worker
    /// created from a `data:` URL has an opaque origin, but its graph is
    /// fetched with the outside settings' origin; a later dynamic `import()`
    /// uses the worker's own origin
    /// (<https://html.spec.whatwg.org/multipage/workers.html#worker-processing-model>).
    pub(crate) module_client_origin: Option<url::Origin>,
    /// The credentials mode every module fetch in a module worker's graph
    /// uses. It comes from `WorkerOptions.credentials` and replaces the
    /// `crossorigin`-derived mode a document's module graph uses
    /// (<https://html.spec.whatwg.org/multipage/webappapis.html#fetch-a-single-module-script>).
    pub(crate) module_credentials: Option<net::cors::CredentialsMode>,
    /// Whether a module is declaring, so a fetch it starts is resolving a static
    /// import and takes the referencing module's URL as its referrer. A
    /// dynamic `import()` runs after this clears and names the worker's own
    /// script instead
    /// (<https://html.spec.whatwg.org/multipage/webappapis.html#fetch-a-single-module-script>).
    pub(crate) declaring_module: bool,
    /// The `Referrer-Policy` of a worker's own script response, which governs
    /// the dynamic imports its body runs. Static imports keep the policy the
    /// worker inherited from its creator
    /// (<https://w3c.github.io/webappsec-referrer-policy/#integration-with-html>).
    pub(crate) worker_referrer_policy: Option<net::ReferrerPolicy>,
    /// The content security policies a worker's own script response carried,
    /// appended to the ones it inherited from its creator. They govern the
    /// dynamic imports its body runs, while its static imports stay under the
    /// creator's policy
    /// (<https://html.spec.whatwg.org/multipage/browsers.html#initialize-a-policy-container-from-a-fetch-response>).
    pub(crate) worker_module_csp: Option<crate::csp::List>,

    /// Existing elements being synchronously upgraded by a custom-element
    /// constructor. `HTMLElement()` consumes the top candidate.
    pub(crate) custom_construction: Vec<NodeId>,
    /// One `DOMImplementation` per document, for identity.
    implementations: HashMap<u32, Persistent<Value<'static>>>,
    /// The focused element of each document
    /// (<https://html.spec.whatwg.org/multipage/interaction.html#focused-area-of-the-document>).
    active_elements: HashMap<u32, NodeId>,
    /// Text selection per control `(start, end, direction code)`: the
    /// selection APIs clamp to the control's value length on read.
    selections: HashMap<NodeId, (u32, u32, u8)>,
    /// Stable `WebDriver` element ids for nodes, and the reverse lookup.
    remote_ids: HashMap<NodeId, u64>,
    remote_nodes: HashMap<u64, NodeId>,
    /// Per-element cryptographic nonce slots, set by the `nonce` IDL setter
    /// without touching the content attribute
    /// (<https://html.spec.whatwg.org/multipage/urls-and-fetching.html#nonce-attributes>).
    /// Invariant: the slot mirrors the spec's `[[CryptographicNonce]]`.
    /// Content-attribute writes feed it through `set_attribute_sync`, and
    /// removals clear it through `remove_attribute_sync` (the attribute
    /// change steps, where `null` resets the slot). Parser-created `nonce`
    /// attributes bypass both hooks, so a missing entry falls back to the
    /// content attribute in `nonce_of` — which matches the spec state for
    /// every transition except a remove-after-parse with no intervening
    /// write, closed by clearing on removal. Clones copy the slot, adopts
    /// retarget it, and `forget_document` drops a dead document's entries.
    pub(crate) nonce_slots: HashMap<NodeId, String>,
    /// The executing script's URL, line, and column captured when a
    /// subresource URL entered an element: a `src` set, or an insertion when
    /// no set chose the URL. A later CSP violation for that element's load
    /// reports these as `sourceFile`, `lineNumber`, and `columnNumber`
    /// (<https://w3c.github.io/webappsec-csp/#create-violation-for-global>).
    pub(crate) creator_stacks: HashMap<NodeId, CreatorInfo>,
    /// Per form-associated element state: the flags and edit buffers that are
    /// neither content attributes nor part of Blitz's sanitized value slot.
    /// They live here so every path that changes a control (a value set, a
    /// type change, a reset) updates them together instead of leaving a JS
    /// `WeakMap` behind.
    ///
    /// Entries deliberately outlive a node's removal: custom validity and a
    /// user edit survive a temporary detach and reinsertion. Only document
    /// teardown prunes them, so the bound is one entry per element that ever
    /// used a form API in this document.
    pub(crate) controls: HashMap<NodeId, ControlState>,
}

/// The mutable state of one form-associated element.
///
/// The value itself (and its dirty flag) stays in Blitz's sanitized slot; this
/// is everything the validity and editing algorithms derive from or maintain
/// beside it
/// (<https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#the-constraint-validation-api>).
#[derive(Clone, Debug, Default)]
pub(crate) struct ControlState {
    /// The value was last changed by a user edit, so `maxlength`/`minlength`
    /// apply.
    pub(crate) user_edited: bool,
    /// The value cannot be converted to its state's value.
    pub(crate) bad_input: bool,
    /// The message from `setCustomValidity`, as UTF-16 units so a lone
    /// surrogate survives the round trip through `validationMessage`.
    pub(crate) custom_validity: Vec<u16>,
    /// The `number` state's in-progress edit buffer, which can hold a partial
    /// number the exposed value does not.
    pub(crate) number_buffer: Option<String>,
    /// The caret for a control without a real selection API.
    pub(crate) caret: Option<u32>,
    /// `<output>`'s value mode; when set, the text is the value and the
    /// frozen default lives beside it.
    pub(crate) output_value_mode: bool,
    /// `<output>`'s frozen default value, as UTF-16 units so a lone surrogate
    /// survives the round trip through `defaultValue`. `None` while the value
    /// mode flag is clear: the text itself is then the default.
    pub(crate) output_default_value: Option<Vec<u16>>,
}

impl ControlState {
    /// Drops the state a reset, a script value set, or a type change ends.
    /// Custom validity is not an edit and survives all three.
    pub(crate) fn clear_edit_state(&mut self) {
        self.user_edited = false;
        self.bad_input = false;
        self.number_buffer = None;
        self.caret = None;
    }

    /// Drops the edit position and buffer that a cross-document adoption
    /// invalidates. The value-derived state (the user-edit flag and bad input)
    /// and custom validity stay: adoption does not change the value.
    pub(crate) fn clear_edit_position(&mut self) {
        self.number_buffer = None;
        self.caret = None;
    }
}

impl DocumentState {
    /// Drops everything keyed to the active document, which a navigation is
    /// about to replace. Entries of *other* documents this realm still owns
    /// (`owned`) survive, so `nonce_slots`, `creator_stacks`, and `controls`
    /// are pruned by id in [`Self::forget_document`] instead.
    fn reset(&mut self) {
        self.applied_csp_metas.clear();
        self.blocked_bases.clear();
        self.base_href_seen.clear();
        self.base_results.clear();
        self.frame_navigations.clear();
        self.parser_inserted.clear();
        self.decoders.clear();
        self.current_script = None;
        self.module_fetch_root = None;
        self.current_script_url = None;
        self.module_client_origin = None;
        self.module_credentials = None;
        self.declaring_module = false;
        self.worker_referrer_policy = None;
        self.worker_module_csp = None;
        self.custom_construction.clear();
        self.implementations.clear();
        self.active_elements.clear();
        self.selections.clear();
        self.remote_ids.clear();
        self.remote_nodes.clear();
    }

    /// Drops the entries one document's nodes left behind.
    fn forget_document(&mut self, document: u32) {
        self.blocked_bases.retain(|node| node.document != document);
        self.base_href_seen
            .retain(|node, _| node.document != document);
        self.base_results
            .retain(|node, _| node.document != document);
        self.nonce_slots.retain(|node, _| node.document != document);
        self.creator_stacks
            .retain(|node, _| node.document != document);
        self.controls.retain(|node, _| node.document != document);
    }
}

/// The top JS frame that triggered a subresource fetch: the script URL as
/// written (fragments and credentials stripped at report time), with 1-based
/// line and column. Zero line or column means unknown.
#[derive(Clone, Debug)]
pub(crate) struct CreatorInfo {
    pub url: String,
    pub line: u32,
    pub column: u32,
}

/// One streaming `TextDecoder` session: the decoder plus what recreates it.
/// A `last=true` call ends the decoder (`encoding_rs` panics on reuse), so
/// every non-streaming `decode()` replaces it with a fresh one. `carry`
/// holds input bytes past a fatal error for the next call: the error itself
/// is consumed, but the queue behind it is not.
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
        self.unload_object_urls_for_frame();
        self.clear_outbox();
    }
}

/// What one `window.open` call resolved to before any context exists
/// (<https://html.spec.whatwg.org/multipage/nav-history-apis.html#window-open-steps>).
pub(crate) struct WindowOpenPlan {
    /// The URL encoded against this document, or `None` when the call named
    /// no URL.
    pub(crate) url: Option<Url>,
    /// What `noopener`, `noreferrer`, and the blob rule do to the opener.
    pub(crate) opener: crate::protocol::OpenerPolicy,
    /// Whether the call returns no `WindowProxy` even though the context it
    /// selected exists: `noopener` withholds everything but this context and
    /// the ancestors it may target directly
    /// (<https://html.spec.whatwg.org/multipage/nav-history-apis.html#window-open-steps>).
    pub(crate) withholds: bool,
    /// The navigation a browser-hosted tab's first request runs under.
    pub(crate) source: crate::protocol::NavigationSource,
}

impl World {
    pub(crate) fn new(
        document_url: Url,
        frame: FrameId,
        runtime: &FrameRuntime,
        init_scripts: Rc<RefCell<Vec<(u64, String)>>>,
    ) -> Self {
        let secure_context = secure_context::is_secure_environment(&document_url, None);
        let realm = runtime.registry.borrow_mut().allocate_realm();
        Self {
            runtime: runtime.clone(),
            frame,
            kind: GlobalKind::Document,
            own_window: None,
            realm,
            init_scripts,
            viewport_size: Cell::new(crate::engine::DEFAULT_VIEWPORT),
            pending_frames: Vec::new(),
            deferred_scripts: Vec::new(),
            frame_host: None,
            document: None,
            realm_context: None,
            worker_close_requested: Rc::new(Cell::new(false)),
            compiling_handler: Cell::new(false),
            timer_string: Cell::new(false),
            owned: HashSet::new(),
            origin: document_url.origin(),
            top_level: None,
            first_party: true,
            parent_context: None,
            same_site_cookie_policy: net::SameSiteCookiePolicy::All,
            referrer_url: document_url.clone(),
            document_referrer: None,
            creation_url: document_url.clone(),
            inherited_cookie_url: None,
            document_url,
            base_url: None,
            policy: PolicyContainer {
                secure_context,
                ..PolicyContainer::default()
            },
            sandbox: SandboxFlags::default(),
            history: crate::protocol::HistorySnapshot::default(),
            outbox: Rc::new(RefCell::new(VecDeque::new())),
            input_files: HashMap::new(),
            font_registrations: HashMap::new(),
            next_font_registration: 1,
            budget: runtime.registry.borrow().budget(),
            parser_active: false,
            events: EventTargetState::default(),
            state: DocumentState::default(),
            clicks_in_progress: HashSet::new(),
            transient_activation: None,
            sticky_activation: false,
            brands: HashMap::new(),
            bridge: None,
            location_object: None,
            insertion_load_fired: HashSet::new(),
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

    /// Builds the realm for a dedicated worker.
    ///
    /// The worker owns no document and no browsing context; its global is a
    /// `DedicatedWorkerGlobalScope` sharing the page engine's `QuickJS`
    /// runtime and services with its creator
    /// (<https://html.spec.whatwg.org/multipage/workers.html#dedicated-workers-and-the-dedicatedworkerglobalscope-interface>).
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
        );
        world.kind = GlobalKind::DedicatedWorker {
            id,
            parent,
            name,
            module,
        };
        world
    }

    /// Builds the realm for a shared worker.
    ///
    /// A shared worker has no single owner and no browsing context: its global
    /// is a `SharedWorkerGlobalScope`, and every connection arrives as a
    /// `connect` event carrying its own port
    /// (<https://html.spec.whatwg.org/multipage/workers.html#shared-workers-and-the-sharedworkerglobalscope-interface>).
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
        );
        world.kind = GlobalKind::SharedWorker { id, name, module };
        world
    }

    /// Whether this realm is a shared worker.
    pub(crate) fn is_shared_worker(&self) -> bool {
        matches!(self.kind, GlobalKind::SharedWorker { .. })
    }

    /// Whether this realm is a module worker, which `importScripts()` must
    /// refuse.
    pub(crate) fn worker_is_module(&self) -> bool {
        match &self.kind {
            GlobalKind::Document => false,
            GlobalKind::DedicatedWorker { module, .. }
            | GlobalKind::SharedWorker { module, .. } => *module,
        }
    }

    /// The worker name a worker global reports, or `None` for a document
    /// realm.
    pub(crate) fn worker_name(&self) -> Option<&str> {
        match &self.kind {
            GlobalKind::Document => None,
            GlobalKind::DedicatedWorker { name, .. } | GlobalKind::SharedWorker { name, .. } => {
                Some(name)
            }
        }
    }

    /// The worker identity a worker global reports, or `None` for a document
    /// realm.
    pub(crate) fn worker_id(&self) -> Option<crate::worker::WorkerId> {
        match &self.kind {
            GlobalKind::Document => None,
            GlobalKind::DedicatedWorker { id, .. } | GlobalKind::SharedWorker { id, .. } => {
                Some(*id)
            }
        }
    }

    /// The creator frame a dedicated worker global posts to, or `None` for a
    /// document realm or a shared worker.
    pub(crate) fn worker_parent(&self) -> Option<FrameId> {
        match &self.kind {
            GlobalKind::Document | GlobalKind::SharedWorker { .. } => None,
            GlobalKind::DedicatedWorker { parent, .. } => Some(*parent),
        }
    }

    pub(crate) fn clear_observers(&mut self) {
        self.runtime
            .registry
            .borrow_mut()
            .observers
            .forget_frame(self.frame, &mut self.runtime.documents.borrow_mut());
    }

    /// Installs `parsed` as the active document and returns its id.
    pub(crate) fn replace_document(&mut self, parsed: Parsed) -> u32 {
        self.drop_active_document();
        let id = self.runtime.documents.borrow_mut().insert(parsed);
        self.document = Some(id);
        self.owned.insert(id);
        self.clear_outbox();
        self.state.reset();
        // Navigation keeps the realm's window object, so window-scoped
        // handlers survive; the old document's listeners and element handlers
        // must not fire in the new document
        // (<https://html.spec.whatwg.org/multipage/webappapis.html#event-handlers>).
        self.events.reset_for_navigation();
        // A new realm owns fresh observers; navigation drops the old ones.
        self.clear_observers();
        id
    }

    /// Installs the frame's active document without clearing realm caches.
    /// Returns the document id, reusing the parsed document's id when it
    /// carries one (take-and-reinsert cycles keep wrapper identity).
    pub(crate) fn set_document(&mut self, parsed: Parsed) -> u32 {
        let id = self.runtime.documents.borrow_mut().insert(parsed);
        if self.document != Some(id) {
            self.drop_active_document();
            self.document = Some(id);
            self.owned.insert(id);
        }
        self.state.current_script = None;
        self.state.module_fetch_root = None;
        self.state.current_script_url = None;
        self.state.declaring_module = false;
        id
    }

    fn drop_active_document(&mut self) {
        if let Some(old) = self.document.take() {
            self.owned.remove(&old);
            self.unload_object_urls_for_document(old);
            self.state.forget_document(old);
            self.runtime.documents.borrow_mut().remove(old);
            self.runtime.registry.borrow_mut().forget_document(old);
        }
    }

    /// Drops realm and wrapper associations for every document this realm
    /// owns; called when the frame goes away.
    pub(crate) fn forget_owned_documents(&mut self) {
        let ids: Vec<u32> = self.owned.drain().collect();
        for id in ids {
            self.unload_object_urls_for_document(id);
            self.runtime.documents.borrow_mut().remove(id);
            self.runtime.registry.borrow_mut().forget_document(id);
            self.state.forget_document(id);
            // The realm is going away; its window handlers go with it when
            // the realm tears down (`clear_listeners`).
            self.events.forget_document(id);
        }
        self.runtime
            .registry
            .borrow_mut()
            .forget_realm(self.realm_id());
        self.document = None;
    }

    /// The registry every realm of this renderer process shares.
    pub(crate) fn registry(&self) -> Rc<RefCell<RealmRegistry>> {
        Rc::clone(&self.runtime.registry)
    }

    /// The realm that owns the document `id` points into, when alive.
    pub(crate) fn owner_world(&self, id: NodeId) -> Option<Rc<RefCell<World>>> {
        self.runtime.registry.borrow().owner_world(id)
    }

    /// The shared wrapper cached for `id`, when one exists.
    pub(crate) fn shared_wrapper(&self, id: NodeId) -> Option<Persistent<Value<'static>>> {
        self.runtime.registry.borrow().wrapper(id)
    }

    /// Publishes the shared wrapper cached for `id`.
    pub(crate) fn intern_shared_wrapper(&self, id: NodeId, value: Persistent<Value<'static>>) {
        self.runtime.registry.borrow_mut().intern_wrapper(id, value);
    }

    /// The active document of the frame this realm belongs to.
    pub(crate) fn main_document(&self) -> Option<Ref<'_, Parsed>> {
        let id = self.document?;
        Ref::filter_map(self.runtime.documents.borrow(), |store| store.get(id)).ok()
    }

    /// Mutable access to the active document.
    pub(crate) fn main_document_mut(&self) -> Option<RefMut<'_, Parsed>> {
        let id = self.document?;
        RefMut::filter_map(self.runtime.documents.borrow_mut(), |store| {
            store.get_mut(id)
        })
        .ok()
    }

    /// One `DOMImplementation` object per document, for identity.
    pub(crate) fn implementation(&self, id: NodeId) -> Option<Persistent<Value<'static>>> {
        self.state.implementations.get(&id.document_id()).cloned()
    }

    pub(crate) fn intern_implementation(&mut self, id: NodeId, value: Persistent<Value<'static>>) {
        self.state.implementations.insert(id.document_id(), value);
    }

    /// The document tree that owns `id`.
    pub(crate) fn document(&self, id: NodeId) -> Option<Ref<'_, Parsed>> {
        Ref::filter_map(self.runtime.documents.borrow(), |store| {
            store.get(id.document_id())
        })
        .ok()
    }

    /// The fetch-relevant attributes of a subresource element: its
    /// `crossorigin` state and its `referrerpolicy` override, read from one
    /// document lookup.
    ///
    /// Both are per-request inputs, not reflected properties the load path may
    /// re-derive, so one helper keeps the load and every reader of the
    /// attribute in agreement
    /// (<https://html.spec.whatwg.org/multipage/urls-and-fetching.html#cors-settings-attributes>,
    /// <https://html.spec.whatwg.org/multipage/urls-and-fetching.html#referrer-policy-attribute>).
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

    /// The tree store, for realm-free algorithms that take `&DocumentStore`.
    pub(crate) fn store(&self) -> Ref<'_, DocumentStore> {
        self.runtime.documents.borrow()
    }

    /// Whether this realm created the document `id` belongs to. Canvas
    /// identity is carried by page-visible properties, so a forged pair must
    /// not reach another frame's registry entry.
    pub(crate) fn owns_node(&self, id: NodeId) -> bool {
        self.owned.contains(&id.document)
    }

    /// Runs `reader` against the tree owning `id`.
    ///
    /// Prefer this to [`World::document`] when a guard would outlive the
    /// caller's `World` borrow; the tree borrow ends with the closure.
    pub(crate) fn with_document<R>(
        &self,
        id: NodeId,
        reader: impl FnOnce(&Parsed) -> R,
    ) -> Option<R> {
        let store = self.runtime.documents.borrow();
        store.get(id.document_id()).map(reader)
    }

    /// Runs `reader` against the frame's active document.
    pub(crate) fn with_main_document<R>(&self, reader: impl FnOnce(&Parsed) -> R) -> Option<R> {
        let id = self.document?;
        let store = self.runtime.documents.borrow();
        store.get(id).map(reader)
    }

    /// Mutable version of [`World::document`].
    pub(crate) fn document_mut(&self, id: NodeId) -> Option<RefMut<'_, Parsed>> {
        RefMut::filter_map(self.runtime.documents.borrow_mut(), |store| {
            store.get_mut(id.document_id())
        })
        .ok()
    }

    /// Mutable access to a document by store id, without a node handle, for
    /// state (such as live ranges) keyed by document alone.
    pub(crate) fn document_mut_by_id(&self, document: u32) -> Option<RefMut<'_, Parsed>> {
        RefMut::filter_map(self.runtime.documents.borrow_mut(), |store| {
            store.get_mut(document)
        })
        .ok()
    }

    /// Frees a live-range slot when the document store is free.
    ///
    /// The range finalizer runs at an arbitrary point, possibly inside a
    /// borrow of the store, so a busy store silently leaves the slot to the
    /// document's own teardown
    /// (<https://dom.spec.whatwg.org/#concept-range>).
    pub(crate) fn try_free_range(&self, document: u32, slot: usize) {
        let Ok(mut documents) = self.runtime.documents.try_borrow_mut() else {
            return;
        };
        if let Some(parsed) = documents.get_mut(document) {
            parsed.document.side.ranges.free(slot);
        }
    }

    /// Stores a secondary document and returns its root id.
    pub(crate) fn add_document(&mut self, parsed: Parsed) -> NodeId {
        let node = parsed.document.base.root_node().id;
        let document = self.runtime.documents.borrow_mut().insert(parsed);
        self.owned.insert(document);
        NodeId { document, node }
    }

    pub(crate) fn frame_document(&self, container: NodeId) -> Option<NodeId> {
        self.runtime.registry.borrow().frame_document(container)
    }

    /// This realm's registry id, the owner of its offscreen canvases.
    pub(crate) fn realm_id(&self) -> u64 {
        self.realm
    }

    /// The active document id without borrowing the tree store.
    pub(crate) fn main_document_id(&self) -> Option<u32> {
        self.document
    }

    /// The frame this realm belongs to.
    pub(crate) fn frame(&self) -> FrameId {
        self.frame
    }

    /// The origin of this frame's top-level traversable
    /// (<https://html.spec.whatwg.org/multipage/webappapis.html#concept-environment-top-level-origin>).
    ///
    /// A top-level realm answers its own origin. A child's is recorded when
    /// the child realm is created, so it cannot be missing or change under the
    /// caller.
    pub(crate) fn top_level_origin(&self) -> url::Origin {
        self.top_level
            .clone()
            .unwrap_or_else(|| self.origin.clone())
    }

    /// Records the parent browsing context this child realm is embedded in, so
    /// its first-party verdict can be recomputed whenever its own origin
    /// changes
    /// (<https://privacycg.github.io/storage-access/#first-party-site-context>).
    pub(crate) fn set_parent_context(&mut self, parent: &Self) {
        self.parent_context = Some(ParentContext {
            origin: parent.origin.clone(),
            first_party: parent.first_party,
        });
        self.refresh_first_party();
    }

    /// Recomputes [`Self::first_party`]: a top-level context is never embedded
    /// and so is first-party, and a child is first-party only when its parent
    /// is and its own site is same-site with the parent's.
    ///
    /// A navigation that changes this realm's origin calls this, because a
    /// cross-site frame stops being first-party even though its ancestors do
    /// not change.
    fn refresh_first_party(&mut self) {
        self.first_party = self.parent_context.as_ref().is_none_or(|parent| {
            parent.first_party && net::origins_same_site(&self.origin, &parent.origin)
        });
    }

    /// The cookie partition key this realm's requests and `document.cookie`
    /// operations use: the site of its top-level document, or `None` when that
    /// origin is opaque
    /// (<https://github.com/privacycg/CHIPS#computing-the-cookie-partition-key>).
    pub(crate) fn partition_key(&self) -> Option<String> {
        net::site_for_origin(&self.top_level_origin())
    }

    /// Whether this realm's environment is a secure context. The model is
    /// Chromium's, not HTML's text; the disagreement is in
    /// `docs/upstream.md`.
    pub(crate) fn is_secure_context(&self) -> bool {
        self.policy.secure_context
    }

    /// Whether this realm may use APIs gated by cross-origin isolation.
    ///
    /// The bit is computed when the response installs and inherits through the
    /// policy container, so a worker reads the creator's decision
    /// (<https://html.spec.whatwg.org/multipage/webappapis.html#concept-settings-object-cross-origin-isolated-capability>).
    pub(crate) fn is_cross_origin_isolated(&self) -> bool {
        self.policy.cross_origin_isolated
    }

    /// Whether this realm's frame is still attached to its document. A
    /// removed iframe's frame may still sit in the tree until the engine
    /// reconciles, so the container's connectedness decides.
    pub(crate) fn is_attached(&self) -> bool {
        if self.frame == FrameId::MAIN {
            return true;
        }
        let container = self.runtime.shared.borrow().tree.container(self.frame);
        let Some(container) = container else {
            // An auxiliary context has no container; it stays attached until
            // its browsing context is closed.
            return self.runtime.shared.borrow().tree.contains(self.frame);
        };
        self.owner_world(container).is_some_and(|owner| {
            owner
                .borrow()
                .main_document()
                .is_some_and(|parsed| is_connected(&parsed.document.base, container.node))
        })
    }

    /// The renderer-process state shared by every realm.
    pub(crate) fn shared(&self) -> SharedHandle {
        self.runtime.shared.clone()
    }

    /// Registers a browsing context for every connected `iframe` in this
    /// frame's document that does not have one yet.
    ///
    /// Safe to call while a script runs: it allocates frame identities and
    /// tree entries only. The document and its realm are created later, by
    /// the frame host the `document` layer installs, because `QuickJS`
    /// forbids entering a new realm while another realm executes.
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

    /// Creates a browsing context for one connected `iframe` if it does not
    /// already have one
    /// (<https://html.spec.whatwg.org/multipage/iframe-embed-object.html#the-iframe-element:html-element-insertion-steps>).
    ///
    /// Allocates the frame identity only. The nested document is created
    /// later, by the frame host the `document` layer installs.
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
        // `window[i]` sorts by container tree order: the new frame goes
        // before the first following sibling that already has one.
        let before = self.frame_sort_before(container);
        let frame = {
            let mut shared = self.runtime.shared.borrow_mut();
            let frame = shared.allocate_frame();
            shared
                .tree
                .add_container(self.frame, frame, container, before);
            if !name.is_empty() {
                shared.tree.set_name(frame, name);
            }
            frame
        };
        self.pending_frames.push((frame, container));
        true
    }

    /// The frame that sorts immediately after `container`'s frame in document
    /// tree order: the first following container in a pre-order walk that
    /// already has a frame, if any. Sibling order is not enough: after
    /// `a.appendChild(iframe)`, the moved frame belongs before `b`'s frame
    /// in tree order but has no following sibling. Both insertion and
    /// `moveBefore` sort through this, so `window[i]` follows container
    /// tree order
    /// (<https://html.spec.whatwg.org/multipage/document-sequences.html#document-tree-child-navigable>).
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

    /// Creates the initial `about:blank` document for one connected `iframe`
    /// so `contentDocument` is available in the inserting script
    /// (<https://html.spec.whatwg.org/multipage/iframe-embed-object.html#the-iframe-element:html-element-insertion-steps>).
    ///
    /// The nested realm is deferred: `QuickJS` cannot enter a new context while
    /// the parent script still runs. Document creation belongs to the frame
    /// host the `document` layer installs; a realm built without one just
    /// leaves the frame registered for a later drain (test worlds).
    pub(crate) fn materialize_iframe(world_rc: &Rc<RefCell<World>>, container: NodeId) -> bool {
        world_rc
            .borrow_mut()
            .register_frame_for_container(container);
        let pending = {
            let mut world = world_rc.borrow_mut();
            world
                .pending_frames
                .iter()
                .position(|(_, existing)| *existing == container)
                .map(|index| world.pending_frames.remove(index))
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
        let host = world_rc.borrow().frame_host.clone();
        let Some(host) = host else {
            world_rc
                .borrow_mut()
                .pending_frames
                .push((frame, container));
            return false;
        };
        (host.child)(world_rc, frame, container);
        true
    }

    /// Installs the hook that builds child documents for this realm's
    /// `iframe`s. Called by the `document` layer when the frame is created.
    pub(crate) fn set_frame_host(&mut self, host: FrameHost) {
        self.frame_host = Some(host);
    }

    /// Installs the active sandboxing flag set a commit imposes on this
    /// document: the creation flags unioned with the popup sandboxing flag set
    /// of this document's browsing context.
    ///
    /// A child navigable's creation flags already include its embedder's active
    /// set, so its popup set is empty and the union is a no-op. A top-level
    /// context's creation flags name no embedder: the union is what makes the
    /// popup set, whether the auxiliary context's or a browser tab's, apply to
    /// every document that context commits, not only the first
    /// (<https://html.spec.whatwg.org/multipage/browsers.html#determining-the-creation-sandboxing-flags>,
    /// <https://html.spec.whatwg.org/multipage/browsers.html#popup-sandboxing-flag-set>).
    pub(crate) fn install_sandbox(&mut self, created: SandboxFlags) {
        let popup = self.runtime.shared.borrow().tree.popup_sandbox(self.frame);
        self.sandbox = created.union(popup);
    }

    /// Whether a sandboxed document may create an auxiliary context
    /// (`allow-popups`)
    /// (<https://html.spec.whatwg.org/multipage/browsers.html#sandboxed-auxiliary-navigation-browsing-context-flag>).
    pub(crate) fn allows_auxiliary_navigation(&self) -> bool {
        !self.sandbox.contains(SandboxFlag::AuxiliaryNavigation)
    }

    /// Reads one `window.open` call: the URL encoded against this document, and
    /// the feature list, the blob-URL opener rule, and the calling document's
    /// navigation inputs, each read once
    /// (<https://html.spec.whatwg.org/multipage/nav-history-apis.html#window-open-steps>,
    /// <https://html.spec.whatwg.org/multipage/nav-history-apis.html#get-noopener-for-window-open>).
    ///
    /// `None` when the URL cannot be encoded against this document.
    pub(crate) fn plan_window_open(
        &self,
        url: &str,
        target: &TargetName,
        features: &str,
    ) -> Option<WindowOpenPlan> {
        // A null `urlRecord` is the window open steps' "no URL": an existing
        // context is not navigated, and a new one starts on `about:blank`
        // (<https://html.spec.whatwg.org/multipage/nav-history-apis.html#window-open-steps>).
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
        Some(WindowOpenPlan {
            url,
            opener,
            withholds: opener != crate::protocol::OpenerPolicy::Keep && target.noopener_withholds(),
            source: crate::protocol::NavigationSource {
                origin: crate::protocol::SerializedOrigin::from(&self.origin),
                referrer: self.referrer_url.clone(),
                referrer_policy: self.policy.referrer_policy.unwrap_or_default(),
                frame: self.frame,
                secure_context: self.is_secure_context(),
            },
        })
    }

    /// Navigates the context a `target` name already selected in this
    /// renderer, queuing the navigation so it runs after the caller's script
    /// stops. A `None` `url` navigates nothing
    /// (<https://html.spec.whatwg.org/multipage/nav-history-apis.html#window-open-steps>).
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

    /// Creates an auxiliary top-level browsing context for `target`.
    ///
    /// `None` means this renderer cannot host it: either the calling document
    /// is sandboxed without `allow-popups`, or the URL belongs to another site
    /// and `allow_cross_origin` is false, so the caller falls back to a browser
    /// tab. The navigation is queued and applied after the caller's script
    /// stops, so the auxiliary document exists first
    /// (<https://html.spec.whatwg.org/multipage/nav-history-apis.html#window-open-steps>).
    pub(crate) fn create_auxiliary(
        world_rc: &Rc<RefCell<World>>,
        url: Option<&Url>,
        target: &TargetName,
        opener: crate::protocol::OpenerPolicy,
        allow_cross_origin: bool,
    ) -> Option<FrameId> {
        let current = world_rc.borrow().frame;
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
            let sandbox = world.sandbox.auxiliary_flags();
            shared.tree.add_auxiliary(
                current,
                frame,
                opener == crate::protocol::OpenerPolicy::Keep,
                sandbox,
            );
            if let Some(name) = target.new_context_name() {
                shared.tree.set_name(frame, name.as_str().to_owned());
            }
            (frame, world.frame_host.clone())
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

    /// Whether a new auxiliary context for `url` may stay in this renderer.
    ///
    /// A context with no URL starts on `about:blank`; one on the opener's own
    /// origin keeps the opener's realm reachable as `window.opener` and as a
    /// same-origin `WindowProxy`. Every other URL normally stays with the
    /// browser tab's cross-origin context
    /// (<https://html.spec.whatwg.org/multipage/browsers.html#concept-origin>).
    /// `allow_cross_origin` is the host's answer to
    /// [`BrowserServices::opens_window_as_tab`]: a host with no tab carrier
    /// keeps the context here whatever its origin.
    ///
    /// [`BrowserServices::opens_window_as_tab`]: crate::protocol::BrowsingContextHost::opens_window_as_tab
    fn may_host_auxiliary(&self, url: Option<&Url>, allow_cross_origin: bool) -> bool {
        if allow_cross_origin {
            return true;
        }
        url.is_none_or(|url| {
            if url.scheme() == "about" {
                return true;
            }
            if url.scheme() == "blob" {
                // A blob's origin is its store entry's, which re-parsing the
                // URL cannot recover for an opaque creator. The store already
                // answers only for an entry this document may use.
                return self.object_url_origin(url.as_str()).is_some();
            }
            url.origin() == self.origin
        })
    }

    /// Queues a classic inline script that could not run in the realm that
    /// inserted it (`window.open`'s document has no realm until the opening
    /// script returns).
    pub(crate) fn queue_deferred_script(&mut self, id: NodeId) {
        self.deferred_scripts.push(id);
    }

    /// Takes the inline scripts waiting for this realm to exist.
    pub(crate) fn take_deferred_scripts(&mut self) -> Vec<NodeId> {
        std::mem::take(&mut self.deferred_scripts)
    }

    /// Takes every registered frame waiting for a document, for the frame host
    /// to materialize.
    pub(crate) fn take_pending_frames(&mut self) -> Vec<(FrameId, NodeId)> {
        std::mem::take(&mut self.pending_frames)
    }

    /// Connected `iframe`s in tree order that precede `before`, or every
    /// connected iframe when `before` is `None`.
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

    /// The realm of `frame`, while it is alive.
    pub(crate) fn frame_world(&self, frame: FrameId) -> Option<Rc<RefCell<World>>> {
        self.runtime.registry.borrow().frame_world(frame)
    }

    /// The child frame owned by `container`, when its browsing context
    /// exists (<https://html.spec.whatwg.org/multipage/iframe-embed-object.html#dom-iframe-contentwindow>).
    pub(crate) fn frame_for_container(&self, container: NodeId) -> Option<FrameId> {
        self.runtime
            .shared
            .borrow()
            .tree
            .frame_for_container(container)
    }

    /// Destroys the child navigable of each `iframe` container.
    ///
    /// HTML iframe [removing steps] run destroy-a-child-navigable and do not
    /// fire `pagehide`/`unload` synchronously
    /// (<https://html.spec.whatwg.org/multipage/iframe-embed-object.html#the-iframe-element:html-element-removing-steps>).
    pub(crate) fn destroy_child_navigables(&self, containers: &[NodeId]) {
        let mut shared = self.runtime.shared.borrow_mut();
        for container in containers {
            if let Some(frame) = shared.tree.frame_for_container(*container) {
                shared.remove_child_navigable(frame, *container);
            }
        }
    }

    /// The serialized origin of this realm's document
    /// (<https://html.spec.whatwg.org/multipage/browsers.html#concept-origin>).
    pub(crate) fn origin_string(&self) -> String {
        self.origin.ascii_serialization()
    }

    /// Replaces the document URL and captures a new origin for that document.
    pub(crate) fn set_url(&mut self, url: Url) {
        let origin = url.origin();
        self.set_url_with_origin(url, origin);
    }

    /// Replaces the document URL and stores `origin` as the captured origin.
    ///
    /// `about:blank` and `blob:` documents inherit an origin. `Url::origin()`
    /// would allocate a fresh `Origin::Opaque` for those URLs, so the child
    /// would not match its parent
    /// (<https://html.spec.whatwg.org/multipage/browsers.html#determining-the-origin>).
    pub(crate) fn set_url_with_origin(&mut self, url: Url, origin: url::Origin) {
        self.referrer_url = url.clone();
        self.document_url = url;
        self.origin = origin;
        self.refresh_first_party();
    }

    /// Resolves `spec` against this document's URL, falling back to the
    /// creator's base URL for an `about:srcdoc` document, whose own URL
    /// cannot be a base
    /// (<https://html.spec.whatwg.org/multipage/urls-and-fetching.html#fallback-base-url>).
    pub(crate) fn resolve_relative(&self, spec: &str) -> Option<Url> {
        Url::parse(spec).ok().or_else(|| {
            self.document_url
                .join(spec)
                .ok()
                .or_else(|| self.base_url.as_ref().and_then(|base| base.join(spec).ok()))
        })
    }

    /// Whether `url` is same-origin with this document's captured origin.
    /// Tuple origins compare with `Url::origin()`. Opaque origins cannot:
    /// `about:blank`, `data:`, and `file:` each allocate a new `Origin::Opaque`.
    /// Those match only this document's own URL, or a blob URL whose stored
    /// origin is this captured origin.
    pub(crate) fn url_matches_origin(&self, url: &Url) -> bool {
        match self.origin {
            url::Origin::Tuple(..) => url.origin() == self.origin,
            url::Origin::Opaque(_) => {
                let blob_origin = if url.scheme() == "blob" {
                    self.runtime.stored_object_url_origin(url.as_str())
                } else {
                    None
                };
                opaque_url_matches(&self.document_url, &self.origin, url, blob_origin.as_ref())
            }
        }
    }

    /// The origin scoping this realm's storage areas, or `None` for an opaque
    /// origin, where the storage getters must throw `SecurityError`
    /// (<https://html.spec.whatwg.org/multipage/webstorage.html#the-localstorage-attribute>).
    pub(crate) fn storage_origin(&self) -> Option<StorageKey> {
        StorageKey::from_origin(&self.origin)
    }

    /// `Storage.getItem(key)`.
    pub(crate) fn storage_get(&self, kind: StorageKind, key: &str) -> Option<String> {
        let origin = self.storage_origin()?;
        self.runtime.services.storage_get(kind, &origin, key)
    }

    /// The keys of one storage area, in the area's iteration order.
    pub(crate) fn storage_keys(&self, kind: StorageKind) -> Vec<String> {
        let Some(origin) = self.storage_origin() else {
            return Vec::new();
        };
        self.runtime.services.storage_keys(kind, &origin)
    }

    /// `Storage.setItem(key, value)`; `Ok(None)` means no change and the error
    /// is a refused write. A change queues the `storage` event for every other
    /// same-origin frame of the engine
    /// (<https://html.spec.whatwg.org/multipage/webstorage.html#concept-storage-broadcast>).
    pub(crate) fn storage_set(
        &self,
        kind: StorageKind,
        key: &str,
        value: &str,
    ) -> Result<Option<StorageChange>, StorageError> {
        let Some(origin) = self.storage_origin() else {
            return Ok(None);
        };
        let change = self.runtime.services.storage_set(
            kind,
            &origin,
            self.document_url.as_str(),
            key,
            value,
            self.frame,
        )?;
        if let Some(change) = &change {
            self.queue_storage_event(kind, &origin, change);
        }
        Ok(change)
    }

    /// `Storage.removeItem(key)`; `None` means the key was absent.
    pub(crate) fn storage_remove(&self, kind: StorageKind, key: &str) -> Option<StorageChange> {
        let origin = self.storage_origin()?;
        let change = self.runtime.services.storage_remove(
            kind,
            &origin,
            self.document_url.as_str(),
            key,
            self.frame,
        );
        if let Some(change) = &change {
            self.queue_storage_event(kind, &origin, change);
        }
        change
    }

    /// `Storage.clear()`; `None` means the area was empty.
    pub(crate) fn storage_clear(&self, kind: StorageKind) -> Option<StorageChange> {
        let origin = self.storage_origin()?;
        let change = self.runtime.services.storage_clear(
            kind,
            &origin,
            self.document_url.as_str(),
            self.frame,
        );
        if let Some(change) = &change {
            self.queue_storage_event(kind, &origin, change);
        }
        change
    }

    /// Queues the event for a change this engine made. Only `sessionStorage`
    /// needs it: `localStorage` changes return as a browser broadcast, which
    /// excludes the source frame.
    fn queue_storage_event(&self, kind: StorageKind, origin: &StorageKey, change: &StorageChange) {
        if kind != StorageKind::Session {
            return;
        }
        self.runtime
            .pending_storage
            .borrow_mut()
            .push(PendingStorageEvent {
                source: Some(self.frame),
                origin: origin.clone(),
                kind,
                key: change.key.clone(),
                old_value: change.old_value.clone(),
                new_value: change.new_value.clone(),
                url: self.document_url.to_string(),
            });
    }

    /// The root node of the frame's active document.
    pub(crate) fn main_document_root(&self) -> Option<NodeId> {
        let document = self.document?;
        self.main_document().map(|parsed| NodeId {
            document,
            node: parsed.document.base.root_node().id,
        })
    }

    /// The URL `document.URL` reports for `id`
    /// (<https://dom.spec.whatwg.org/#dom-document-url>).
    ///
    /// Script-created documents carry their own URL (a `DOMParser` result
    /// takes the realm's URL); everything else falls back to the world's
    /// document.
    pub(crate) fn document_url_of(&self, id: NodeId) -> String {
        if let Some(url) = self.document(id).and_then(|parsed| parsed.url.clone()) {
            return url;
        }
        if self.is_main_document(id) {
            return self.document_url.as_str().to_owned();
        }
        "about:blank".to_owned()
    }

    /// Runs HTML's set-the-frozen-base-URL for one `base` element: resolve
    /// its `href` against the fallback base URL and run the `base-uri` check
    /// (<https://html.spec.whatwg.org/multipage/semantics.html#the-base-element>,
    /// <https://w3c.github.io/webappsec-csp/#is-base-allowed-for-document>).
    ///
    /// Returns the base URL the element may freeze. A repeated call with the
    /// same `href` reuses the earlier answer, so a resumed parse walk neither
    /// repeats the check nor the violation.
    pub(crate) fn apply_base_element(&mut self, id: NodeId) -> Option<Url> {
        let href = self.document(id).and_then(|parsed| {
            crate::dom::tree::attr(&parsed.document.base, id.node, "href").map(str::to_owned)
        });
        if self.state.base_href_seen.get(&id) == Some(&href) {
            return self.state.base_results.get(&id).cloned().flatten();
        }
        self.state.base_href_seen.insert(id, href.clone());
        let Some(href) = href else {
            self.state.blocked_bases.remove(&id);
            self.state.base_results.insert(id, None);
            return None;
        };
        let fallback = self.document_base_url(id);
        // HTML returns the fallback before CSP runs when the value is not a
        // URL or names a `data:`/`javascript:` resource
        // (<https://html.spec.whatwg.org/multipage/semantics.html#the-base-element>).
        let Some(url) = fallback
            .join(&href)
            .ok()
            .filter(|url| !matches!(url.scheme(), "data" | "javascript"))
        else {
            self.state.blocked_bases.remove(&id);
            self.state.base_results.insert(id, None);
            return None;
        };
        let document_url =
            Url::parse(&self.document_url_of(id)).unwrap_or_else(|_| self.document_url.clone());
        let violations = self
            .policy
            .csp
            .check_base(&self.origin, &document_url, &url);
        if !violations.is_empty() {
            self.queue_csp_violations(id, violations.clone());
        }
        if crate::csp::blocks(&violations) {
            self.state.blocked_bases.insert(id);
            self.state.base_results.insert(id, None);
            None
        } else {
            self.state.blocked_bases.remove(&id);
            self.state.base_results.insert(id, Some(url.clone()));
            Some(url)
        }
    }

    /// The document's base URL: the first `base` element's `href` resolved
    /// against the fallback base URL, or the fallback itself
    /// (<https://html.spec.whatwg.org/multipage/urls-and-fetching.html#document-base-url>).
    pub(crate) fn document_base_url_of(&self, id: NodeId) -> String {
        self.document_base_url(id).to_string()
    }

    /// This realm's document base URL, resolved from the frozen base URL
    /// (<https://html.spec.whatwg.org/multipage/urls-and-fetching.html#document-base-url>).
    pub(crate) fn document_base_url(&self, id: NodeId) -> Url {
        let document_url = self.document_url_of(id);
        let parsed = Url::parse(&document_url).ok();
        // An `about:srcdoc` document's fallback base URL is the creator's base
        // URL; its own URL cannot be a base
        // (<https://html.spec.whatwg.org/multipage/urls-and-fetching.html#fallback-base-url>).
        let fallback = self
            .is_main_document(id)
            .then(|| self.base_url.clone())
            .flatten()
            .or(parsed)
            .unwrap_or_else(|| Url::parse("about:blank").expect("static URL parses"));
        let Some(parsed) = self.document(id) else {
            return fallback;
        };
        let Some(href) = self.frozen_base_href(&parsed) else {
            return fallback;
        };
        // A `data:` or `javascript:` href is not a base, and an unparseable
        // one leaves the fallback in place
        // (<https://html.spec.whatwg.org/multipage/semantics.html#the-base-element>).
        fallback
            .join(&href)
            .ok()
            .filter(|url| !matches!(url.scheme(), "data" | "javascript"))
            .unwrap_or(fallback)
    }

    /// The URL the document's cookies belong to: its own creation URL, or the
    /// creator's cookie URL when the document inherits its origin. A `<base
    /// href>` never moves it
    /// (<https://html.spec.whatwg.org/multipage/dom.html#dom-document-cookie>).
    pub(crate) fn cookie_url(&self) -> Url {
        self.inherited_cookie_url
            .clone()
            .unwrap_or_else(|| self.creation_url.clone())
    }

    /// This realm's main document base URL, for a caller that only has the
    /// realm.
    pub(crate) fn main_base_url(&self) -> Url {
        let fallback = self
            .base_url
            .clone()
            .unwrap_or_else(|| self.document_url.clone());
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

    /// The frozen base URL's `href`: the first `base` element in tree order
    /// that carries one and that no `base-uri` policy refused
    /// (<https://html.spec.whatwg.org/multipage/urls-and-fetching.html#document-base-url>,
    /// <https://w3c.github.io/webappsec-csp/#is-base-allowed-for-document>).
    ///
    /// A refused element stays the one whose `href` defines the frozen base
    /// URL, so the walk does not move on to the next `base`: the document
    /// falls back to its fallback base URL instead.
    pub(crate) fn frozen_base_href(&self, parsed: &Parsed) -> Option<String> {
        let base = &parsed.document.base;
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
                if self.state.blocked_bases.contains(&id) {
                    return None;
                }
                return Some(value.to_owned());
            }
            stack.extend(node.children.iter().rev().copied());
        }
        None
    }

    pub(crate) fn queue_frame_navigation(&mut self, navigation: FrameNavigation) {
        self.state.frame_navigations.push(navigation);
    }

    pub(crate) fn take_frame_navigations(&mut self) -> Vec<FrameNavigation> {
        std::mem::take(&mut self.state.frame_navigations)
    }

    /// Whether a queued navigation has not been handed to the engine yet.
    pub(crate) fn has_frame_navigations(&self) -> bool {
        !self.state.frame_navigations.is_empty()
    }

    /// Queues a connected `<script src>` element for its fetch prepare steps.
    pub(crate) fn queue_script(&mut self, element: NodeId) {
        self.outbox
            .borrow_mut()
            .push_back(RealmRequest::Script(element));
    }

    /// Queues a declarative refresh value from a `meta http-equiv=refresh` the
    /// realm inserted or changed. The document owns the timer and the
    /// navigation.
    pub(crate) fn queue_declarative_refresh(&mut self, value: String) {
        self.outbox
            .borrow_mut()
            .push_back(RealmRequest::DeclarativeRefresh(value));
    }

    /// Queues violations for targets the caller cannot dispatch into (the
    /// module loader runs inside `QuickJS`).
    pub(crate) fn queue_csp_violations(
        &mut self,
        target: NodeId,
        violations: Vec<crate::csp::Violation>,
    ) {
        self.outbox
            .borrow_mut()
            .push_back(RealmRequest::CspViolation(target, violations));
    }

    /// Queues one `document.write` body, reserving the shared stream budget
    /// until the document consumes or drops it.
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

    /// Pops the oldest outgoing request, in enqueue order.
    pub(crate) fn pop_outbox(&self) -> Option<RealmRequest> {
        self.outbox.borrow_mut().pop_front()
    }

    pub(crate) fn has_outbox_work(&self) -> bool {
        !self.outbox.borrow().is_empty()
    }

    /// Drops work the replaced document owns. Cancellations and history
    /// updates still belong to the realm, so they survive the swap.
    pub(crate) fn clear_outbox(&self) {
        let mut outbox = self.outbox.borrow_mut();
        outbox.retain(|request| {
            matches!(
                request,
                RealmRequest::ClearTimer(_)
                    | RealmRequest::AbortFetch(_)
                    | RealmRequest::HistoryPush { .. }
                    | RealmRequest::HistoryTraverse { .. }
            )
        });
    }

    /// Drops the old document's realm work before `document.open`, but keeps
    /// writes the same script queued after the open control so they reach the
    /// parser the open creates.
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
            )
        });
    }

    /// Appends one in-head `meta` policy the first time it is seen
    /// (<https://w3c.github.io/webappsec-csp/#meta-element>).
    pub(crate) fn apply_meta_policy(&mut self, id: NodeId) {
        if self.state.applied_csp_metas.contains(&id) {
            return;
        }
        let content = self
            .document(id)
            .and_then(|parsed| crate::csp::meta_policy(&parsed.document.base, id.node));
        if let Some(content) = content
            && self.state.applied_csp_metas.insert(id)
        {
            self.policy.csp.push_meta(&content);
        }
    }

    /// Marks a script element the HTML parser inserted
    /// (<https://html.spec.whatwg.org/multipage/scripting.html#parser-inserted>).
    pub(crate) fn mark_parser_inserted(&mut self, id: NodeId) {
        self.state.parser_inserted.insert(id);
    }

    /// Clears the parser-inserted flag: script insertion unsets it.
    pub(crate) fn clear_parser_inserted(&mut self, id: NodeId) {
        self.state.parser_inserted.remove(&id);
    }

    pub(crate) fn is_parser_inserted(&self, id: NodeId) -> bool {
        self.state.parser_inserted.contains(&id)
    }

    /// Records the file list a script assigned to a `type=file` input.
    pub(crate) fn set_input_files(&mut self, id: NodeId, files: Vec<Persistent<Value<'static>>>) {
        self.input_files.insert(id, files);
    }

    /// The file list a script assigned to a `type=file` input, if any.
    pub(crate) fn input_files(&self, id: NodeId) -> Option<&[Persistent<Value<'static>>]> {
        self.input_files.get(&id).map(Vec::as_slice)
    }

    /// The form-associated element's state, created on first use.
    pub(crate) fn control(&mut self, id: NodeId) -> &mut ControlState {
        self.state.controls.entry(id).or_default()
    }

    /// The form-associated element's state, when something already created it.
    pub(crate) fn control_state(&self, id: NodeId) -> Option<&ControlState> {
        self.state.controls.get(&id)
    }

    /// Drops the edit state a reset, a script value set, or a type change
    /// ends, keeping the element's custom validity message.
    pub(crate) fn clear_control_edit_state(&mut self, id: NodeId) {
        if let Some(control) = self.state.controls.get_mut(&id) {
            control.clear_edit_state();
        }
    }

    /// The `<img>` request `id`'s pixels belong to, cloned out of Blitz's
    /// per-node record. Blitz owns request identity, so `currentSrc`,
    /// `complete`, `naturalWidth`, and `drawImage` read this instead of
    /// keeping parallel maps
    /// (<https://html.spec.whatwg.org/multipage/images.html#updating-the-image-data>).
    pub(crate) fn image_request(&self, id: NodeId) -> Option<blitz_dom::ImageRequestState> {
        let parsed = self.document(id)?;
        parsed.document.base.image_request(id.node).cloned()
    }

    /// The decoded size Blitz holds for `id`, when its current request loaded.
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
        let url = format!("blob:{}/{id}", self.origin.ascii_serialization());
        self.runtime.insert_object_url(
            url.clone(),
            ObjectUrlEntry {
                contents: Arc::from(contents),
                content_type: Arc::from(content_type),
                origin: self.origin.clone(),
                frame: self.frame,
                document: self.document,
            },
        );
        budget.object_url_bytes = total;
        Some(url)
    }

    pub(crate) fn object_url_contents(&self, url: &str) -> Option<Arc<str>> {
        self.runtime.object_url_contents(url, &self.origin)
    }

    pub(crate) fn object_url_type(&self, url: &str) -> Option<Arc<str>> {
        self.runtime.object_url_type(url, &self.origin)
    }

    /// The creator origin stored for `url`, when this document may use it.
    pub(crate) fn object_url_origin(&self, url: &str) -> Option<url::Origin> {
        self.runtime.object_url_origin(url, &self.origin)
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
        let frame = self.frame;
        let released = self.runtime.take_object_urls(|entry| entry.frame == frame);
        self.refund_object_url_bytes(released);
    }

    pub(crate) fn revoke_object_url(&mut self, url: &str) {
        // Same-partition check: a cross-origin caller must not remove the
        // entry or refund bytes it did not allocate
        // (<https://w3c.github.io/FileAPI/#dfn-revokeObjectURL>).
        if self.runtime.object_url_origin(url, &self.origin).is_none() {
            return;
        }
        if let Some(released) = self.runtime.remove_object_url(url) {
            let mut budget = self.budget.borrow_mut();
            budget.object_url_bytes = budget.object_url_bytes.saturating_sub(released);
        }
    }

    pub(crate) fn add_listener(&mut self, target: EventTargetKey, listener: Rc<Listener>) {
        if let EventTargetKey::Attribute { id, .. } = target {
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
            return;
        }
        self.events
            .listeners
            .entry(target)
            .or_default()
            .push(listener);
    }

    /// A clone of one target's listener list, taken when dispatch invokes the
    /// target (<https://dom.spec.whatwg.org/#concept-event-listener-invoke>).
    pub(crate) fn listener_snapshot(&self, target: EventTargetKey) -> Vec<Rc<Listener>> {
        if let EventTargetKey::Attribute { id, .. } = target {
            return self
                .runtime
                .registry
                .borrow()
                .attributes
                .entries
                .get(&id)
                .map_or_default(|entry| entry.listeners.clone());
        }
        self.events
            .listeners
            .get(&target)
            .cloned()
            .unwrap_or_default()
    }

    /// Drops one listener from a target's list; the listener's `removed` flag
    /// is what a concurrent dispatch checks, so both happen together.
    pub(crate) fn remove_listener(&mut self, target: EventTargetKey, listener: &Rc<Listener>) {
        if let EventTargetKey::Attribute { id, .. } = target {
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
            return;
        }
        if let Some(list) = self.events.listeners.get_mut(&target) {
            list.retain(|existing| !Rc::ptr_eq(existing, listener));
        }
    }

    pub(crate) fn next_standalone_target(&mut self) -> u64 {
        let id = self.events.next_standalone_target;
        self.events.next_standalone_target = self.events.next_standalone_target.wrapping_add(1);
        id
    }

    pub(crate) fn intern_standalone_target(
        &mut self,
        id: u64,
        target: Persistent<Object<'static>>,
    ) {
        self.events.standalone_targets.insert(id, target);
    }

    pub(crate) fn standalone_target(&self, id: u64) -> Option<Persistent<Object<'static>>> {
        self.events.standalone_targets.get(&id).cloned()
    }

    /// The window object of this realm; event dispatch uses it when a target
    /// from this world is reached from another realm's call frame.
    pub(crate) fn set_window(&mut self, window: Persistent<Object<'static>>) {
        self.events.window = Some(window);
    }

    pub(crate) fn window_object(&self) -> Option<Persistent<Object<'static>>> {
        self.events.window.clone()
    }

    /// The active document's readiness
    /// (<https://html.spec.whatwg.org/multipage/dom.html#current-document-readiness>).
    pub(crate) fn main_ready_state(&self) -> ReadyState {
        self.main_document()
            .map_or(ReadyState::Complete, |parsed| parsed.ready_state)
    }

    pub(crate) fn set_main_ready_state(&mut self, state: ReadyState) {
        if let Some(mut parsed) = self.main_document_mut() {
            parsed.ready_state = state;
        }
    }

    /// The parent of `id` in its tree, if any.
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

    /// Whether `id` is the root document node of its tree.
    pub(crate) fn node_is_document(&self, id: NodeId) -> bool {
        self.document(id)
            .is_some_and(|parsed| parsed.document.base.root_node().id == id.node)
    }

    pub(crate) fn clear_listeners(&mut self) {
        self.events.clear_realm();
        self.state.implementations.clear();
        self.brands.clear();
        self.clear_observers();
        // `set_input_files` roots the page's `File` objects so a form upload
        // keeps them alive. Left in place, those roots keep the realm's
        // context alive, the context keeps the globals (and their `Rc<World>`
        // closures) alive, and the cycle never frees the runtime
        // (<https://w3c.github.io/FileAPI/#file>).
        self.input_files.clear();
    }

    /// Drops the captured host primitives. Like every other JS-holding field,
    /// they must go before the realm's context does: a `Persistent` keeps its
    /// context alive, and a live context keeps the globals (which own
    /// `Rc<World>` closures) alive, so an unreleased primitive deadlocks
    /// teardown and trips `JS_FreeRuntime`'s live-object assertion.
    pub(crate) fn release_host_primitives(&mut self) {
        self.runtime
            .registry
            .borrow_mut()
            .reactions
            .forget_frame(self.frame);
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
    }

    /// The cached `[SameObject]` member for `(id, member)`, when this
    /// renderer process created one.
    pub(crate) fn same_object(
        &self,
        id: NodeId,
        member: SameObject,
    ) -> Option<Persistent<Value<'static>>> {
        self.runtime.registry.borrow().same_object(id, member)
    }

    /// Caches one `[SameObject]` member for the node.
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

    /// The focused element of a document, if any.
    pub(crate) fn active_element(&self, document: u32) -> Option<NodeId> {
        self.state.active_elements.get(&document).copied()
    }

    /// A control's `(start, end, direction)` selection, defaulting to a
    /// collapsed caret at 0.
    pub(crate) fn selection_of(&self, id: NodeId) -> (u32, u32, u8) {
        self.state.selections.get(&id).copied().unwrap_or((0, 0, 0))
    }

    /// Stores a control's `(start, end, direction)` selection.
    pub(crate) fn set_selection(&mut self, id: NodeId, start: u32, end: u32, direction: u8) {
        self.state.selections.insert(id, (start, end, direction));
    }

    /// The stable `WebDriver` element id for `node`, allocating one on first
    /// use. Ids come from the process-wide registry so they cannot alias
    /// between windows
    /// (<https://w3c.github.io/webdriver/#elements>).
    pub(crate) fn remote_id(&mut self, node: NodeId) -> u64 {
        if let Some(&remote) = self.state.remote_ids.get(&node) {
            return remote;
        }
        let remote = self.runtime.registry.borrow_mut().allocate_remote();
        self.state.remote_ids.insert(node, remote);
        self.state.remote_nodes.insert(remote, node);
        remote
    }

    /// The node behind a `WebDriver` element id.
    pub(crate) fn node_for_remote(&self, remote: u64) -> Option<NodeId> {
        self.state.remote_nodes.get(&remote).copied()
    }

    /// Whether `node` is the realm's active document, which is what decides
    /// `document.defaultView`
    /// (<https://html.spec.whatwg.org/multipage/nav-history-apis.html#dom-document-defaultview>).
    pub(crate) fn is_main_document(&self, node: NodeId) -> bool {
        self.document == Some(node.document_id())
    }

    /// One event handler property, when set.
    pub(crate) fn handler_attribute(
        &self,
        node: Option<NodeId>,
        name: &str,
    ) -> Option<Persistent<Value<'static>>> {
        self.events
            .handler_attributes
            .get(&(node, name.to_owned()))
            .cloned()
    }

    /// Sets or clears one event handler property.
    pub(crate) fn set_handler_attribute(
        &mut self,
        node: Option<NodeId>,
        name: &str,
        value: Option<Persistent<Value<'static>>>,
    ) {
        if let Some(value) = value {
            self.events
                .cleared_handlers
                .remove(&(node, name.to_owned()));
            self.events
                .handler_attributes
                .insert((node, name.to_owned()), value);
        } else {
            self.events
                .handler_attributes
                .remove(&(node, name.to_owned()));
            self.events.cleared_handlers.insert((node, name.to_owned()));
        }
    }

    /// Whether one handler property was explicitly cleared by script.
    pub(crate) fn handler_cleared(&self, node: Option<NodeId>, name: &str) -> bool {
        self.events
            .cleared_handlers
            .contains(&(node, name.to_owned()))
    }

    pub(crate) fn set_active_element(&mut self, document: u32, node: Option<NodeId>) {
        match node {
            Some(node) => {
                self.state.active_elements.insert(document, node);
            }
            None => {
                self.state.active_elements.remove(&document);
            }
        }
        // Mirror into the document so `:focus` and `:focus-within` match
        // (<https://drafts.csswg.org/selectors-4/#the-focus-pseudo>).
        if let Some(parsed) = self.runtime.documents.borrow_mut().get_mut(document) {
            match node {
                Some(focused) => {
                    parsed.document.base.set_focus_to(focused.node);
                }
                None => parsed.document.base.clear_focus(),
            }
        }
    }

    /// Whether `node`'s `click()` is already running.
    pub(crate) fn click_in_progress(&self, node: NodeId) -> bool {
        self.clicks_in_progress.contains(&node)
    }

    /// Marks `node`'s `click()` as running; the caller clears it when done.
    pub(crate) fn set_click_in_progress(&mut self, node: NodeId, running: bool) {
        if running {
            self.clicks_in_progress.insert(node);
        } else {
            self.clicks_in_progress.remove(&node);
        }
    }

    /// Grants transient activation and sets the sticky flag for a trusted
    /// user interaction
    /// (<https://html.spec.whatwg.org/multipage/interaction.html#activation-notification>).
    pub(crate) fn activate_user(&mut self) {
        // The transient activation duration is implementation-defined; the
        // HTML Standard's non-normative guidance suggests a few seconds.
        self.transient_activation = Some(Instant::now() + Duration::from_secs(5));
        self.sticky_activation = true;
    }

    /// Whether the realm has transient activation that has not expired.
    pub(crate) fn user_activation_active(&self) -> bool {
        self.transient_activation
            .is_some_and(|expires| Instant::now() < expires)
    }

    /// Whether the realm was ever activated.
    pub(crate) fn user_activation_sticky(&self) -> bool {
        self.sticky_activation
    }

    /// Consumes transient activation, returning whether it was active
    /// (<https://html.spec.whatwg.org/multipage/interaction.html#consume-user-activation>).
    pub(crate) fn consume_user_activation(&mut self) -> bool {
        if self.user_activation_active() {
            self.transient_activation = None;
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

/// Whether `mutation` is observable by `observer`, producing a record when
/// it is (<https://dom.spec.whatwg.org/#concept-mo-queue>).
///
/// Every in-scope, enabled registration contributes; one observer gets one
/// record per mutation, and it carries the old value when *any* interested
/// registration asked for it (the spec's `interestedObservers` map folds the
/// registrations per observer).
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
    for observation in &observer.observations {
        // `Attr` targets never match: journal records target elements and
        // `Attr` nodes are ancestors of nothing, so observing one registers
        // successfully but legitimately never fires.
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
                        // A filter only ever matches unnamespaced attributes;
                        // namespaced ones are always skipped
                        // (<https://dom.spec.whatwg.org/#queue-a-mutation-record>).
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

/// One recorded tree mutation, in observer-delivery shape.
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

/// Identity of one `Attr` platform object.
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

/// Agent-owned Attr identities, attachment indexes, and weak wrappers.
/// Adoption changes the node document, never the creation scope or id
/// (<https://dom.spec.whatwg.org/#concept-element-attributes-append>).
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

    /// Validates the source identity and owner before mutating the destination,
    /// then publishes replacement and adoption in the same registry borrow.
    /// <https://dom.spec.whatwg.org/#concept-element-attributes-set>
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

    /// Ending an edit drops only the edit-derived state; a custom validity
    /// message and `<output>`'s value mode survive, matching the reset and
    /// script-value-set rules.
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
