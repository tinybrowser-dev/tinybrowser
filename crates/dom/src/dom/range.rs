// https://dom.spec.whatwg.org/#ranges

use std::cmp::Ordering;

use blitz_dom::NodeData;

use crate::dom::tree::{NodeId, child_ids};

// https://dom.spec.whatwg.org/#concept-range-bp
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BoundaryPoint {
    pub node: NodeId,
    pub offset: u32,
}

// https://dom.spec.whatwg.org/#concept-range-bp
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AttrPoint {
    pub scope: NodeId,
    pub id: u64,
}

// https://dom.spec.whatwg.org/#concept-range
#[derive(Clone, Copy, Debug)]
pub struct RangeState {
    pub start: BoundaryPoint,
    pub end: BoundaryPoint,
    pub attr: Option<AttrPoint>,
}

impl RangeState {
    #[must_use]
    pub fn collapsed(point: BoundaryPoint) -> Self {
        Self {
            start: point,
            end: point,
            attr: None,
        }
    }

    #[must_use]
    pub fn attribute(attr: AttrPoint) -> Self {
        let placeholder = BoundaryPoint {
            node: attr.scope,
            offset: 0,
        };
        Self {
            start: placeholder,
            end: placeholder,
            attr: Some(attr),
        }
    }

    #[must_use]
    pub fn document(&self) -> u32 {
        match self.attr {
            Some(attr) => attr.scope.document,
            None => self.start.node.document,
        }
    }
}

// https://dom.spec.whatwg.org/#concept-range
#[derive(Default)]
pub struct RangeTable {
    slots: Vec<Option<RangeState>>,
    free: Vec<usize>,
    live: usize,
}

impl RangeTable {
    pub fn alloc(&mut self, state: RangeState) -> usize {
        self.live += 1;
        if let Some(slot) = self.free.pop() {
            self.slots[slot] = Some(state);
            slot
        } else {
            self.slots.push(Some(state));
            self.slots.len() - 1
        }
    }

    #[must_use]
    pub fn get(&self, slot: usize) -> Option<RangeState> {
        self.slots.get(slot).copied().flatten()
    }

    pub fn set(&mut self, slot: usize, state: RangeState) -> bool {
        match self.slots.get_mut(slot) {
            Some(entry) if entry.is_some() => {
                *entry = Some(state);
                true
            }
            _ => false,
        }
    }

    pub fn free(&mut self, slot: usize) {
        if let Some(entry) = self.slots.get_mut(slot)
            && entry.take().is_some()
        {
            self.free.push(slot);
            self.live -= 1;
        }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.live == 0
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut RangeState> {
        self.slots.iter_mut().flatten()
    }
}

fn utf16_length(text: &str) -> u32 {
    u32::try_from(text.encode_utf16().count()).unwrap_or(u32::MAX)
}

// https://dom.spec.whatwg.org/#concept-node-length
pub fn node_length(base: &blitz_dom::BaseDocument, id: NodeId) -> u32 {
    let Some(node) = base.get_node(id.node) else {
        return 0;
    };
    match &node.data {
        NodeData::Document(_)
        | NodeData::Element(_)
        | NodeData::AnonymousBlock(_)
        | NodeData::Fragment { .. }
        | NodeData::ShadowRoot { .. } => u32::try_from(node.children.len()).unwrap_or(u32::MAX),
        NodeData::Text(text) => utf16_length(&text.content),
        NodeData::Comment { contents }
        | NodeData::ProcessingInstruction { contents, .. }
        | NodeData::CDataSection { contents } => utf16_length(contents),
        NodeData::Doctype { .. } => 0,
    }
}

pub fn is_text(base: &blitz_dom::BaseDocument, id: NodeId) -> bool {
    matches!(
        base.get_node(id.node).map(|node| &node.data),
        Some(NodeData::Text(_) | NodeData::CDataSection { .. })
    )
}

// https://dom.spec.whatwg.org/#interface-characterdata
pub fn is_character_data(base: &blitz_dom::BaseDocument, id: NodeId) -> bool {
    matches!(
        base.get_node(id.node).map(|node| &node.data),
        Some(
            NodeData::Text(_)
                | NodeData::CDataSection { .. }
                | NodeData::Comment { .. }
                | NodeData::ProcessingInstruction { .. }
        )
    )
}

// https://dom.spec.whatwg.org/#dom-range-stringifier
pub fn range_text(
    document: &crate::documents::BlitzDocument,
    _document_id: u32,
    state: &RangeState,
) -> Vec<u16> {
    let base = &document.base;
    let mut units = Vec::new();
    let data_of = |node: NodeId| document.character_data(node.node).units().into_owned();
    if state.start.node == state.end.node && is_text(base, state.start.node) {
        let data = data_of(state.start.node);
        let start = (state.start.offset as usize).min(data.len());
        let end = (state.end.offset as usize).min(data.len()).max(start);
        units.extend_from_slice(&data[start..end]);
        return units;
    }
    if is_text(base, state.start.node) {
        let data = data_of(state.start.node);
        units.extend_from_slice(&data[(state.start.offset as usize).min(data.len())..]);
    }
    let ancestor = common_ancestor(base, state.start.node, state.end.node);
    append_contained_text(document, state, ancestor, &mut units);
    if is_text(base, state.end.node) {
        let data = data_of(state.end.node);
        units.extend_from_slice(&data[..(state.end.offset as usize).min(data.len())]);
    }
    units
}

fn append_contained_text(
    document: &crate::documents::BlitzDocument,
    state: &RangeState,
    ancestor: NodeId,
    units: &mut Vec<u16>,
) {
    let base = &document.base;
    for descendant in child_ids(base, ancestor.document, ancestor.node) {
        if is_text(base, descendant) {
            if is_contained(base, ancestor.document, state, descendant) {
                units.extend_from_slice(&document.character_data(descendant.node).units());
            }
        } else {
            append_contained_text(document, state, descendant, units);
        }
    }
}

// https://dom.spec.whatwg.org/#concept-range-bp
pub fn is_doctype(base: &blitz_dom::BaseDocument, id: NodeId) -> bool {
    matches!(
        base.get_node(id.node).map(|node| &node.data),
        Some(NodeData::Doctype { .. })
    )
}

fn parent_of(base: &blitz_dom::BaseDocument, id: NodeId) -> Option<NodeId> {
    base.get_node(id.node)
        .and_then(|node| node.parent)
        .map(|parent| NodeId {
            document: id.document,
            node: parent,
        })
}

pub fn is_inclusive_ancestor(
    base: &blitz_dom::BaseDocument,
    possible: NodeId,
    node: NodeId,
) -> bool {
    if possible.document != node.document {
        return false;
    }
    let mut cursor = node.node;
    loop {
        if cursor == possible.node {
            return true;
        }
        let Some(parent) = base.get_node(cursor).and_then(|node| node.parent) else {
            return false;
        };
        cursor = parent;
    }
}

fn is_inclusive_descendant(base: &blitz_dom::BaseDocument, possible: NodeId, node: NodeId) -> bool {
    is_inclusive_ancestor(base, node, possible)
}

// https://dom.spec.whatwg.org/#concept-range-bp-after
pub fn position(
    base: &blitz_dom::BaseDocument,
    document: u32,
    a: BoundaryPoint,
    b: BoundaryPoint,
) -> Ordering {
    if a.node == b.node {
        return a.offset.cmp(&b.offset);
    }
    if crate::dom::tree::tree_order(base, document, b.node.node, a.node.node) == Ordering::Less {
        return position(base, document, b, a).reverse();
    }
    if is_inclusive_ancestor(base, a.node, b.node) {
        let mut child = b.node.node;
        while base.get_node(child).and_then(|node| node.parent) != Some(a.node.node) {
            let Some(parent) = base.get_node(child).and_then(|node| node.parent) else {
                return Ordering::Less;
            };
            child = parent;
        }
        let index = child_ids(base, document, a.node.node)
            .iter()
            .position(|sibling| sibling.node == child)
            .and_then(|index| u32::try_from(index).ok())
            .unwrap_or(u32::MAX);
        if index < a.offset {
            return Ordering::Greater;
        }
        return Ordering::Less;
    }
    Ordering::Less
}

pub fn same_root(base: &blitz_dom::BaseDocument, a: NodeId, b: NodeId) -> bool {
    if a.document != b.document {
        return false;
    }
    if base.get_node(a.node).is_none() || base.get_node(b.node).is_none() {
        return false;
    }
    crate::dom::tree::root_of(base, a.node) == crate::dom::tree::root_of(base, b.node)
}

// https://dom.spec.whatwg.org/#concept-range-common-ancestor
pub fn common_ancestor(base: &blitz_dom::BaseDocument, a: NodeId, b: NodeId) -> NodeId {
    let mut container = a;
    while !is_inclusive_ancestor(base, container, b) {
        let Some(parent) = parent_of(base, container) else {
            break;
        };
        container = parent;
    }
    container
}

// https://dom.spec.whatwg.org/#concept-range-contained
pub fn is_contained(
    base: &blitz_dom::BaseDocument,
    document: u32,
    state: &RangeState,
    node: NodeId,
) -> bool {
    if !same_root(base, node, state.start.node) {
        return false;
    }
    let length = node_length(base, node);
    position(
        base,
        document,
        BoundaryPoint { node, offset: 0 },
        state.start,
    ) == Ordering::Greater
        && position(
            base,
            document,
            BoundaryPoint {
                node,
                offset: length,
            },
            state.end,
        ) == Ordering::Less
}

// https://dom.spec.whatwg.org/#concept-range-partially-contained
pub fn is_partially_contained(
    base: &blitz_dom::BaseDocument,
    state: &RangeState,
    node: NodeId,
) -> bool {
    is_inclusive_ancestor(base, node, state.start.node)
        != is_inclusive_ancestor(base, node, state.end.node)
}

pub fn ancestor_span(
    base: &blitz_dom::BaseDocument,
    document: u32,
    state: &RangeState,
    ancestor: NodeId,
    want_first: bool,
    want_last: bool,
) -> Option<(Option<NodeId>, Vec<NodeId>, Option<NodeId>)> {
    if !is_inclusive_ancestor(base, ancestor, state.start.node)
        || !is_inclusive_ancestor(base, ancestor, state.end.node)
    {
        return None;
    }
    let mut first = None;
    let mut last = None;
    let mut contained = Vec::new();
    for child in child_ids(base, ancestor.document, ancestor.node) {
        if is_partially_contained(base, state, child) {
            if want_first && first.is_none() {
                first = Some(child);
            }
            if want_last {
                last = Some(child);
            }
        } else if is_contained(base, document, state, child) {
            contained.push(child);
        }
    }
    Some((first, contained, last))
}

// https://dom.spec.whatwg.org/#concept-node-insert
pub fn adjust_for_insert(ranges: &mut RangeTable, parent: NodeId, index: u32, count: u32) {
    for range in ranges.iter_mut() {
        for point in [&mut range.start, &mut range.end] {
            if point.node == parent && point.offset > index {
                point.offset = point.offset.saturating_add(count);
            }
        }
    }
}

// https://dom.spec.whatwg.org/#concept-node-remove
pub fn adjust_for_remove(
    base: &blitz_dom::BaseDocument,
    ranges: &mut RangeTable,
    parent: NodeId,
    index: u32,
    removed: NodeId,
) {
    for range in ranges.iter_mut() {
        for point in [&mut range.start, &mut range.end] {
            if is_inclusive_descendant(base, point.node, removed) {
                *point = BoundaryPoint {
                    node: parent,
                    offset: index,
                };
            } else if point.node == parent && point.offset > index {
                point.offset = point.offset.saturating_sub(1);
            }
        }
    }
}

// https://dom.spec.whatwg.org/#concept-cd-replace
pub fn adjust_for_replace_data(
    ranges: &mut RangeTable,
    node: NodeId,
    offset: u32,
    count: u32,
    length: u32,
) {
    for range in ranges.iter_mut() {
        for point in [&mut range.start, &mut range.end] {
            if point.node != node {
                continue;
            }
            if point.offset > offset && point.offset <= offset.saturating_add(count) {
                point.offset = offset;
            } else if point.offset > offset.saturating_add(count) {
                point.offset = point
                    .offset
                    .saturating_add(length)
                    .saturating_sub(count.min(point.offset.saturating_sub(offset)));
            }
        }
    }
}

// https://dom.spec.whatwg.org/#dom-node-normalize
pub fn adjust_for_normalize(
    ranges: &mut RangeTable,
    node: NodeId,
    sibling: NodeId,
    parent: NodeId,
    index: u32,
    length: u32,
) {
    for range in ranges.iter_mut() {
        for point in [&mut range.start, &mut range.end] {
            if point.node == sibling {
                point.node = node;
                point.offset = point.offset.saturating_add(length);
            } else if point.node == parent && point.offset == index {
                point.node = node;
                point.offset = length;
            }
        }
    }
}

// https://dom.spec.whatwg.org/#concept-text-split
pub fn adjust_for_split(
    ranges: &mut RangeTable,
    node: NodeId,
    new_node: NodeId,
    parent: NodeId,
    index: u32,
    offset: u32,
) {
    for range in ranges.iter_mut() {
        for point in [&mut range.start, &mut range.end] {
            if point.node == node && point.offset > offset {
                point.node = new_node;
                point.offset -= offset;
            }
            if point.node == parent && point.offset == index.saturating_add(1) {
                point.offset += 1;
            }
        }
    }
}

// https://dom.spec.whatwg.org/#concept-node-replace-all
pub fn adjust_for_replace_all(
    base: &blitz_dom::BaseDocument,
    ranges: &mut RangeTable,
    parent: NodeId,
) {
    for range in ranges.iter_mut() {
        for point in [&mut range.start, &mut range.end] {
            if point.node == parent {
                if point.offset > 0 {
                    point.offset = 0;
                }
            } else if point.node != parent && is_inclusive_descendant(base, point.node, parent) {
                *point = BoundaryPoint {
                    node: parent,
                    offset: 0,
                };
            }
        }
    }
}
