use std::collections::{BTreeMap, BTreeSet};

use oxc_allocator::Allocator;
use oxc_ast::ast::{self, ClassElement, MethodDefinitionKind};
use oxc_ast_visit::{Visit, walk};
use oxc_parser::Parser;
use oxc_span::{SourceType, Span};
use weedle::attribute::{ExtendedAttribute, ExtendedAttributeList};
use weedle::interface::InterfaceMember;

use crate::contracts::native_type;
use crate::database::Database;
use crate::model::ReturnType;
use crate::{Error, Javascript, Source};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Kind {
    Constructor,
    Method,
    Get,
    Set,
}

pub(crate) fn compile(idl: &[Source<'_>], source: &Source<'_>) -> Result<Javascript, Error> {
    let database = Database::parse(idl)?;
    let allocator = Allocator::default();
    let parsed = Parser::new(&allocator, source.text, SourceType::cjs()).parse();
    if let Some(error) = parsed.diagnostics.first() {
        return Err(Error(format!(
            "{}: invalid JavaScript: {error}",
            source.name
        )));
    }
    let mut discovery = Discovery {
        database: &database,
        source: source.text,
        replacements: Vec::new(),
        interfaces: BTreeSet::new(),
        error: None,
    };
    discovery.visit_program(&parsed.program);
    if let Some(error) = discovery.error {
        return Err(Error(format!("{}: {error}", source.name)));
    }
    discovery.replacements.sort_by_key(|(span, _)| span.start);
    let mut output = String::new();
    let mut offset = 0;
    for (span, replacement) in discovery.replacements {
        let start = span.start as usize;
        if start < offset {
            return Err(Error(
                "nested interface installations are not supported".into(),
            ));
        }
        output.push_str(&source.text[offset..start]);
        output.push_str(&replacement);
        offset = span.end as usize;
    }
    output.push_str(&source.text[offset..]);
    Ok(Javascript {
        source: output,
        interfaces: discovery.interfaces.into_iter().collect(),
    })
}

struct Discovery<'db, 'idl, 'source> {
    database: &'db Database<'idl>,
    source: &'source str,
    replacements: Vec<(Span, String)>,
    interfaces: BTreeSet<String>,
    error: Option<Error>,
}

impl<'ast> Visit<'ast> for Discovery<'_, '_, '_> {
    fn visit_call_expression(&mut self, call: &ast::CallExpression<'ast>) {
        let form = match &call.callee {
            ast::Expression::Identifier(identifier)
                if identifier.name == "__tbInstallInterface" =>
            {
                Some(InstallForm::Interface)
            }
            ast::Expression::Identifier(identifier) if identifier.name == "__tbInstallGlobal" => {
                Some(InstallForm::Global)
            }
            _ => None,
        };
        if let Some(form) = form {
            let result = self.installation(call, form);
            match result {
                Ok(replacement) => self.replacements.push((call.span, replacement)),
                Err(error) => {
                    let line = self
                        .source
                        .get(..call.span.start as usize)
                        .map_or(0, |prefix| {
                            prefix.bytes().filter(|byte| *byte == b'\n').count() + 1
                        });
                    self.error = Some(Error(format!("line {line}: {error}")));
                }
            }
        }
        walk::walk_call_expression(self, call);
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum InstallForm {
    Interface,
    Global,
}

impl Discovery<'_, '_, '_> {
    fn installation(
        &mut self,
        call: &ast::CallExpression<'_>,
        form: InstallForm,
    ) -> Result<String, Error> {
        let (name, class, unimplemented) = match (form, call.arguments.as_slice()) {
            (InstallForm::Interface, [ast::Argument::ClassExpression(class)]) => {
                (None, class, BTreeSet::new())
            }
            (
                InstallForm::Interface,
                [
                    ast::Argument::ClassExpression(class),
                    ast::Argument::ArrayExpression(list),
                ],
            ) => (None, class, string_list(list)?),
            (
                InstallForm::Global,
                [
                    ast::Argument::StringLiteral(name),
                    ast::Argument::ClassExpression(class),
                ],
            ) => (Some(name), class, BTreeSet::new()),
            (
                InstallForm::Global,
                [
                    ast::Argument::StringLiteral(name),
                    ast::Argument::ClassExpression(class),
                    ast::Argument::ArrayExpression(list),
                ],
            ) => (Some(name), class, string_list(list)?),
            _ => {
                return Err(Error(match form {
                    InstallForm::Interface => {
                        "interface installation requires one class expression and an optional list of unimplemented members".into()
                    }
                    InstallForm::Global => {
                        "global installation requires a name string, a class expression, and an optional list of unimplemented members".into()
                    }
                }));
            }
        };
        if call.optional || class.heritage.is_some() || !class.decorators.is_empty() {
            return Err(Error(
                "conditional or inherited JS class installation is not supported yet".into(),
            ));
        }
        let class_name = class.id.as_ref().map(|id| id.name.as_str());
        let name = match name {
            Some(name) => name.value.as_str(),
            None => class_name.ok_or_else(|| Error("interface class must be named".into()))?,
        };
        if !self.interfaces.insert(name.into()) {
            return Err(Error(format!("duplicate JS implementation of {name}")));
        }
        let declaration = self.database.interface(name)?;
        let interface_attributes = attributes(
            declaration.attributes.as_ref(),
            &["Exposed", "SecureContext"],
        )?;
        let secure_only = interface_attributes.contains(&"SecureContext");
        if secure_only && form == InstallForm::Interface {
            return Err(Error(
                "a secure-only JS interface must use __tbInstallGlobal so the installer owns global registration".into(),
            ));
        }
        let exposed = crate::database::exposed_in(declaration.attributes.as_ref(), "Window");
        if !exposed || declaration.parent.is_some() {
            return Err(Error(
                "JS interface exposure or inheritance is not supported yet".into(),
            ));
        }
        let (constructor, members) = lower_members(
            self.database,
            &declaration,
            implementation_methods(class)?,
            &unimplemented,
        )?;
        let constructor = constructor.unwrap_or_else(|| "null".into());
        let implementation = class.span.source_text(self.source);
        Ok(match form {
            InstallForm::Interface => format!(
                "__tbInstallInterface({implementation}, {name:?}, {constructor}, [{}])",
                members.join(",")
            ),
            InstallForm::Global => format!(
                "__tbInstallGlobal({implementation}, {name:?}, {constructor}, [{}], {secure_only})",
                members.join(",")
            ),
        })
    }
}

fn string_list(list: &ast::ArrayExpression<'_>) -> Result<BTreeSet<String>, Error> {
    let mut names = BTreeSet::new();
    for element in &list.elements {
        let ast::ArrayExpressionElement::StringLiteral(name) = element else {
            return Err(Error(
                "unimplemented members must be string literals".into(),
            ));
        };
        names.insert(name.value.as_str().to_owned());
    }
    Ok(names)
}

fn implementation_methods(
    class: &ast::Class<'_>,
) -> Result<BTreeMap<(String, Kind), usize>, Error> {
    let mut implemented = BTreeMap::new();
    for member in &class.body.body {
        let ClassElement::MethodDefinition(method) = member else {
            return Err(Error(
                "JS interface classes contain algorithm methods only".into(),
            ));
        };
        if method.computed
            || method.r#static
            || method.value.r#async
            || method.value.generator
            || !method.decorators.is_empty()
        {
            return Err(Error(
                "JS special algorithm methods are not supported yet".into(),
            ));
        }
        let key = method
            .key
            .static_name()
            .ok_or_else(|| Error("JS interface member must have an IDL name".into()))?
            .into_owned();
        let kind = match method.kind {
            MethodDefinitionKind::Constructor => Kind::Constructor,
            MethodDefinitionKind::Method => Kind::Method,
            MethodDefinitionKind::Get => Kind::Get,
            MethodDefinitionKind::Set => Kind::Set,
        };
        if method.value.params.rest.is_some()
            || method.value.params.items.iter().any(|parameter| {
                !matches!(parameter.pattern, ast::BindingPattern::BindingIdentifier(_))
            })
        {
            return Err(Error("JS algorithm parameters must be plain names; IDL supplies defaults and conversions".into()));
        }
        if implemented
            .insert((key.clone(), kind), method.value.params.items.len())
            .is_some()
        {
            return Err(Error(format!("duplicate JS member {key}")));
        }
    }
    Ok(implemented)
}

fn lower_members(
    database: &Database<'_>,
    declaration: &crate::database::Interface<'_>,
    mut implemented: BTreeMap<(String, Kind), usize>,
    unimplemented: &BTreeSet<String>,
) -> Result<(Option<String>, Vec<String>), Error> {
    let provided: BTreeSet<_> = implemented.keys().cloned().collect();
    let mut constructor = None;
    let mut members = Vec::new();
    let mut missing = Vec::new();
    for resolved in &declaration.members {
        let exposure = resolved.exposure();
        match &resolved.declaration {
            InterfaceMember::Constructor(member)
                if provided.contains(&("constructor".into(), Kind::Constructor)) =>
            {
                if constructor.is_some() {
                    return Err(Error(
                        "JS constructor overloads are not supported yet".into(),
                    ));
                }
                constructor = Some(lower_constructor_member(
                    database,
                    member,
                    resolved,
                    &mut implemented,
                )?);
            }
            InterfaceMember::Attribute(member)
                if provided.contains(&(member.identifier.0.into(), Kind::Get))
                    || provided.contains(&(member.identifier.0.into(), Kind::Set)) =>
            {
                lower_attribute_member(
                    database,
                    member,
                    resolved,
                    exposure,
                    &mut implemented,
                    &mut members,
                )?;
            }
            InterfaceMember::Operation(member)
                if member
                    .identifier
                    .is_some_and(|name| provided.contains(&(name.0.into(), Kind::Method))) =>
            {
                lower_operation_member(
                    database,
                    member,
                    resolved,
                    exposure,
                    &mut implemented,
                    &mut members,
                )?;
            }
            InterfaceMember::Constructor(member) => {
                missing.push(format!("constructor({})", member.args.body.list.len()));
            }
            InterfaceMember::Attribute(member) => missing.push(member.identifier.0.to_owned()),
            InterfaceMember::Operation(member) => missing.push(
                member
                    .identifier
                    .map_or_else(|| "operation".to_owned(), |name| name.0.to_owned()),
            ),
            _ => {}
        }
    }
    if let Some(name) = unimplemented
        .iter()
        .find(|name| !declaration.members.iter().any(|member| matches!(
            &member.declaration,
            InterfaceMember::Attribute(attribute) if attribute.identifier.0 == name.as_str()
        ) || matches!(
            &member.declaration,
            InterfaceMember::Operation(operation) if operation.identifier.map(|id| id.0) == Some(name.as_str())
        )))
    {
        return Err(Error(format!(
            "{0}: unimplemented member {name:?} is not declared by the IDL",
            declaration.name
        )));
    }
    let missing: Vec<String> = missing
        .into_iter()
        .filter(|name| !unimplemented.contains(name))
        .collect();
    if !missing.is_empty() {
        return Err(Error(format!(
            "{0}: JS implementation does not cover IDL members {missing:?}; list them in the installation's unimplemented array",
            declaration.name
        )));
    }
    if !implemented.is_empty() {
        let name = declaration.name;
        return Err(Error(format!(
            "{name}: JS members do not match supported IDL contracts: {:?}",
            implemented.keys().collect::<Vec<_>>()
        )));
    }
    Ok((constructor, members))
}

fn lower_constructor_member(
    database: &Database<'_>,
    member: &weedle::interface::ConstructorInterfaceMember<'_>,
    resolved: &crate::database::Member<'_>,
    implemented: &mut BTreeMap<(String, Kind), usize>,
) -> Result<String, Error> {
    resolved.validate_scopes()?;
    attributes(member.attributes.as_ref(), &[])?;
    let (required, conversion) = arguments(database, &member.args.body.list)?;
    signature(
        implemented,
        "constructor",
        Kind::Constructor,
        member.args.body.list.len(),
    )?;
    Ok(format!("[{required}, args => {conversion}]"))
}

fn lower_attribute_member(
    database: &Database<'_>,
    member: &weedle::interface::AttributeInterfaceMember<'_>,
    resolved: &crate::database::Member<'_>,
    exposure: crate::database::Exposure,
    implemented: &mut BTreeMap<(String, Kind), usize>,
    members: &mut Vec<String>,
) -> Result<(), Error> {
    resolved.validate_scopes()?;
    attributes(
        member.attributes.as_ref(),
        &["Exposed", "SecureContext", "SameObject"],
    )?;
    attributes(member.type_.attributes.as_ref(), &[])?;
    if member.modifier.is_some() {
        return Err(Error("JS special attributes are not supported yet".into()));
    }
    let name = member.identifier.0;
    signature(implemented, name, Kind::Get, 0)?;
    let lowered = js_type(database, &member.type_.type_)?;
    let result = output(database, &lowered, "value")?;
    members.push(member_descriptor(
        name,
        "get",
        0,
        "args => []",
        &format!("(value, realm) => {result}"),
        exposure,
    ));
    if member.readonly.is_none() {
        signature(implemented, name, Kind::Set, 1)?;
        let converted = input(database, &lowered, "args[0]", false)?;
        members.push(member_descriptor(
            name,
            "set",
            1,
            &format!("args => [{converted}]"),
            "(value, realm) => undefined",
            exposure,
        ));
    }
    Ok(())
}

fn lower_operation_member(
    database: &Database<'_>,
    member: &weedle::interface::OperationInterfaceMember<'_>,
    resolved: &crate::database::Member<'_>,
    exposure: crate::database::Exposure,
    implemented: &mut BTreeMap<(String, Kind), usize>,
    members: &mut Vec<String>,
) -> Result<(), Error> {
    resolved.validate_scopes()?;
    attributes(
        member.attributes.as_ref(),
        &["Exposed", "NewObject", "SecureContext"],
    )?;
    if member.modifier.is_some() || member.special.is_some() {
        return Err(Error("JS special operations are not supported yet".into()));
    }
    let name = member
        .identifier
        .as_ref()
        .ok_or_else(|| Error("JS operations must be named".into()))?
        .0;
    signature(implemented, name, Kind::Method, member.args.body.list.len())?;
    let (required, conversion) = arguments(database, &member.args.body.list)?;
    let result = match &member.return_type {
        weedle::types::ReturnType::Undefined(_) => "undefined".into(),
        weedle::types::ReturnType::Type(type_) => {
            let lowered = js_type(database, type_)?;
            output(database, &lowered, "value")?
        }
    };
    members.push(member_descriptor(
        name,
        "method",
        required,
        &format!("args => {conversion}"),
        &format!("(value, realm) => {result}"),
        exposure,
    ));
    Ok(())
}

fn member_descriptor(
    name: &str,
    kind: &str,
    count: usize,
    convert: &str,
    result: &str,
    exposure: crate::database::Exposure,
) -> String {
    format!(
        "{{ name: {name:?}, kind: {kind:?}, count: {count}, convert: {convert}, result: {result}, exposure: {{ secureContext: {} }} }}",
        exposure.secure_context
    )
}

enum JsType {
    Native(ReturnType),
    // https://webidl.spec.whatwg.org/#ArrayBufferView
    ArrayBufferView,
    // https://webidl.spec.whatwg.org/#es-sequence
    Sequence(Box<JsType>),
    // https://webidl.spec.whatwg.org/#es-frozen-array
    FrozenArray(Box<JsType>),
    // https://webidl.spec.whatwg.org/#es-promise
    Promise(Box<JsType>),
}

fn js_type(database: &Database<'_>, type_: &weedle::types::Type<'_>) -> Result<JsType, Error> {
    if let weedle::types::Type::Single(weedle::types::SingleType::NonAny(non_any)) = type_ {
        match non_any {
            weedle::types::NonAnyType::ArrayBufferView(item) if item.q_mark.is_none() => {
                return Ok(JsType::ArrayBufferView);
            }
            weedle::types::NonAnyType::FrozenArrayType(item) if item.q_mark.is_none() => {
                return Ok(JsType::FrozenArray(Box::new(js_type(
                    database,
                    &item.type_.generics.body,
                )?)));
            }
            weedle::types::NonAnyType::Sequence(item) if item.q_mark.is_none() => {
                return Ok(JsType::Sequence(Box::new(js_type(
                    database,
                    &item.type_.generics.body.type_,
                )?)));
            }
            weedle::types::NonAnyType::Promise(promise) => {
                let weedle::types::ReturnType::Type(inner) = &*promise.generics.body else {
                    return Err(Error(
                        "JS Promise<undefined> results are not supported yet".into(),
                    ));
                };
                return Ok(JsType::Promise(Box::new(js_type(database, inner)?)));
            }
            _ => {}
        }
    }
    native_type(database, type_, &mut BTreeSet::new()).map(JsType::Native)
}

fn signature(
    implemented: &mut BTreeMap<(String, Kind), usize>,
    name: &str,
    kind: Kind,
    count: usize,
) -> Result<(), Error> {
    if implemented.remove(&(name.into(), kind)) == Some(count) {
        Ok(())
    } else {
        Err(Error(format!(
            "{name}: JS algorithm signature does not match IDL {kind:?}"
        )))
    }
}

fn attributes<'a>(
    list: Option<&'a ExtendedAttributeList<'a>>,
    supported: &[&str],
) -> Result<Vec<&'a str>, Error> {
    let mut names = Vec::new();
    if let Some(list) = list {
        for attribute in &list.body.list {
            let name = match attribute {
                ExtendedAttribute::NoArgs(item) => item.0.0,
                ExtendedAttribute::WildCard(item) => item.identifier.0,
                ExtendedAttribute::Ident(item) => item.lhs_identifier.0,
                ExtendedAttribute::IdentList(item) => item.identifier.0,
                _ => {
                    return Err(Error(
                        "JS extended attribute form is not supported yet".into(),
                    ));
                }
            };
            if !supported.contains(&name)
                || (name != "Exposed" && !matches!(attribute, ExtendedAttribute::NoArgs(_)))
            {
                return Err(Error(format!(
                    "JS IDL semantics are not supported yet: {attribute:?}"
                )));
            }
            names.push(name);
        }
    }
    Ok(names)
}

fn arguments(
    database: &Database<'_>,
    arguments: &[weedle::argument::Argument<'_>],
) -> Result<(usize, String), Error> {
    let mut conversions = Vec::new();
    let mut required = 0;
    let mut optional = false;
    for (index, argument) in arguments.iter().enumerate() {
        let weedle::argument::Argument::Single(argument) = argument else {
            return Err(Error("JS variadic arguments are not supported yet".into()));
        };
        attributes(argument.attributes.as_ref(), &["AllowShared"])?;
        attributes(argument.type_.attributes.as_ref(), &["AllowShared"])?;
        let allow_shared = argument.attributes.iter().chain(argument.type_.attributes.iter())
            .any(|attributes| attributes.body.list.iter().any(|attribute| matches!(attribute, ExtendedAttribute::NoArgs(item) if item.0.0 == "AllowShared")));
        let present = format!("args.length > {index}");
        let value = format!("{present} ? args[{index}] : undefined");
        let lowered = js_type(database, &argument.type_.type_)?;
        let converted = input(database, &lowered, &value, allow_shared)?;
        let converted = match (&argument.optional, &argument.default) {
            (None, None) if !optional => {
                required += 1;
                input(database, &lowered, &format!("args[{index}]"), allow_shared)?
            }
            (Some(_), Some(default)) => {
                optional = true;
                let literal = default_literal(&lowered, &default.value)?;
                // https://webidl.spec.whatwg.org/#idl-default-values
                let literal = input(database, &lowered, &literal, allow_shared)?;
                format!("{present} && args[{index}] !== undefined ? {converted} : {literal}")
            }
            _ => return Err(Error("JS argument optionality is not supported yet".into())),
        };
        conversions.push(converted);
    }
    Ok((required, format!("[{}]", conversions.join(","))))
}

// https://webidl.spec.whatwg.org/#idl-default-values
fn default_literal(
    type_: &JsType,
    default: &weedle::literal::DefaultValue<'_>,
) -> Result<String, Error> {
    use weedle::literal::{DefaultValue, FloatLit};

    let literal = match default {
        DefaultValue::String(default) => match type_ {
            JsType::Native(ReturnType::String | ReturnType::UsvString) => {
                format!("{:?}", default.0)
            }
            _ => return Err(Error("JS string defaults require string arguments".into())),
        },
        DefaultValue::Boolean(default) => match type_ {
            JsType::Native(ReturnType::Boolean) => default.0.to_string(),
            _ => {
                return Err(Error(
                    "JS boolean defaults require boolean arguments".into(),
                ));
            }
        },
        // https://webidl.spec.whatwg.org/#idl-default-values
        DefaultValue::Integer(default) => match type_ {
            JsType::Native(
                ReturnType::UnsignedShort
                | ReturnType::UnsignedLong
                | ReturnType::UnsignedLongLong
                | ReturnType::Short
                | ReturnType::Long
                | ReturnType::Double
                | ReturnType::RestrictedDouble,
            ) => integer_default(default)?,
            _ => {
                return Err(Error(
                    "JS integer defaults require numeric arguments".into(),
                ));
            }
        },
        DefaultValue::Float(default) => match type_ {
            JsType::Native(ReturnType::Double | ReturnType::RestrictedDouble) => match default {
                FloatLit::Value(value) => value.0.trim().to_owned(),
                FloatLit::NaN(_) => "NaN".to_owned(),
                FloatLit::Infinity(_) => "Infinity".to_owned(),
                FloatLit::NegInfinity(_) => "-Infinity".to_owned(),
            },
            _ => return Err(Error("JS float defaults require double arguments".into())),
        },
        DefaultValue::Null(_) => "null".to_owned(),
        DefaultValue::EmptyArray(_) | DefaultValue::EmptyDictionary(_) => {
            return Err(Error("JS argument defaults are not supported yet".into()));
        }
    };
    Ok(literal)
}

fn integer_default(default: &weedle::literal::IntegerLit<'_>) -> Result<String, Error> {
    use weedle::literal::IntegerLit;

    let (text, radix) = match default {
        IntegerLit::Dec(value) => (value.0, 10),
        IntegerLit::Hex(value) => (value.0, 16),
        IntegerLit::Oct(value) => (value.0, 8),
    };
    let text = text.trim();
    let (negative, digits) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text),
    };
    let digits = match radix {
        16 => digits
            .strip_prefix("0x")
            .or_else(|| digits.strip_prefix("0X"))
            .unwrap_or(digits),
        8 => digits.strip_prefix('0').unwrap_or(digits),
        _ => digits,
    };
    let magnitude = if digits.is_empty() {
        0
    } else {
        i128::from_str_radix(digits, radix)
            .map_err(|_| Error(format!("IDL integer default is out of range: {text}")))?
    };
    Ok(if negative { -magnitude } else { magnitude }.to_string())
}

fn input(
    database: &Database<'_>,
    type_: &JsType,
    value: &str,
    allow_shared: bool,
) -> Result<String, Error> {
    // https://webidl.spec.whatwg.org/#es-type-mapping
    match type_ {
        JsType::ArrayBufferView => Ok(format!("__tbIDLBufferSource({value}, {allow_shared})")),
        JsType::Sequence(element) => {
            let converted = input(database, element, "item", false)?;
            Ok(format!("__tbIDLSequence({value}, item => {converted})"))
        }
        JsType::FrozenArray(_) => Err(Error("FrozenArray is a result type only".into())),
        JsType::Promise(_) => Err(Error("Promise is a result type only".into())),
        JsType::Native(ReturnType::String) if !allow_shared => {
            Ok(format!("__tbIDLString({value})"))
        }
        JsType::Native(ReturnType::UsvString) if !allow_shared => {
            Ok(format!("__tbIDLUSVString({value})"))
        }
        JsType::Native(ReturnType::Boolean) => Ok(format!("__tbIDLBoolean({value})")),
        JsType::Native(ReturnType::UnsignedLong) => Ok(format!("__tbIDLUnsignedLong({value})")),
        JsType::Native(ReturnType::UnsignedLongLong) => {
            Ok(format!("__tbIDLUnsignedLongLong({value})"))
        }
        JsType::Native(ReturnType::Long) => Ok(format!("__tbIDLLong({value})")),
        JsType::Native(ReturnType::Double) => Ok(format!("__tbIDLDouble({value})")),
        JsType::Native(ReturnType::RestrictedDouble) => {
            Ok(format!("__tbIDLRestrictedDouble({value})"))
        }
        JsType::Native(ReturnType::Uint8Array) => {
            Ok(format!("__tbIDLUint8Array({value}, {allow_shared})"))
        }
        JsType::Native(ReturnType::Enumeration(name)) => {
            let keywords = enum_keywords(database, name)?;
            Ok(format!(
                "__tbIDLEnum({value}, [{}])",
                keywords
                    .iter()
                    .map(|keyword| format!("{keyword:?}"))
                    .collect::<Vec<_>>()
                    .join(",")
            ))
        }
        JsType::Native(ReturnType::Dictionary(name)) => {
            let dictionary = database.dictionary(name)?;
            attributes(dictionary.attributes.as_ref(), &[])?;
            if dictionary.inheritance.is_some() {
                return Err(Error(
                    "JS argument dictionary inheritance is not supported yet".into(),
                ));
            }
            let mut fields = Vec::new();
            for member in dictionary.members.body {
                attributes(member.attributes.as_ref(), &[])?;
                attributes(member.type_.attributes.as_ref(), &[])?;
                if member.default.is_some() {
                    return Err(Error(
                        "JS argument dictionary defaults are not supported yet".into(),
                    ));
                }
                let lowered = js_type(database, &member.type_.type_)?;
                let converted = input(database, &lowered, "member", false)?;
                fields.push(format!(
                    "[{:?}, member => {converted}, {}]",
                    member.identifier.0,
                    member.required.is_some()
                ));
            }
            Ok(format!(
                "__tbIDLDictionary({value}, [{}])",
                fields.join(",")
            ))
        }
        JsType::Native(
            ReturnType::Any
            | ReturnType::PlatformObject
            | ReturnType::Node
            | ReturnType::NullableNode
            | ReturnType::NullableDocumentType,
        ) => Ok(value.to_owned()),
        JsType::Native(other) => Err(Error(format!(
            "JS argument type is not supported yet: {other:?}"
        ))),
    }
}

fn output(database: &Database<'_>, type_: &JsType, value: &str) -> Result<String, Error> {
    match type_ {
        // https://webidl.spec.whatwg.org/#es-buffer-source-types
        JsType::ArrayBufferView
        | JsType::Native(
            ReturnType::Any
            | ReturnType::PlatformObject
            | ReturnType::Node
            | ReturnType::NullableNode
            | ReturnType::NullableDocumentType,
        ) => Ok(value.to_owned()),
        JsType::Sequence(element) => {
            let converted = output(database, element, "item")?;
            Ok(format!(
                "__tbIDLSequenceResult({value}, item => {converted})"
            ))
        }
        JsType::FrozenArray(element) => {
            let converted = output(database, element, "item")?;
            Ok(format!(
                "__tbIDLFrozenArrayResult({value}, item => {converted})"
            ))
        }
        JsType::Promise(inner) => {
            let converted = output(database, inner, "item")?;
            Ok(format!("__tbIDLPromise({value}, item => {converted})"))
        }
        JsType::Native(ReturnType::String | ReturnType::UsvString) => {
            Ok(format!("__tbIDLResult({value}, 'string')"))
        }
        JsType::Native(ReturnType::Uint8Array) => {
            Ok(format!("__tbIDLRealmUint8Array({value}, false, realm)"))
        }
        JsType::Native(ReturnType::Boolean) => Ok(format!("__tbIDLResult({value}, 'boolean')")),
        JsType::Native(
            ReturnType::UnsignedLong
            | ReturnType::UnsignedLongLong
            | ReturnType::Long
            | ReturnType::Double
            | ReturnType::RestrictedDouble,
        ) => Ok(format!("__tbIDLResultNumber({value})")),
        JsType::Native(ReturnType::Enumeration(name)) => {
            let keywords = enum_keywords(database, name)?;
            Ok(format!(
                "__tbIDLResultEnum({value}, [{}])",
                keywords
                    .iter()
                    .map(|keyword| format!("{keyword:?}"))
                    .collect::<Vec<_>>()
                    .join(",")
            ))
        }
        // https://webidl.spec.whatwg.org/#es-to-js-dictionary
        JsType::Native(ReturnType::Dictionary(name)) => {
            let dictionary = database.dictionary(name)?;
            attributes(dictionary.attributes.as_ref(), &[])?;
            if dictionary.inheritance.is_some() {
                return Err(Error(
                    "JS result dictionary inheritance is not supported yet".into(),
                ));
            }
            let mut fields = Vec::new();
            for member in dictionary.members.body {
                attributes(member.attributes.as_ref(), &[])?;
                attributes(member.type_.attributes.as_ref(), &[])?;
                if member.required.is_some() || member.default.is_some() {
                    return Err(Error(
                        "JS result dictionary required/default fields are not supported yet".into(),
                    ));
                }
                let lowered = js_type(database, &member.type_.type_)?;
                let converted = output(database, &lowered, "member")?;
                fields.push(format!(
                    "[{:?}, member => {converted}]",
                    member.identifier.0
                ));
            }
            fields.sort_unstable();
            if fields.windows(2).any(|pair| pair[0] == pair[1]) {
                return Err(Error("duplicate JS result dictionary field".into()));
            }
            Ok(format!(
                "__tbIDLResultDictionary({value}, [{}])",
                fields.join(",")
            ))
        }
        JsType::Native(other) => Err(Error(format!(
            "JS result type is not supported yet: {other:?}"
        ))),
    }
}

fn enum_keywords(database: &Database<'_>, name: &str) -> Result<Vec<String>, Error> {
    let Some(weedle::Definition::Enum(definition)) = database.definition(name) else {
        return Err(Error(format!("unknown enumeration: {name}")));
    };
    Ok(crate::model::Enumeration::parse(definition)?
        .values
        .into_iter()
        .map(|(keyword, _)| keyword)
        .collect())
}
