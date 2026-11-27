use std::collections::HashSet;

use proc_macro2::Ident;
use weedle::attribute::{ExtendedAttribute, ExtendedAttributeList};
use weedle::literal::{ConstValue, IntegerLit};
use weedle::types::{ConstType, IntegerType};

use crate::Error;
use crate::database::Exposure;

#[derive(Clone)]
pub(crate) struct Payload {
    pub(crate) rust: Ident,
    pub(crate) has_lifetime: bool,
}

pub(crate) struct Interface {
    pub(crate) contract: Option<proc_macro2::TokenStream>,
    pub(crate) name: String,
    pub(crate) rust: Ident,
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
    pub(crate) unions: Vec<Union>,
    pub(crate) properties: PropertyHooks,
    pub(crate) stringifier: Option<Stringifier>,
    // https://webidl.spec.whatwg.org/#SecureContext
    pub(crate) secure_context: bool,
    pub(crate) indexed_setter: Option<IndexedSetter>,
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
    // https://webidl.spec.whatwg.org/#es-operations
    pub(crate) unscopable: bool,
    pub(crate) getter: Option<PropertyGetter>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum PropertyGetter {
    Indexed,
    Named,
}

pub(crate) enum OperationResult {
    Object,
    Undefined,
    // https://webidl.spec.whatwg.org/#es-promise
    PromiseUndefined,
    Sequence,
    StringSequence,
    String,
    NullableString,
    Boolean,
    UnsignedShort,
    Short,
    Long,
    Union,
    // https://webidl.spec.whatwg.org/#js-nullable-type
    NullableUnion,
}

pub(crate) struct OperationArgument {
    pub(crate) type_: ReturnType,
    pub(crate) arity: ArgumentArity,
    pub(crate) null_default: bool,
    pub(crate) legacy_null_to_empty: bool,
    pub(crate) boolean_default: Option<bool>,
    pub(crate) ushort_default: Option<u16>,
    pub(crate) union_default: Option<UnionDefault>,
}

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
    // https://webidl.spec.whatwg.org/#es-attributes
    pub(crate) unscopable: bool,
}

pub(crate) enum Setter {
    Method { rust: Ident },
    Reflect { content: String },
    PutForwards { target: String, nullable: bool },
}

pub(crate) enum GetterMapping {
    Method,
    Reflect {
        content: String,
    },
    ReflectUrl {
        content: String,
    },
    ReflectEnumerated {
        content: String,
        type_path: syn::Path,
        missing: Option<syn::Path>,
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
    Short,
    Boolean,
    // https://webidl.spec.whatwg.org/#js-nullable-type
    NullableUnsignedLong,
    UsvString,
    NullableDocumentType,
    NullableNode,
    Node,
    Callback,
    Dictionary(String),
    Enumeration(String),
    InterfaceSequence,
    StringSequence,
    PlatformObject,
    // https://webidl.spec.whatwg.org/#idl-DOMHighResTimeStamp
    Double,
    // https://webidl.spec.whatwg.org/#es-double
    RestrictedDouble,
    // https://webidl.spec.whatwg.org/#es-union
    Union(String, Vec<UnionMember>),
    // https://webidl.spec.whatwg.org/#js-nullable-type
    NullableUnion(String, Vec<UnionMember>),
    // https://webidl.spec.whatwg.org/#es-promise
    PromiseUndefined,
    // https://webidl.spec.whatwg.org/#es-buffer-source-types
    Uint8Array,
    // https://webidl.spec.whatwg.org/#idl-unsigned-long-long
    UnsignedLongLong,
    // https://webidl.spec.whatwg.org/#idl-any
    Any,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct UnionMember {
    pub(crate) variant: Ident,
    pub(crate) type_: UnionMemberType,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum UnionMemberType {
    Interface { name: String, node: bool },
    String,
    Boolean,
    Long,
    Dictionary(String),
}

impl UnionMemberType {
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
    DomString,
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

// https://webidl.spec.whatwg.org/#idl-constants
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
