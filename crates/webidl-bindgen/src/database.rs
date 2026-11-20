use std::collections::{BTreeMap, BTreeSet};

use weedle::attribute::{ExtendedAttribute, ExtendedAttributeList};
use weedle::interface::{
    AsyncIterableInterfaceMember, InterfaceMember, IterableInterfaceMember, Special,
    StringifierOrInheritOrStatic, StringifierOrStatic,
};
use weedle::mixin::MixinMember;
use weedle::{Definition, Definitions, Parse};

use crate::{Error, Source};

pub(crate) struct Database<'idl> {
    definitions: BTreeMap<&'idl str, Definition<'idl>>,
    partials: BTreeMap<&'idl str, Vec<Definition<'idl>>>,
    includes: BTreeMap<&'idl str, BTreeSet<&'idl str>>,
}

pub(crate) struct Interface<'idl> {
    pub(crate) name: &'idl str,
    pub(crate) attributes: Option<weedle::attribute::ExtendedAttributeList<'idl>>,
    pub(crate) parent: Option<&'idl str>,
    pub(crate) members: Vec<Member<'idl>>,
}

#[derive(Clone)]
pub(crate) struct Member<'idl> {
    pub(crate) declaration: InterfaceMember<'idl>,
    scopes: Vec<ExtendedAttributeList<'idl>>,
}

/// What a member's declaration and enclosing scopes say about its exposure.
///
/// `[SecureContext]` is one fact no matter where it is declared: on the member,
/// or on a partial interface or mixin the interface includes
/// (<https://webidl.spec.whatwg.org/#SecureContext>). An interface-level
/// `[SecureContext]` is not part of this: it gates the interface object and
/// prototype themselves, so its members are never installed outside a secure
/// context in the first place. Both emitters read this one value.
#[derive(Clone, Copy)]
pub(crate) struct Exposure {
    pub(crate) secure_context: bool,
}

impl Member<'_> {
    pub(crate) fn attributes(&self) -> Option<&ExtendedAttributeList<'_>> {
        match &self.declaration {
            InterfaceMember::Const(member) => member.attributes.as_ref(),
            InterfaceMember::Attribute(member) => member.attributes.as_ref(),
            InterfaceMember::Constructor(member) => member.attributes.as_ref(),
            InterfaceMember::Operation(member) => member.attributes.as_ref(),
            InterfaceMember::Iterable(IterableInterfaceMember::Single(member)) => {
                member.attributes.as_ref()
            }
            InterfaceMember::Iterable(IterableInterfaceMember::Double(member)) => {
                member.attributes.as_ref()
            }
            InterfaceMember::AsyncIterable(AsyncIterableInterfaceMember::Single(member)) => {
                member.attributes.as_ref()
            }
            InterfaceMember::AsyncIterable(AsyncIterableInterfaceMember::Double(member)) => {
                member.attributes.as_ref()
            }
            InterfaceMember::Maplike(member) => member.attributes.as_ref(),
            InterfaceMember::Setlike(member) => member.attributes.as_ref(),
            InterfaceMember::Stringifier(member) => member.attributes.as_ref(),
        }
    }

    /// The exposure of this member's own declaration and enclosing scopes.
    ///
    /// The interface's own `[SecureContext]` is deliberately absent: it gates
    /// the interface object and prototype, so no member of it is installed
    /// outside a secure context anyway.
    pub(crate) fn exposure(&self) -> Exposure {
        Exposure {
            secure_context: has_no_args(self.attributes(), "SecureContext")
                || self
                    .scopes
                    .iter()
                    .any(|scope| has_no_args(Some(scope), "SecureContext")),
        }
    }

    pub(crate) fn validate_scopes(&self) -> Result<(), Error> {
        // https://webidl.spec.whatwg.org/#SecureContext
        // https://webidl.spec.whatwg.org/#using-mixins-and-partials
        // `[SecureContext]` is a resolved exposure, not scope syntax. A
        // `[LegacyOverrideBuiltIns]` scope only changes how named properties
        // resolve against built-ins; on an interface without a named getter it
        // is unobservable. Any other scope attribute needs a lowering.
        if let Some(scope) = self.scopes.iter().find(|scope| {
            !scope.body.list.is_empty()
                && !scope.body.list.iter().all(|attribute| {
                    matches!(
                        attribute,
                        ExtendedAttribute::NoArgs(attribute)
                            if matches!(attribute.0.0, "LegacyOverrideBuiltIns" | "SecureContext")
                    )
                })
        }) {
            return Err(Error(format!(
                "resolved declaration attributes are not supported yet: {scope:?}"
            )));
        }
        Ok(())
    }
}

pub(crate) fn has_no_args(list: Option<&ExtendedAttributeList<'_>>, name: &str) -> bool {
    list.is_some_and(|list| {
        list.body.list.iter().any(|attribute| match attribute {
            ExtendedAttribute::NoArgs(item) => item.0.0 == name,
            _ => false,
        })
    })
}

pub(crate) fn exposed_in(list: Option<&ExtendedAttributeList<'_>>, target: &str) -> bool {
    list.is_some_and(|list| {
        list.body.list.iter().any(|attribute| match attribute {
            ExtendedAttribute::WildCard(item) if item.identifier.0 == "Exposed" => true,
            ExtendedAttribute::Ident(item) if item.lhs_identifier.0 == "Exposed" => {
                item.rhs.0 == target
            }
            ExtendedAttribute::IdentList(item) if item.identifier.0 == "Exposed" => {
                item.list.body.list.iter().any(|item| item.0 == target)
            }
            _ => false,
        })
    })
}

impl<'idl> Database<'idl> {
    pub(crate) fn parse(sources: &[Source<'idl>]) -> Result<Self, Error> {
        let mut result = Self {
            definitions: BTreeMap::new(),
            partials: BTreeMap::new(),
            includes: BTreeMap::new(),
        };
        // Partial definitions merge in encounter order, so the source order
        // would otherwise leak into the generated member order. Sort by the
        // caller-supplied name: two callers that hand over the same corpus in
        // a different order must compile to the same bytes.
        let mut ordered: Vec<&Source<'idl>> = sources.iter().collect();
        ordered.sort_by(|left, right| left.name.cmp(right.name));
        for source in ordered {
            let (remaining, definitions) = Definitions::parse(source.text)
                .map_err(|error| Error(format!("{}: invalid Web IDL: {error}", source.name)))?;
            if !remaining.trim().is_empty() {
                let line = source.text[..source.text.len() - remaining.len()]
                    .bytes()
                    .filter(|byte| *byte == b'\n')
                    .count()
                    + 1;
                let preview: String = remaining.chars().take(120).collect();
                return Err(Error(format!(
                    "{}:{line}: unsupported IDL syntax near {preview:?}",
                    source.name
                )));
            }
            for definition in definitions {
                match definition {
                    Definition::IncludesStatement(include) => {
                        if include.attributes.is_some() {
                            return Err(Error(
                                "includes statement attributes are not supported".into(),
                            ));
                        }
                        let interface = include.lhs_identifier.0;
                        let mixin = include.rhs_identifier.0;
                        if !result.includes.entry(interface).or_default().insert(mixin) {
                            return Err(Error(format!(
                                "duplicate includes: {interface} includes {mixin}"
                            )));
                        }
                    }
                    Definition::Implements(_) => {
                        return Err(Error(format!(
                            "{}: obsolete implements statement",
                            source.name
                        )));
                    }
                    Definition::PartialInterface(ref item) => {
                        result
                            .partials
                            .entry(item.identifier.0)
                            .or_default()
                            .push(definition);
                    }
                    Definition::PartialInterfaceMixin(ref item) => {
                        result
                            .partials
                            .entry(item.identifier.0)
                            .or_default()
                            .push(definition);
                    }
                    Definition::PartialDictionary(ref item) => {
                        result
                            .partials
                            .entry(item.identifier.0)
                            .or_default()
                            .push(definition);
                    }
                    Definition::PartialNamespace(ref item) => {
                        result
                            .partials
                            .entry(item.identifier.0)
                            .or_default()
                            .push(definition);
                    }
                    definition => {
                        let name = definition_name(&definition)?;
                        if result.definitions.insert(name, definition).is_some() {
                            return Err(Error(format!(
                                "{}: duplicate definition {name}",
                                source.name
                            )));
                        }
                    }
                }
            }
        }
        result.validate_structure()?;
        Ok(result)
    }

    fn validate_structure(&self) -> Result<(), Error> {
        // https://webidl.spec.whatwg.org/#using-mixins-and-partials
        // https://chromium.googlesource.com/chromium/src/+/main/third_party/blink/renderer/bindings/scripts/web_idl/idl_compiler.py
        for (name, partials) in &self.partials {
            let definition = self
                .definitions
                .get(name)
                .ok_or_else(|| Error(format!("partial {name} has no complete definition")))?;
            for partial in partials {
                if !matches!(
                    (definition, partial),
                    (Definition::Interface(_), Definition::PartialInterface(_))
                        | (
                            Definition::InterfaceMixin(_),
                            Definition::PartialInterfaceMixin(_)
                        )
                        | (Definition::Dictionary(_), Definition::PartialDictionary(_))
                        | (Definition::Namespace(_), Definition::PartialNamespace(_))
                ) {
                    return Err(Error(format!(
                        "partial {name} has a different declaration kind"
                    )));
                }
            }
        }
        for (interface, mixins) in &self.includes {
            if !matches!(
                self.definitions.get(interface),
                Some(Definition::Interface(_))
            ) {
                return Err(Error(format!(
                    "includes target {interface} is not an interface"
                )));
            }
            for mixin in mixins {
                if !matches!(
                    self.definitions.get(mixin),
                    Some(Definition::InterfaceMixin(_))
                ) {
                    return Err(Error(format!("included {mixin} is not an interface mixin")));
                }
            }
        }
        for (name, definition) in &self.definitions {
            if parent_name(definition).is_some() {
                let mut visited = BTreeSet::new();
                let mut current = *name;
                loop {
                    if !visited.insert(current) {
                        return Err(Error(format!("inheritance cycle at {current}")));
                    }
                    let item = self.definitions.get(current).ok_or_else(|| {
                        Error(format!("missing inheritance dependency {current}"))
                    })?;
                    if std::mem::discriminant(item) != std::mem::discriminant(definition) {
                        return Err(Error(format!(
                            "{name} inherits a different declaration kind: {current}"
                        )));
                    }
                    let Some(parent) = parent_name(item) else {
                        break;
                    };
                    current = parent;
                }
            }
        }
        Ok(())
    }

    pub(crate) fn interface(&self, name: &str) -> Result<Interface<'idl>, Error> {
        let Some(Definition::Interface(definition)) = self.definitions.get(name) else {
            return Err(Error(format!("no interface declaration for {name}")));
        };
        let mut members: Vec<_> = definition
            .members
            .body
            .iter()
            .cloned()
            .map(|declaration| Member {
                declaration,
                scopes: Vec::new(),
            })
            .collect();
        if let Some(partials) = self.partials.get(name) {
            for partial in partials {
                let Definition::PartialInterface(partial) = partial else {
                    return Err(Error(format!("invalid partial interface {name}")));
                };
                members.extend(
                    partial
                        .members
                        .body
                        .iter()
                        .cloned()
                        .map(|declaration| Member {
                            declaration,
                            scopes: partial.attributes.iter().cloned().collect(),
                        }),
                );
            }
        }
        if let Some(mixins) = self.includes.get(name) {
            for mixin in mixins {
                let Some(Definition::InterfaceMixin(definition)) = self.definitions.get(mixin)
                else {
                    return Err(Error(format!("invalid mixin {mixin}")));
                };
                members.extend(definition.members.body.iter().map(|member| Member {
                    declaration: mixin_member(member),
                    scopes: definition.attributes.iter().cloned().collect(),
                }));
                if let Some(partials) = self.partials.get(mixin) {
                    for partial in partials {
                        let Definition::PartialInterfaceMixin(partial) = partial else {
                            return Err(Error(format!("invalid partial mixin {mixin}")));
                        };
                        members.extend(partial.members.body.iter().map(|member| {
                            Member {
                                declaration: mixin_member(member),
                                scopes: definition
                                    .attributes
                                    .iter()
                                    .chain(partial.attributes.iter())
                                    .cloned()
                                    .collect(),
                            }
                        }));
                    }
                }
            }
        }
        Ok(Interface {
            name: definition.identifier.0,
            attributes: definition.attributes.clone(),
            parent: definition
                .inheritance
                .as_ref()
                .map(|item| item.identifier.0),
            members,
        })
    }

    pub(crate) fn definition(&self, name: &str) -> Option<&Definition<'idl>> {
        self.definitions.get(name)
    }

    /// Resolve a mixin and the interfaces that include it. Mixins have no
    /// interface object; the generated installer targets each includer.
    /// Member scopes carry the mixin and partial-mixin declaration
    /// attributes, following partial interfaces
    /// (<https://webidl.spec.whatwg.org/#using-mixins-and-partials>).
    /// Chromium resolves the same includes statements in
    /// `third_party/blink/renderer/bindings/scripts/web_idl/idl_compiler.py`;
    /// Firefox reads them in `dom/bindings/parser/WebIDL.py`.
    pub(crate) fn mixin(&self, name: &str) -> Result<Mixin<'idl>, Error> {
        let Some(Definition::InterfaceMixin(definition)) = self.definitions.get(name) else {
            return Err(Error(format!("no mixin declaration for {name}")));
        };
        let mut members: Vec<Member<'idl>> = definition
            .members
            .body
            .iter()
            .map(|member| Member {
                declaration: mixin_member(member),
                scopes: definition.attributes.iter().cloned().collect(),
            })
            .collect();
        if let Some(partials) = self.partials.get(name) {
            for partial in partials {
                let Definition::PartialInterfaceMixin(partial) = partial else {
                    return Err(Error(format!("invalid partial mixin {name}")));
                };
                members.extend(partial.members.body.iter().map(|member| {
                    Member {
                        declaration: mixin_member(member),
                        scopes: definition
                            .attributes
                            .iter()
                            .chain(partial.attributes.iter())
                            .cloned()
                            .collect(),
                    }
                }));
            }
        }
        Ok(Mixin {
            name: definition.identifier.0,
            members,
        })
    }

    /// Interfaces that include the mixin, in IDL source order.
    pub(crate) fn includers(&self, mixin: &str) -> Vec<String> {
        self.includes
            .iter()
            .filter(|(_, mixins)| mixins.contains(mixin))
            .map(|(interface, _)| interface.to_string())
            .collect()
    }

    /// Indexed and named getter operations inherited through the parent
    /// chain, for hook support. An interface whose hook drivers come from a
    /// parent lowers its own copies, derived here instead of repeated by
    /// hand; anything else stays on the parent's prototype and dispatches
    /// through the shared payload arms.
    pub(crate) fn hook_members(&self, name: &str) -> Result<Vec<Member<'idl>>, Error> {
        let mut members = Vec::new();
        let mut seen = BTreeSet::new();
        let Ok(interface) = self.interface(name) else {
            return Ok(members);
        };
        let mut next = interface.parent;
        while let Some(parent) = next {
            if !seen.insert(parent) {
                return Err(Error(format!("inheritance cycle at {parent}")));
            }
            let Ok(declaration) = self.interface(parent) else {
                // Parents can be mixins, which terminate the chain.
                self.mixin(parent)?;
                return Ok(members);
            };
            for member in &declaration.members {
                if let InterfaceMember::Operation(operation) = &member.declaration
                    && matches!(operation.special, Some(Special::Getter(_)))
                {
                    members.push(member.clone());
                }
            }
            next = declaration.parent;
        }
        Ok(members)
    }

    /// Whether `name` is `Node` or inherits from it, walking the declared
    /// inheritance chain.
    pub(crate) fn is_node_interface(&self, name: &str) -> bool {
        let mut current = name;
        let mut seen = BTreeSet::new();
        while seen.insert(current) {
            if current == "Node" {
                return true;
            }
            let Some(Definition::Interface(definition)) = self.definitions.get(current) else {
                return false;
            };
            let Some(parent) = definition
                .inheritance
                .as_ref()
                .map(|parent| parent.identifier.0)
            else {
                return false;
            };
            current = parent;
        }
        false
    }

    pub(crate) fn dictionary(
        &self,
        name: &str,
    ) -> Result<weedle::DictionaryDefinition<'idl>, Error> {
        let Some(Definition::Dictionary(definition)) = self.definitions.get(name) else {
            return Err(Error(format!("no dictionary declaration for {name}")));
        };
        let mut definition = definition.clone();
        // https://webidl.spec.whatwg.org/#dfn-partial-dictionary
        if let Some(partials) = self.partials.get(name) {
            for partial in partials {
                let Definition::PartialDictionary(partial) = partial else {
                    return Err(Error(format!("invalid partial dictionary {name}")));
                };
                if partial.attributes.is_some() {
                    return Err(Error(format!(
                        "partial dictionary attributes are not supported yet: {name}"
                    )));
                }
                definition
                    .members
                    .body
                    .extend(partial.members.body.iter().cloned());
            }
        }
        Ok(definition)
    }
}

pub(crate) struct Mixin<'idl> {
    pub(crate) name: &'idl str,
    pub(crate) members: Vec<Member<'idl>>,
}

fn definition_name<'idl>(definition: &Definition<'idl>) -> Result<&'idl str, Error> {
    match definition {
        Definition::Callback(item) => Ok(item.identifier.0),
        Definition::CallbackInterface(item) => Ok(item.identifier.0),
        Definition::Interface(item) => Ok(item.identifier.0),
        Definition::InterfaceMixin(item) => Ok(item.identifier.0),
        Definition::Namespace(item) => Ok(item.identifier.0),
        Definition::Dictionary(item) => Ok(item.identifier.0),
        Definition::Enum(item) => Ok(item.identifier.0),
        Definition::Typedef(item) => Ok(item.identifier.0),
        Definition::PartialInterface(_)
        | Definition::PartialInterfaceMixin(_)
        | Definition::PartialDictionary(_)
        | Definition::PartialNamespace(_)
        | Definition::IncludesStatement(_)
        | Definition::Implements(_) => Err(Error("expected a complete named definition".into())),
    }
}

fn parent_name<'idl>(definition: &Definition<'idl>) -> Option<&'idl str> {
    match definition {
        Definition::Interface(item) => item.inheritance.as_ref(),
        Definition::CallbackInterface(item) => item.inheritance.as_ref(),
        Definition::Dictionary(item) => item.inheritance.as_ref(),
        _ => None,
    }
    .map(|item| item.identifier.0)
}

fn mixin_member<'idl>(member: &MixinMember<'idl>) -> InterfaceMember<'idl> {
    match member {
        MixinMember::Const(item) => InterfaceMember::Const(item.clone()),
        MixinMember::Stringifier(item) => InterfaceMember::Stringifier(item.clone()),
        MixinMember::Attribute(item) => {
            InterfaceMember::Attribute(weedle::interface::AttributeInterfaceMember {
                attributes: item.attributes.clone(),
                modifier: item
                    .stringifier
                    .map(StringifierOrInheritOrStatic::Stringifier),
                readonly: item.readonly,
                attribute: item.attribute,
                type_: item.type_.clone(),
                identifier: item.identifier,
                semi_colon: item.semi_colon,
            })
        }
        MixinMember::Operation(item) => {
            InterfaceMember::Operation(weedle::interface::OperationInterfaceMember {
                attributes: item.attributes.clone(),
                modifier: item.stringifier.map(StringifierOrStatic::Stringifier),
                special: None,
                return_type: item.return_type.clone(),
                identifier: item.identifier,
                args: item.args.clone(),
                semi_colon: item.semi_colon,
            })
        }
    }
}
