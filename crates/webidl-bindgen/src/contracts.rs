use std::collections::{BTreeMap, BTreeSet, HashSet};

use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use weedle::argument::Argument;
use weedle::attribute::{ExtendedAttribute, ExtendedAttributeList};
use weedle::interface::{InterfaceMember, Special};
use weedle::literal::DefaultValue;
use weedle::types::{IntegerType, NonAnyType, SingleType, Type, UnionMemberType};

use crate::database::{Database, Exposure, has_no_args};
use crate::model::{
    self, ArgumentArity, ConstructorArgumentKind, GetterMapping, InterfaceKind, OperationResult,
    PropertyGetter, PropertyHooks, PrototypeParent, ReturnType,
};
use crate::names::snake_case;
use crate::{Binding, Error, Source};

/// One discovered implementation method: its parameter count after `self` and
/// `ctx`, and whether the first such parameter is the JS receiver object.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Method {
    has_self: bool,
    parameters: usize,
    receiver: bool,
}

impl Method {
    fn parse(member: &syn::ImplItemFn, source: &str) -> Result<Self, Error> {
        let inputs = &member.sig.inputs;
        let (ctx_index, has_self) = match inputs.first() {
            Some(syn::FnArg::Receiver(_)) => (1, true),
            Some(syn::FnArg::Typed(_)) => (0, false),
            None => {
                return Err(Error(format!(
                    "{source}: binding methods take a Ctx parameter first"
                )));
            }
        };
        let Some(syn::FnArg::Typed(ctx)) = inputs.get(ctx_index) else {
            return Err(Error(format!(
                "{source}: binding methods take a Ctx parameter first"
            )));
        };
        let is_ctx = matches!(&*ctx.ty, syn::Type::Reference(reference) if is_ctx_type(&reference.elem))
            || is_ctx_type(&ctx.ty);
        if !is_ctx {
            return Err(Error(format!(
                "{source}: binding methods take a Ctx parameter first"
            )));
        }
        let rest: Vec<_> = inputs.iter().skip(ctx_index + 1).collect();
        let receiver = rest.first().is_some_and(|argument| {
            matches!(argument, syn::FnArg::Typed(argument) if is_object_type(&argument.ty))
        });
        Ok(Self {
            has_self,
            parameters: rest.len(),
            receiver,
        })
    }
}

fn is_ctx_type(type_: &syn::Type) -> bool {
    matches!(type_, syn::Type::Path(path) if path.path.segments.last().is_some_and(|segment| segment.ident == "Ctx"))
}

fn is_object_type(type_: &syn::Type) -> bool {
    match type_ {
        syn::Type::Path(path) => path
            .path
            .segments
            .last()
            .is_some_and(|segment| segment.ident == "Object"),
        // `Object<'js>` inside a reference is not a receiver.
        _ => false,
    }
}

struct Implementation {
    interface: String,
    payload: syn::Ident,
    lifetime: bool,
    /// Additional platform types implementing the same interface, discovered
    /// in source order after the primary payload.
    payloads: Vec<model::Payload>,
    methods: BTreeMap<String, Method>,
}

impl Implementation {
    /// Fold a second implementation of the same interface into this one. Every
    /// payload must provide the same method signatures, so the merged method
    /// set is what the generated trait and every `impl` share.
    fn absorb(&mut self, other: Self, source: &str) -> Result<(), Error> {
        if self.payload == other.payload {
            return Err(Error(format!(
                "{source}: {} is implemented twice for {}",
                other.interface, other.payload
            )));
        }
        for (name, method) in other.methods {
            match self.methods.get(&name) {
                Some(existing) if *existing != method => {
                    return Err(Error(format!(
                        "{source}: {name} has a different signature on each {} payload",
                        other.interface
                    )));
                }
                Some(_) => {}
                None => {
                    self.methods.insert(name, method);
                }
            }
        }
        self.payloads.push(model::Payload {
            rust: other.payload,
            has_lifetime: other.lifetime,
        });
        Ok(())
    }

    /// Choose the payload that owns the interface's prototype. A declared
    /// class named for the interface already provides it (brands.js copies
    /// the native prototype when defining the global), so it is elected
    /// however discovery ordered the implementations. Otherwise the single
    /// class-less payload dedicated to this interface gets a generated
    /// class; several dedicated class-less payloads with no name match is
    /// ambiguous and fails the build.
    fn elect(
        &mut self,
        classes: &BTreeMap<String, String>,
        usage: &BTreeMap<String, usize>,
        source: &str,
    ) -> Result<(), Error> {
        let named = |payload: &syn::Ident| {
            classes
                .get(&payload.to_string())
                .is_some_and(|name| name == &self.interface)
        };
        let dedicated = |payload: &syn::Ident| {
            !classes.contains_key(&payload.to_string())
                && usage
                    .get(&payload.to_string())
                    .is_some_and(|used| *used == 1)
        };
        if let Some(found) = std::iter::once(&self.payload)
            .chain(self.payloads.iter().map(|payload| &payload.rust))
            .position(named)
        {
            if found > 0 {
                let elected = self.payloads.remove(found - 1);
                let previous = std::mem::replace(&mut self.payload, elected.rust);
                let previous_lifetime = std::mem::replace(&mut self.lifetime, elected.has_lifetime);
                self.payloads.push(model::Payload {
                    rust: previous,
                    has_lifetime: previous_lifetime,
                });
            }
            return Ok(());
        }
        let mut exclusive = std::iter::once(&self.payload)
            .chain(self.payloads.iter().map(|payload| &payload.rust))
            .filter(|payload| dedicated(payload));
        let Some(first) = exclusive.next() else {
            return Ok(());
        };
        if exclusive.next().is_some() {
            return Err(Error(format!(
                "{source}: {} has no prototype owner: several class-less payloads and none is named for the interface",
                self.interface
            )));
        }
        if first != &self.payload {
            let found = std::iter::once(&self.payload)
                .chain(self.payloads.iter().map(|payload| &payload.rust))
                .position(|payload| payload == first)
                .expect("dedicated payload is listed");
            let elected = self.payloads.remove(found - 1);
            let previous = std::mem::replace(&mut self.payload, elected.rust);
            let previous_lifetime = std::mem::replace(&mut self.lifetime, elected.has_lifetime);
            self.payloads.push(model::Payload {
                rust: previous,
                has_lifetime: previous_lifetime,
            });
        }
        Ok(())
    }
}

/// Parse the reflected-enumerated metadata table.
pub(crate) fn parse_reflected_enumerated(
    source: &Source<'_>,
) -> Result<Vec<crate::ReflectedEnum>, Error> {
    let mut entries = Vec::new();
    let mut seen: BTreeSet<(String, String)> = BTreeSet::new();
    for (index, line) in source.text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let columns: Vec<&str> = line.split_whitespace().collect();
        let [interface, attribute, type_path, content, missing, invalid] = columns.as_slice()
        else {
            return Err(Error(format!(
                "{}:{}: expected `interface attribute type content missing invalid`",
                source.name,
                index + 1
            )));
        };
        let path = |raw: &str, role: &str| -> Result<Option<syn::Ident>, Error> {
            if raw == "-" {
                return Ok(None);
            }
            syn::parse_str::<syn::Ident>(raw)
                .map(Some)
                .map_err(|error| {
                    Error(format!(
                        "{}:{}: {role} must be one variant name, not `{raw}`: {error}",
                        source.name,
                        index + 1
                    ))
                })
        };
        if !seen.insert(((*interface).to_owned(), (*attribute).to_owned())) {
            return Err(Error(format!(
                "{}:{}: {interface}.{attribute} is listed twice",
                source.name,
                index + 1
            )));
        }
        // Validate the type path the same way; the variant paths stay strings
        // here and are joined with it by the lowering.
        syn::parse_str::<syn::Path>(type_path).map_err(|error| {
            Error(format!(
                "{}:{}: invalid type `{type_path}`: {error}",
                source.name,
                index + 1
            ))
        })?;
        let missing = path(missing, "missing-value default")?;
        let invalid = path(invalid, "invalid-value default")?;
        entries.push(crate::ReflectedEnum {
            interface: (*interface).to_owned(),
            attribute: (*attribute).to_owned(),
            content: (*content).to_owned(),
            type_path: (*type_path).to_owned(),
            missing: missing.map(|path| quote!(#path).to_string()),
            invalid: invalid.map(|path| quote!(#path).to_string()),
        });
    }
    Ok(entries)
}

pub(crate) fn compile(
    idl: &[Source<'_>],
    rust: &[Source<'_>],
    reflected: &[crate::ReflectedEnum],
) -> Result<Vec<Binding>, Error> {
    let database = Database::parse(idl)?;
    let mut implementations = BTreeMap::new();
    let mut classes = BTreeMap::new();
    let mut declared = BTreeSet::new();
    for source in rust {
        let syntax = syn::parse_file(source.text)
            .map_err(|error| Error(format!("{}: invalid Rust: {error}", source.name)))?;
        discover(
            &syntax.items,
            source.name,
            &mut implementations,
            &mut classes,
        )?;
        declared_modules(&syntax.items, &mut declared);
    }
    // An interface whose members are all generator-supplied has no hand-written
    // `impl` block, because there is no trait for it to implement. Its generated
    // module is still installed, and the `include!` that installs it is the
    // declaration that the module must exist. Its payload is the nearest
    // implemented ancestor's: a platform type that implements an interface also
    // implements everything that interface inherits from, which is why the same
    // payload is the only sound choice.
    for name in declared {
        if implementations.contains_key(&name) {
            continue;
        }
        let Some((payload, lifetime)) = inherited_payload(&database, &name, &implementations)?
        else {
            return Err(Error(format!(
                "{name} needs a generated module but has no implemented ancestor to take a payload from"
            )));
        };
        implementations.insert(
            name.clone(),
            Implementation {
                interface: name,
                payload,
                lifetime,
                payloads: Vec::new(),
                methods: BTreeMap::new(),
            },
        );
    }
    let mut usage: BTreeMap<String, usize> = BTreeMap::new();
    for implementation in implementations.values() {
        for payload in std::iter::once(&implementation.payload)
            .chain(implementation.payloads.iter().map(|payload| &payload.rust))
        {
            *usage.entry(payload.to_string()).or_default() += 1;
        }
    }
    let implemented: BTreeSet<String> = implementations.keys().cloned().collect();
    implementations
        .into_values()
        .map(|mut implementation| {
            implementation.elect(&classes, &usage, "(discovered)")?;
            let kind = if classes.contains_key(&implementation.payload.to_string()) {
                InterfaceKind::Partial
            } else {
                InterfaceKind::Complete
            };
            let interface = lower(&database, &implementation, kind, &implemented, reflected)
                .map_err(|error| Error(format!("{}: {error}", implementation.interface)))?;
            let generated = crate::emit::interface(&interface);
            let syntax = syn::parse2(generated).map_err(|error| {
                Error(format!(
                    "invalid generated contract for {}: {error}",
                    implementation.interface
                ))
            })?;
            Ok(Binding {
                interface: implementation.interface,
                rust: prettyplease::unparse(&syntax),
            })
        })
        .collect()
}

/// Collects the interface names whose generated module a source `include!`s.
///
/// The include is what actually needs the file: a missing module is a compile
/// error there, so this set is the complete list of modules the build must
/// write, including the interfaces that have no hand-written implementation.
fn declared_modules(items: &[syn::Item], declared: &mut BTreeSet<String>) {
    for item in items {
        match item {
            syn::Item::Mod(module) => {
                if let Some((_, items)) = &module.content {
                    declared_modules(items, declared);
                }
            }
            syn::Item::Macro(item) => {
                if let Some(name) = included_module(&item.mac) {
                    declared.insert(name);
                }
            }
            _ => {}
        }
    }
}

/// The file stem of the one `*.rs` string in an `include!` invocation, if this
/// macro is one.
fn included_module(mac: &syn::Macro) -> Option<String> {
    if !mac.path.is_ident("include") {
        return None;
    }
    // The file name is nested inside `concat!(env!("OUT_DIR"), "/Name.rs")`,
    // so the literal search has to walk groups rather than only the top level.
    let literal = module_path(&mac.tokens)?;
    std::path::Path::new(&literal.value())
        .file_stem()
        .and_then(|stem| stem.to_str())
        .map(str::to_owned)
}

/// The one string literal ending in `.rs` anywhere in a token stream.
fn module_path(tokens: &proc_macro2::TokenStream) -> Option<syn::LitStr> {
    for token in tokens.clone() {
        match token {
            proc_macro2::TokenTree::Literal(literal) => {
                let Ok(literal) = syn::parse_str::<syn::LitStr>(&literal.to_string()) else {
                    continue;
                };
                let value = literal.value();
                let path = std::path::Path::new(&value);
                if path.extension().is_some_and(|extension| extension == "rs") {
                    return Some(literal);
                }
            }
            proc_macro2::TokenTree::Group(group) => {
                if let Some(literal) = module_path(&group.stream()) {
                    return Some(literal);
                }
            }
            proc_macro2::TokenTree::Ident(_) | proc_macro2::TokenTree::Punct(_) => {}
        }
    }
    None
}

/// The payload and lifetime of the nearest implemented ancestor of `name`.
///
/// Walking `interface.inheritance` is what makes an interface with no
/// implementation of its own bindable: the engine implements the ancestor, and
/// every platform type implements a child interface the same way because the
/// child interface's members are a superset reached through the same object.
///
/// A mixin with no implementation of its own is bindable the same way: its
/// members install on every implemented interface that includes it, so the
/// payload is the first such includer's. That happens when every mixin member
/// is generator-supplied and there is no trait to implement.
fn inherited_payload(
    database: &Database<'_>,
    name: &str,
    implementations: &BTreeMap<String, Implementation>,
) -> Result<Option<(syn::Ident, bool)>, Error> {
    if database.mixin(name).is_ok() {
        for includer in database.includers(name) {
            if let Some(implementation) = implementations.get(&includer) {
                return Ok(Some((
                    implementation.payload.clone(),
                    implementation.lifetime,
                )));
            }
        }
        return Ok(None);
    }
    let mut parent = database.interface(name)?.parent;
    while let Some(candidate) = parent {
        if let Some(implementation) = implementations.get(candidate) {
            return Ok(Some((
                implementation.payload.clone(),
                implementation.lifetime,
            )));
        }
        parent = database.interface(candidate)?.parent;
    }
    Ok(None)
}

fn discover(
    items: &[syn::Item],
    source: &str,
    implementations: &mut BTreeMap<String, Implementation>,
    classes: &mut BTreeMap<String, String>,
) -> Result<(), Error> {
    for item in items {
        if let syn::Item::Mod(module) = item
            && let Some((_, items)) = &module.content
        {
            discover(items, source, implementations, classes)?;
        }
        if let syn::Item::Struct(item) = item
            && item.attrs.iter().any(|attribute| {
                attribute
                    .path()
                    .segments
                    .iter()
                    .map(|segment| segment.ident.to_string())
                    .eq(["rquickjs", "class"].map(String::from))
            })
        {
            if item.attrs.iter().any(|attribute| {
                attribute.path().is_ident("cfg") || attribute.path().is_ident("cfg_attr")
            }) {
                return Err(Error(format!(
                    "{source}: native classes must not conditionally disappear"
                )));
            }
            classes.insert(item.ident.to_string(), class_name(item, source)?);
        }
        let syn::Item::Impl(item) = item else {
            continue;
        };
        let Some(implementation) = Implementation::parse(item, source)? else {
            continue;
        };
        let name = implementation.interface.clone();
        if let Some(existing) = implementations.get_mut(&name) {
            existing.absorb(implementation, source)?;
        } else {
            implementations.insert(name, implementation);
        }
    }
    Ok(())
}

/// The JavaScript name a declared class provides a prototype for: the
/// `rename` value when present, otherwise the Rust type name. This is the
/// derived signal that elects a prototype owner, never a mapping table.
fn class_name(item: &syn::ItemStruct, source: &str) -> Result<String, Error> {
    for attribute in &item.attrs {
        let syn::Meta::List(meta) = &attribute.meta else {
            continue;
        };
        if !meta
            .path
            .segments
            .iter()
            .map(|segment| segment.ident.to_string())
            .eq(["rquickjs", "class"].map(String::from))
        {
            continue;
        }
        let renamed: Vec<_> = meta
            .parse_args_with(
                syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated,
            )
            .map_err(|error| Error(format!("{source}: invalid class attribute: {error}")))?
            .into_iter()
            .filter_map(|meta| match meta {
                syn::Meta::NameValue(named) if named.path.is_ident("rename") => Some(named.value),
                _ => None,
            })
            .collect();
        let [syn::Expr::Lit(renamed)] = renamed.as_slice() else {
            continue;
        };
        let syn::Lit::Str(renamed) = &renamed.lit else {
            return Err(Error(format!("{source}: class rename must be a string")));
        };
        return Ok(renamed.value());
    }
    Ok(item.ident.to_string())
}

impl Implementation {
    fn parse(item: &syn::ItemImpl, source: &str) -> Result<Option<Self>, Error> {
        let Some((path, _)) = &item.trait_ else {
            return Ok(None);
        };
        let segments: Vec<_> = path.segments.iter().collect();
        let [module, interface] = segments.as_slice() else {
            return Ok(None);
        };
        if !module.ident.to_string().ends_with("_generated") {
            return Ok(None);
        }
        let name = interface.ident.to_string();
        if module.ident != format!("{}_generated", snake_case(&name)) {
            return Err(Error(format!(
                "{source}: generated contract module must follow the interface name {name}"
            )));
        }
        if item.modifiers.polarity.is_some()
            || item.modifiers.defaultness.is_some()
            || item.unsafety.is_some()
            || item.attrs.iter().any(|attribute| {
                attribute.path().is_ident("cfg") || attribute.path().is_ident("cfg_attr")
            })
        {
            return Err(Error(format!(
                "{source}: binding implementations must be unconditional safe trait implementations"
            )));
        }
        let syn::Type::Path(payload) = &*item.self_ty else {
            return Err(Error(format!(
                "{source}: binding payload must be a named local type"
            )));
        };
        let payload = payload_type(&payload.path).ok_or_else(|| {
            Error(format!(
                "{source}: binding payload must be a local type with at most a 'js lifetime"
            ))
        })?;
        let mut methods = BTreeMap::new();
        for member in &item.items {
            let syn::ImplItem::Fn(member) = member else {
                return Err(Error(format!(
                    "{source}: binding implementations contain only platform algorithm methods"
                )));
            };
            if member.attrs.iter().any(|attribute| {
                attribute.path().is_ident("cfg") || attribute.path().is_ident("cfg_attr")
            }) {
                return Err(Error(format!(
                    "{source}: binding methods must not conditionally disappear"
                )));
            }
            let method = Method::parse(member, source)?;
            if methods
                .insert(member.sig.ident.to_string(), method)
                .is_some()
            {
                return Err(Error(format!(
                    "{source}: duplicate binding method {}",
                    member.sig.ident
                )));
            }
        }
        Ok(Some(Self {
            interface: name,
            payload: payload.0,
            lifetime: payload.1,
            payloads: Vec::new(),
            methods,
        }))
    }
}

fn payload_type(path: &syn::Path) -> Option<(syn::Ident, bool)> {
    if path.leading_colon.is_some() || path.segments.len() != 1 {
        return None;
    }
    let segment = path.segments.first()?;
    match &segment.arguments {
        syn::PathArguments::None => Some((segment.ident.clone(), false)),
        syn::PathArguments::AngleBracketed(arguments)
            if arguments.args.len() == 1
                && matches!(arguments.args.first(), Some(syn::GenericArgument::Lifetime(lifetime)) if lifetime.ident == "js") =>
        {
            Some((segment.ident.clone(), true))
        }
        _ => None,
    }
}

fn lower(
    database: &Database<'_>,
    implementation: &Implementation,
    kind: InterfaceKind,
    implemented: &BTreeSet<String>,
    reflected: &[crate::ReflectedEnum],
) -> Result<model::Interface, Error> {
    // Mixins have no interface object; their members install on every
    // *implemented* including interface's prototype, derived from the IDL
    // includes. Included interfaces without an implementation have no
    // global to install onto, so they are excluded (a JS-only includer
    // added later needs an implementation to become an install target —
    // it is silently skipped until then, by design).
    if let Ok(mixin) = database.mixin(&implementation.interface) {
        return lower_mixin(
            database,
            implementation,
            kind,
            &mixin,
            implemented,
            reflected,
        );
    }
    let declaration = database.interface(&implementation.interface)?;
    validate_interface_attributes(declaration.attributes.as_ref())?;
    let secure_context = has_no_args(declaration.attributes.as_ref(), "SecureContext");
    // https://webidl.spec.whatwg.org/#js-DOMException-specialness
    let parent = declaration
        .parent
        .map(|parent| PrototypeParent::Interface(parent.into()))
        .or_else(|| {
            (declaration.name == "DOMException").then(|| PrototypeParent::Intrinsic("Error".into()))
        });
    let mut interface = model::Interface {
        name: declaration.name.into(),
        rust: implementation.payload.clone(),
        payloads: implementation.payloads.clone(),
        has_lifetime: implementation.lifetime,
        parent,
        constructor: None,
        attributes: Vec::new(),
        constants: Vec::new(),
        kind,
        operations: Vec::new(),
        dictionaries: Vec::new(),
        enumerations: Vec::new(),
        unions: Vec::new(),
        properties: PropertyHooks::None,
        stringifier: None,
        indexed_setter: None,
        install_targets: Vec::new(),
        secure_context,
        contract: None,
    };
    interface.contract = lower_members(
        database,
        &declaration,
        implementation,
        &mut interface,
        reflected,
    )?;
    if matches!(interface.kind, InterfaceKind::Partial)
        && (interface.constructor.is_some() || !matches!(interface.properties, PropertyHooks::None))
    {
        return Err(Error(
            "bindings for existing classes cannot replace constructors or property hooks".into(),
        ));
    }
    add_referenced_definitions(database, &mut interface)?;
    Ok(interface)
}

/// Lower a mixin implementation. The mixin itself has no prototype; the
/// generated installer targets every implemented interface that includes
/// it, resolved from the imported includes statements rather than a
/// handwritten list. Included interfaces without an implementation have no
/// global to install onto, so they are excluded rather than failing the
/// install at runtime.
fn lower_mixin(
    database: &Database<'_>,
    implementation: &Implementation,
    kind: InterfaceKind,
    mixin: &crate::database::Mixin<'_>,
    implemented: &BTreeSet<String>,
    reflected: &[crate::ReflectedEnum],
) -> Result<model::Interface, Error> {
    if !matches!(kind, InterfaceKind::Partial) {
        return Err(Error(format!(
            "{}: mixin bindings require an existing class payload",
            implementation.interface
        )));
    }
    let mut interface = model::Interface {
        name: implementation.interface.clone(),
        rust: implementation.payload.clone(),
        payloads: implementation.payloads.clone(),
        has_lifetime: implementation.lifetime,
        parent: None,
        constructor: None,
        attributes: Vec::new(),
        constants: Vec::new(),
        kind,
        operations: Vec::new(),
        dictionaries: Vec::new(),
        enumerations: Vec::new(),
        unions: Vec::new(),
        properties: PropertyHooks::None,
        stringifier: None,
        indexed_setter: None,
        secure_context: false,
        install_targets: database
            .includers(&implementation.interface)
            .into_iter()
            .filter(|includer| implemented.contains(includer))
            .collect(),
        contract: None,
    };
    if interface.install_targets.is_empty() {
        return Err(Error(format!(
            "{}: mixin is included by no implemented interface",
            implementation.interface
        )));
    }
    let declaration = crate::database::Interface {
        name: mixin.name,
        attributes: None,
        parent: None,
        members: mixin.members.clone(),
    };
    interface.contract = lower_members(
        database,
        &declaration,
        implementation,
        &mut interface,
        reflected,
    )?;
    add_referenced_definitions(database, &mut interface)?;
    Ok(interface)
}

/// The IDL member name for diagnostics.
fn member_name<'a>(member: &InterfaceMember<'a>) -> &'a str {
    match member {
        InterfaceMember::Const(member) => member.identifier.0,
        InterfaceMember::Constructor(_) => "constructor",
        InterfaceMember::Attribute(member) => member.identifier.0,
        InterfaceMember::Operation(member) => member
            .identifier
            .as_ref()
            .map_or("<anonymous>", |identifier| identifier.0),
        InterfaceMember::Iterable(_)
        | InterfaceMember::AsyncIterable(_)
        | InterfaceMember::Maplike(_)
        | InterfaceMember::Setlike(_)
        | InterfaceMember::Stringifier(_) => "<special>",
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "one match arm per IDL interface member kind; splitting the arms would thread the shared accumulators through several helpers"
)]
fn lower_members(
    database: &Database<'_>,
    declaration: &crate::database::Interface<'_>,
    implementation: &Implementation,
    interface: &mut model::Interface,
    reflected: &[crate::ReflectedEnum],
) -> Result<Option<TokenStream>, Error> {
    let mut remaining = implementation.methods.clone();
    let mut methods = Vec::new();
    let mut getters = Vec::new();
    // Indexed and named getters inherited through the parent chain lower as
    // own members: the exotic hooks and direct calls both resolve on this
    // interface's prototype. This derives the repetition the legacy IDL
    // used to hand-write, instead of editing the contract.
    let inherited = database.hook_members(declaration.name)?;
    for member in declaration.members.iter().chain(inherited.iter()) {
        let resolved = member;
        let exposure = member.exposure();
        match &member.declaration {
            InterfaceMember::Const(member) => {
                resolved.validate_scopes().map_err(|error| {
                    crate::Error(format!("{}: {error}", member_name(&resolved.declaration)))
                })?;
                interface
                    .constants
                    .push(model::Constant::parse(member, exposure)?);
            }
            InterfaceMember::Constructor(member)
                if implementation.methods.contains_key("constructor") =>
            {
                lower_constructor_member(
                    database,
                    member,
                    resolved,
                    implementation,
                    &mut remaining,
                    &mut methods,
                    interface,
                )?;
            }
            InterfaceMember::Attribute(member) => {
                let Some((attribute, signature)) = lower_attribute(
                    database,
                    declaration.name,
                    member,
                    exposure,
                    &implementation.methods,
                    reflected,
                )?
                else {
                    continue;
                };
                resolved.validate_scopes().map_err(|error| {
                    crate::Error(format!("{}: {error}", member_name(&resolved.declaration)))
                })?;
                remaining.remove(&attribute.rust.to_string());
                if let Some(model::Setter::Method { rust, .. }) = &attribute.setter {
                    remaining.remove(&rust.to_string());
                }
                if matches!(
                    member.modifier,
                    Some(weedle::interface::StringifierOrInheritOrStatic::Stringifier(_))
                ) && interface
                    .stringifier
                    .replace(model::Stringifier::Attribute(interface.attributes.len()))
                    .is_some()
                {
                    return Err(Error("multiple stringifiers are not supported".into()));
                }
                // Reflected attributes contribute no trait methods.
                if !signature.is_empty() {
                    methods.push(signature);
                }
                interface.attributes.push(attribute);
            }
            InterfaceMember::Operation(member) => {
                if member.identifier.is_none() {
                    if let Some(signature) = lower_indexed_setter(
                        database,
                        member,
                        &implementation.methods,
                        &mut *interface,
                    )? {
                        remaining.remove("set_indexed");
                        methods.push(signature);
                    }
                    continue;
                }
                if overload_consumed(member, &remaining) {
                    continue;
                }
                let Some((operation, signature, property)) = lower_operation(
                    database,
                    member,
                    exposure,
                    &implementation.methods,
                    &mut interface.unions,
                )?
                else {
                    continue;
                };
                resolved.validate_scopes().map_err(|error| {
                    crate::Error(format!("{}: {error}", member_name(&resolved.declaration)))
                })?;
                remaining.remove(&operation.rust.to_string());
                if let Some(property) = property {
                    getters.push(property);
                }
                methods.push(signature);
                interface.operations.push(operation);
            }
            InterfaceMember::Constructor(_)
            | InterfaceMember::Iterable(_)
            | InterfaceMember::AsyncIterable(_)
            | InterfaceMember::Maplike(_)
            | InterfaceMember::Setlike(_) => {}
            InterfaceMember::Stringifier(member) => {
                // An anonymous `stringifier;` lowers to a `toString` method
                // the implementation provides; anything else stays absent.
                validate_empty_attributes(member.attributes.as_ref())?;
                if !implementation.methods.contains_key("stringifier") {
                    continue;
                }
                check_method_signature(&implementation.methods, "stringifier", 0, "stringifier")?;
                if interface
                    .stringifier
                    .replace(model::Stringifier::Method)
                    .is_some()
                {
                    return Err(Error("multiple stringifiers are not supported".into()));
                }
                remaining.remove("stringifier");
                methods.push(
                    quote! { fn stringifier(&self, ctx: &Ctx<'js>) -> Result<rquickjs::String<'js>>; },
                );
            }
        }
    }
    interface.properties =
        infer_property_hooks(declaration, &getters, &mut remaining, &mut methods)?;
    ensure_no_extra_methods(&remaining)?;
    // A fully generated contract (such as `[Reflect]`-only members) has no
    // trait methods: the generator supplies the whole dispatch and there is
    // nothing an implementation could fill in. An empty trait would be an item
    // with no reader, so it is not emitted at all, and the implementation has
    // no trait to name either.
    if methods.is_empty() {
        return Ok(None);
    }
    let name = format_ident!("{}", declaration.name);
    Ok(Some(
        quote! { pub(crate) trait #name<'js> { #(#methods)* } },
    ))
}

/// Reject implementation methods that no IDL member consumed.
fn ensure_no_extra_methods(remaining: &BTreeMap<String, Method>) -> Result<(), Error> {
    if remaining.is_empty() {
        return Ok(());
    }
    Err(Error(format!(
        "methods do not match a supported IDL contract: {}",
        remaining.keys().cloned().collect::<Vec<_>>().join(", ")
    )))
}

/// Emit the dictionaries and enumerations the lowered members reference.
fn add_referenced_definitions(
    database: &Database<'_>,
    interface: &mut model::Interface,
) -> Result<(), Error> {
    let mut referenced = BTreeSet::new();
    for attribute in &interface.attributes {
        collect_reference(&attribute.return_type, &mut referenced);
    }
    for operation in &interface.operations {
        for argument in &operation.arguments {
            collect_reference(&argument.type_, &mut referenced);
        }
    }
    if let Some(constructor) = &interface.constructor {
        for argument in &constructor.arguments {
            if let ConstructorArgumentKind::Dictionary(name) = &argument.kind {
                referenced.insert(name.clone());
            }
        }
    }
    for name in referenced {
        match database.definition(&name) {
            Some(weedle::Definition::Dictionary(_)) => {
                let dictionary = lower_dictionary(database, &name)?;
                // Dictionaries name their own dependencies: enumerations
                // their fields convert through.
                for field in &dictionary.fields {
                    if let model::DictionaryFieldType::Enumeration { name, .. } = &field.type_
                        && !interface
                            .enumerations
                            .iter()
                            .any(|enumeration| enumeration.name == *name)
                        && let Some(weedle::Definition::Enum(definition)) =
                            database.definition(name)
                    {
                        interface
                            .enumerations
                            .push(model::Enumeration::parse(definition)?);
                    }
                }
                interface.dictionaries.push(dictionary);
            }
            Some(weedle::Definition::Enum(definition)) => {
                interface
                    .enumerations
                    .push(model::Enumeration::parse(definition)?);
            }
            Some(weedle::Definition::Callback(_)) => {}
            _ => {
                return Err(Error(format!(
                    "native reference {name} is not supported yet"
                )));
            }
        }
    }
    for attribute in &interface.attributes {
        collect_union(&attribute.return_type, &mut interface.unions);
    }
    for operation in &interface.operations {
        for argument in &operation.arguments {
            collect_union(&argument.type_, &mut interface.unions);
        }
    }
    Ok(())
}

/// Note a union a lowered member references, so its generated enum is
/// emitted into the same module. The union carries its members inline
/// because unions have no named declaration to re-lower.
fn collect_union(type_: &ReturnType, unions: &mut Vec<model::Union>) {
    if let ReturnType::Union(name, members) | ReturnType::NullableUnion(name, members) = type_
        && !unions.iter().any(|union| union.name == *name)
    {
        unions.push(model::Union {
            name: name.clone(),
            members: members.clone(),
        });
    }
}

/// Note a dictionary or enumeration a lowered member references, so its
/// definition is emitted into the same generated module.
fn collect_reference(type_: &ReturnType, referenced: &mut BTreeSet<String>) {
    match type_ {
        ReturnType::Dictionary(name) | ReturnType::Enumeration(name) => {
            referenced.insert(name.clone());
        }
        // Dictionaries a union member references still need their struct.
        ReturnType::Union(_, members) => {
            for member in members {
                if let model::UnionMemberType::Dictionary(name) = &member.type_ {
                    referenced.insert(name.clone());
                }
            }
        }
        _ => {}
    }
}

/// Lower an unchanged dictionary declaration. Field names are the IDL names
/// snake-cased; no `Rust` annotations are read.
/// One dictionary member's Rust representation. Anything without a
/// generated conversion fails the build.
fn dictionary_field_type(
    database: &Database<'_>,
    member: &weedle::dictionary::DictionaryMember<'_>,
    required: bool,
) -> Result<model::DictionaryFieldType, Error> {
    match &member.type_.type_ {
        Type::Single(SingleType::NonAny(NonAnyType::Boolean(value))) if value.q_mark.is_none() => {
            let default = match &member.default {
                None => None,
                Some(default) => {
                    let DefaultValue::Boolean(default) = &default.value else {
                        return Err(Error(
                            "only boolean dictionary defaults are supported yet".into(),
                        ));
                    };
                    Some(default.0)
                }
            };
            Ok(model::DictionaryFieldType::Boolean { default, required })
        }
        Type::Single(SingleType::NonAny(NonAnyType::Sequence(value))) if value.q_mark.is_none() => {
            let element = &value.type_.generics.body;
            validate_empty_attributes(element.attributes.as_ref())?;
            let is_string = matches!(
                &element.type_,
                Type::Single(SingleType::NonAny(NonAnyType::DOMString(element)))
                    if element.q_mark.is_none()
            );
            if !is_string {
                return Err(Error(
                    "only sequence<DOMString> dictionary fields are supported yet".into(),
                ));
            }
            if member.default.is_some() {
                return Err(Error(
                    "sequence dictionary defaults are not supported yet".into(),
                ));
            }
            Ok(model::DictionaryFieldType::StringSequence)
        }
        Type::Single(SingleType::NonAny(NonAnyType::DOMString(value)))
            if value.q_mark.is_none() =>
        {
            if member.default.is_some() {
                return Err(Error(
                    "string dictionary defaults are not supported yet".into(),
                ));
            }
            Ok(model::DictionaryFieldType::DomString)
        }
        Type::Single(SingleType::NonAny(NonAnyType::Identifier(value)))
            if matches!(
                database.definition(value.type_.0),
                Some(weedle::Definition::Enum(_))
            ) =>
        {
            dictionary_enum_field(
                database,
                member,
                value.type_.0,
                value.q_mark.is_some(),
                required,
            )
        }
        Type::Single(SingleType::NonAny(NonAnyType::Identifier(value))) => {
            dictionary_interface_field(database, member, value.type_.0, value.q_mark.is_some())
        }
        _ => Err(Error(format!(
            "dictionary field type is not supported yet: {}",
            member.identifier.0
        ))),
    }
}

/// An enumeration-typed dictionary member. Anything else named by the member
/// fails the build.
fn dictionary_enum_field(
    database: &Database<'_>,
    member: &weedle::dictionary::DictionaryMember<'_>,
    name: &str,
    nullable: bool,
    required: bool,
) -> Result<model::DictionaryFieldType, Error> {
    if !matches!(database.definition(name), Some(weedle::Definition::Enum(_))) {
        return Err(Error(format!(
            "dictionary field type is not supported yet: {}",
            member.identifier.0
        )));
    }
    if required && nullable {
        return Err(Error(format!(
            "required dictionary field cannot be nullable: {}",
            member.identifier.0
        )));
    }
    // A default that is not a keyword would fail conversion at runtime, so
    // resolve it to a variant at lowering instead.
    let default = match &member.default {
        None => None,
        Some(default) => {
            let DefaultValue::String(default) = &default.value else {
                return Err(Error(
                    "only string dictionary defaults are supported yet".into(),
                ));
            };
            Some(dictionary_enum_variant(database, name, default.0)?)
        }
    };
    Ok(model::DictionaryFieldType::Enumeration {
        name: name.into(),
        nullable,
        default,
        required,
    })
}

/// An interface- or callback-interface-typed dictionary member.
fn dictionary_interface_field(
    database: &Database<'_>,
    member: &weedle::dictionary::DictionaryMember<'_>,
    name: &str,
    nullable: bool,
) -> Result<model::DictionaryFieldType, Error> {
    if !matches!(
        database.definition(name),
        Some(weedle::Definition::Interface(_) | weedle::Definition::CallbackInterface(_))
    ) {
        return Err(Error(format!(
            "dictionary field type is not supported yet: {}",
            member.identifier.0
        )));
    }
    if member.default.is_some() {
        return Err(Error(
            "interface dictionary defaults are not supported yet".into(),
        ));
    }
    Ok(model::DictionaryFieldType::Interface { nullable })
}

/// Resolve a dictionary default keyword to its enumeration variant, so a
/// default that is not a keyword fails the build instead of conversion.
fn dictionary_enum_variant(
    database: &Database<'_>,
    name: &str,
    text: &str,
) -> Result<proc_macro2::Ident, Error> {
    let Some(weedle::Definition::Enum(definition)) = database.definition(name) else {
        return Err(Error(format!("unknown enumeration: {name}")));
    };
    let enumeration = model::Enumeration::parse(definition)?;
    enumeration
        .values
        .iter()
        .find(|(keyword, _)| keyword == text)
        .map(|(_, variant)| variant.clone())
        .ok_or_else(|| {
            Error(format!(
                "unknown {name} keyword in dictionary default: {text}"
            ))
        })
}

fn lower_dictionary(database: &Database<'_>, name: &str) -> Result<model::Dictionary, Error> {
    let definition = database.dictionary(name)?;
    validate_dictionary_attributes(definition.attributes.as_ref())?;
    let inherited = if let Some(parent) = &definition.inheritance {
        lower_dictionary(database, parent.identifier.0)?.fields
    } else {
        Vec::new()
    };
    let mut fields = Vec::new();
    let mut taken: HashSet<_> = inherited.iter().map(|field| field.name.as_str()).collect();
    for member in &definition.members.body {
        if !taken.insert(member.identifier.0) {
            return Err(Error(format!(
                "duplicate dictionary field: {}",
                member.identifier.0
            )));
        }
        validate_empty_attributes(member.attributes.as_ref())?;
        validate_empty_attributes(member.type_.attributes.as_ref())?;
        let required = member.required.is_some();
        if required && member.default.is_some() {
            return Err(Error(format!(
                "required dictionary field cannot have a default: {}",
                member.identifier.0
            )));
        }
        let rust = format_ident!("{}", snake_case(member.identifier.0));
        let type_ = dictionary_field_type(database, member, required)?;
        fields.push(model::DictionaryField {
            name: member.identifier.0.into(),
            rust,
            type_,
        });
    }
    // https://webidl.spec.whatwg.org/#es-dictionary
    // https://github.com/chromium/chromium/blob/main/third_party/blink/renderer/bindings/scripts/web_idl/dictionary.py
    fields.sort_by(|left, right| left.name.cmp(&right.name));
    let fields = inherited.into_iter().chain(fields).collect();
    Ok(model::Dictionary {
        name: definition.identifier.0.into(),
        fields,
    })
}

/// Dictionary-level extended attributes the contract path consumes.
fn validate_dictionary_attributes(
    attributes: Option<&ExtendedAttributeList<'_>>,
) -> Result<(), Error> {
    if attributes.is_some_and(|attributes| !attributes.body.list.is_empty()) {
        Err(Error(format!(
            "native dictionary semantics are not supported yet: {attributes:?}"
        )))
    } else {
        Ok(())
    }
}

/// Infer legacy platform-object property hooks from declared getters.
fn infer_property_hooks(
    declaration: &crate::database::Interface<'_>,
    getters: &[PropertyGetter],
    remaining: &mut BTreeMap<String, Method>,
    methods: &mut Vec<TokenStream>,
) -> Result<PropertyHooks, Error> {
    let indexed = getters.contains(&PropertyGetter::Indexed);
    let named = getters.contains(&PropertyGetter::Named);
    match (indexed, named) {
        (false, false) => Ok(PropertyHooks::None),
        (true, false) => Ok(PropertyHooks::Indexed),
        (false, true) => Err(Error(
            "named-only property hooks are not supported yet".into(),
        )),
        (true, true) => {
            let supported = format_ident!("supported_names");
            if remaining.remove("supported_names").is_none() {
                return Err(Error(
                    "named property hooks require a supported_names implementation".into(),
                ));
            }
            methods
                .push(quote! { fn supported_names(&self, ctx: &Ctx<'js>) -> Result<Vec<String>>; });
            let attributes = declaration.attributes.as_ref();
            Ok(PropertyHooks::IndexedNamed {
                names: supported,
                unenumerable: has_no_args(attributes, "LegacyUnenumerableNamedProperties"),
                override_builtins: has_no_args(attributes, "LegacyOverrideBuiltIns"),
            })
        }
    }
}

/// One constructor member's lowering.
fn lower_constructor_member(
    database: &Database<'_>,
    member: &weedle::interface::ConstructorInterfaceMember<'_>,
    resolved: &crate::database::Member<'_>,
    implementation: &Implementation,
    remaining: &mut BTreeMap<String, Method>,
    methods: &mut Vec<TokenStream>,
    interface: &mut model::Interface,
) -> Result<(), Error> {
    resolved.validate_scopes().map_err(|error| {
        crate::Error(format!("{}: {error}", member_name(&resolved.declaration)))
    })?;
    if interface.constructor.is_some() {
        return Err(Error(
            "native constructor overloads are not supported yet".into(),
        ));
    }
    remaining.remove("constructor");
    if implementation.methods["constructor"].has_self {
        return Err(Error(
            "native constructors are associated functions, not methods".into(),
        ));
    }
    let (constructor, signature) = lower_constructor(database, member)?;
    interface.constructor = Some(constructor);
    methods.push(signature);
    Ok(())
}

/// Whether an IDL operation was already consumed by an earlier overload.
/// Overloads share one implementation method: the first IDL overload in
/// declaration order consumes it and later same-name overloads are skipped,
/// matching overload-set order. The implementation fully describes the
/// shared behavior; unimplemented overloads stay absent.
fn overload_consumed(
    member: &weedle::interface::OperationInterfaceMember<'_>,
    remaining: &BTreeMap<String, Method>,
) -> bool {
    match &member.identifier {
        Some(identifier) => !remaining.contains_key(snake_case(identifier.0).as_str()),
        None => false,
    }
}

/// An anonymous indexed setter: `setter undefined (unsigned long index, T
/// value)`. The value passes through as-is for the platform method to
/// convert, mirroring `[RustValue]`; the trait carries the `set_indexed`
/// method the exotic hooks call. Anything else anonymous stays absent like
/// any unimplemented member.
fn lower_indexed_setter(
    database: &Database<'_>,
    member: &weedle::interface::OperationInterfaceMember<'_>,
    implemented: &BTreeMap<String, Method>,
    interface: &mut model::Interface,
) -> Result<Option<TokenStream>, Error> {
    if !matches!(member.special, Some(Special::Setter(_))) {
        return Ok(None);
    }
    if member.modifier.is_some() {
        return Err(Error("indexed setters must not be static".into()));
    }
    if !matches!(member.return_type, weedle::types::ReturnType::Undefined(_)) {
        return Err(Error("indexed setters require an undefined result".into()));
    }
    let mut reactions = false;
    if let Some(attributes) = member.attributes.as_ref() {
        for attribute in &attributes.body.list {
            match attribute {
                ExtendedAttribute::NoArgs(attribute) if attribute.0.0 == "CEReactions" => {
                    reactions = true;
                }
                _ => {
                    return Err(Error(format!(
                        "indexed setter attributes are not supported yet: {attribute:?}"
                    )));
                }
            }
        }
    }
    let [Argument::Single(index), Argument::Single(value)] = member.args.body.list.as_slice()
    else {
        return Err(Error("indexed setters take an index and a value".into()));
    };
    if index.optional.is_some() || index.default.is_some() {
        return Err(Error("indexed setter index must be required".into()));
    }
    validate_empty_attributes(index.attributes.as_ref())?;
    validate_empty_attributes(index.type_.attributes.as_ref())?;
    if !matches!(
        native_type(database, &index.type_.type_, &mut BTreeSet::new())?,
        ReturnType::UnsignedLong
    ) {
        return Err(Error("indexed setter index requires unsigned long".into()));
    }
    if value.optional.is_some() || value.default.is_some() {
        return Err(Error("indexed setter value must be required".into()));
    }
    validate_empty_attributes(value.attributes.as_ref())?;
    validate_empty_attributes(value.type_.attributes.as_ref())?;
    // The value type must lower so nonsense fails here, but the raw value
    // passes through for the platform method to convert.
    native_type(database, &value.type_.type_, &mut BTreeSet::new())?;
    let Some(method) = implemented.get("set_indexed") else {
        return Ok(None);
    };
    if !method.has_self || method.parameters != 2 || method.receiver {
        return Err(Error(
            "indexed setter implementation takes self, ctx, an index, and a value".into(),
        ));
    }
    if interface.indexed_setter.is_some() {
        return Err(Error(
            "multiple indexed setters are not supported yet".into(),
        ));
    }
    interface.indexed_setter = Some(model::IndexedSetter {
        rust: format_ident!("set_indexed"),
        reactions,
    });
    Ok(Some(
        quote! { fn set_indexed(&self, ctx: Ctx<'js>, index: u32, value: Value<'js>) -> Result<()>; },
    ))
}

fn lower_operation(
    database: &Database<'_>,
    member: &weedle::interface::OperationInterfaceMember<'_>,
    exposure: Exposure,
    implemented: &BTreeMap<String, Method>,
    unions: &mut Vec<model::Union>,
) -> Result<Option<(model::Operation, TokenStream, Option<PropertyGetter>)>, Error> {
    let Some(identifier) = &member.identifier else {
        return Ok(None);
    };
    let name = identifier.0;
    let rust = format_ident!("{}", snake_case(name));
    let Some(method) = implemented.get(&rust.to_string()) else {
        return Ok(None);
    };
    if member.modifier.is_some() {
        return Err(Error(
            "native static and stringifier operations are not supported yet".into(),
        ));
    }
    validate_operation_attributes(member.attributes.as_ref())?;
    let reactions = has_no_args(member.attributes.as_ref(), "CEReactions");
    let unscopable = has_no_args(member.attributes.as_ref(), "Unscopable");
    if !method.has_self {
        return Err(Error(format!(
            "native operation {name} must take self as its first parameter"
        )));
    }
    let property = match &member.special {
        None => None,
        Some(Special::Getter(_)) => Some(property_getter(database, member)?),
        Some(_) => {
            return Err(Error(
                "only readonly property getters are supported yet".into(),
            ));
        }
    };
    let mut arguments = Vec::new();
    let mut parameters = Vec::new();
    let mut optional_seen = false;
    for (index, argument) in member.args.body.list.iter().enumerate() {
        let LoweredArgument {
            argument,
            parameter,
            arity,
        } = lower_argument(database, argument)?;
        if arity == ArgumentArity::Variadic && index + 1 != member.args.body.list.len() {
            return Err(Error("variadic arguments must be last".into()));
        }
        if optional_seen && arity == ArgumentArity::Required {
            return Err(Error(
                "native required argument after an optional one is not supported yet".into(),
            ));
        }
        optional_seen |= arity == ArgumentArity::Optional;
        let argument_name = format_ident!("arg_{index}");
        parameters.push(quote! { #argument_name: #parameter });
        arguments.push(argument);
    }
    let expected = arguments.len();
    let takes_this = if method.parameters == expected {
        false
    } else if method.parameters == expected + 1 && method.receiver && property.is_none() {
        true
    } else {
        return Err(Error(format!(
            "native operation {name} has an implementation signature that does not match its IDL arguments"
        )));
    };
    let this_parameter = if takes_this {
        quote! { this: rquickjs::Object<'js>, }
    } else {
        quote! {}
    };
    let (result, returns) = operation_result(database, &member.return_type, unions)?;
    let signature = quote! {
        fn #rust(&self, ctx: Ctx<'js>, #this_parameter #(#parameters),*) -> Result<#returns>;
    };
    let operation = model::Operation {
        exposure,
        name: name.into(),
        rust,
        result,
        takes_this,
        arguments,
        reactions,
        unscopable,
        getter: property,
    };
    Ok(Some((operation, signature, property)))
}

struct LoweredArgument {
    argument: model::OperationArgument,
    parameter: TokenStream,
    arity: ArgumentArity,
}

fn lower_argument(
    database: &Database<'_>,
    argument: &weedle::argument::Argument<'_>,
) -> Result<LoweredArgument, Error> {
    let argument = match argument {
        Argument::Single(argument) => argument,
        Argument::Variadic(argument) => {
            validate_empty_attributes(argument.attributes.as_ref())?;
            let type_ = native_type(database, &argument.type_, &mut BTreeSet::new())?;
            let element = argument_parameter(&type_, ArgumentArity::Required, None)?;
            return Ok(LoweredArgument {
                argument: model::OperationArgument {
                    type_,
                    arity: ArgumentArity::Variadic,
                    null_default: false,
                    legacy_null_to_empty: false,
                    boolean_default: None,
                    union_default: None,
                },
                parameter: quote! { Vec<#element> },
                arity: ArgumentArity::Variadic,
            });
        }
    };
    validate_argument_attributes(argument.attributes.as_ref())?;
    validate_argument_attributes(argument.type_.attributes.as_ref())?;
    let legacy_null_to_empty = has_no_args(argument.attributes.as_ref(), "LegacyNullToEmptyString")
        || has_no_args(
            argument.type_.attributes.as_ref(),
            "LegacyNullToEmptyString",
        );
    let type_ = native_type(database, &argument.type_.type_, &mut BTreeSet::new())?;
    if legacy_null_to_empty && !matches!(type_, ReturnType::String) {
        return Err(Error("LegacyNullToEmptyString requires DOMString".into()));
    }
    // https://webidl.spec.whatwg.org/#dfn-overload-resolution-algorithm
    match (&type_, argument.optional.is_some(), &argument.default) {
        (_, false, None)
        | (
            ReturnType::String
            | ReturnType::UsvString
            | ReturnType::Boolean
            | ReturnType::Union(_, _)
            | ReturnType::Any,
            true,
            None,
        ) => {}
        (ReturnType::Any, true, Some(default))
            if matches!(default.value, DefaultValue::Null(_)) => {}
        (ReturnType::Boolean, true, Some(default))
            if matches!(default.value, DefaultValue::Boolean(_)) => {}
        (ReturnType::NullableDocumentType, true, Some(default))
            if matches!(default.value, DefaultValue::Null(_)) => {}
        (ReturnType::Dictionary(_), true, Some(default))
            if matches!(default.value, DefaultValue::EmptyDictionary(_)) => {}
        // An optional union converts an omitted `{}` or `null` by its own
        // algorithm; a dictionary member fills the default.
        (ReturnType::Union(_, _) | ReturnType::NullableUnion(_, _), true, Some(default))
            if matches!(
                default.value,
                DefaultValue::EmptyDictionary(_) | DefaultValue::Null(_)
            ) => {}
        // An optional union with a boolean default materializes it before
        // conversion, mirroring boolean defaults.
        (ReturnType::Union(_, _) | ReturnType::NullableUnion(_, _), true, Some(default))
            if matches!(default.value, DefaultValue::Boolean(_)) => {}
        _ => {
            return Err(Error(
                "native operation optionality or default is not supported yet".into(),
            ));
        }
    }
    let arity = if argument.optional.is_some() {
        ArgumentArity::Optional
    } else {
        ArgumentArity::Required
    };
    let boolean_default = argument
        .default
        .as_ref()
        .and_then(|default| match default.value {
            DefaultValue::Boolean(value) => Some(value.0),
            _ => None,
        });
    let parameter = argument_parameter(&type_, arity, boolean_default)?;
    let union_default = union_default(&type_, argument)?;
    Ok(LoweredArgument {
        argument: model::OperationArgument {
            type_,
            arity,
            null_default: argument
                .default
                .as_ref()
                .is_some_and(|default| matches!(default.value, DefaultValue::Null(_))),
            legacy_null_to_empty,
            boolean_default,
            union_default,
        },
        parameter,
        arity,
    })
}

/// An optional union argument's IDL default, if it is a boolean or an empty
/// dictionary. Anything else fails the build.
fn union_default(
    type_: &ReturnType,
    argument: &weedle::argument::SingleArgument<'_>,
) -> Result<Option<model::UnionDefault>, Error> {
    let is_union = matches!(type_, ReturnType::Union(..) | ReturnType::NullableUnion(..));
    let Some(default) = argument.default.as_ref().filter(|_| is_union) else {
        return Ok(None);
    };
    match default.value {
        DefaultValue::Boolean(value) => Ok(Some(model::UnionDefault::Boolean(value.0))),
        DefaultValue::EmptyDictionary(_) => Ok(Some(model::UnionDefault::EmptyDictionary)),
        // `= null` on a nullable union needs no materialization: the
        // conversion already maps a missing value to `None`.
        DefaultValue::Null(_) => Ok(None),
        _ => Err(Error(
            "union argument defaults must be boolean, null, or {} for now".into(),
        )),
    }
}

/// The Rust parameter type for one lowered operation argument. These mirror the
/// conversions in `emit`, and match the payload signatures the platform
/// algorithms already use.
fn argument_parameter(
    type_: &ReturnType,
    arity: ArgumentArity,
    boolean_default: Option<bool>,
) -> Result<TokenStream, Error> {
    let optional = arity == ArgumentArity::Optional;
    Ok(match type_ {
        ReturnType::Node => quote! { host::NodeReference },
        ReturnType::NullableNode | ReturnType::NullableDocumentType => {
            quote! { Option<host::NodeReference> }
        }
        // `USVString` arrives as a converted string; only the argument
        // conversion differs.
        ReturnType::String | ReturnType::UsvString if optional => {
            quote! { Option<rquickjs::String<'js>> }
        }
        ReturnType::String | ReturnType::UsvString => quote! { rquickjs::String<'js> },
        ReturnType::NullableString => quote! { Option<rquickjs::String<'js>> },
        ReturnType::Callback => quote! { rquickjs::Function<'js> },
        // A platform object or `any` argument arrives as the original value;
        // the platform algorithm performs any further check
        // (<https://webidl.spec.whatwg.org/#idl-any>).
        ReturnType::PlatformObject | ReturnType::Any => quote! { Value<'js> },
        ReturnType::Dictionary(name) | ReturnType::Enumeration(name) => {
            let name = format_ident!("{name}");
            quote! { #name }
        }
        ReturnType::Union(name, members) | ReturnType::NullableUnion(name, members) => {
            let name = format_ident!("{name}");
            let union = if members.iter().any(|member| member.type_.needs_lifetime()) {
                quote! { #name<'js> }
            } else {
                quote! { #name }
            };
            if matches!(type_, ReturnType::NullableUnion(_, _)) {
                quote! { Option<#union> }
            } else {
                union
            }
        }
        ReturnType::Boolean if optional && boolean_default.is_none() => quote! { Option<bool> },
        ReturnType::Boolean => quote! { bool },
        ReturnType::UnsignedShort => quote! { u16 },
        ReturnType::Short => quote! { i16 },
        ReturnType::UnsignedLong => quote! { u32 },
        ReturnType::Long => quote! { i32 },
        ReturnType::Double | ReturnType::RestrictedDouble => quote! { f64 },
        _ => {
            return Err(Error(
                "native operation argument type is not supported yet".into(),
            ));
        }
    })
}

fn property_getter(
    database: &Database<'_>,
    member: &weedle::interface::OperationInterfaceMember<'_>,
) -> Result<PropertyGetter, Error> {
    let [Argument::Single(argument)] = member.args.body.list.as_slice() else {
        return Err(Error("property getters require one argument".into()));
    };
    if argument.optional.is_some() || argument.default.is_some() {
        return Err(Error("property getter arguments must be required".into()));
    }
    match native_type(database, &argument.type_.type_, &mut BTreeSet::new())? {
        ReturnType::UnsignedLong => Ok(PropertyGetter::Indexed),
        ReturnType::String => Ok(PropertyGetter::Named),
        _ => Err(Error(
            "property getters require DOMString or unsigned long".into(),
        )),
    }
}

fn operation_result(
    database: &Database<'_>,
    return_type: &weedle::types::ReturnType<'_>,
    unions: &mut Vec<model::Union>,
) -> Result<(OperationResult, TokenStream), Error> {
    match return_type {
        weedle::types::ReturnType::Undefined(_) => Ok((OperationResult::Undefined, quote! { () })),
        weedle::types::ReturnType::Type(type_) => {
            let lowered = native_type(database, type_, &mut BTreeSet::new())?;
            match &lowered {
                ReturnType::Node
                | ReturnType::NullableNode
                | ReturnType::PlatformObject
                | ReturnType::Any => Ok((OperationResult::Object, quote! { Value<'js> })),
                ReturnType::String => {
                    Ok((OperationResult::String, quote! { rquickjs::String<'js> }))
                }
                ReturnType::NullableString => Ok((
                    OperationResult::NullableString,
                    quote! { Option<rquickjs::String<'js>> },
                )),
                ReturnType::Boolean => Ok((OperationResult::Boolean, quote! { bool })),
                ReturnType::PromiseUndefined => {
                    Ok((OperationResult::PromiseUndefined, quote! { () }))
                }
                ReturnType::UnsignedShort => Ok((OperationResult::UnsignedShort, quote! { u16 })),
                ReturnType::Short => Ok((OperationResult::Short, quote! { i16 })),
                ReturnType::Long => Ok((OperationResult::Long, quote! { i32 })),
                ReturnType::InterfaceSequence => {
                    Ok((OperationResult::Sequence, quote! { Vec<Value<'js>> }))
                }
                ReturnType::StringSequence => {
                    Ok((OperationResult::StringSequence, quote! { Vec<String> }))
                }
                ReturnType::Union(name, members) | ReturnType::NullableUnion(name, members) => {
                    collect_union(&lowered, unions);
                    union_result(&lowered, name, members)
                }
                _ => Err(Error(
                    "native operation result type is not supported yet".into(),
                )),
            }
        }
    }
}

/// A union in return position: the generated enum converts with `IntoJs`.
/// Members the conversion cannot emit fail the build instead of shipping a
/// partial conversion.
fn union_result(
    lowered: &ReturnType,
    name: &str,
    members: &[model::UnionMember],
) -> Result<(OperationResult, TokenStream), Error> {
    for member in members {
        match member.type_ {
            model::UnionMemberType::Interface { node: false, .. }
            | model::UnionMemberType::String
            | model::UnionMemberType::Boolean
            | model::UnionMemberType::Long => {}
            model::UnionMemberType::Interface { node: true, .. } => {
                return Err(Error(
                    "node members in returned unions are not supported yet".into(),
                ));
            }
            model::UnionMemberType::Dictionary(_) => {
                return Err(Error(
                    "dictionary members in returned unions are not supported yet".into(),
                ));
            }
        }
    }
    let name = format_ident!("{name}");
    let mut result = if members.iter().any(|member| member.type_.needs_lifetime()) {
        quote! { #name<'js> }
    } else {
        quote! { #name }
    };
    let kind = if matches!(lowered, ReturnType::NullableUnion(..)) {
        result = quote! { Option<#result> };
        OperationResult::NullableUnion
    } else {
        OperationResult::Union
    };
    Ok((kind, result))
}

/// Whether a reflected member has a shape that must stay absent rather than
/// fall through to method installation: `SameObject`/`PutForwards` members
/// and non-stringifier special attributes lose their semantics as plain
/// methods.
fn is_special_reflect_shape(member: &weedle::interface::AttributeInterfaceMember<'_>) -> bool {
    has_no_args(member.attributes.as_ref(), "SameObject")
        || has_no_args(member.attributes.as_ref(), "PutForwards")
        || member.modifier.as_ref().is_some_and(|modifier| {
            !matches!(
                modifier,
                weedle::interface::StringifierOrInheritOrStatic::Stringifier(_)
            )
        })
}

/// Rejects `[LegacyNullToEmptyString]` on a reflected member whose type is
/// not plain `DOMString`: anything else reaching the method path is invalid
/// IDL (like operation arguments). Auto-generated string reflection handled
/// its own flag before this runs.
fn reject_legacy_non_string_reflect(
    member: &weedle::interface::AttributeInterfaceMember<'_>,
) -> Result<(), Error> {
    let legacy = has_no_args(member.attributes.as_ref(), "LegacyNullToEmptyString")
        || has_no_args(member.type_.attributes.as_ref(), "LegacyNullToEmptyString");
    if legacy
        && reflect_content(member.attributes.as_ref(), member.identifier.0).is_some()
        && !matches!(
            &member.type_.type_,
            Type::Single(SingleType::NonAny(NonAnyType::DOMString(item))) if item.q_mark.is_none()
        )
    {
        return Err(Error(format!(
            "{}: LegacyNullToEmptyString requires DOMString",
            member.identifier.0
        )));
    }
    Ok(())
}

/// The table entry for one attribute, lowered when it is listed.
fn reflected_enumerated_attribute(
    interface: &str,
    member: &weedle::interface::AttributeInterfaceMember<'_>,
    exposure: Exposure,
    implemented: &BTreeMap<String, Method>,
    reflected: &[crate::ReflectedEnum],
) -> Result<Option<(model::Attribute, TokenStream)>, Error> {
    let Some(entry) = reflected
        .iter()
        .find(|entry| entry.interface == interface && entry.attribute == member.identifier.0)
    else {
        return Ok(None);
    };
    lower_reflected_enumerated_attribute(member, exposure, entry, implemented).map(Some)
}

/// Lower one reflected enumerated attribute from the metadata table.
///
/// The table names the keyword type, the content attribute, and the two
/// defaults; the IDL names the interface, the member, and its mutability. The
/// generated getter and setter are the only definitions, so a hand-written one
/// for the same member is reported instead of silently winning.
fn lower_reflected_enumerated_attribute(
    member: &weedle::interface::AttributeInterfaceMember<'_>,
    exposure: Exposure,
    entry: &crate::ReflectedEnum,
    implemented: &BTreeMap<String, Method>,
) -> Result<(model::Attribute, TokenStream), Error> {
    // A nullable `DOMString?` reports the spec's "no state" as `null`; a plain
    // `DOMString` reports it as the empty string. Both are enumerated
    // reflection shapes.
    let nullable = match &member.type_.type_ {
        Type::Single(SingleType::NonAny(NonAnyType::DOMString(item))) => item.q_mark.is_some(),
        _ => {
            return Err(Error(format!(
                "{}: a reflected enumerated attribute must be DOMString",
                member.identifier.0
            )));
        }
    };
    validate_attribute_attributes(member.attributes.as_ref())?;
    validate_argument_attributes(member.type_.attributes.as_ref())?;
    let getter_name = format!("get_{}", snake_case(member.identifier.0));
    if implemented.contains_key(&getter_name) {
        return Err(Error(format!(
            "{}: the metadata table generates this getter; remove the hand-written one",
            member.identifier.0
        )));
    }
    let writable = member.readonly.is_none();
    let setter_name = format!("set_{}", snake_case(member.identifier.0));
    if writable && implemented.contains_key(&setter_name) {
        return Err(Error(format!(
            "{}: the metadata table generates this setter; remove the hand-written one",
            member.identifier.0
        )));
    }
    let type_path = {
        let raw = syn::parse_str::<syn::Path>(&entry.type_path)
            .map_err(|error| Error(format!("{}: invalid type path: {error}", entry.attribute)))?;
        // A bare name is a type in the renderer's `enumerated` module; anything
        // with a path (such as `net::ReferrerPolicy`) is used as written.
        if raw.segments.len() == 1 {
            let name = &raw.segments[0].ident;
            syn::parse_str::<syn::Path>(&format!("crate::js::bindings::enumerated::{name}"))
                .expect("a bare identifier is a valid path")
        } else {
            raw
        }
    };
    let type_text = quote!(#type_path).to_string();
    let default = |raw: &Option<String>| -> Result<Option<syn::Path>, Error> {
        raw.as_ref()
            .map(|raw| {
                syn::parse_str::<syn::Path>(&format!("{type_text}::{raw}")).map_err(|error| {
                    Error(format!(
                        "{}: invalid default `{raw}`: {error}",
                        entry.attribute
                    ))
                })
            })
            .transpose()
    };
    let getter = format_ident!("{getter_name}");
    let setter = writable.then(|| model::Setter::Reflect {
        content: entry.content.clone(),
    });
    let attribute = model::Attribute {
        exposure,
        name: member.identifier.0.into(),
        rust: getter,
        return_type: if nullable {
            ReturnType::NullableString
        } else {
            ReturnType::String
        },
        mapping: GetterMapping::ReflectEnumerated {
            content: entry.content.clone(),
            type_path,
            missing: default(&entry.missing)?,
            invalid: default(&entry.invalid)?,
        },
        setter,
        legacy_null_to_empty: false,
        reactions: has_no_args(member.attributes.as_ref(), "CEReactions"),
    };
    Ok((attribute, quote! {}))
}

fn lower_attribute(
    database: &Database<'_>,
    interface: &str,
    member: &weedle::interface::AttributeInterfaceMember<'_>,
    exposure: Exposure,
    implemented: &BTreeMap<String, Method>,
    reflected: &[crate::ReflectedEnum],
) -> Result<Option<(model::Attribute, TokenStream)>, Error> {
    // A reflected enumerated attribute is listed in tinybrowser's metadata
    // table, because the IDL extract carries the member but not its keyword
    // set or its two defaults. The generated getter is the only definition,
    // so a hand-written one is a mistake the compiler reports.
    if let Some(lowered) =
        reflected_enumerated_attribute(interface, member, exposure, implemented, reflected)?
    {
        return Ok(Some(lowered));
    }
    // Reflected attributes install without implementation methods, so
    // resolve them before the implemented-member checks below. Only shapes
    // the generator auto-generates (plain string/boolean/`USVString`) take
    // this path; anything else falls through to the normal method path when
    // implemented, so hand-written getters encode the spec algorithm. Other
    // members keep skip-if-unimplemented semantics.
    if let Some(content) = reflect_content(member.attributes.as_ref(), member.identifier.0)
        && let Some(lowered) = lower_reflect_attribute(member, exposure, &content, implemented)?
    {
        return Ok(Some(lowered));
    }
    // Special shapes stay absent rather than falling through: a plain
    // method installation would lose `SameObject`/`PutForwards` semantics.
    // This covers both `[Reflect]` and `[ReflectURL]` shapes. An implemented
    // getter asks for the member explicitly and brings those semantics with
    // it — `HTMLIFrameElement.sandbox` returns the `DOMTokenList` over its
    // content attribute, which no generated reflection can build.
    let implemented_getter =
        implemented.contains_key(&format!("get_{}", snake_case(member.identifier.0)));
    if !implemented_getter
        && is_special_reflect_shape(member)
        && (reflect_content(member.attributes.as_ref(), member.identifier.0).is_some()
            || reflect_url_content(member.attributes.as_ref(), member.identifier.0).is_some())
    {
        return Ok(None);
    }
    reject_legacy_non_string_reflect(member)?;
    // `ReflectSetter` reflects on set while the getter stays custom: the
    // implementation provides the getter, the generator owns the setter.
    if let Some(content) = reflect_setter_content(member.attributes.as_ref(), member.identifier.0) {
        return lower_reflect_setter_attribute(database, member, exposure, &content, implemented);
    }
    // `ReflectURL` resolves the content attribute against the document base
    // on get and reflects plainly on set, fully generated. Non-`USVString`
    // shapes (like `object.codeBase`) fall through for hand implementation.
    if let Some(content) = reflect_url_content(member.attributes.as_ref(), member.identifier.0)
        && let Some(lowered) = lower_reflect_url_attribute(member, exposure, &content, implemented)?
    {
        return Ok(Some(lowered));
    }
    lower_method_attribute(database, member, exposure, implemented)
}

/// Lower a hand-implemented attribute: the trait method and its signature.
fn lower_method_attribute(
    database: &Database<'_>,
    member: &weedle::interface::AttributeInterfaceMember<'_>,
    exposure: Exposure,
    implemented: &BTreeMap<String, Method>,
) -> Result<Option<(model::Attribute, TokenStream)>, Error> {
    let getter_name = format!("get_{}", snake_case(member.identifier.0));
    let setter_name = format!("set_{}", snake_case(member.identifier.0));
    let readable = implemented.contains_key(&getter_name);
    let writable = implemented.contains_key(&setter_name);
    if !readable && !writable {
        return Ok(None);
    }
    if !readable || writable != member.readonly.is_none() {
        return Err(Error(format!(
            "{}: implementation does not match attribute mutability",
            member.identifier.0
        )));
    }
    check_method_signature(implemented, &getter_name, 0, "attribute getter")?;
    if writable {
        check_method_signature(implemented, &setter_name, 1, "attribute setter")?;
    }
    if member.modifier.is_some()
        && !matches!(
            member.modifier,
            Some(weedle::interface::StringifierOrInheritOrStatic::Stringifier(_))
        )
    {
        return Err(Error(
            "native special attributes are not supported yet".into(),
        ));
    }
    validate_attribute_attributes(member.attributes.as_ref())?;
    validate_argument_attributes(member.type_.attributes.as_ref())?;
    let reactions = has_no_args(member.attributes.as_ref(), "CEReactions");
    let legacy_null_to_empty = has_no_args(member.attributes.as_ref(), "LegacyNullToEmptyString")
        || has_no_args(member.type_.attributes.as_ref(), "LegacyNullToEmptyString")
        || match &member.type_.type_ {
            Type::Union(union_) => {
                union_member_has_no_args(&union_.type_.body.list, "LegacyNullToEmptyString")
            }
            Type::Single(_) => false,
        };
    let type_ = match native_type(database, &member.type_.type_, &mut BTreeSet::new())? {
        // Getter dispatch hands a platform object or its null directly to JS.
        ReturnType::Node | ReturnType::NullableNode | ReturnType::NullableDocumentType => {
            ReturnType::PlatformObject
        }
        // A getter returns its value without conversion, so a union of
        // platform objects passes through as the value or null.
        ReturnType::Union(_, members) | ReturnType::NullableUnion(_, members)
            if members
                .iter()
                .all(|member| matches!(member.type_, model::UnionMemberType::Interface { .. })) =>
        {
            ReturnType::PlatformObject
        }
        ReturnType::PromiseUndefined => {
            return Err(Error("promise attributes are not supported yet".into()));
        }
        type_ => type_,
    };
    if member.modifier.is_some() && !matches!(type_, ReturnType::String) {
        return Err(Error(
            "native attribute stringifiers require DOMString".into(),
        ));
    }
    let result = attribute_result(&type_)?;
    let getter = format_ident!("{getter_name}");
    let mut signature = quote! { fn #getter(&self, ctx: &Ctx<'js>) -> Result<#result>; };
    let setter = attribute_setter(member, &type_, writable, &setter_name, &mut signature)?;
    let attribute = model::Attribute {
        exposure,
        name: member.identifier.0.into(),
        rust: getter,
        return_type: type_,
        mapping: GetterMapping::Method,
        setter,
        legacy_null_to_empty,
        reactions,
    };
    Ok(Some((attribute, signature)))
}

/// The Rust getter return type for one lowered attribute type.
fn attribute_result(type_: &ReturnType) -> Result<TokenStream, Error> {
    Ok(match type_ {
        ReturnType::String => quote! { rquickjs::String<'js> },
        // USVString getters hand a code-unit string to the lossy conversion.
        ReturnType::UsvString => quote! { crate::dom_string::DomString },
        ReturnType::NullableString => quote! { Option<rquickjs::String<'js>> },
        ReturnType::Boolean => quote! { bool },
        ReturnType::UnsignedShort => quote! { u16 },
        // Plain-method `unsigned long` getters answer `usize` (they convert
        // fallibly from the parsed `u32`, saturating past the address space),
        // while `[ReflectSetter]` getters answer the converted `u32`
        // directly; the setter conversion is identical in both paths.
        ReturnType::UnsignedLong => quote! { usize },
        ReturnType::Long => quote! { i32 },
        ReturnType::NullableUnsignedLong => quote! { Option<u32> },
        ReturnType::Double | ReturnType::RestrictedDouble => quote! { f64 },
        ReturnType::PlatformObject => quote! { Value<'js> },
        ReturnType::Enumeration(name) => {
            let name = format_ident!("{name}");
            quote! { #name }
        }
        _ => return Err(Error("native attribute type is not supported yet".into())),
    })
}

/// An attribute's setter: an implemented method, a generated `[PutForwards]`
/// forwarding assignment, or nothing for readonly attributes. A hand-written
/// setter alongside `[PutForwards]` fails the build, and forwarding requires
/// a readonly platform object
/// (<https://webidl.spec.whatwg.org/#PutForwards>).
fn attribute_setter(
    member: &weedle::interface::AttributeInterfaceMember<'_>,
    type_: &ReturnType,
    writable: bool,
    setter_name: &str,
    signature: &mut TokenStream,
) -> Result<Option<model::Setter>, Error> {
    let put_forwards = put_forwards_target(member.attributes.as_ref());
    if put_forwards.is_some() {
        if writable {
            return Err(Error(format!(
                "{}: PutForwards forbids an implemented setter",
                member.identifier.0
            )));
        }
        if !matches!(type_, ReturnType::PlatformObject) {
            return Err(Error(format!(
                "{}: PutForwards requires a readonly platform object",
                member.identifier.0
            )));
        }
    }
    if writable {
        let setter = format_ident!("{setter_name}");
        let parameter = setter_parameter(type_)?;
        *signature = quote! { #signature fn #setter(&self, ctx: &Ctx<'js>, value: #parameter) -> Result<()>; };
        return Ok(Some(model::Setter::Method { rust: setter }));
    }
    Ok(put_forwards.map(|target| model::Setter::PutForwards {
        target: target.into(),
        nullable: matches!(
            &member.type_.type_,
            Type::Single(SingleType::NonAny(NonAnyType::Identifier(type_)))
                if type_.q_mark.is_some()
        ),
    }))
}

/// The `[PutForwards]` member name, if the attribute forwards assignment to
/// it (<https://webidl.spec.whatwg.org/#PutForwards>).
fn put_forwards_target<'a>(attributes: Option<&ExtendedAttributeList<'a>>) -> Option<&'a str> {
    attributes?
        .body
        .list
        .iter()
        .find_map(|attribute| match attribute {
            ExtendedAttribute::Ident(attribute) if attribute.lhs_identifier.0 == "PutForwards" => {
                Some(attribute.rhs.0)
            }
            _ => None,
        })
}

/// The content attribute a `[ReflectURL]` member mirrors: the `ReflectURL`
/// value or the lowercase IDL name. Only `USVString` is auto-generated;
/// other shapes (like `object.codeBase`, a `DOMString`) fall through to
/// hand implementation in `lower_attribute`.
fn reflect_url_content(
    attributes: Option<&ExtendedAttributeList<'_>>,
    idl_name: &str,
) -> Option<String> {
    let attributes = attributes?;
    for attribute in &attributes.body.list {
        match attribute {
            ExtendedAttribute::NoArgs(item) if item.0.0 == "ReflectURL" => {
                return Some(idl_name.to_ascii_lowercase());
            }
            _ => {}
        }
    }
    None
}

/// The content attribute a `[Reflect]` member mirrors: the `Reflect` value
/// or the lowercase IDL name, following Chromium's key derivation.
/// Numeric shapes are not auto-generated (they fall through to hand-written
/// methods); `ReflectURL` has its own lowering below.
fn reflect_content(
    attributes: Option<&ExtendedAttributeList<'_>>,
    idl_name: &str,
) -> Option<String> {
    let attributes = attributes?;
    for attribute in &attributes.body.list {
        match attribute {
            ExtendedAttribute::NoArgs(item) if item.0.0 == "Reflect" => {
                return Some(idl_name.to_ascii_lowercase());
            }
            ExtendedAttribute::String(item) if item.lhs_identifier.0 == "Reflect" => {
                return Some(item.rhs.0.to_string());
            }
            _ => {}
        }
    }
    None
}

/// The content attribute a `[ReflectSetter]` member writes: the value or
/// the lowercase IDL name. Plain `[Reflect]` is handled separately; other
/// parameterized forms stay unsupported.
fn reflect_setter_content(
    attributes: Option<&ExtendedAttributeList<'_>>,
    idl_name: &str,
) -> Option<String> {
    let attributes = attributes?;
    for attribute in &attributes.body.list {
        match attribute {
            ExtendedAttribute::NoArgs(item) if item.0.0 == "ReflectSetter" => {
                return Some(idl_name.to_ascii_lowercase());
            }
            ExtendedAttribute::String(item) if item.lhs_identifier.0 == "ReflectSetter" => {
                return Some(item.rhs.0.to_string());
            }
            _ => {}
        }
    }
    None
}

/// Lower a `[ReflectSetter]` attribute: the getter is a normal implemented
/// method, the setter writes the content attribute through the shared
/// helper. The implementation must provide the getter but not the setter.
fn lower_reflect_setter_attribute(
    database: &Database<'_>,
    member: &weedle::interface::AttributeInterfaceMember<'_>,
    exposure: Exposure,
    content: &str,
    implemented: &BTreeMap<String, Method>,
) -> Result<Option<(model::Attribute, TokenStream)>, Error> {
    let getter_name = format!("get_{}", snake_case(member.identifier.0));
    let setter_name = format!("set_{}", snake_case(member.identifier.0));
    if implemented.contains_key(&setter_name) {
        return Err(Error(format!(
            "{}: reflected setters are generated, not implemented",
            member.identifier.0
        )));
    }
    if !implemented.contains_key(&getter_name) {
        return Ok(None);
    }
    if member.modifier.is_some()
        && !matches!(
            member.modifier,
            Some(weedle::interface::StringifierOrInheritOrStatic::Stringifier(_))
        )
    {
        return Err(Error(
            "reflect setter special attributes are not supported yet".into(),
        ));
    }
    if member.readonly.is_some() {
        return Err(Error("reflect setters require a writable attribute".into()));
    }
    validate_argument_attributes(member.type_.attributes.as_ref())?;
    validate_attribute_attributes(member.attributes.as_ref())?;
    if has_no_args(member.attributes.as_ref(), "SameObject")
        || has_no_args(member.attributes.as_ref(), "PutForwards")
    {
        return Err(Error(format!(
            "{}: reflect setter semantics are not supported yet",
            member.identifier.0
        )));
    }
    // The setter writes a string, so only string-family getters pair with
    // it. `long`, `unsigned long`, and `double` pair too: the setter
    // converts the number and writes its decimal form (used by `tabindex`,
    // dimension attributes, and meter/progress values).
    let type_ = native_type(database, &member.type_.type_, &mut BTreeSet::new())?;
    if !matches!(
        type_,
        ReturnType::String
            | ReturnType::UsvString
            | ReturnType::Long
            | ReturnType::UnsignedLong
            | ReturnType::Double
            | ReturnType::RestrictedDouble
    ) {
        return Err(Error(format!(
            "{}: reflect setter type is not supported yet",
            member.identifier.0
        )));
    }
    if !implemented[&getter_name].has_self || implemented[&getter_name].parameters != 0 {
        return Err(Error(format!(
            "{}: attribute getter must take only self and ctx",
            member.identifier.0
        )));
    }
    let getter = format_ident!("{getter_name}");
    let legacy_null_to_empty = has_no_args(member.attributes.as_ref(), "LegacyNullToEmptyString")
        || has_no_args(member.type_.attributes.as_ref(), "LegacyNullToEmptyString");
    // Like operation arguments, `[LegacyNullToEmptyString]` requires
    // `DOMString`: on numerics it would otherwise be silently dropped by
    // the generated setter.
    if legacy_null_to_empty && !matches!(type_, ReturnType::String) {
        return Err(Error(format!(
            "{}: LegacyNullToEmptyString requires DOMString",
            member.identifier.0
        )));
    }
    // The generated `unsigned long` setter writes 0 for out-of-range
    // values, which is only correct for a zero default: a non-zero
    // `ReflectDefault` would be silently ignored, so fail the build
    // instead of emitting the wrong fallback.
    if matches!(type_, ReturnType::UnsignedLong)
        && reflect_default(member.attributes.as_ref())?.is_some_and(|default| default != 0.0)
    {
        return Err(Error(format!(
            "{}: ReflectSetter on unsigned long needs its ReflectDefault plumbed",
            member.identifier.0
        )));
    }
    let signature = reflect_setter_signature(member.identifier.0, &getter, &type_)?;
    let attribute = model::Attribute {
        exposure,
        name: member.identifier.0.into(),
        rust: getter,
        return_type: type_,
        mapping: GetterMapping::Method,
        setter: Some(model::Setter::Reflect {
            content: content.into(),
        }),
        legacy_null_to_empty,
        reactions: has_no_args(member.attributes.as_ref(), "CEReactions"),
    };
    Ok(Some((attribute, signature)))
}

/// The numeric `ReflectDefault` on a member's extended attributes, if
/// present. Integer defaults (`ReflectDefault=20`) parse as `Ident`,
/// floating ones (`ReflectDefault=1.0`) as `Decimal` (see the weedle4 note
/// in [`validate_attribute_attributes`]).
fn reflect_default(attributes: Option<&ExtendedAttributeList<'_>>) -> Result<Option<f64>, Error> {
    let Some(attributes) = attributes else {
        return Ok(None);
    };
    for attribute in &attributes.body.list {
        match attribute {
            ExtendedAttribute::Ident(item) if item.lhs_identifier.0 == "ReflectDefault" => {
                return item.rhs.0.parse().map(Some).map_err(|_| {
                    Error(format!(
                        "ReflectDefault needs a numeric default, got `{}`",
                        item.rhs.0
                    ))
                });
            }
            ExtendedAttribute::Decimal(item) if item.lhs_identifier.0 == "ReflectDefault" => {
                return item.rhs.0.parse().map(Some).map_err(|_| {
                    Error(format!(
                        "ReflectDefault needs a numeric default, got `{}`",
                        item.rhs.0
                    ))
                });
            }
            _ => {}
        }
    }
    Ok(None)
}

/// Reject an implementation method whose receiver or parameter count does
/// not match the IDL member kind.
fn check_method_signature(
    implemented: &BTreeMap<String, Method>,
    name: &str,
    parameters: usize,
    kind: &str,
) -> Result<(), Error> {
    let method = &implemented[name];
    if !method.has_self || method.parameters != parameters {
        return Err(Error(format!(
            "{name}: {kind} must take only self and ctx plus {parameters} value(s)"
        )));
    }
    Ok(())
}

/// The trait signature for a `[ReflectSetter]` getter: the getter stays
/// hand-written while the setter is generated, so each supported type
/// names its own return.
fn reflect_setter_signature(
    attribute: &str,
    getter: &proc_macro2::Ident,
    type_: &ReturnType,
) -> Result<proc_macro2::TokenStream, Error> {
    let returns = match type_ {
        ReturnType::UsvString => quote! { crate::dom_string::DomString },
        ReturnType::String => quote! { rquickjs::String<'js> },
        ReturnType::Long => quote! { i32 },
        ReturnType::UnsignedLong => quote! { u32 },
        ReturnType::Double | ReturnType::RestrictedDouble => quote! { f64 },
        _ => {
            return Err(Error(format!(
                "{attribute}: reflect setter type has no getter signature"
            )));
        }
    };
    Ok(quote! { fn #getter(&self, ctx: &Ctx<'js>) -> Result<#returns>; })
}

/// Lower a `[Reflect]` attribute to generated content-attribute access.
/// The trait carries no method and the implementation provides none: like
/// Chromium's generated reflectors, the binding is complete by itself.
/// Claiming an auto-generatable reflected name with implementation methods
/// fails the build, so a custom algorithm cannot silently replace the
/// reflection. Shapes the generator does not auto-generate (numeric
/// reflection with spec-prose defaults, `LegacyNullToEmptyString` pairs
/// handled below) fall through to the normal method path when implemented,
/// so hand-written getters encode the spec algorithm.
fn lower_reflect_attribute(
    member: &weedle::interface::AttributeInterfaceMember<'_>,
    exposure: Exposure,
    content: &str,
    implemented: &BTreeMap<String, Method>,
) -> Result<Option<(model::Attribute, TokenStream)>, Error> {
    // Auto-generatable shapes first: plain string/boolean, plus `USVString`
    // under a plain `[Reflect]` (which reflects the raw value exactly like
    // `DOMString`: `a.ping`, `img.srcset`). Anything else falls through
    // for hand implementation instead of erroring here.
    let auto = matches!(
        &member.type_.type_,
        Type::Single(SingleType::NonAny(NonAnyType::DOMString(item))) if item.q_mark.is_none()
    ) || matches!(
        &member.type_.type_,
        Type::Single(SingleType::NonAny(NonAnyType::USVString(item))) if item.q_mark.is_none()
    ) || matches!(
        &member.type_.type_,
        Type::Single(SingleType::NonAny(NonAnyType::Boolean(item))) if item.q_mark.is_none()
    );
    if !auto {
        return Ok(None);
    }
    let getter_name = format!("get_{}", snake_case(member.identifier.0));
    let setter_name = format!("set_{}", snake_case(member.identifier.0));
    if implemented.contains_key(&getter_name) || implemented.contains_key(&setter_name) {
        return Err(Error(format!(
            "{}: reflected attributes are generated, not implemented",
            member.identifier.0
        )));
    }
    if member.modifier.is_some()
        && !matches!(
            member.modifier,
            Some(weedle::interface::StringifierOrInheritOrStatic::Stringifier(_))
        )
    {
        return Ok(None);
    }
    let writable = member.readonly.is_none();
    validate_argument_attributes(member.type_.attributes.as_ref())?;
    if has_no_args(member.attributes.as_ref(), "SameObject")
        || has_no_args(member.attributes.as_ref(), "PutForwards")
    {
        return Ok(None);
    }
    // `[LegacyNullToEmptyString]` reflection is auto-generatable for
    // strings: the getter is plain reflection and the setter converts null
    // to the empty string. On booleans it is invalid IDL.
    let legacy_null_to_empty = has_no_args(member.attributes.as_ref(), "LegacyNullToEmptyString")
        || has_no_args(member.type_.attributes.as_ref(), "LegacyNullToEmptyString");
    // Only plain string, boolean, and `USVString` reflection so far.
    // `USVString` under a plain `[Reflect]` reads the raw content attribute
    // exactly like `DOMString` (`a.ping`, `img.srcset`): stored content is
    // a Rust `String`, which cannot hold lone surrogates, so no scalar
    // replacement can be hiding in it; only the setter argument still
    // converts as USV (<https://webidl.spec.whatwg.org/#es-USVString>).
    // The type needs no database lookup, keeping reflected members
    // independent of typedefs.
    let type_ = match &member.type_.type_ {
        Type::Single(SingleType::NonAny(NonAnyType::DOMString(item))) if item.q_mark.is_none() => {
            ReturnType::String
        }
        Type::Single(SingleType::NonAny(NonAnyType::USVString(item))) if item.q_mark.is_none() => {
            // Like operation arguments, `[LegacyNullToEmptyString]` requires
            // `DOMString`.
            if legacy_null_to_empty {
                return Err(Error(format!(
                    "{}: LegacyNullToEmptyString requires DOMString",
                    member.identifier.0
                )));
            }
            ReturnType::UsvString
        }
        Type::Single(SingleType::NonAny(NonAnyType::Boolean(item))) if item.q_mark.is_none() => {
            if legacy_null_to_empty {
                return Err(Error(format!(
                    "{}: LegacyNullToEmptyString requires DOMString",
                    member.identifier.0
                )));
            }
            ReturnType::Boolean
        }
        _ => return Ok(None),
    };
    // The shape is supported and installing, so unknown attributes fail
    // instead of silently changing the reflection.
    validate_attribute_attributes(member.attributes.as_ref())?;
    let getter = format_ident!("{getter_name}");
    let setter = if writable {
        Some(model::Setter::Reflect {
            content: content.into(),
        })
    } else {
        None
    };
    let attribute = model::Attribute {
        exposure,
        name: member.identifier.0.into(),
        rust: getter,
        return_type: type_,
        mapping: GetterMapping::Reflect {
            content: content.into(),
        },
        setter,
        legacy_null_to_empty,
        reactions: has_no_args(member.attributes.as_ref(), "CEReactions"),
    };
    Ok(Some((attribute, quote! {})))
}

/// Lower a `[ReflectURL]` attribute to generated content-attribute access
/// with URL resolution. Like `[Reflect]`, the trait carries no method and
/// the implementation provides none for auto-generatable shapes; other
/// shapes fall through for hand implementation.
fn lower_reflect_url_attribute(
    member: &weedle::interface::AttributeInterfaceMember<'_>,
    exposure: Exposure,
    content: &str,
    implemented: &BTreeMap<String, Method>,
) -> Result<Option<(model::Attribute, TokenStream)>, Error> {
    // URL reflection resolves a string against the document base; only
    // `USVString` is auto-generated. Anything else falls through to hand
    // implementation (like `object.codeBase`, a `DOMString`).
    let auto = matches!(
        &member.type_.type_,
        Type::Single(SingleType::NonAny(NonAnyType::USVString(item))) if item.q_mark.is_none()
    );
    if !auto {
        return Ok(None);
    }
    let getter_name = format!("get_{}", snake_case(member.identifier.0));
    let setter_name = format!("set_{}", snake_case(member.identifier.0));
    if implemented.contains_key(&getter_name) || implemented.contains_key(&setter_name) {
        return Err(Error(format!(
            "{}: URL-reflected attributes are generated, not implemented",
            member.identifier.0
        )));
    }
    if member.modifier.is_some()
        && !matches!(
            member.modifier,
            Some(weedle::interface::StringifierOrInheritOrStatic::Stringifier(_))
        )
    {
        return Ok(None);
    }
    let writable = member.readonly.is_none();
    validate_argument_attributes(member.type_.attributes.as_ref())?;
    if has_no_args(member.attributes.as_ref(), "SameObject")
        || has_no_args(member.attributes.as_ref(), "PutForwards")
        || has_no_args(member.attributes.as_ref(), "LegacyNullToEmptyString")
        || has_no_args(member.type_.attributes.as_ref(), "LegacyNullToEmptyString")
    {
        return Ok(None);
    }
    validate_attribute_attributes(member.attributes.as_ref())?;
    let getter = format_ident!("{getter_name}");
    let setter = if writable {
        Some(model::Setter::Reflect {
            content: content.into(),
        })
    } else {
        None
    };
    let attribute = model::Attribute {
        exposure,
        name: member.identifier.0.into(),
        rust: getter,
        return_type: ReturnType::UsvString,
        mapping: GetterMapping::ReflectUrl {
            content: content.into(),
        },
        setter,
        legacy_null_to_empty: false,
        reactions: has_no_args(member.attributes.as_ref(), "CEReactions"),
    };
    Ok(Some((attribute, quote! {})))
}

/// The Rust parameter type for one lowered attribute setter, mirroring
/// `emit`'s setter conversions.
fn setter_parameter(type_: &ReturnType) -> Result<TokenStream, Error> {
    Ok(match type_ {
        // `USVString` arrives as a converted string; only the argument
        // conversion differs (see `setter_value_conversion`).
        ReturnType::String | ReturnType::UsvString => quote! { rquickjs::String<'js> },
        ReturnType::NullableString => quote! { Option<rquickjs::String<'js>> },
        ReturnType::Boolean => quote! { bool },
        ReturnType::UnsignedLong => quote! { u32 },
        ReturnType::Long => quote! { i32 },
        ReturnType::NullableUnsignedLong => quote! { Option<u32> },
        ReturnType::Double | ReturnType::RestrictedDouble => quote! { f64 },
        // A platform-object setter takes the value as-is; the method owns
        // the conversion.
        ReturnType::PlatformObject => quote! { Value<'js> },
        ReturnType::Enumeration(name) => {
            let name = format_ident!("{name}");
            quote! { #name }
        }
        _ => {
            return Err(Error(
                "native attribute setter type is not supported yet".into(),
            ));
        }
    })
}

fn lower_constructor(
    database: &Database<'_>,
    member: &weedle::interface::ConstructorInterfaceMember<'_>,
) -> Result<(model::Constructor, TokenStream), Error> {
    validate_empty_attributes(member.attributes.as_ref())?;
    let (arguments, parameters) = constructor_arguments(database, member)?;
    let method = format_ident!("constructor");
    let constructor = model::Constructor {
        rust: method.clone(),
        arguments,
    };
    let signature =
        quote! { fn #method(ctx: &Ctx<'js>, #(#parameters),*) -> Result<Self> where Self: Sized; };
    Ok((constructor, signature))
}

fn constructor_arguments(
    database: &Database<'_>,
    member: &weedle::interface::ConstructorInterfaceMember<'_>,
) -> Result<(Vec<model::ConstructorArgument>, Vec<TokenStream>), Error> {
    let mut arguments = Vec::new();
    let mut parameters = Vec::new();
    for (index, argument) in member.args.body.list.iter().enumerate() {
        let Argument::Single(argument) = argument else {
            return Err(Error(
                "native variadic constructors are not supported yet".into(),
            ));
        };
        validate_empty_attributes(argument.attributes.as_ref())?;
        validate_empty_attributes(argument.type_.attributes.as_ref())?;
        let type_ = native_type(database, &argument.type_.type_, &mut BTreeSet::new())?;
        let (kind, parameter) = match type_ {
            ReturnType::String => {
                let default = match (&argument.optional, &argument.default) {
                    (None, None) => None,
                    (Some(_), Some(default)) => {
                        let DefaultValue::String(default) = &default.value else {
                            return Err(Error(
                                "native constructor default is not supported yet".into(),
                            ));
                        };
                        Some(default.0.into())
                    }
                    _ => {
                        return Err(Error(
                            "native constructor optionality is not supported yet".into(),
                        ));
                    }
                };
                (
                    ConstructorArgumentKind::String { default },
                    quote! { rquickjs::String<'js> },
                )
            }
            ReturnType::Callback => match (&argument.optional, &argument.default) {
                (None, None) => (
                    ConstructorArgumentKind::Callback,
                    quote! { rquickjs::Function<'js> },
                ),
                _ => {
                    return Err(Error(
                        "native optional callback arguments are not supported yet".into(),
                    ));
                }
            },
            ReturnType::Dictionary(name) => match (&argument.optional, &argument.default) {
                (Some(_), Some(default))
                    if matches!(default.value, DefaultValue::EmptyDictionary(_)) =>
                {
                    let struct_name = format_ident!("{name}");
                    (
                        ConstructorArgumentKind::Dictionary(name),
                        quote! { #struct_name },
                    )
                }
                _ => {
                    return Err(Error(
                        "native constructor dictionaries need a trailing empty-object default"
                            .into(),
                    ));
                }
            },
            _ => {
                return Err(Error(
                    "native constructor argument type is not supported yet".into(),
                ));
            }
        };
        arguments.push(model::ConstructorArgument { kind });
        let name = format_ident!("arg_{index}");
        parameters.push(quote! { #name: #parameter });
    }
    Ok((arguments, parameters))
}

pub(crate) fn native_type(
    database: &Database<'_>,
    type_: &Type<'_>,
    visited: &mut BTreeSet<String>,
) -> Result<ReturnType, Error> {
    match type_ {
        Type::Single(SingleType::NonAny(NonAnyType::Uint8Array(item))) if item.q_mark.is_none() => {
            Ok(ReturnType::Uint8Array)
        }
        Type::Single(SingleType::NonAny(NonAnyType::DOMString(item))) => {
            Ok(if item.q_mark.is_some() {
                ReturnType::NullableString
            } else {
                ReturnType::String
            })
        }
        Type::Single(SingleType::NonAny(NonAnyType::USVString(item))) if item.q_mark.is_none() => {
            Ok(ReturnType::UsvString)
        }
        Type::Single(SingleType::NonAny(NonAnyType::Boolean(item))) if item.q_mark.is_none() => {
            Ok(ReturnType::Boolean)
        }
        Type::Single(SingleType::NonAny(NonAnyType::Integer(item)))
            if item.q_mark.is_none()
                && matches!(item.type_, IntegerType::Short(item) if item.unsigned.is_some()) =>
        {
            Ok(ReturnType::UnsignedShort)
        }
        Type::Single(SingleType::NonAny(NonAnyType::Integer(item)))
            if item.q_mark.is_none()
                && matches!(item.type_, IntegerType::Short(item) if item.unsigned.is_none()) =>
        {
            Ok(ReturnType::Short)
        }
        Type::Single(SingleType::NonAny(NonAnyType::Integer(item)))
            if item.q_mark.is_none()
                && matches!(item.type_, IntegerType::LongLong(item) if item.unsigned.is_some()) =>
        {
            Ok(ReturnType::UnsignedLongLong)
        }
        Type::Single(SingleType::NonAny(NonAnyType::Integer(item)))
            if item.q_mark.is_none()
                && matches!(item.type_, IntegerType::Long(item) if item.unsigned.is_some()) =>
        {
            Ok(ReturnType::UnsignedLong)
        }
        Type::Single(SingleType::NonAny(NonAnyType::Integer(item)))
            if item.q_mark.is_none()
                && matches!(item.type_, IntegerType::Long(item) if item.unsigned.is_none()) =>
        {
            Ok(ReturnType::Long)
        }
        Type::Single(SingleType::NonAny(NonAnyType::Integer(item)))
            if item.q_mark.is_some()
                && matches!(item.type_, IntegerType::Long(item) if item.unsigned.is_some()) =>
        {
            Ok(ReturnType::NullableUnsignedLong)
        }
        Type::Single(SingleType::NonAny(NonAnyType::Sequence(item))) if item.q_mark.is_none() => {
            native_sequence(database, item)
        }
        Type::Single(SingleType::NonAny(NonAnyType::FloatingPoint(item)))
            if item.q_mark.is_none()
                && matches!(
                    item.type_,
                    weedle::types::FloatingPointType::Double(double) if double.unrestricted.is_some()
                ) =>
        {
            // `Coerced<f64>` is `ToNumber`, which admits NaN and infinities:
            // exactly `unrestricted double`.
            Ok(ReturnType::Double)
        }
        Type::Single(SingleType::NonAny(NonAnyType::FloatingPoint(item)))
            if item.q_mark.is_none()
                && matches!(item.type_, weedle::types::FloatingPointType::Double(_)) =>
        {
            // Restricted `double` rejects NaN and infinities on conversion
            // (<https://webidl.spec.whatwg.org/#es-double>).
            Ok(ReturnType::RestrictedDouble)
        }
        Type::Single(SingleType::NonAny(NonAnyType::Promise(promise)))
            if matches!(
                &*promise.generics.body,
                weedle::types::ReturnType::Undefined(_)
            ) =>
        {
            Ok(ReturnType::PromiseUndefined)
        }
        Type::Single(SingleType::NonAny(NonAnyType::Identifier(item))) => {
            native_named(database, item.type_.0, item.q_mark.is_some(), visited)
        }
        Type::Single(SingleType::Any(_)) => Ok(ReturnType::Any),
        Type::Union(union_) => lower_union(database, union_),
        Type::Single(_) => Err(Error(format!(
            "native type is not supported yet: {type_:?}"
        ))),
    }
}

/// A `sequence<T>` member type. Only the element shapes with a generated
/// conversion are accepted.
fn native_sequence(
    database: &Database<'_>,
    item: &weedle::types::MayBeNull<weedle::types::SequenceType<'_>>,
) -> Result<ReturnType, Error> {
    let element = &item.type_.generics.body;
    validate_empty_attributes(element.attributes.as_ref())?;
    match &element.type_ {
        Type::Single(SingleType::NonAny(NonAnyType::DOMString(element)))
            if element.q_mark.is_none() =>
        {
            Ok(ReturnType::StringSequence)
        }
        Type::Single(SingleType::NonAny(NonAnyType::Identifier(element)))
            if element.q_mark.is_none()
                && matches!(
                    database.definition(element.type_.0),
                    Some(weedle::Definition::Interface(_))
                ) =>
        {
            Ok(ReturnType::InterfaceSequence)
        }
        _ => Err(Error(
            "only sequences of known interfaces or DOMString are supported yet".into(),
        )),
    }
}

/// A named type: a platform object, a typedef to follow, or a declared
/// callback, dictionary, or enumeration.
fn native_named(
    database: &Database<'_>,
    name: &str,
    nullable: bool,
    visited: &mut BTreeSet<String>,
) -> Result<ReturnType, Error> {
    if let Some(interface) = native_interface(database, name, nullable) {
        return Ok(interface);
    }
    if !visited.insert(name.into()) {
        return Err(Error(format!("typedef cycle at {name}")));
    }
    match database.definition(name) {
        Some(weedle::Definition::Typedef(definition)) => {
            validate_empty_attributes(definition.attributes.as_ref())?;
            validate_empty_attributes(definition.type_.attributes.as_ref())?;
            let resolved = native_type(database, &definition.type_.type_, visited)?;
            // Nullability applies after resolving the typedef, so a nullable
            // typedef over a union or string keeps its null conversion.
            if nullable {
                make_nullable(resolved)
            } else {
                Ok(resolved)
            }
        }
        Some(_) if nullable => Err(Error(format!(
            "nullable native type {name} is not supported yet"
        ))),
        Some(weedle::Definition::Callback(definition)) => {
            model::Callback::parse(definition)?;
            Ok(ReturnType::Callback)
        }
        Some(weedle::Definition::Dictionary(_)) => Ok(ReturnType::Dictionary(name.into())),
        Some(weedle::Definition::Enum(_)) => Ok(ReturnType::Enumeration(name.into())),
        _ => Err(Error(format!("native type {name} is not supported yet"))),
    }
}

/// Apply `?` to a resolved typedef target. Representations that already
/// carry null absorb it; anything else without a nullable shape fails.
fn make_nullable(type_: ReturnType) -> Result<ReturnType, Error> {
    match type_ {
        ReturnType::String => Ok(ReturnType::NullableString),
        ReturnType::Node => Ok(ReturnType::NullableNode),
        ReturnType::Union(name, members) => Ok(ReturnType::NullableUnion(name, members)),
        // A value already carries null, so nullability is absorbed.
        ReturnType::PlatformObject
        | ReturnType::NullableString
        | ReturnType::NullableNode
        | ReturnType::NullableDocumentType
        | ReturnType::NullableUnsignedLong
        | ReturnType::NullableUnion(_, _) => Ok(type_),
        type_ => Err(Error(format!(
            "nullable native type is not supported yet: {type_:?}"
        ))),
    }
}

/// Lower a union to a generated enum. Flattened member order follows the IDL
/// declaration; conversion order follows the `WebIDL` union algorithm, which
/// tries platform objects before dictionaries, then primitives, then string
/// coercion (<https://webidl.spec.whatwg.org/#es-union>).
/// Chromium groups identical flattened member sets into one union class
/// (`third_party/blink/renderer/bindings/scripts/web_idl/union.py`);
/// Firefox tries members in the same order in its generated `Init`
/// (`dom/bindings/Codegen.py`). Trusted Types interfaces have no implementation
/// in this runtime, so they collapse to the string member they accompany; a
/// union that collapses to one member type lowers to that type, not an enum.
fn lower_union(
    database: &Database<'_>,
    union_: &weedle::types::MayBeNull<weedle::types::UnionType<'_>>,
) -> Result<ReturnType, Error> {
    let nullable = union_.q_mark.is_some();
    let mut members = Vec::new();
    let mut visited = BTreeSet::new();
    collect_union_members(
        database,
        &union_.type_.body.list,
        &mut members,
        &mut visited,
    )?;
    finish_union(members, nullable)
}

/// Flatten nested unions and typedef'd unions into distinct member types,
/// mirroring Chromium's `UnionType.flattened_member_types`. Members whose
/// Rust representation repeats collapse into one; anything without a
/// generated conversion fails the build.
fn collect_union_members<'idl>(
    database: &Database<'idl>,
    list: &[UnionMemberType<'idl>],
    members: &mut Vec<model::UnionMember>,
    visited: &mut BTreeSet<String>,
) -> Result<(), Error> {
    for member in list {
        match member {
            UnionMemberType::Single(member) => {
                validate_union_member_attributes(member.attributes.as_ref())?;
                collect_union_member(
                    database,
                    &Type::Single(SingleType::NonAny(member.type_.clone())),
                    members,
                    visited,
                )?;
            }
            UnionMemberType::Union(inner) => {
                if inner.q_mark.is_some() {
                    return Err(Error("nullable nested unions are not supported yet".into()));
                }
                collect_union_members(database, &inner.type_.body.list, members, visited)?;
            }
        }
    }
    Ok(())
}

/// Reject member attributes the generated conversion cannot honor. A
/// `[LegacyNullToEmptyString]` member is meaningful only to the attribute
/// lowering that reads it back, so it is allowed here.
fn validate_union_member_attributes(
    attributes: Option<&ExtendedAttributeList<'_>>,
) -> Result<(), Error> {
    if let Some(attributes) = attributes {
        for attribute in &attributes.body.list {
            match attribute {
                ExtendedAttribute::NoArgs(item) if item.0.0 == "LegacyNullToEmptyString" => {}
                _ => {
                    return Err(Error(format!(
                        "union member attributes are not supported yet: {attribute:?}"
                    )));
                }
            }
        }
    }
    Ok(())
}

fn collect_union_member<'idl>(
    database: &Database<'idl>,
    type_: &Type<'idl>,
    members: &mut Vec<model::UnionMember>,
    visited: &mut BTreeSet<String>,
) -> Result<(), Error> {
    match type_ {
        Type::Union(inner) => {
            if inner.q_mark.is_some() {
                return Err(Error("nullable nested unions are not supported yet".into()));
            }
            collect_union_members(database, &inner.type_.body.list, members, visited)
        }
        Type::Single(SingleType::NonAny(NonAnyType::DOMString(item))) if item.q_mark.is_none() => {
            push_union_member(
                members,
                format_ident!("DOMString"),
                model::UnionMemberType::String,
            );
            Ok(())
        }
        Type::Single(SingleType::NonAny(NonAnyType::Boolean(item))) if item.q_mark.is_none() => {
            push_union_member(
                members,
                format_ident!("Boolean"),
                model::UnionMemberType::Boolean,
            );
            Ok(())
        }
        Type::Single(SingleType::NonAny(NonAnyType::Integer(item)))
            if item.q_mark.is_none()
                && matches!(item.type_, IntegerType::Long(value) if value.unsigned.is_none()) =>
        {
            push_union_member(members, format_ident!("Long"), model::UnionMemberType::Long);
            Ok(())
        }
        Type::Single(SingleType::NonAny(NonAnyType::Identifier(item))) if item.q_mark.is_none() => {
            let name = item.type_.0;
            match database.definition(name) {
                // Trusted Types interfaces are uninhabited here, so a union
                // containing one behaves as its string member
                // (<https://w3c.github.io/trusted-types/dist/spec/>).
                Some(weedle::Definition::Interface(_))
                    if matches!(name, "TrustedHTML" | "TrustedScript" | "TrustedScriptURL") =>
                {
                    push_union_member(
                        members,
                        format_ident!("DOMString"),
                        model::UnionMemberType::String,
                    );
                    Ok(())
                }
                Some(weedle::Definition::Interface(_)) => {
                    let variant = format_ident!("{name}");
                    push_union_member(
                        members,
                        variant,
                        model::UnionMemberType::Interface {
                            name: name.into(),
                            node: database.is_node_interface(name),
                        },
                    );
                    Ok(())
                }
                Some(weedle::Definition::Dictionary(_)) => {
                    let variant = format_ident!("{name}");
                    push_union_member(
                        members,
                        variant,
                        model::UnionMemberType::Dictionary(name.into()),
                    );
                    Ok(())
                }
                Some(weedle::Definition::Typedef(definition)) => {
                    if !visited.insert(name.into()) {
                        return Err(Error(format!("union typedef cycle at {name}")));
                    }
                    validate_empty_attributes(definition.attributes.as_ref())?;
                    collect_union_member(database, &definition.type_.type_, members, visited)
                }
                _ => Err(Error(format!("union member {name} is not supported yet"))),
            }
        }
        Type::Single(_) => Err(Error(format!(
            "union member is not supported yet: {type_:?}"
        ))),
    }
}

/// One variant per distinct Rust representation; duplicates collapse, so
/// `(TrustedHTML or DOMString)` lowers to the string member alone.
fn push_union_member(
    members: &mut Vec<model::UnionMember>,
    variant: proc_macro2::Ident,
    type_: model::UnionMemberType,
) {
    let member = model::UnionMember { variant, type_ };
    if !members.contains(&member) {
        members.push(member);
    }
}

fn finish_union(members: Vec<model::UnionMember>, nullable: bool) -> Result<ReturnType, Error> {
    if members.is_empty() {
        return Err(Error("unions need at least one member type".into()));
    }
    // A nullable union stays a union even when it collapses to one
    // representation, so the null conversion stays explicit.
    if let [member] = members.as_slice()
        && !nullable
    {
        return Ok(member_return_type(&member.type_));
    }
    // Canonical declaration-independent name, like Chromium's sorted union
    // type names (`UnionType.type_name_without_extended_attributes`).
    let mut names: Vec<String> = members
        .iter()
        .map(|member| member.variant.to_string())
        .collect();
    names.sort_unstable();
    names.dedup();
    let name = names.join("Or");
    if nullable {
        Ok(ReturnType::NullableUnion(name, members))
    } else {
        Ok(ReturnType::Union(name, members))
    }
}

/// The `ReturnType` a single union member lowers to when the union collapses
/// to one representation.
fn member_return_type(type_: &model::UnionMemberType) -> ReturnType {
    match type_ {
        model::UnionMemberType::Interface { node: true, .. } => ReturnType::Node,
        model::UnionMemberType::Interface { node: false, .. } => ReturnType::PlatformObject,
        model::UnionMemberType::String => ReturnType::String,
        model::UnionMemberType::Boolean => ReturnType::Boolean,
        model::UnionMemberType::Long => ReturnType::Long,
        model::UnionMemberType::Dictionary(name) => ReturnType::Dictionary(name.clone()),
    }
}

/// Whether any union member or nested member carries `name`, so attribute
/// lowering can read `[LegacyNullToEmptyString]` off a collapsed member.
fn union_member_has_no_args(list: &[UnionMemberType<'_>], name: &str) -> bool {
    list.iter().any(|member| match member {
        UnionMemberType::Single(member) => has_no_args(member.attributes.as_ref(), name),
        UnionMemberType::Union(inner) => union_member_has_no_args(&inner.type_.body.list, name),
    })
}

/// Interface types the contract path carries. Node arguments need the
/// `NodeReference` conversion; every other DOM interface is a platform object
/// whose getter or result hands the value to JS directly.
fn native_interface(database: &Database<'_>, name: &str, nullable: bool) -> Option<ReturnType> {
    match (name, nullable) {
        ("Node", false) => Some(ReturnType::Node),
        ("Node", true) => Some(ReturnType::NullableNode),
        ("DocumentType", true) => Some(ReturnType::NullableDocumentType),
        // `WindowProxy` is a platform object the HTML Standard defines in
        // prose, so it has no declaration in the extracts to inspect; it is
        // carried as an opaque value
        // (<https://html.spec.whatwg.org/multipage/nav-history-apis.html#windowproxy>).
        ("WindowProxy", _) => Some(ReturnType::PlatformObject),
        _ if matches!(
            database.definition(name),
            Some(weedle::Definition::Interface(_) | weedle::Definition::CallbackInterface(_))
        ) =>
        {
            // An interface or callback interface is a platform object carried
            // as an opaque value; a callback interface is any object whose
            // methods are looked up on invocation
            // (<https://webidl.spec.whatwg.org/#es-callback-interface>).
            Some(ReturnType::PlatformObject)
        }
        _ => None,
    }
}

/// Attribute extended attributes the contract path consumes or documents.
fn validate_attribute_attributes(
    attributes: Option<&ExtendedAttributeList<'_>>,
) -> Result<(), Error> {
    if let Some(attributes) = attributes {
        for attribute in &attributes.body.list {
            match attribute {
                ExtendedAttribute::NoArgs(item)
                    if matches!(
                        item.0.0,
                        "SameObject"
                            | "CEReactions"
                            | "LegacyNullToEmptyString"
                            | "LegacyUnforgeable"
                            | "Reflect"
                            | "ReflectSetter"
                            | "ReflectURL"
                            | "SecureContext"
                    ) => {}
                // Numeric reflection parameters: hand-written getters encode
                // the spec algorithm, so the normal method path documents
                // them as consumed here. Only the standardized names and
                // shapes are accepted, so a typo still fails the build.
                // Note a weedle4 quirk: `Identifier` accepts leading digits
                // (`third_party/weedle4/src/common.rs`), so integer defaults
                // like `ReflectDefault=20` parse as `Ident` with rhs `"20"`,
                // while `ReflectDefault=1.0` parses as `Decimal`. The value
                // checks below depend on this split: do not "simplify" the
                // `Ident` arm without moving integer-default validation.
                ExtendedAttribute::NoArgs(item)
                    if matches!(
                        item.0.0,
                        "ReflectNonNegative" | "ReflectPositive" | "ReflectPositiveWithFallback"
                    ) => {}
                // `[PutForwards]` lowers to a generated forwarding setter.
                // `[LegacyUnforgeable]` shapes the instance property in the
                // interface's own shim, as the legacy path also accepts it;
                // the generator installs the prototype accessor.
                // `[PutForwards]` takes an identifier naming the target.
                ExtendedAttribute::Ident(item) if item.lhs_identifier.0 == "PutForwards" => {}
                // An integer reflection default, e.g. `ReflectDefault=20`.
                ExtendedAttribute::Ident(item) if item.lhs_identifier.0 == "ReflectDefault" => {
                    if item.rhs.0.parse::<i64>().is_err() {
                        return Err(Error(format!(
                            "ReflectDefault needs an integer default, got `{}`",
                            item.rhs.0
                        )));
                    }
                }
                ExtendedAttribute::Decimal(item)
                    if matches!(item.lhs_identifier.0, "ReflectDefault") => {}
                ExtendedAttribute::ArgList(item) if item.identifier.0 == "ReflectRange" => {
                    if item.args.body.list.len() != 2 {
                        return Err(Error(format!(
                            "ReflectRange needs exactly two bounds, got {}",
                            item.args.body.list.len()
                        )));
                    }
                }
                ExtendedAttribute::IdentList(item) if item.identifier.0 == "ReflectRange" => {
                    let bounds: Vec<&str> =
                        item.list.body.list.iter().map(|bound| bound.0).collect();
                    if bounds.len() != 2 || bounds.iter().any(|bound| bound.parse::<i64>().is_err())
                    {
                        return Err(Error(format!(
                            "ReflectRange needs two integer bounds, got `{item:?}`"
                        )));
                    }
                }
                ExtendedAttribute::String(item) if item.lhs_identifier.0 == "Reflect" => {}
                _ => {
                    return Err(Error(format!(
                        "native attribute semantics are not supported yet: {attribute:?}"
                    )));
                }
            }
        }
    }
    Ok(())
}

/// Operation extended attributes the contract path consumes.
fn validate_operation_attributes(
    attributes: Option<&ExtendedAttributeList<'_>>,
) -> Result<(), Error> {
    if let Some(attributes) = attributes {
        for attribute in &attributes.body.list {
            match attribute {
                ExtendedAttribute::NoArgs(item)
                    if matches!(
                        item.0.0,
                        "NewObject" | "CEReactions" | "Unscopable" | "SecureContext"
                    ) => {}
                _ => {
                    return Err(Error(format!(
                        "native operation semantics are not supported yet: {attribute:?}"
                    )));
                }
            }
        }
    }
    Ok(())
}

/// Argument extended attributes the contract path consumes.
fn validate_argument_attributes(
    attributes: Option<&ExtendedAttributeList<'_>>,
) -> Result<(), Error> {
    if let Some(attributes) = attributes {
        for attribute in &attributes.body.list {
            match attribute {
                ExtendedAttribute::NoArgs(item) if item.0.0 == "LegacyNullToEmptyString" => {}
                _ => {
                    return Err(Error(format!(
                        "native argument semantics are not supported yet: {attribute:?}"
                    )));
                }
            }
        }
    }
    Ok(())
}

fn validate_empty_attributes(attributes: Option<&ExtendedAttributeList<'_>>) -> Result<(), Error> {
    if attributes.is_some_and(|attributes| !attributes.body.list.is_empty()) {
        Err(Error(format!(
            "native contract semantics are not supported yet: {attributes:?}"
        )))
    } else {
        Ok(())
    }
}

fn validate_interface_attributes(
    attributes: Option<&ExtendedAttributeList<'_>>,
) -> Result<(), Error> {
    if let Some(attributes) = attributes {
        for attribute in &attributes.body.list {
            match attribute {
                ExtendedAttribute::WildCard(item) if item.identifier.0 == "Exposed" => {}
                ExtendedAttribute::Ident(item) if item.lhs_identifier.0 == "Exposed" => {}
                ExtendedAttribute::IdentList(item) if item.identifier.0 == "Exposed" => {}
                ExtendedAttribute::NoArgs(item)
                    if matches!(
                        item.0.0,
                        "Serializable"
                            | "LegacyUnenumerableNamedProperties"
                            | "LegacyOverrideBuiltIns"
                            | "SecureContext"
                    ) => {}
                // A named factory function (`new Option(...)`) is a JS shim
                // concern; the generator installs no member for it.
                ExtendedAttribute::NamedArgList(item)
                    if item.lhs_identifier.0 == "LegacyFactoryFunction" => {}
                _ => {
                    return Err(Error(format!(
                        "native interface semantics are not supported yet: {attribute:?}"
                    )));
                }
            }
        }
    }
    if crate::database::exposed_in(attributes, "Window") {
        Ok(())
    } else {
        Err(Error("native interface is not exposed in Window".into()))
    }
}
