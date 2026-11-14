//! Renderer-process document storage.
//!
//! Trees do not belong to a realm: the renderer stores every document by its
//! globally unique id, and realms refer to them by id. This mirrors the
//! realm-agnostic C++ DOM layer of Blink and Gecko; only the JS wrappers are
//! realm-associated.

use std::collections::{HashMap, HashSet};

use crate::Parsed;
use crate::dom::tree::NodeId;
use crate::dom_string::DomString;

use blitz_dom::NodeData;

/// A Blitz document plus the side data our engine keeps per document.
pub(crate) struct BlitzDocument {
    /// The Blitz tree, style, and layout state.
    pub base: blitz_dom::BaseDocument,
    /// Every per-document side table, all default-constructed.
    pub side: SideTables,
}

/// Side data that is not part of the Blitz tree.
#[derive(Default)]
pub struct SideTables {
    /// Mutations since the last observer drain, in operation order.
    pub journal: Vec<(u64, JournalEntry)>,
    next_journal_position: u64,
    /// Whether any observer watches this document; unobserved mutations are
    /// not recorded.
    recording: bool,
    /// Script elements whose [already started] flag is set
    /// (<https://html.spec.whatwg.org/multipage/scripting.html#already-started>).
    started_scripts: HashSet<blitz_traits::node_id::NodeId>,
    /// Selects whose option selectedness was materialized from attributes and
    /// the reset rule. Insertions, removals, and the relevant attribute
    /// changes invalidate the entry
    /// (<https://html.spec.whatwg.org/multipage/form-elements.html#selectedness-setting-algorithm>).
    selectedness_normalized: HashSet<blitz_traits::node_id::NodeId>,
    /// Live ranges by slot, adjusted by every structural mutation. Slots
    /// free with the document; see `dom::range::RangeTable`.
    pub ranges: crate::dom::range::RangeTable,
    /// Character data that contains an unpaired surrogate. The UTF-8 tree
    /// stores the lossy form for layout; this map is what DOM reads return
    /// (<https://infra.spec.whatwg.org/#javascript-string-convert>).
    exact_text: HashMap<blitz_traits::node_id::NodeId, DomString>,
    /// Attribute values that contain an unpaired surrogate. The tree attribute
    /// drops those units so a CSS `id` selector stays distinct from U+FFFD.
    exact_attrs: HashMap<(blitz_traits::node_id::NodeId, String), DomString>,
    /// Template element -> `(inert document id, contents fragment)`.
    /// The fragment's node document is not the host
    /// (<https://html.spec.whatwg.org/multipage/scripting.html#template-contents>).
    template_owners: HashMap<blitz_traits::node_id::NodeId, (u32, blitz_traits::node_id::NodeId)>,
    /// `(element, content attribute)` pairs whose nullable enumerated IDL
    /// attribute was set to null. ARIA's reflection reads that back as null
    /// even when the attribute's missing-value default is a keyword
    /// (<https://w3c.github.io/aria/#enumerated-attribute-values-html>); any
    /// content-attribute change clears the entry.
    enumerated_nulls: HashSet<(blitz_traits::node_id::NodeId, String)>,
    /// `(element, content attribute)` pairs whose IDL element reference setter
    /// stored an element (`Element`) or a list of elements (`Elements`). An
    /// absent entry is the spec's null, which falls back to the content
    /// attribute; an empty `Elements` list is an explicitly set empty list
    /// (<https://html.spec.whatwg.org/multipage/common-dom-interfaces.html#reflecting-content-attributes-in-idl-attributes:element>).
    element_references: HashMap<(blitz_traits::node_id::NodeId, String), ElementReference>,
    /// The `(document id, node)` of the template element whose contents this
    /// inert document holds. The contents fragment's `host` cannot name an
    /// element in another document, so this is the back link
    /// (<https://html.spec.whatwg.org/multipage/scripting.html#template-contents>).
    template_host: Option<(u32, blitz_traits::node_id::NodeId)>,
}

/// The state one reflected element reference carries
/// (<https://html.spec.whatwg.org/multipage/common-dom-interfaces.html#reflecting-content-attributes-in-idl-attributes:element>).
pub(crate) enum ElementReference {
    /// An `Element?` reflection.
    Element(NodeId),
    /// A `FrozenArray<Element>?` reflection; empty is an explicitly set
    /// empty list, not "never set".
    Elements(Vec<NodeId>),
}

impl ElementReference {
    /// Rewrites the stored nodes after a cross-document adoption, where the
    /// same element keeps its identity but moves to a new tree id.
    fn retargeted(self, adopted: &[(NodeId, NodeId)]) -> Self {
        match self {
            Self::Element(node) => Self::Element(retarget_node(node, adopted)),
            Self::Elements(nodes) => Self::Elements(
                nodes
                    .into_iter()
                    .map(|node| retarget_node(node, adopted))
                    .collect(),
            ),
        }
    }
}

/// The adopted copy of `node`, or `node` itself when it did not move.
fn retarget_node(node: NodeId, adopted: &[(NodeId, NodeId)]) -> NodeId {
    adopted
        .iter()
        .find(|(from, _)| *from == node)
        .map_or(node, |(_, to)| *to)
}

impl BlitzDocument {
    /// Builds an empty document from `config` (providers ride along).
    pub(crate) fn new(config: blitz_dom::DocumentConfig) -> Self {
        Self::from_base(blitz_dom::BaseDocument::new(config))
    }

    /// Wraps an already-parsed tree.
    pub(crate) fn from_base(base: blitz_dom::BaseDocument) -> Self {
        Self {
            base,
            side: SideTables::default(),
        }
    }

    /// Whether a select's option selectedness is already materialized.
    pub(crate) fn selectedness_normalized(&self, select: blitz_traits::node_id::NodeId) -> bool {
        self.side.selectedness_normalized.contains(&select)
    }

    /// Marks a select's option selectedness as materialized.
    pub(crate) fn mark_selectedness_normalized(&mut self, select: blitz_traits::node_id::NodeId) {
        self.side.selectedness_normalized.insert(select);
    }

    /// Drops the materialized selectedness for one select.
    pub(crate) fn invalidate_selectedness(&mut self, select: blitz_traits::node_id::NodeId) {
        self.side.selectedness_normalized.remove(&select);
    }

    /// Records `entry` when an observer watches this document.
    pub(crate) fn record(&mut self, entry: JournalEntry) {
        if self.side.recording {
            let position = self.side.next_journal_position;
            self.side.next_journal_position = self.side.next_journal_position.wrapping_add(1);
            self.side.journal.push((position, entry));
        }
    }

    /// Enables or disables observer recording for this document.
    pub(crate) fn set_recording(&mut self, recording: bool) {
        self.side.recording = recording;
    }

    /// Marks `(element, name)` as an IDL-null `DOMString?` enumerated value.
    pub(crate) fn mark_enumerated_null(&mut self, id: blitz_traits::node_id::NodeId, name: &str) {
        self.side.enumerated_nulls.insert((id, name.to_owned()));
    }

    /// Clears the IDL-null state after a content-attribute change.
    pub(crate) fn clear_enumerated_null(&mut self, id: blitz_traits::node_id::NodeId, name: &str) {
        self.side.enumerated_nulls.remove(&(id, name.to_owned()));
    }

    /// Whether the last write to the nullable enumerated IDL attribute was
    /// null (with no content-attribute change since).
    pub(crate) fn is_enumerated_null(&self, id: blitz_traits::node_id::NodeId, name: &str) -> bool {
        self.side.enumerated_nulls.contains(&(id, name.to_owned()))
    }

    /// Stores the element an IDL element reference setter received.
    pub(crate) fn set_element_reference(
        &mut self,
        id: blitz_traits::node_id::NodeId,
        name: &str,
        element: NodeId,
    ) {
        self.side
            .element_references
            .insert((id, name.to_owned()), ElementReference::Element(element));
    }

    /// Stores the element list an IDL element reference list setter received.
    pub(crate) fn set_element_reference_list(
        &mut self,
        id: blitz_traits::node_id::NodeId,
        name: &str,
        elements: Vec<NodeId>,
    ) {
        self.side
            .element_references
            .insert((id, name.to_owned()), ElementReference::Elements(elements));
    }

    /// The element or element list the IDL setter stored, if any.
    pub(crate) fn element_reference(
        &self,
        id: blitz_traits::node_id::NodeId,
        name: &str,
    ) -> Option<&ElementReference> {
        self.side.element_references.get(&(id, name.to_owned()))
    }

    /// Clears the explicitly set reference after a content-attribute change
    /// (<https://html.spec.whatwg.org/multipage/common-dom-interfaces.html#reflecting-content-attributes-in-idl-attributes:element>).
    pub(crate) fn clear_element_reference(
        &mut self,
        id: blitz_traits::node_id::NodeId,
        name: &str,
    ) {
        self.side.element_references.remove(&(id, name.to_owned()));
    }

    /// Drains every element reference so the store can re-file it after an
    /// adoption changed the element's document.
    pub(crate) fn take_element_references(
        &mut self,
    ) -> Vec<((blitz_traits::node_id::NodeId, String), ElementReference)> {
        std::mem::take(&mut self.side.element_references)
            .into_iter()
            .collect()
    }

    /// Files one element reference under its (possibly new) document.
    pub(crate) fn insert_element_reference(
        &mut self,
        element: blitz_traits::node_id::NodeId,
        name: String,
        reference: ElementReference,
    ) {
        self.side
            .element_references
            .insert((element, name), reference);
    }

    /// Whether `id` is a `DocumentFragment`: a fragment node or a shadow
    /// root, which is one too
    /// (<https://dom.spec.whatwg.org/#interface-documentfragment>,
    /// <https://dom.spec.whatwg.org/#interface-shadowroot>).
    pub(crate) fn is_fragment(&self, id: blitz_traits::node_id::NodeId) -> bool {
        self.base.get_node(id).is_some_and(|node| {
            matches!(
                node.data,
                NodeData::Fragment { .. } | NodeData::ShadowRoot { .. }
            )
        })
    }

    /// Creates a detached `DocumentFragment`
    /// (<https://dom.spec.whatwg.org/#dom-document-createdocumentfragment>).
    pub(crate) fn create_fragment(&mut self) -> blitz_traits::node_id::NodeId {
        self.base.mutate().create_fragment_node()
    }

    /// Creates a document type whose node document is this document
    /// (<https://dom.spec.whatwg.org/#dom-domimplementation-createdocumenttype>).
    pub(crate) fn create_doctype(
        &mut self,
        name: &str,
        public_id: &str,
        system_id: &str,
    ) -> blitz_traits::node_id::NodeId {
        self.base
            .mutate()
            .create_doctype_node(name, public_id, system_id)
    }

    /// Creates a processing instruction, parsing its attribute map from the
    /// data
    /// (<https://dom.spec.whatwg.org/#create-a-processing-instruction-node>).
    pub(crate) fn create_processing_instruction(
        &mut self,
        target: &str,
        data: &str,
    ) -> blitz_traits::node_id::NodeId {
        self.base
            .mutate()
            .create_processing_instruction_node(target, data)
    }

    /// Creates a processing instruction with the attribute map the source node
    /// carried, which can hold attribute names the XML grammar rejects
    /// (<https://dom.spec.whatwg.org/#update-data-from-attributes>).
    pub(crate) fn create_processing_instruction_with_attributes(
        &mut self,
        target: &str,
        data: &str,
        attributes: Vec<(String, String)>,
    ) -> blitz_traits::node_id::NodeId {
        self.base
            .mutate()
            .create_processing_instruction_node_with_attributes(target, data, attributes)
    }

    /// Creates a CDATA section whose data is `data`
    /// (<https://dom.spec.whatwg.org/#dom-document-createcdatasection>).
    pub(crate) fn create_cdata_section(&mut self, data: &str) -> blitz_traits::node_id::NodeId {
        self.base.mutate().create_cdata_section_node(data)
    }

    /// Creates a text node whose data is `data`
    /// (<https://dom.spec.whatwg.org/#create-a-text-node>).
    pub(crate) fn create_text(&mut self, data: &DomString) -> blitz_traits::node_id::NodeId {
        let id = self.base.mutate().create_text_node(&data.to_string_lossy());
        self.remember_character_data(id, data);
        id
    }

    /// Creates a comment whose data is `data`
    /// (<https://dom.spec.whatwg.org/#create-a-comment-node>).
    pub(crate) fn create_comment(&mut self, data: &DomString) -> blitz_traits::node_id::NodeId {
        let id = self
            .base
            .mutate()
            .create_comment_node(&data.to_string_lossy());
        self.remember_character_data(id, data);
        id
    }

    /// Keeps `data` when it contains an unpaired surrogate. The tree itself
    /// stores the lossy UTF-8 form.
    pub(crate) fn remember_character_data(
        &mut self,
        id: blitz_traits::node_id::NodeId,
        data: &DomString,
    ) {
        if data.has_unpaired_surrogate() {
            self.side.exact_text.insert(id, data.clone());
        } else {
            self.side.exact_text.remove(&id);
        }
    }

    /// Keeps an attribute value that contains an unpaired surrogate. The tree
    /// attribute is the form with those units dropped.
    pub(crate) fn remember_attribute(
        &mut self,
        id: blitz_traits::node_id::NodeId,
        name: &str,
        value: DomString,
    ) {
        let key = (id, name.to_owned());
        if value.has_unpaired_surrogate() {
            self.side.exact_attrs.insert(key, value);
        } else {
            self.side.exact_attrs.remove(&key);
        }
    }

    /// The exact attribute value when it contains an unpaired surrogate.
    pub(crate) fn attribute_units(
        &self,
        id: blitz_traits::node_id::NodeId,
        name: &str,
    ) -> Option<&DomString> {
        self.side.exact_attrs.get(&(id, name.to_owned()))
    }

    /// Records the inert document that owns a template's contents.
    pub(crate) fn set_template_owner(
        &mut self,
        template: blitz_traits::node_id::NodeId,
        document: u32,
        fragment: blitz_traits::node_id::NodeId,
    ) {
        self.side
            .template_owners
            .insert(template, (document, fragment));
    }

    /// The inert document and fragment for a template, when migrated.
    pub(crate) fn template_owner(
        &self,
        template: blitz_traits::node_id::NodeId,
    ) -> Option<(u32, blitz_traits::node_id::NodeId)> {
        self.side.template_owners.get(&template).copied()
    }

    /// Records the template element whose contents this inert document holds.
    pub(crate) fn set_template_host(
        &mut self,
        document: u32,
        template: blitz_traits::node_id::NodeId,
    ) {
        self.side.template_host = Some((document, template));
    }

    /// The `(document id, node)` of the template element whose contents this
    /// inert document holds.
    pub(crate) fn template_host(&self) -> Option<(u32, blitz_traits::node_id::NodeId)> {
        self.side.template_host
    }

    /// The `CharacterData` of `id` as UTF-16 code units
    /// (<https://dom.spec.whatwg.org/#characterdata>).
    ///
    /// Unpaired surrogates live in `exact_text`. The UTF-8 tree holds the
    /// lossy form for layout.
    pub(crate) fn character_data(&self, id: blitz_traits::node_id::NodeId) -> DomString {
        if let Some(exact) = self.side.exact_text.get(&id) {
            return exact.clone();
        }
        match self.base.get_node(id).map(|node| &node.data) {
            Some(NodeData::Text(text)) => DomString::from(text.content.clone()),
            Some(
                NodeData::Comment { contents }
                | NodeData::ProcessingInstruction { contents, .. }
                | NodeData::CDataSection { contents },
            ) => DomString::from(contents.clone()),
            _ => DomString::default(),
        }
    }

    /// Descendant text content, keeping unpaired surrogates
    /// (<https://dom.spec.whatwg.org/#concept-descendant-text-content>).
    pub(crate) fn descendant_dom_text(&self, id: blitz_traits::node_id::NodeId) -> DomString {
        let mut text = DomString::default();
        let mut stack = self
            .base
            .get_node(id)
            .map_or_default(|node| node.children.iter().rev().copied().collect::<Vec<_>>());
        while let Some(current) = stack.pop() {
            let Some(node) = self.base.get_node(current) else {
                continue;
            };
            match &node.data {
                NodeData::Text(_) | NodeData::CDataSection { .. } => {
                    text.push_dom(&self.character_data(current));
                }
                NodeData::Element(_) => {
                    stack.extend(node.children.iter().rev().copied());
                }
                _ => {}
            }
        }
        text
    }

    /// Whether `id` has already started as a script
    /// (<https://html.spec.whatwg.org/multipage/scripting.html#already-started>).
    pub(crate) fn script_already_started(&self, id: blitz_traits::node_id::NodeId) -> bool {
        self.side.started_scripts.contains(&id)
    }

    /// Sets the already-started flag. Returns whether this was the first set.
    pub(crate) fn mark_script_started(&mut self, id: blitz_traits::node_id::NodeId) -> bool {
        self.side.started_scripts.insert(id)
    }

    /// Copies the already-started flag of `from` onto `to`, then walks both
    /// subtrees in lockstep. Cloning sets the copy's flag from the original
    /// (<https://html.spec.whatwg.org/multipage/scripting.html#script-processing-model:cloning-steps>).
    pub(crate) fn copy_script_started(
        &mut self,
        from: blitz_traits::node_id::NodeId,
        to: blitz_traits::node_id::NodeId,
    ) {
        // Pure tax when no script ever started: skip the walk entirely.
        if self.side.started_scripts.is_empty() {
            return;
        }
        let mut stack = vec![(from, to)];
        while let Some((from, to)) = stack.pop() {
            if self.side.started_scripts.contains(&from) {
                self.side.started_scripts.insert(to);
            }
            stack.extend(crate::dom::adoption::lockstep_children(
                &self.base, from, &self.base, to,
            ));
        }
    }
}

/// Every document tree the renderer process holds, keyed by document id.
#[derive(Default)]
pub(crate) struct DocumentStore {
    documents: HashMap<u32, Parsed>,
    next_id: u32,
}

impl DocumentStore {
    pub(crate) fn values_mut(&mut self) -> impl Iterator<Item = &mut Parsed> {
        self.documents.values_mut()
    }

    /// Re-files every element reference after a cross-document adoption, and
    /// points it at the adopted copy of the node it stored
    /// (<https://html.spec.whatwg.org/multipage/common-dom-interfaces.html#reflecting-content-attributes-in-idl-attributes:element>).
    ///
    /// A stored reference belongs to the element's document, so an element
    /// that moves takes its entries with it; the stored node's tree id changes
    /// even though its identity does not.
    pub(crate) fn retarget_element_references(&mut self, adopted: &[(NodeId, NodeId)]) {
        let mut entries = Vec::new();
        for parsed in self.documents.values_mut() {
            let document = parsed.id;
            for (key, reference) in parsed.document.take_element_references() {
                entries.push((document, key, reference));
            }
        }
        for (document, (element, name), reference) in entries {
            let element = retarget_node(
                NodeId {
                    document,
                    node: element,
                },
                adopted,
            );
            let reference = reference.retargeted(adopted);
            if let Some(parsed) = self.documents.get_mut(&element.document) {
                parsed
                    .document
                    .insert_element_reference(element.node, name, reference);
            }
        }
    }

    /// Stores `parsed` and returns its document id, assigning one when the
    /// parsed document does not carry one yet. Take-and-reinsert cycles keep
    /// their id, so wrappers and observers never lose their document.
    pub(crate) fn insert(&mut self, mut parsed: Parsed) -> u32 {
        if parsed.id == 0 {
            self.next_id = self.next_id.wrapping_add(1);
            parsed.id = self.next_id;
        }
        let id = parsed.id;
        self.documents.insert(id, parsed);
        id
    }

    /// The tree with this document id.
    pub(crate) fn get(&self, id: u32) -> Option<&Parsed> {
        self.documents.get(&id)
    }

    /// Mutable access to the tree with this document id.
    pub(crate) fn get_mut(&mut self, id: u32) -> Option<&mut Parsed> {
        self.documents.get_mut(&id)
    }

    /// Removes and returns the tree with this document id.
    pub(crate) fn remove(&mut self, id: u32) -> Option<Parsed> {
        self.documents.remove(&id)
    }
}

/// One recorded tree mutation, in observer-delivery shape.
///
/// Binding and parser call sites push these onto the owning document's
/// journal; Blitz only tracks a coarse changed set, so the delivery payload
/// lives here.
#[derive(Clone, Debug)]
pub(crate) enum JournalEntry {
    ChildList {
        target: NodeId,
        added: Vec<NodeId>,
        removed: Vec<NodeId>,
        previous: Option<NodeId>,
        next: Option<NodeId>,
    },
    Attributes {
        target: NodeId,
        name: String,
        namespace: String,
        old_value: Option<String>,
    },
    CharacterData {
        target: NodeId,
        old_value: DomString,
    },
}
