use std::collections::{HashMap, HashSet};

use crate::Parsed;
use crate::dom::tree::NodeId;
use crate::dom_string::DomString;

use blitz_dom::NodeData;

pub struct BlitzDocument {
    pub base: blitz_dom::BaseDocument,
    pub side: SideTables,
}

#[derive(Default)]
pub struct SideTables {
    pub journal: Vec<(u64, JournalEntry)>,
    next_journal_position: u64,
    recording: bool,
    // https://html.spec.whatwg.org/multipage/scripting.html#already-started
    started_scripts: HashSet<blitz_traits::node_id::NodeId>,
    // https://html.spec.whatwg.org/multipage/form-elements.html#selectedness-setting-algorithm
    selectedness_normalized: HashSet<blitz_traits::node_id::NodeId>,
    pub ranges: crate::dom::range::RangeTable,
    // https://infra.spec.whatwg.org/#javascript-string-convert
    exact_text: HashMap<blitz_traits::node_id::NodeId, DomString>,
    exact_attrs: HashMap<(blitz_traits::node_id::NodeId, String), DomString>,
    // https://html.spec.whatwg.org/multipage/scripting.html#template-contents
    template_owners: HashMap<blitz_traits::node_id::NodeId, (u32, blitz_traits::node_id::NodeId)>,
    // https://w3c.github.io/aria/#enumerated-attribute-values-html
    enumerated_nulls: HashSet<(blitz_traits::node_id::NodeId, String)>,
    // https://html.spec.whatwg.org/multipage/common-dom-interfaces.html#reflecting-content-attributes-in-idl-attributes:element
    element_references: HashMap<(blitz_traits::node_id::NodeId, String), ElementReference>,
    // https://html.spec.whatwg.org/multipage/scripting.html#template-contents
    template_host: Option<(u32, blitz_traits::node_id::NodeId)>,
}

// https://html.spec.whatwg.org/multipage/common-dom-interfaces.html#reflecting-content-attributes-in-idl-attributes:element
pub enum ElementReference {
    Element(NodeId),
    Elements(Vec<NodeId>),
}

impl ElementReference {
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

fn retarget_node(node: NodeId, adopted: &[(NodeId, NodeId)]) -> NodeId {
    adopted
        .iter()
        .find(|(from, _)| *from == node)
        .map_or(node, |(_, to)| *to)
}

impl BlitzDocument {
    #[must_use]
    pub fn new(config: blitz_dom::DocumentConfig) -> Self {
        Self::from_base(blitz_dom::BaseDocument::new(config))
    }

    pub fn from_base(base: blitz_dom::BaseDocument) -> Self {
        Self {
            base,
            side: SideTables::default(),
        }
    }

    pub fn selectedness_normalized(&self, select: blitz_traits::node_id::NodeId) -> bool {
        self.side.selectedness_normalized.contains(&select)
    }

    pub fn mark_selectedness_normalized(&mut self, select: blitz_traits::node_id::NodeId) {
        self.side.selectedness_normalized.insert(select);
    }

    pub fn invalidate_selectedness(&mut self, select: blitz_traits::node_id::NodeId) {
        self.side.selectedness_normalized.remove(&select);
    }

    pub fn record(&mut self, entry: JournalEntry) {
        if self.side.recording {
            let position = self.side.next_journal_position;
            self.side.next_journal_position = self.side.next_journal_position.wrapping_add(1);
            self.side.journal.push((position, entry));
        }
    }

    pub fn set_recording(&mut self, recording: bool) {
        self.side.recording = recording;
    }

    pub fn mark_enumerated_null(&mut self, id: blitz_traits::node_id::NodeId, name: &str) {
        self.side.enumerated_nulls.insert((id, name.to_owned()));
    }

    pub fn clear_enumerated_null(&mut self, id: blitz_traits::node_id::NodeId, name: &str) {
        self.side.enumerated_nulls.remove(&(id, name.to_owned()));
    }

    pub fn is_enumerated_null(&self, id: blitz_traits::node_id::NodeId, name: &str) -> bool {
        self.side.enumerated_nulls.contains(&(id, name.to_owned()))
    }

    pub fn set_element_reference(
        &mut self,
        id: blitz_traits::node_id::NodeId,
        name: &str,
        element: NodeId,
    ) {
        self.side
            .element_references
            .insert((id, name.to_owned()), ElementReference::Element(element));
    }

    pub fn set_element_reference_list(
        &mut self,
        id: blitz_traits::node_id::NodeId,
        name: &str,
        elements: Vec<NodeId>,
    ) {
        self.side
            .element_references
            .insert((id, name.to_owned()), ElementReference::Elements(elements));
    }

    pub fn element_reference(
        &self,
        id: blitz_traits::node_id::NodeId,
        name: &str,
    ) -> Option<&ElementReference> {
        self.side.element_references.get(&(id, name.to_owned()))
    }

    // https://html.spec.whatwg.org/multipage/common-dom-interfaces.html#reflecting-content-attributes-in-idl-attributes:element
    pub fn clear_element_reference(&mut self, id: blitz_traits::node_id::NodeId, name: &str) {
        self.side.element_references.remove(&(id, name.to_owned()));
    }

    pub fn take_element_references(
        &mut self,
    ) -> Vec<((blitz_traits::node_id::NodeId, String), ElementReference)> {
        std::mem::take(&mut self.side.element_references)
            .into_iter()
            .collect()
    }

    pub fn insert_element_reference(
        &mut self,
        element: blitz_traits::node_id::NodeId,
        name: String,
        reference: ElementReference,
    ) {
        self.side
            .element_references
            .insert((element, name), reference);
    }

    // https://dom.spec.whatwg.org/#interface-documentfragment
    // https://dom.spec.whatwg.org/#interface-shadowroot
    pub fn is_fragment(&self, id: blitz_traits::node_id::NodeId) -> bool {
        self.base.get_node(id).is_some_and(|node| {
            matches!(
                node.data,
                NodeData::Fragment { .. } | NodeData::ShadowRoot { .. }
            )
        })
    }

    // https://dom.spec.whatwg.org/#dom-document-createdocumentfragment
    pub fn create_fragment(&mut self) -> blitz_traits::node_id::NodeId {
        self.base.mutate().create_fragment_node()
    }

    // https://dom.spec.whatwg.org/#dom-domimplementation-createdocumenttype
    pub fn create_doctype(
        &mut self,
        name: &str,
        public_id: &str,
        system_id: &str,
    ) -> blitz_traits::node_id::NodeId {
        self.base
            .mutate()
            .create_doctype_node(name, public_id, system_id)
    }

    // https://dom.spec.whatwg.org/#create-a-processing-instruction-node
    pub fn create_processing_instruction(
        &mut self,
        target: &str,
        data: &str,
    ) -> blitz_traits::node_id::NodeId {
        self.base
            .mutate()
            .create_processing_instruction_node(target, data)
    }

    // https://dom.spec.whatwg.org/#update-data-from-attributes
    pub fn create_processing_instruction_with_attributes(
        &mut self,
        target: &str,
        data: &str,
        attributes: Vec<(String, String)>,
    ) -> blitz_traits::node_id::NodeId {
        self.base
            .mutate()
            .create_processing_instruction_node_with_attributes(target, data, attributes)
    }

    // https://dom.spec.whatwg.org/#dom-document-createcdatasection
    pub fn create_cdata_section(&mut self, data: &str) -> blitz_traits::node_id::NodeId {
        self.base.mutate().create_cdata_section_node(data)
    }

    // https://dom.spec.whatwg.org/#create-a-text-node
    pub fn create_text(&mut self, data: &DomString) -> blitz_traits::node_id::NodeId {
        let id = self.base.mutate().create_text_node(&data.to_string_lossy());
        self.remember_character_data(id, data);
        id
    }

    // https://dom.spec.whatwg.org/#create-a-comment-node
    pub fn create_comment(&mut self, data: &DomString) -> blitz_traits::node_id::NodeId {
        let id = self
            .base
            .mutate()
            .create_comment_node(&data.to_string_lossy());
        self.remember_character_data(id, data);
        id
    }

    pub fn remember_character_data(&mut self, id: blitz_traits::node_id::NodeId, data: &DomString) {
        if data.has_unpaired_surrogate() {
            self.side.exact_text.insert(id, data.clone());
        } else {
            self.side.exact_text.remove(&id);
        }
    }

    pub fn remember_attribute(
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

    pub fn attribute_units(
        &self,
        id: blitz_traits::node_id::NodeId,
        name: &str,
    ) -> Option<&DomString> {
        self.side.exact_attrs.get(&(id, name.to_owned()))
    }

    pub fn set_template_owner(
        &mut self,
        template: blitz_traits::node_id::NodeId,
        document: u32,
        fragment: blitz_traits::node_id::NodeId,
    ) {
        self.side
            .template_owners
            .insert(template, (document, fragment));
    }

    pub fn template_owner(
        &self,
        template: blitz_traits::node_id::NodeId,
    ) -> Option<(u32, blitz_traits::node_id::NodeId)> {
        self.side.template_owners.get(&template).copied()
    }

    pub fn set_template_host(&mut self, document: u32, template: blitz_traits::node_id::NodeId) {
        self.side.template_host = Some((document, template));
    }

    pub fn template_host(&self) -> Option<(u32, blitz_traits::node_id::NodeId)> {
        self.side.template_host
    }

    // https://dom.spec.whatwg.org/#characterdata
    pub fn character_data(&self, id: blitz_traits::node_id::NodeId) -> DomString {
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

    // https://dom.spec.whatwg.org/#concept-descendant-text-content
    pub fn descendant_dom_text(&self, id: blitz_traits::node_id::NodeId) -> DomString {
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

    // https://html.spec.whatwg.org/multipage/scripting.html#already-started
    pub fn script_already_started(&self, id: blitz_traits::node_id::NodeId) -> bool {
        self.side.started_scripts.contains(&id)
    }

    pub fn mark_script_started(&mut self, id: blitz_traits::node_id::NodeId) -> bool {
        self.side.started_scripts.insert(id)
    }

    // https://html.spec.whatwg.org/multipage/dynamic-markup-insertion.html#dom-document-parsehtmlunsafe
    pub fn mark_all_scripts_started(&mut self) {
        let mut stack = vec![self.base.root_node().id];
        let mut scripts = Vec::new();
        while let Some(id) = stack.pop() {
            let Some(node) = self.base.get_node(id) else {
                continue;
            };
            if node
                .element_data()
                .is_some_and(|el| el.name.local == markup5ever::local_name!("script"))
            {
                scripts.push(id);
            }
            stack.extend(node.children.iter().copied());
        }
        self.side.started_scripts.extend(scripts);
    }

    // https://html.spec.whatwg.org/multipage/scripting.html#script-processing-model:cloning-steps
    pub fn copy_script_started(
        &mut self,
        from: blitz_traits::node_id::NodeId,
        to: blitz_traits::node_id::NodeId,
    ) {
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

#[derive(Default)]
pub struct DocumentStore {
    documents: HashMap<u32, Parsed>,
    next_id: u32,
}

impl DocumentStore {
    pub fn values_mut(&mut self) -> impl Iterator<Item = &mut Parsed> {
        self.documents.values_mut()
    }

    // https://html.spec.whatwg.org/multipage/common-dom-interfaces.html#reflecting-content-attributes-in-idl-attributes:element
    pub fn retarget_element_references(&mut self, adopted: &[(NodeId, NodeId)]) {
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

    pub fn insert(&mut self, mut parsed: Parsed) -> u32 {
        if parsed.id == 0 {
            self.next_id = self.next_id.wrapping_add(1);
            parsed.id = self.next_id;
        }
        let id = parsed.id;
        self.documents.insert(id, parsed);
        id
    }

    #[must_use]
    pub fn get(&self, id: u32) -> Option<&Parsed> {
        self.documents.get(&id)
    }

    pub fn get_mut(&mut self, id: u32) -> Option<&mut Parsed> {
        self.documents.get_mut(&id)
    }

    pub fn remove(&mut self, id: u32) -> Option<Parsed> {
        self.documents.remove(&id)
    }

    #[must_use]
    pub fn contains_node(&self, id: NodeId) -> bool {
        self.documents
            .get(&id.document)
            .is_some_and(|parsed| parsed.document.base.get_node(id.node).is_some())
    }
}

#[derive(Clone, Debug)]
pub enum JournalEntry {
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
