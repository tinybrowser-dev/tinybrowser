// https://html.spec.whatwg.org/multipage/canvas.html#the-canvas-element
// https://html.spec.whatwg.org/multipage/canvas.html#the-offscreencanvas-interface

use rquickjs::{Ctx, Value};

use crate::dom::tree::{NodeId, is_html_tag};
use crate::js::bindings::{host_node_id, world};

use super::SourceImage;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct CanvasId(pub(crate) u64);

pub(crate) trait CanvasHost {
    fn canvas_id(&self, ctx: &Ctx<'_>) -> Option<CanvasId>;

    fn dimensions(&self, ctx: &Ctx<'_>) -> (u32, u32);

    fn snapshot(&self, ctx: &Ctx<'_>) -> Option<SourceImage>;

    fn origin_clean(&self, ctx: &Ctx<'_>) -> Option<bool>;

    fn has_open_layer(&self, ctx: &Ctx<'_>) -> bool;
}

// https://html.spec.whatwg.org/multipage/canvas.html#dom-canvas-width
#[derive(Clone, Copy)]
pub(crate) struct ElementCanvas {
    pub(crate) node: NodeId,
}

impl ElementCanvas {
    pub(crate) fn resolve<'js>(ctx: &Ctx<'js>, value: &Value<'js>) -> Option<Self> {
        let node = host_node_id(ctx, value)?;
        let world = world(ctx).ok()?;
        let owner = world.borrow();
        if !owner.owns_node(node) {
            return None;
        }
        let is_canvas = owner
            .with_document(node, |parsed| {
                parsed
                    .document
                    .base
                    .get_node(node.node)
                    .is_some_and(|node| is_html_tag(Some(&node.data), "canvas"))
            })
            .unwrap_or(false);
        is_canvas.then_some(Self { node })
    }
}

impl CanvasHost for ElementCanvas {
    fn canvas_id(&self, ctx: &Ctx<'_>) -> Option<CanvasId> {
        let world = world(ctx).ok()?;
        let registry = world.borrow().registry();
        registry.borrow().canvas_id_for_node(self.node)
    }

    fn dimensions(&self, ctx: &Ctx<'_>) -> (u32, u32) {
        (
            super::canvas_dimension(ctx, self.node, "width", 300),
            super::canvas_dimension(ctx, self.node, "height", 150),
        )
    }

    fn snapshot(&self, ctx: &Ctx<'_>) -> Option<SourceImage> {
        let id = self.canvas_id(ctx)?;
        let world = world(ctx).ok()?;
        let registry = world.borrow().registry();
        registry.borrow().canvas_snapshot(id)
    }

    fn origin_clean(&self, ctx: &Ctx<'_>) -> Option<bool> {
        let id = self.canvas_id(ctx)?;
        let world = world(ctx).ok()?;
        let registry = world.borrow().registry();
        registry.borrow().canvas_origin_clean(id)
    }

    fn has_open_layer(&self, ctx: &Ctx<'_>) -> bool {
        let Some(id) = self.canvas_id(ctx) else {
            return false;
        };
        let Ok(world) = world(ctx) else {
            return false;
        };
        let registry = world.borrow().registry();
        registry.borrow().canvas_has_open_layer(id)
    }
}

// https://html.spec.whatwg.org/multipage/canvas.html#dom-offscreencanvas-width
#[derive(Clone, Copy)]
pub(crate) struct OffscreenCanvasHost {
    pub(crate) id: CanvasId,
}

impl CanvasHost for OffscreenCanvasHost {
    fn canvas_id(&self, _ctx: &Ctx<'_>) -> Option<CanvasId> {
        Some(self.id)
    }

    fn dimensions(&self, ctx: &Ctx<'_>) -> (u32, u32) {
        let Ok(world) = world(ctx) else {
            return (0, 0);
        };
        let registry = world.borrow().registry();
        registry
            .borrow()
            .canvas_dimensions(self.id)
            .unwrap_or((0, 0))
    }

    fn snapshot(&self, ctx: &Ctx<'_>) -> Option<SourceImage> {
        let world = world(ctx).ok()?;
        let registry = world.borrow().registry();
        registry.borrow().canvas_snapshot(self.id)
    }

    fn origin_clean(&self, ctx: &Ctx<'_>) -> Option<bool> {
        let world = world(ctx).ok()?;
        let registry = world.borrow().registry();
        registry.borrow().canvas_origin_clean(self.id)
    }

    fn has_open_layer(&self, ctx: &Ctx<'_>) -> bool {
        let Ok(world) = world(ctx) else {
            return false;
        };
        let registry = world.borrow().registry();
        registry.borrow().canvas_has_open_layer(self.id)
    }
}
