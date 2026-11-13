//! Binding data lowered from unchanged upstream Web IDL into generated contracts.

use std::collections::HashSet;

use proc_macro2::Ident;
use weedle::attribute::{ExtendedAttribute, ExtendedAttributeList};
use weedle::literal::{ConstValue, IntegerLit};
use weedle::types::{ConstType, IntegerType};

use crate::Error;
use crate::database::Exposure;

/// One platform type implementing an interface. The primary payload owns the
/// interface's prototype and class; alternates are additional Rust types the
/// same interface contract dispatches to (for example every node interface is
/// also an `EventTarget`, whose primary payload is `JsEventTarget`).
#[derive(Clone)]
pub(crate) struct Payload {
    pub(crate) rust: Ident,
    pub(crate) has_lifetime: bool,
}

pub(crate) struct Interface {
    pub(crate) contract: Option<proc_macro2::TokenStream>,
    pub(crate) name: String,
    pub(crate) rust: Ident,
    /// Additional payload types implementing this interface, in discovery
    /// order. `rust` is always the primary payload.
    pub(crate) payloads: Vec<Payload>,
    pub(crate) has_lifetime: bool,
    pub(crate) parent: Option<PrototypeParent>,
    pub(crate) constructor: Option<Constructor>,
    pub(crate) attributes: Vec<Attribute>,
    pub(crate) constants: Vec<Constant>,
    pub(crate) kind: InterfaceKind,
    pub(crate) operations: Vec<Operation>,
    pub(crate) dictionaries: Vec<Dictionary>,
    pub(crate) enumerations: Vec<Enumeration>,
    /// Unions referenced by lowered members, deduplicated by name.
    pub(crate) unions: Vec<Union>,
    pub(crate) properties: PropertyHooks,
    /// How `toString` stringifies: an attribute index or a provided method.
    pub(crate) stringifier: Option<Stringifier>,
    /// `[SecureContext]` on the interface: the interface object, its prototype,
    /// and every member are absent outside a secure context
    /// (<https://webidl.spec.whatwg.org/#SecureContext>).
    pub(crate) secure_context: bool,
    pub(crate) indexed_setter: Option<IndexedSetter>,
    /// Spec mixin members installed on each named interface's prototype
    /// instead of one interface that shares the payload.
    pub(crate) install_targets: Vec<String>,
}

pub(crate) struct IndexedSetter {
    pub(crate) rust: Ident,
    pub(crate) reactions: bool,
}

pub(crate) enum PropertyHooks {
    None,
    Indexed,
    IndexedNamed {
        names: Ident,
        unenumerable: bool,
        override_builtins: bool,
    },
}

/// How an interface stringifies for `toString`: a stringifier attribute, or
/// an anonymous `stringifier;` the implementation provides as a method.
pub(crate) enum Stringifier {
    Attribute(usize),
    Method,
}

pub(crate) enum InterfaceKind {
    Complete,
    Partial,
}

pub(crate) enum PrototypeParent {
    Intrinsic(String),
    Interface(String),
}

pub(crate) struct Operation {
    pub(crate) name: String,
    pub(crate) exposure: crate::database::Exposure,
    pub(crate) rust: Ident,
    pub(crate) result: OperationResult,
    pub(crate) takes_this: bool,
    pub(crate) arguments: Vec<OperationArgument>,
    pub(crate) reactions: bool,
    /// `[Unscopable]`: the member is listed in `@@unscopables`
    /// (<https://webidl.spec.whatwg.org/#es-operations>).
    pub(crate) unscopable: bool,
    pub(crate) getter: Option<PropertyGetter>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum PropertyGetter {
    Indexed,
    Named,
}

pub(crate) enum OperationResult {
    /// The method returns the platform object or value to hand back.
    Object,
    Undefined,
    /// The method runs synchronously and the dispatch hands back a promise
    /// resolved with `undefined`
    /// (<https://webidl.spec.whatwg.org/#es-promise>).
    PromiseUndefined,
    Sequence,
    StringSequence,
    /// The method returns `rquickjs::String<'js>`.
    String,
    NullableString,
    Boolean,
    UnsignedShort,
    /// IDL `short`, an `i16`.
    Short,
    Long,
    /// The method returns a union; the dispatch converts it with `IntoJs`.
    Union,
    /// The method returns a nullable union; `None` converts to `null`
    /// (<https://webidl.spec.whatwg.org/#js-nullable-type>).
    NullableUnion,
}

pub(crate) struct OperationArgument {
    pub(crate) type_: ReturnType,
    pub(crate) arity: ArgumentArity,
    pub(crate) null_default: bool,
    pub(crate) legacy_null_to_empty: bool,
    pub(crate) boolean_default: Option<bool>,
    /// An optional union's IDL default, materialized as the value before
    /// conversion, mirroring boolean defaults.
    pub(crate) union_default: Option<UnionDefault>,
}

/// An optional union argument's default value.
#[derive(Clone, Copy)]
pub(crate) enum UnionDefault {
    Boolean(bool),
    EmptyDictionary,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ArgumentArity {
    Required,
    Optional,
    Variadic,
}

pub(crate) struct Constructor {
    pub(crate) rust: Ident,
    pub(crate) arguments: Vec<ConstructorArgument>,
}

pub(crate) struct ConstructorArgument {
    pub(crate) kind: ConstructorArgumentKind,
}

pub(crate) enum ConstructorArgumentKind {
    String { default: Option<String> },
    Callback,
    Dictionary(String),
}

impl ConstructorArgumentKind {
    pub(crate) fn is_required(&self) -> bool {
        match self {
            Self::String { default } => default.is_none(),
            Self::Callback => true,
            Self::Dictionary(_) => false,
        }
    }
}

pub(crate) struct Attribute {
    pub(crate) name: String,
    pub(crate) exposure: crate::database::Exposure,
    pub(crate) rust: Ident,
    pub(crate) return_type: ReturnType,
    pub(crate) mapping: GetterMapping,
    pub(crate) setter: Option<Setter>,
    pub(crate) legacy_null_to_empty: bool,
    pub(crate) reactions: bool,
    /// `[Unscopable]`: the attribute is listed in `@@unscopables`
    /// (<https://webidl.spec.whatwg.org/#es-attributes>).
    pub(crate) unscopable: bool,
}

pub(crate) enum Setter {
    Method {
        rust: Ident,
    },
    /// `[Reflect]`: the setter writes the content attribute through the
    /// shared helper. Like the getter, the trait carries no method. This
    /// also serves `[ReflectURL]`, whose setter reflects plainly.
    Reflect {
        content: String,
    },
    PutForwards {
        target: String,
        /// The attributed type is nullable, so forwarding must no-op when the
        /// getter returns null (`Document.location` on a detached document).
        nullable: bool,
    },
}

pub(crate) enum GetterMapping {
    Method,
    /// `[Reflect]`: the attribute mirrors a content attribute. Generated
    /// dispatch reads and writes element storage directly; the trait carries
    /// no method and the implementation provides none. The content name is
    /// the `Reflect` value or the lowercase IDL name, following Chromium's
    /// content-attribute key derivation
    /// (`bind_gen/interface.py::_make_reflect_content_attribute_key`).
    Reflect {
        content: String,
    },
    /// `[ReflectURL]`: like `[Reflect]`, but the getter resolves a URL
    /// against the document base.
    ReflectUrl {
        content: String,
    },
    /// A reflected enumerated attribute. The getter reads the content
    /// attribute and maps it through one `Enumerated` type, answering the
    /// canonical keyword, the missing-value default, or the invalid-value
    /// default. The metadata table is the one place the three values are
    /// written.
    ReflectEnumerated {
        content: String,
        /// The `Enumerated` type the keyword set belongs to.
        type_path: syn::Path,
        /// The missing-value default, when the spec defines one.
        missing: Option<syn::Path>,
        /// The invalid-value default, when the spec defines one.
        invalid: Option<syn::Path>,
    },
}

#[derive(Debug)]
pub(crate) enum ReturnType {
    String,
    NullableString,
    UnsignedShort,
    UnsignedLong,
    Long,
    /// IDL `short`, an `i16`.
    Short,
    Boolean,
    /// Nullable `unsigned long`: `null` or `undefined` convert to `None`
    /// (<https://webidl.spec.whatwg.org/#js-nullable-type>).
    NullableUnsignedLong,
    UsvString,
    NullableDocumentType,
    NullableNode,
    Node,
    Callback,
    Dictionary(String),
    Enumeration(String),
    InterfaceSequence,
    /// `sequence<DOMString>` result, mapped to a JavaScript array of strings.
    StringSequence,
    PlatformObject,
    /// `DOMHighResTimeStamp`, a `double`
    /// (<https://webidl.spec.whatwg.org/#idl-DOMHighResTimeStamp>).
    Double,
    /// Restricted `double`: rejects NaN and infinities on conversion
    /// (<https://webidl.spec.whatwg.org/#es-double>).
    RestrictedDouble,
    /// A union of IDL member types, with the generated enum named by the
    /// second field. Conversion tries members in `WebIDL` order
    /// (<https://webidl.spec.whatwg.org/#es-union>).
    Union(String, Vec<UnionMember>),
    /// A nullable union: `null` or `undefined` convert to `None`
    /// (<https://webidl.spec.whatwg.org/#js-nullable-type>).
    NullableUnion(String, Vec<UnionMember>),
    /// `Promise<undefined>`: the method runs synchronously and the dispatch
    /// resolves the promise (<https://webidl.spec.whatwg.org/#es-promise>).
    PromiseUndefined,
    /// A `Uint8Array` argument or result. Only the JavaScript emitter lowers
    /// this shape today; the native side rejects it where it renders
    /// (<https://webidl.spec.whatwg.org/#es-buffer-source-types>).
    Uint8Array,
    /// `unsigned long long`: only JavaScript result dictionaries use this
    /// shape today; the native side rejects it where it renders
    /// (<https://webidl.spec.whatwg.org/#idl-unsigned-long-long>).
    UnsignedLongLong,
    /// IDL `any`: the value passes through with no conversion
    /// (<https://webidl.spec.whatwg.org/#idl-any>).
    Any,
}

/// One flattened member of a union: the generated variant and its IDL type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct UnionMember {
    pub(crate) variant: Ident,
    pub(crate) type_: UnionMemberType,
}

/// Member types a generated union conversion supports. Anything else fails
/// the build at lowering instead of shipping a partial conversion.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum UnionMemberType {
    /// An interface member. `name` is the IDL interface and `node` says
    /// whether it is `Node` or inherits from it (converted to
    /// `host::NodeReference`) rather than another platform object.
    Interface {
        name: String,
        node: bool,
    },
    String,
    Boolean,
    Long,
    Dictionary(String),
}

impl UnionMemberType {
    /// Whether the member's Rust representation borrows the context lifetime,
    /// making the generated union enum generic over `'js`.
    pub(crate) const fn needs_lifetime(&self) -> bool {
        matches!(self, Self::String | Self::Interface { node: false, .. })
    }
}

pub(crate) struct Callback;

pub(crate) struct Enumeration {
    pub(crate) name: String,
    pub(crate) values: Vec<(String, Ident)>,
}

pub(crate) struct Dictionary {
    pub(crate) name: String,
    pub(crate) fields: Vec<DictionaryField>,
}

/// A generated union enum: one variant per flattened IDL member type.
#[derive(Clone)]
pub(crate) struct Union {
    pub(crate) name: String,
    pub(crate) members: Vec<UnionMember>,
}

pub(crate) struct DictionaryField {
    pub(crate) name: String,
    pub(crate) rust: Ident,
    pub(crate) type_: DictionaryFieldType,
}

pub(crate) enum DictionaryFieldType {
    Boolean {
        default: Option<bool>,
        required: bool,
    },
    Enumeration {
        name: String,
        nullable: bool,
        default: Option<Ident>,
        required: bool,
    },
    StringSequence,
    /// A `DOMString` dictionary member, converted with `ToString` when present.
    DomString,
    /// An interface- or callback-interface-typed member. Nullable members
    /// map a present null to `None`; anything else must be an object.
    Interface {
        nullable: bool,
    },
}

pub(crate) struct Constant {
    pub(crate) name: String,
    pub(crate) value: u16,
    pub(crate) exposure: Exposure,
}

impl Constant {
    pub(crate) fn parse(
        member: &weedle::interface::ConstMember<'_>,
        exposure: Exposure,
    ) -> Result<Self, Error> {
        let ConstType::Integer(type_) = member.const_type else {
            return Err(Error("expected an unsigned short constant".into()));
        };
        if type_.q_mark.is_some() || !is_unsigned_short(type_.type_) {
            return Err(Error("expected an unsigned short constant".into()));
        }
        // https://webidl.spec.whatwg.org/#idl-constants
        // weedle lexes `0` as an octal literal, so accept every integer form.
        let value = match member.const_value {
            ConstValue::Integer(IntegerLit::Dec(value)) => parse_const_integer(value.0, 10)?,
            ConstValue::Integer(IntegerLit::Hex(value)) => {
                let digits = value.0.strip_prefix("0x").unwrap_or(value.0);
                parse_const_integer(digits, 16)?
            }
            ConstValue::Integer(IntegerLit::Oct(value)) => parse_const_integer(value.0, 8)?,
            _ => return Err(Error("expected an integer constant".into())),
        };
        Attributes::parse(member.attributes.as_ref())?.finish()?;
        Ok(Self {
            name: identifier(member.identifier.0)?.into(),
            value,
            exposure,
        })
    }
}

impl Callback {
    pub(crate) fn parse(definition: &weedle::CallbackDefinition<'_>) -> Result<Self, Error> {
        Attributes::parse(definition.attributes.as_ref())?.finish()?;
        // https://webidl.spec.whatwg.org/#idl-callback-functions
        if !matches!(
            definition.return_type,
            weedle::types::ReturnType::Undefined(_)
        ) {
            return Err(Error(
                "only undefined callback results are supported yet".into(),
            ));
        }
        // The parameter list documents the call contract the hand-written
        // delivery code implements; conversion never runs on it, so only the
        // shape is checked here and WPT covers the behavior.
        for argument in &definition.arguments.body.list {
            let weedle::argument::Argument::Single(argument) = argument else {
                return Err(Error("variadic callbacks are not supported yet".into()));
            };
            identifier_spelling(argument.identifier.0)?;
            if argument.optional.is_some() || argument.default.is_some() {
                return Err(Error(
                    "optional callback parameters are not supported yet".into(),
                ));
            }
        }
        identifier(definition.identifier.0)?;
        Ok(Self)
    }
}

impl Enumeration {
    pub(crate) fn parse(definition: &weedle::EnumDefinition<'_>) -> Result<Self, Error> {
        Attributes::parse(definition.attributes.as_ref())?.finish()?;
        let name = identifier(definition.identifier.0)?.to_owned();
        let mut values = Vec::new();
        let mut taken = HashSet::new();
        for value in &definition.values.body.list {
            let text = value.0;
            let mut variant = String::new();
            for word in text
                .split(|char: char| !char.is_ascii_alphanumeric())
                .filter(|word| !word.is_empty())
            {
                let mut chars = word.chars();
                if let Some(first) = chars.next() {
                    variant.push(first.to_ascii_uppercase());
                    variant.extend(chars);
                }
            }
            if variant.is_empty() {
                variant.push_str("Empty");
            }
            if !taken.insert(variant.clone()) {
                return Err(Error(format!(
                    "duplicate Rust enumeration variant: {variant}"
                )));
            }
            let variant = syn::parse_str(&variant)
                .map_err(|error| Error(format!("invalid enumeration variant: {error}")))?;
            values.push((text.into(), variant));
        }
        Ok(Self { name, values })
    }
}

fn is_unsigned_short(type_: IntegerType) -> bool {
    matches!(type_, IntegerType::Short(value) if value.unsigned.is_some())
}

/// Parse a Web IDL integer constant literal
/// (<https://webidl.spec.whatwg.org/#idl-constants>) into its `unsigned short`
/// value.
fn parse_const_integer(digits: &str, radix: u32) -> Result<u16, Error> {
    u16::from_str_radix(digits, radix).map_err(|error| Error(format!("invalid constant: {error}")))
}

fn identifier(token: &str) -> Result<&str, Error> {
    // https://webidl.spec.whatwg.org/#idl-names
    identifier_spelling(token)?;
    let name = token.strip_prefix('_').unwrap_or(token);
    if matches!(name, "constructor" | "toString" | "toJSON") || name.starts_with('_') {
        Err(Error(format!("reserved identifier: {token}")))
    } else {
        Ok(name)
    }
}

fn identifier_spelling(token: &str) -> Result<(), Error> {
    // https://webidl.spec.whatwg.org/#prod-identifier
    let spelling = token
        .strip_prefix('_')
        .or_else(|| token.strip_prefix('-'))
        .unwrap_or(token);
    if spelling
        .as_bytes()
        .first()
        .is_some_and(u8::is_ascii_alphabetic)
        && spelling
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        Ok(())
    } else {
        Err(Error(format!("invalid identifier: {token}")))
    }
}

struct Attributes(std::collections::BTreeMap<String, Option<String>>);

impl Attributes {
    fn parse(list: Option<&ExtendedAttributeList<'_>>) -> Result<Self, Error> {
        let mut result = std::collections::BTreeMap::new();
        if let Some(list) = list {
            for attribute in &list.body.list {
                let (key, value) = match attribute {
                    ExtendedAttribute::NoArgs(attribute) => (attribute.0.0, None::<String>),
                    _ => {
                        return Err(Error(format!(
                            "unsupported extended attribute: {attribute:?}"
                        )));
                    }
                };
                if result.insert(key.into(), value).is_some() {
                    return Err(Error(format!("duplicate extended attribute: {key}")));
                }
            }
        }
        Ok(Self(result))
    }

    fn finish(self) -> Result<(), Error> {
        if self.0.is_empty() {
            Ok(())
        } else {
            Err(Error(format!(
                "unsupported extended attributes: {:?}",
                self.0
            )))
        }
    }
}
