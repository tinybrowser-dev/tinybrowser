use proc_macro2::TokenStream;
use quote::{format_ident, quote};

use crate::model::{
    ArgumentArity, Attribute, ConstructorArgumentKind, Dictionary, DictionaryField,
    DictionaryFieldType, Enumeration, GetterMapping, Interface, InterfaceKind, Operation,
    OperationArgument, OperationResult, Payload, PropertyGetter, PropertyHooks, PrototypeParent,
    ReturnType, Setter, Union, UnionDefault, UnionMemberType,
};

pub(crate) fn interface(interface: &Interface) -> TokenStream {
    let contract = &interface.contract;
    let rust = &interface.rust;
    let module = module_name(&interface.name);
    let payload = if interface.has_lifetime {
        quote! { #rust<'js> }
    } else {
        quote! { #rust }
    };
    let (required, constructor_body) = constructor(interface);
    let dictionaries = interface.dictionaries.iter().map(dictionary);
    let enumerations = interface.enumerations.iter().map(enumeration);
    let unions = interface.unions.iter().map(union);
    let tables = member_tables(interface);
    let unscopables = unscopables(interface);
    let (routes, groups) = dispatch_groups(interface, &payload, "dispatch");
    let (alternate_dispatch, alternate_groups) = alternate_dispatch(interface);
    let constructor_dispatch = match interface.kind {
        InterfaceKind::Complete => quote! {
            if operation.index() == 0 {
                #constructor_body
            }
        },
        InterfaceKind::Partial => quote! {},
    };
    let has_dispatch =
        !routes.is_empty() || !constructor_dispatch.is_empty() || !alternate_dispatch.is_empty();
    let definition = definition(interface, &payload, required, has_dispatch);
    let receiver_check = receiver_check(interface, &interface.rust);
    let unknown_operation =
        quote! { Err(rquickjs::Exception::throw_internal(&ctx, "unknown native operation")) };
    let operation_routes = if routes.is_empty() {
        unknown_operation
    } else {
        quote! {
            match operation.index() {
                #(#routes,)*
                _ => #unknown_operation,
            }
        }
    };
    let dispatch_fn = if has_dispatch {
        quote! {
            // https://webidl.spec.whatwg.org/#es-interface-call
            fn dispatch<'js>(operation: host::Operation, params: &Params<'_, 'js>) -> Result<Value<'js>> {
                let ctx = params.ctx().clone();
                #constructor_dispatch
                #alternate_dispatch
                // https://webidl.spec.whatwg.org/#es-attributes — omitted setter argument converts undefined per WebIDL rather than throwing like Chromium and Firefox
                // https://webidl.spec.whatwg.org/#es-operations
                let receiver = host::receiver::<#payload>(params)?;
                let receiver = receiver.borrow();
                #receiver_check
                #operation_routes
            }

            #(#groups)*
            #(#alternate_groups)*
        }
    } else {
        quote! {}
    };
    let imports = if has_dispatch {
        quote! {
            use super::#rust;
            use crate::js::bindings::host;
            use rquickjs::{Ctx, Result, Value};
            use rquickjs::function::Params;
        }
    } else {
        quote! {
            use crate::js::bindings::host;
            use rquickjs::{Ctx, Result};
        }
    };
    quote! {
        pub(crate) mod #module {
            #imports

            #tables
            #contract
            #definition
            #(#dictionaries)*
            #(#enumerations)*
            #(#unions)*
            #unscopables
            #dispatch_fn
        }
    }
}

fn optional_keyword(path: Option<&syn::Path>) -> TokenStream {
    if let Some(path) = path {
        quote! { Some(#path.as_keyword()) }
    } else {
        quote! { None }
    }
}

fn reflect_getter_target(interface: &str, attribute: &Attribute) -> Option<TokenStream> {
    let (content, kind) = match &attribute.mapping {
        GetterMapping::Method => return None,
        GetterMapping::Reflect { content } => {
            let kind = match attribute.return_type {
                ReturnType::String | ReturnType::UsvString => {
                    quote! { host::ReflectKind::GetString }
                }
                ReturnType::Boolean => quote! { host::ReflectKind::GetBool },
                _ => unreachable!("validated reflect mapping"),
            };
            (content.as_str(), kind)
        }
        GetterMapping::ReflectUrl { content } => {
            (content.as_str(), quote! { host::ReflectKind::GetUrl })
        }
        GetterMapping::ReflectEnumerated {
            content,
            type_path,
            missing,
            invalid,
        } => {
            let missing = optional_keyword(missing.as_ref());
            let invalid = optional_keyword(invalid.as_ref());
            let nullable = matches!(attribute.return_type, ReturnType::NullableString);
            let kind = quote! {
                host::ReflectKind::GetEnumerated {
                    keywords: <#type_path as crate::js::bindings::enumerated::Enumerated>::KEYWORD_STRS,
                    missing: #missing,
                    invalid: #invalid,
                    nullable: #nullable,
                }
            };
            (content.as_str(), kind)
        }
    };
    Some(quote! {
        host::AccessorTarget::Reflect(&host::ReflectBinding {
            interface: #interface,
            content: #content,
            kind: #kind,
        })
    })
}

fn reflect_setter_target(interface: &str, attribute: &Attribute) -> Option<TokenStream> {
    let Some(Setter::Reflect { content }) = &attribute.setter else {
        return None;
    };
    let reactions = attribute.reactions;
    let kind = match attribute.return_type {
        ReturnType::String => {
            let legacy_null = attribute.legacy_null_to_empty;
            quote! { host::ReflectKind::SetString { legacy_null: #legacy_null, reactions: #reactions } }
        }
        ReturnType::UsvString => {
            quote! { host::ReflectKind::SetUsvString { reactions: #reactions } }
        }
        ReturnType::NullableString => {
            quote! { host::ReflectKind::SetEnumeratedNullable { reactions: #reactions } }
        }
        ReturnType::Boolean => quote! { host::ReflectKind::SetBool { reactions: #reactions } },
        ReturnType::Long => quote! { host::ReflectKind::SetLong { reactions: #reactions } },
        ReturnType::UnsignedLong => {
            quote! { host::ReflectKind::SetUlong { reactions: #reactions } }
        }
        ReturnType::Double => {
            quote! { host::ReflectKind::SetDouble { restricted: false, reactions: #reactions } }
        }
        ReturnType::RestrictedDouble => {
            quote! { host::ReflectKind::SetDouble { restricted: true, reactions: #reactions } }
        }
        _ => unreachable!("validated reflect mapping"),
    };
    Some(quote! {
        host::AccessorTarget::Reflect(&host::ReflectBinding {
            interface: #interface,
            content: #content,
            kind: #kind,
        })
    })
}

fn member_tables(interface: &Interface) -> TokenStream {
    let interface_name = interface.name.as_str();
    let attributes_members = interface
        .attributes
        .iter()
        .enumerate()
        .map(|(index, attribute)| {
            let id = index * 2 + 1;
            let name = &attribute.name;
            let getter = reflect_getter_target(interface_name, attribute)
                .unwrap_or_else(|| quote! { host::AccessorTarget::Operation(host::Operation::new(#id)) });
            let setter = if let Some(reflect) = reflect_setter_target(interface_name, attribute) {
                quote! { Some(#reflect) }
            } else if attribute.setter.is_some() {
                let id = id + 1;
                quote! { Some(host::AccessorTarget::Operation(host::Operation::new(#id))) }
            } else {
                quote! { None }
            };
            let secure_context = attribute.exposure.secure_context;
            quote! { host::Member { name: #name, secure_context: #secure_context, kind: host::MemberKind::Attribute {
                getter: #getter, setter: #setter,
            } } }
        });
    let operation_members = interface.operations.iter().enumerate().map(|(index, operation)| {
        let id = interface.attributes.len() * 2 + index + 1;
        let name = &operation.name;
        let length = operation
        .arguments
        .iter()
        .take_while(|argument| argument.arity == ArgumentArity::Required)
        .count();
        let secure_context = operation.exposure.secure_context;
        quote! { host::Member { name: #name, secure_context: #secure_context, kind: host::MemberKind::Method { operation: host::Operation::new(#id), length: #length } } }
    });
    let stringifier = interface.stringifier.as_ref().map(|_| {
        let id = interface.attributes.len() * 2 + interface.operations.len() + 1;
        quote! { host::Member { name: "toString", secure_context: false, kind: host::MemberKind::Method { operation: host::Operation::new(#id), length: 0 } } }
    });
    let members = attributes_members
        .chain(operation_members)
        .chain(stringifier);
    let constants = interface.constants.iter().map(|constant| {
        let name = &constant.name;
        let value = constant.value;
        let secure_context = constant.exposure.secure_context;
        quote! { host::Constant { name: #name, value: #value, secure_context: #secure_context } }
    });
    quote! {
        const MEMBERS: &[host::Member] = &[#(#members),*];
        const CONSTANTS: &[host::Constant] = &[#(#constants),*];
    }
}

fn receiver_check(interface: &Interface, rust: &proc_macro2::Ident) -> TokenStream {
    let name = &interface.name;
    if matches!(interface.kind, InterfaceKind::Partial) {
        quote! { host::SharedClass::require_interface(&*receiver, &ctx, #name)?; }
    } else if rust == "JsNode" && interface.name != "Node" {
        quote! { host::require_node_interface(&ctx, receiver.node_id(), #name)?; }
    } else {
        quote! {}
    }
}

fn payload_type(payload: &Payload) -> TokenStream {
    let rust = &payload.rust;
    if payload.has_lifetime {
        quote! { crate::js::bindings::#rust<'js> }
    } else {
        quote! { crate::js::bindings::#rust }
    }
}

fn alternate_dispatch(interface: &Interface) -> (TokenStream, Vec<TokenStream>) {
    let mut arms = Vec::new();
    let mut groups = Vec::new();
    for (index, payload) in interface.payloads.iter().enumerate() {
        let payload = payload_type(payload);
        let prefix = format!("dispatch_{index}");
        let (routes, mut payload_groups) = dispatch_groups(interface, &payload, &prefix);
        let check = if interface.payloads[index].rust == "JsNode" {
            let name = &interface.name;
            quote! { host::require_node_interface(&ctx, receiver.node_id(), #name)?; }
        } else {
            let name = &interface.name;
            quote! { host::SharedClass::require_interface(&*receiver, &ctx, #name)?; }
        };
        arms.push(quote! {
            if let Ok(receiver) = rquickjs::Class::<#payload>::from_value(&params.this()) {
                let receiver = receiver.borrow();
                #check
                return match operation.index() {
                    #(#routes,)*
                    _ => Err(rquickjs::Exception::throw_internal(&ctx, "unknown native operation")),
                };
            }
        });
        groups.append(&mut payload_groups);
    }
    (quote! { #(#arms)* }, groups)
}

fn dispatch_groups(
    interface: &Interface,
    payload: &TokenStream,
    prefix: &str,
) -> (Vec<TokenStream>, Vec<TokenStream>) {
    let mut arms = Vec::new();
    for (index, attribute) in interface.attributes.iter().enumerate() {
        if matches!(attribute.mapping, GetterMapping::Method) {
            arms.push((index * 2 + 1, false, getter_dispatch(attribute, interface)));
        }
        if let Some(setter) = setter_dispatch(attribute, interface) {
            arms.push((index * 2 + 2, true, setter));
        }
    }
    for (index, operation) in interface.operations.iter().enumerate() {
        let id = interface.attributes.len() * 2 + index + 1;
        arms.push((id, true, operation_dispatch(operation, interface)));
    }
    if let Some(stringifier) = &interface.stringifier {
        let id = interface.attributes.len() * 2 + interface.operations.len() + 1;
        let body = match stringifier {
            crate::model::Stringifier::Attribute(index) => {
                getter_dispatch(&interface.attributes[*index], interface)
            }
            crate::model::Stringifier::Method => quote! {
                let result: rquickjs::String = receiver.stringifier(&ctx)?;
                rquickjs::IntoJs::into_js(result, &ctx)
            },
        };
        arms.push((id, false, body));
    }
    let mut routes = Vec::new();
    let mut groups = Vec::new();
    for (index, chunk) in arms.chunks(4).enumerate() {
        let name = format_ident!("{prefix}_group_{index}");
        let first = chunk[0].0;
        let last = chunk[chunk.len() - 1].0;
        let uses_params = chunk.iter().any(|(_, uses_params, _)| *uses_params);
        let member_routes = chunk
            .iter()
            .map(|(id, uses_params, body)| {
                let member = format_ident!("{prefix}_op_{id}");
                let call = member_call(*uses_params, body);
                quote! { #id => #member(#call) }
            })
            .collect::<Vec<_>>();
        for (id, uses_params, body) in chunk {
            let member = format_ident!("{prefix}_op_{id}");
            groups.push(member_function(&member, payload, *uses_params, body));
        }
        routes.push(group_route(&name, uses_params, first, last));
        groups.push(group_function(&name, payload, uses_params, &member_routes));
    }
    (routes, groups)
}

fn member_call(uses_params: bool, body: &TokenStream) -> TokenStream {
    let tokens = body.to_string();
    let mut call = Vec::new();
    if tokens.contains("ctx") {
        call.push(quote! { ctx });
    }
    if tokens.contains("receiver") {
        call.push(quote! { receiver });
    }
    if uses_params {
        call.push(quote! { params });
    }
    quote! { #(#call),* }
}

fn member_function(
    member: &proc_macro2::Ident,
    payload: &TokenStream,
    uses_params: bool,
    body: &TokenStream,
) -> TokenStream {
    let tokens = body.to_string();
    let mut signature = Vec::new();
    if tokens.contains("ctx") {
        signature.push(quote! { ctx: &Ctx<'js> });
    }
    if tokens.contains("receiver") {
        signature.push(quote! { receiver: &#payload });
    }
    if uses_params {
        signature.push(quote! { params: &Params<'_, 'js> });
    }
    let shadow = tokens
        .contains("ctx")
        .then(|| quote! { let ctx = ctx.clone(); });
    quote! {
        fn #member<'js>(#(#signature),*) -> Result<Value<'js>> {
            #shadow
            #body
        }
    }
}

fn group_route(
    name: &proc_macro2::Ident,
    uses_params: bool,
    first: usize,
    last: usize,
) -> TokenStream {
    let call = if uses_params {
        quote! { #name(&ctx, &receiver, operation, params) }
    } else {
        quote! { #name(&ctx, &receiver, operation) }
    };
    quote! { #first..=#last => #call }
}

fn group_function(
    name: &proc_macro2::Ident,
    payload: &TokenStream,
    uses_params: bool,
    member_routes: &[TokenStream],
) -> TokenStream {
    let signature = if uses_params {
        quote! {
            ctx: &Ctx<'js>,
            receiver: &#payload,
            operation: host::Operation,
            params: &Params<'_, 'js>,
        }
    } else {
        quote! {
            ctx: &Ctx<'js>,
            receiver: &#payload,
            operation: host::Operation,
        }
    };
    quote! {
        fn #name<'js>(#signature) -> Result<Value<'js>> {
            match operation.index() {
                #(#member_routes,)*
                _ => Err(rquickjs::Exception::throw_internal(ctx, "unknown native operation")),
            }
        }
    }
}

fn definition(
    interface: &Interface,
    payload: &TokenStream,
    required: usize,
    has_dispatch: bool,
) -> TokenStream {
    let name = &interface.name;
    let property_hooks = indexed_property_hooks(interface);
    let parent = match &interface.parent {
        Some(PrototypeParent::Intrinsic(parent) | PrototypeParent::Interface(parent)) => {
            quote! { Some(#parent) }
        }
        None => quote! { None },
    };
    let constructor_parent = match &interface.parent {
        Some(PrototypeParent::Interface(parent)) => quote! {
            let parent: rquickjs::Object = ctx.globals().get(#parent)?;
            constructor.set_prototype(Some(&parent))?;
        },
        Some(PrototypeParent::Intrinsic(_)) | None => quote! {},
    };
    let secure_context = interface.secure_context;
    let prototype_gate = if secure_context {
        quote! { if !host::is_secure_context(ctx)? { return Ok(None); } }
    } else {
        quote! {}
    };
    let constructor_gate = prototype_gate.clone();
    match interface.kind {
        InterfaceKind::Complete => quote! {
            use rquickjs::class::{JsClass, Readable};
            use rquickjs::function::Constructor;
            impl<'js> JsClass<'js> for #payload {
                const NAME: &'static str = #name;
                type Mutable = Readable;
                #property_hooks

                fn prototype(ctx: &Ctx<'js>) -> Result<Option<rquickjs::Object<'js>>> {
                    #prototype_gate
                    let prototype = host::prototype(ctx, #name, #parent, MEMBERS, CONSTANTS, Some(dispatch))?;
                    host::install_unscopables(&prototype, UNSCOPABLES)?;
                    Ok(Some(prototype))
                }

                fn constructor(ctx: &Ctx<'js>) -> Result<Option<Constructor<'js>>> {
                    #constructor_gate
                    let prototype = Self::prototype(ctx)?.ok_or_else(|| {
                        rquickjs::Exception::throw_internal(ctx, "native interface has no prototype")
                    })?;
                    let constructor = host::constructor(ctx, #name, #required, &prototype, CONSTANTS,
                        host::HostCall::new(dispatch, host::Operation::new(0)))?;
                    #constructor_parent
                    Ok(Some(constructor))
                }
            }
        },
        InterfaceKind::Partial => {
            let dispatcher = if has_dispatch {
                quote! { Some(dispatch) }
            } else {
                quote! { None }
            };
            let targets: Vec<&str> = if interface.install_targets.is_empty() {
                vec![name.as_str()]
            } else {
                interface
                    .install_targets
                    .iter()
                    .map(String::as_str)
                    .collect()
            };
            quote! {
                pub(crate) fn install(ctx: &Ctx<'_>) -> Result<()> {
                    #(
                        host::install_interface(
                            ctx,
                            #targets,
                            MEMBERS,
                            CONSTANTS,
                            UNSCOPABLES,
                            #dispatcher,
                        )?;
                    )*
                    Ok(())
                }
            }
        }
    }
}

struct NamedHooks {
    descriptor: TokenStream,
    names: TokenStream,
    define: TokenStream,
    set: TokenStream,
    delete: TokenStream,
}

impl NamedHooks {
    fn none() -> Self {
        Self {
            descriptor: quote! { return Ok(None); },
            names: quote! {},
            define: quote! {},
            set: quote! {},
            delete: quote! { return Ok(true); },
        }
    }

    fn parse(interface: &Interface) -> Self {
        let PropertyHooks::IndexedNamed {
            names,
            unenumerable,
            override_builtins,
        } = &interface.properties
        else {
            return Self::none();
        };
        let named_item = &interface
            .operations
            .iter()
            .find(|operation| operation.getter == Some(PropertyGetter::Named))
            .expect("validated named item")
            .rust;
        let enumerable = !unenumerable;
        // https://html.spec.whatwg.org/multipage/common-dom-interfaces.html#htmloptionscollection
        if *override_builtins {
            return Self {
                descriptor: quote! {
                    if receiver.#names(ctx)?.contains(&name) {
                        let value = receiver.#named_item(ctx.clone(), rquickjs::String::from_str(ctx.clone(), &name)?)?;
                        return Ok(Some(rquickjs::class::PropertyDescriptor::new_value(value, true, #enumerable, false)));
                    }
                    return Ok(None);
                },
                names: quote! {
                    for name in this.borrow().#names(ctx)? {
                        if crate::js::bindings::array_index(&name).is_none() {
                            names.push(rquickjs::class::PropertyName {
                                atom: rquickjs::Atom::from_str(ctx.clone(), &name)?,
                                is_enumerable: #enumerable,
                            });
                        }
                    }
                },
                define: quote! {
                    if this.borrow().#names(ctx)?.contains(&name) {
                        return Ok(rquickjs::class::ExoticDefineResult::Handled(false));
                    }
                },
                set: quote! {
                    if object != receiver && crate::js::bindings::array_index(&name).is_none() {
                        return Ok(rquickjs::class::ExoticSetResult::FallthroughSkippingOwnProperty);
                    }
                },
                delete: quote! {
                    return Ok(!this.borrow().#names(ctx)?.contains(&name));
                },
            };
        }
        Self {
            descriptor: quote! {
                if receiver.#names(ctx)?.contains(&name)
                    && crate::js::bindings::named_key_visible(&object, &name)? {
                    let value = receiver.#named_item(ctx.clone(), rquickjs::String::from_str(ctx.clone(), &name)?)?;
                    return Ok(Some(rquickjs::class::PropertyDescriptor::new_value(value, true, #enumerable, false)));
                }
                return Ok(None);
            },
            names: quote! {
                for name in this.borrow().#names(ctx)? {
                    if crate::js::bindings::array_index(&name).is_none()
                        && crate::js::bindings::named_key_visible(&object, &name)? {
                        names.push(rquickjs::class::PropertyName {
                            atom: rquickjs::Atom::from_str(ctx.clone(), &name)?,
                            is_enumerable: #enumerable,
                        });
                    }
                }
            },
            define: quote! {
                if this.borrow().#names(ctx)?.contains(&name) {
                    return Ok(rquickjs::class::ExoticDefineResult::Handled(false));
                }
            },
            set: quote! {
                if object != receiver && crate::js::bindings::array_index(&name).is_none() {
                    return Ok(rquickjs::class::ExoticSetResult::FallthroughSkippingOwnProperty);
                }
            },
            delete: quote! {
                return Ok(!this.borrow().#names(ctx)?.contains(&name)
                    || !crate::js::bindings::named_key_visible(&object, &name)?);
            },
        }
    }
}

fn indexed_property_hooks(interface: &Interface) -> TokenStream {
    if !matches!(
        interface.properties,
        PropertyHooks::Indexed | PropertyHooks::IndexedNamed { .. }
    ) {
        return quote! {};
    }
    let length = &interface
        .attributes
        .iter()
        .find(|attribute| attribute.name == "length")
        .expect("validated indexed length")
        .rust;
    let item = interface
        .operations
        .iter()
        .find(|operation| operation.getter == Some(PropertyGetter::Indexed))
        .expect("validated indexed item");
    let named = NamedHooks::parse(interface);
    let object = match &interface.properties {
        PropertyHooks::IndexedNamed {
            override_builtins: true,
            ..
        } => format_ident!("_object"),
        PropertyHooks::IndexedNamed { .. } => format_ident!("object"),
        _ => format_ident!("_object"),
    };
    let define_receiver = if matches!(interface.properties, PropertyHooks::IndexedNamed { .. })
        || interface.indexed_setter.is_some()
    {
        format_ident!("this")
    } else {
        format_ident!("_this")
    };
    let mutable_names = if matches!(interface.properties, PropertyHooks::IndexedNamed { .. }) {
        quote! { mut }
    } else {
        quote! {}
    };
    indexed_hooks_body(
        length,
        item,
        interface.indexed_setter.as_ref(),
        &named,
        &object,
        &define_receiver,
        &mutable_names,
    )
}

struct SetterTokens {
    define_descriptor: proc_macro2::Ident,
    indexed_define: TokenStream,
    set_receiver: proc_macro2::Ident,
    set_value: proc_macro2::Ident,
    indexed_set: TokenStream,
    writable: bool,
}

impl SetterTokens {
    fn parse(setter: Option<&crate::model::IndexedSetter>) -> Self {
        let Some(setter) = setter else {
            return Self {
                define_descriptor: format_ident!("_descriptor"),
                indexed_define: quote! {
                    if crate::js::bindings::array_index(&name).is_some() {
                        return Ok(rquickjs::class::ExoticDefineResult::Handled(false));
                    }
                },
                set_receiver: format_ident!("_this"),
                set_value: format_ident!("_value"),
                indexed_set: quote! {},
                writable: false,
            };
        };
        let method = &setter.rust;
        let call = if setter.reactions {
            quote! {
                crate::js::reactions::enter(ctx)?;
                let result = {
                    let receiver = this.borrow();
                    match receiver.#method(ctx.clone(), index, value) {
                        Ok(()) => Ok(Value::new_undefined(ctx.clone())),
                        Err(error) => Err(error),
                    }
                };
                crate::js::reactions::finish(ctx, result)?;
            }
        } else {
            quote! {
                let receiver = this.borrow();
                receiver.#method(ctx.clone(), index, value)?;
            }
        };
        Self {
            define_descriptor: format_ident!("descriptor"),
            indexed_define: quote! {
                if let Some(index) = crate::js::bindings::array_index(&name) {
                    if !descriptor.is_data_descriptor() {
                        return Ok(rquickjs::class::ExoticDefineResult::Handled(false));
                    }
                    let value = descriptor.value.clone().unwrap_or_else(|| Value::new_undefined(ctx.clone()));
                    #call
                    return Ok(rquickjs::class::ExoticDefineResult::Handled(true));
                }
            },
            set_receiver: format_ident!("this"),
            set_value: format_ident!("value"),
            indexed_set: quote! {
                if let Some(index) = crate::js::bindings::array_index(&name) {
                    if object != receiver {
                        return Ok(rquickjs::class::ExoticSetResult::Fallthrough);
                    }
                    #call
                    return Ok(rquickjs::class::ExoticSetResult::Handled(true));
                }
            },
            writable: true,
        }
    }
}

fn indexed_hooks_body(
    length: &proc_macro2::Ident,
    item: &Operation,
    setter: Option<&crate::model::IndexedSetter>,
    hooks: &NamedHooks,
    object: &proc_macro2::Ident,
    define_receiver: &proc_macro2::Ident,
    mutable_names: &TokenStream,
) -> TokenStream {
    let tokens = SetterTokens::parse(setter);
    indexed_hooks_with_tokens(
        length,
        item,
        &tokens,
        hooks,
        object,
        define_receiver,
        mutable_names,
    )
}

fn indexed_hooks_with_tokens(
    length: &proc_macro2::Ident,
    item: &Operation,
    tokens: &SetterTokens,
    hooks: &NamedHooks,
    object: &proc_macro2::Ident,
    define_receiver: &proc_macro2::Ident,
    mutable_names: &TokenStream,
) -> TokenStream {
    let descriptor = &hooks.descriptor;
    let supported = &hooks.names;
    let define = &hooks.define;
    let set = &hooks.set;
    let delete = &hooks.delete;
    // https://webidl.spec.whatwg.org/#legacy-platform-object-getownproperty
    let writable = tokens.writable;
    let define_descriptor = &tokens.define_descriptor;
    let indexed_define = &tokens.indexed_define;
    let set_receiver = &tokens.set_receiver;
    let set_value = &tokens.set_value;
    let indexed_set = &tokens.indexed_set;
    let method = &item.rust;
    let value = if matches!(item.result, OperationResult::NullableString) {
        quote! {
            let value = receiver.#method(ctx.clone(), index)?
                .map_or_else(|| Value::new_null(ctx.clone()), rquickjs::String::into_value);
        }
    } else {
        quote! { let value = rquickjs::IntoJs::into_js(receiver.#method(ctx.clone(), index)?, ctx)?; }
    };
    quote! {
        const KIND: rquickjs::class::ClassKind = rquickjs::class::ClassKind::Exotic;
        const EXOTIC_HOOKS: rquickjs::class::ExoticHooks = rquickjs::class::ExoticHooks {
            get: false, has: false, set: true, delete: true, define_own_property: true,
            get_own_property: true, get_own_property_names: true,
        };

        // https://webidl.spec.whatwg.org/#legacy-platform-object-getownproperty
        fn exotic_get_own_property(
            this: &rquickjs::class::JsCell<'js, Self>, ctx: &Ctx<'js>,
            atom: rquickjs::Atom<'js>, #object: Value<'js>,
        ) -> Result<Option<rquickjs::class::PropertyDescriptor<'js>>> {
            let Some(name) = crate::js::bindings::atom_name(ctx, &atom) else { return Ok(None); };
            let receiver = this.borrow();
            let Some(index) = crate::js::bindings::array_index(&name) else { #descriptor };
            if index as usize >= receiver.#length(ctx)? { return Ok(None); }
            #value
            Ok(Some(rquickjs::class::PropertyDescriptor::new_value(value, true, true, #writable)))
        }

        // https://webidl.spec.whatwg.org/#legacy-platform-object-ownpropertykeys
        fn exotic_get_own_property_names(
            this: &rquickjs::class::JsCell<'js, Self>, ctx: &Ctx<'js>, #object: Value<'js>,
        ) -> Result<Vec<rquickjs::class::PropertyName<'js>>> {
            let #mutable_names names = (0..this.borrow().#length(ctx)?)
                .map(|index| Ok(rquickjs::class::PropertyName {
                    atom: rquickjs::Atom::from_u32(ctx.clone(), u32::try_from(index)
                        .map_err(|_| rquickjs::Exception::throw_range(ctx, "collection index too large"))?)?,
                    is_enumerable: true,
                }))
                .collect::<Result<Vec<_>>>()?;
            #supported
            Ok(names)
        }

        // https://webidl.spec.whatwg.org/#legacy-platform-object-defineownproperty
        fn exotic_define_own_property(
            #define_receiver: &rquickjs::class::JsCell<'js, Self>, ctx: &Ctx<'js>,
            atom: rquickjs::Atom<'js>, #define_descriptor: rquickjs::class::PropertyDefinition<'js>,
        ) -> Result<rquickjs::class::ExoticDefineResult> {
            let Some(name) = crate::js::bindings::atom_name(ctx, &atom) else {
                return Ok(rquickjs::class::ExoticDefineResult::Fallthrough);
            };
            #indexed_define
            #define
            Ok(rquickjs::class::ExoticDefineResult::Fallthrough)
        }

        // https://webidl.spec.whatwg.org/#legacy-platform-object-set
        fn exotic_set_property(
            #set_receiver: &rquickjs::class::JsCell<'js, Self>, ctx: &Ctx<'js>, atom: rquickjs::Atom<'js>,
            object: Value<'js>, receiver: Value<'js>, #set_value: Value<'js>,
        ) -> Result<rquickjs::class::ExoticSetResult> {
            let Some(name) = crate::js::bindings::atom_name(ctx, &atom) else {
                return Ok(rquickjs::class::ExoticSetResult::Fallthrough);
            };
            #indexed_set
            #set
            Ok(crate::js::bindings::reject_indexed_write(&name, &object, &receiver))
        }

        // https://webidl.spec.whatwg.org/#legacy-platform-object-delete
        fn exotic_delete_property(
            this: &rquickjs::class::JsCell<'js, Self>, ctx: &Ctx<'js>,
            atom: rquickjs::Atom<'js>, #object: Value<'js>,
        ) -> Result<bool> {
            let Some(name) = crate::js::bindings::atom_name(ctx, &atom) else { return Ok(true); };
            let Some(index) = crate::js::bindings::array_index(&name) else { #delete };
            Ok(index as usize >= this.borrow().#length(ctx)?)
        }
    }
}

fn method_call(interface: &Interface, method: &syn::Ident, arguments: &TokenStream) -> TokenStream {
    if interface.contract.is_some() {
        let contract = format_ident!("{}", interface.name);
        quote! { #contract::#method(receiver, #arguments) }
    } else {
        quote! { receiver.#method(#arguments) }
    }
}

fn getter_dispatch(getter: &Attribute, interface: &Interface) -> TokenStream {
    let method = &getter.rust;
    let ctx_arg = quote! { &ctx };
    let call = method_call(interface, method, &ctx_arg);
    match getter.return_type {
        ReturnType::String
        | ReturnType::Boolean
        | ReturnType::UnsignedShort
        | ReturnType::UnsignedLong
        | ReturnType::Long
        | ReturnType::Double
        | ReturnType::RestrictedDouble => {
            quote! { host::ret(#call?, &ctx) }
        }
        ReturnType::UsvString => {
            quote! {
                let result = #call?;
                host::ret_usv(&result, &ctx)
            }
        }
        ReturnType::NullableString | ReturnType::NullableUnsignedLong => {
            quote! { host::ret_nullable(#call?, &ctx) }
        }
        ReturnType::NullableNode | ReturnType::PlatformObject => {
            quote! { #call }
        }
        ReturnType::Union(..) | ReturnType::NullableUnion(..) => {
            unreachable!("attribute unions collapse to platform objects at lowering")
        }
        ReturnType::PromiseUndefined => {
            unreachable!("promise attributes are rejected at lowering")
        }
        ReturnType::Enumeration(_) => quote! {
            let result = #call?;
            host::ret_enum(result.as_str(), &ctx)
        },
        ReturnType::Node
        | ReturnType::Callback
        | ReturnType::Dictionary(_)
        | ReturnType::InterfaceSequence
        | ReturnType::StringSequence
        | ReturnType::NullableDocumentType
        | ReturnType::Uint8Array
        | ReturnType::UnsignedLongLong
        | ReturnType::Short
        | ReturnType::Any => {
            unreachable!("validated field mapping")
        }
    }
}

fn setter_value_conversion(return_type: &ReturnType, legacy_null_to_empty: bool) -> TokenStream {
    match return_type {
        ReturnType::String if legacy_null_to_empty => quote! {
            host::legacy_null_string_argument(params, 0)?
        },
        ReturnType::String => quote! { host::string_argument(params, 0, None)? },
        // https://webidl.spec.whatwg.org/#es-USVString
        ReturnType::UsvString => quote! { host::usv_string_argument(params, 0)? },
        ReturnType::NullableString => quote! { host::nullable_string_argument(params, 0)? },
        ReturnType::NullableUnsignedLong => {
            quote! { host::nullable_ulong_argument(params, 0)? }
        }
        // https://webidl.spec.whatwg.org/#es-boolean
        ReturnType::Boolean => quote! { host::boolean_argument(params, 0)? },
        // https://webidl.spec.whatwg.org/#es-unsigned-long
        ReturnType::UnsignedLong => quote! { host::ulong_argument(params, 0)? },
        // https://webidl.spec.whatwg.org/#es-long
        ReturnType::Long => quote! { host::long_argument(params, 0)? },
        // https://webidl.spec.whatwg.org/#es-double
        ReturnType::Double => quote! { host::double_argument(params, 0)? },
        ReturnType::RestrictedDouble => {
            quote! { host::restricted_double_argument(params, 0)? }
        }
        ReturnType::PlatformObject => quote! { host::fetch(params, 0) },
        ReturnType::Enumeration(name) => {
            let name = format_ident!("{name}");
            quote! {
                {
                    let value = host::fetch(params, 0);
                    #name::from_value(&ctx, value)?
                }
            }
        }
        _ => unreachable!(
            "validated attribute setter: string, USVString, boolean, integer, double, enumeration, or platform object"
        ),
    }
}

fn setter_dispatch(attribute: &Attribute, interface: &Interface) -> Option<TokenStream> {
    let setter = attribute.setter.as_ref()?;
    let ctx_arg = quote! { &ctx };
    let method = match setter {
        Setter::Method { rust } => rust,
        Setter::Reflect { .. } => return None,
        Setter::PutForwards { target, nullable } => {
            let name = &attribute.name;
            return Some(quote! {
                // https://webidl.spec.whatwg.org/#es-attributes — omitted setter argument converts undefined per WebIDL rather than throwing like Chromium and Firefox
                host::put_forwards(params, #name, #target, #nullable)
            });
        }
    };
    let convert = setter_value_conversion(&attribute.return_type, attribute.legacy_null_to_empty);
    let call = method_call(interface, method, &quote! { #ctx_arg, value });
    let body = if attribute.reactions {
        quote! {
            crate::js::reactions::enter(&ctx)?;
            let result = match #call {
                Ok(()) => Ok(host::ret_undefined(&ctx)),
                Err(error) => Err(error),
            };
            crate::js::reactions::finish(&ctx, result)
        }
    } else {
        quote! {
            #call?;
            Ok(host::ret_undefined(&ctx))
        }
    };
    Some(quote! {
        // https://webidl.spec.whatwg.org/#es-attributes — omitted setter argument converts undefined per WebIDL rather than throwing like Chromium and Firefox
        let value = #convert;
        #body
    })
}

fn operation_dispatch(operation: &Operation, interface: &Interface) -> TokenStream {
    let method = &operation.rust;
    let required = operation
        .arguments
        .iter()
        .take_while(|argument| argument.arity == ArgumentArity::Required)
        .count();
    let argument_names: Vec<_> = (0..operation.arguments.len())
        .map(|index| format_ident!("arg_{index}"))
        .collect();
    let arguments = operation
        .arguments
        .iter()
        .enumerate()
        .map(|(index, argument)| operation_argument(index, argument, &argument_names[index]));
    let arguments_call = if operation.takes_this {
        quote! {
                ctx.clone(),
                host::this_object(params)?,
                #(#argument_names),*
        }
    } else {
        quote! { ctx.clone(), #(#argument_names),* }
    };
    let call = method_call(interface, method, &arguments_call);
    let body = match operation.result {
        OperationResult::Object => quote! { #call },
        OperationResult::Union
        | OperationResult::String
        | OperationResult::Boolean
        | OperationResult::UnsignedShort
        | OperationResult::Short
        | OperationResult::Long => quote! {
            match #call {
                Ok(value) => host::ret(value, &ctx),
                Err(error) => Err(error),
            }
        },
        OperationResult::NullableUnion | OperationResult::NullableString => quote! {
            match #call {
                Ok(value) => host::ret_nullable(value, &ctx),
                Err(error) => Err(error),
            }
        },
        OperationResult::Undefined => quote! {
            match #call {
                Ok(()) => Ok(host::ret_undefined(&ctx)),
                Err(error) => Err(error),
            }
        },
        OperationResult::PromiseUndefined => quote! {
            match #call {
                Ok(()) => host::resolved_promise(&ctx),
                Err(error) => Err(error),
            }
        },
        OperationResult::Sequence => quote! {
            match #call {
                Ok(result) => host::sequence(&ctx, result),
                Err(error) => Err(error),
            }
        },
        OperationResult::StringSequence => quote! {
            match #call {
                Ok(result) => host::ret_string_sequence(result, &ctx),
                Err(error) => Err(error),
            }
        },
    };
    let body = if operation.reactions {
        quote! {
            crate::js::reactions::enter(&ctx)?;
            let result = #body;
            crate::js::reactions::finish(&ctx, result)
        }
    } else {
        body
    };
    quote! {
        host::require_arguments(params, #required)?;
        #(#arguments)*
        #body
    }
}

fn operation_argument(
    index: usize,
    argument: &OperationArgument,
    variable: &proc_macro2::Ident,
) -> TokenStream {
    if argument.arity == ArgumentArity::Variadic {
        let conversion =
            operation_argument_at(&quote! { index }, argument, &format_ident!("converted"));
        return quote! {
            // https://webidl.spec.whatwg.org/#es-overloads
            let mut #variable = Vec::with_capacity(params.len().saturating_sub(#index));
            for index in #index..params.len() {
                #conversion
                #variable.push(converted);
            }
        };
    }
    operation_argument_at(&quote! { #index }, argument, variable)
}

// https://webidl.spec.whatwg.org/#es-dictionary
fn union_default_value(argument: &OperationArgument) -> TokenStream {
    match argument.union_default {
        None | Some(UnionDefault::EmptyDictionary) => quote! {},
        Some(UnionDefault::Boolean(default)) => quote! {
            let value = if value.is_undefined() {
                Value::new_bool(ctx.clone(), #default)
            } else {
                value
            };
        },
    }
}

fn converted_argument(
    argument: &OperationArgument,
    variable: &proc_macro2::Ident,
    fetch: &TokenStream,
) -> TokenStream {
    match &argument.type_ {
        ReturnType::Node => quote! {
            #fetch
            let #variable = host::node_argument(&ctx, &value)?;
        },
        ReturnType::NullableNode => quote! {
            #fetch
            let #variable = host::nullable_node_argument(&ctx, &value)?;
        },
        ReturnType::Callback => quote! {
            #fetch
            let #variable = host::callback_argument(&ctx, &value)?;
        },
        // https://webidl.spec.whatwg.org/#idl-any
        ReturnType::PlatformObject | ReturnType::Any => quote! {
            #fetch
            let #variable = value;
        },
        ReturnType::Dictionary(name) => {
            let struct_name = format_ident!("{name}");
            quote! {
                #fetch
                let #variable = #struct_name::from_object(&ctx, &value)?;
            }
        }
        ReturnType::Enumeration(name) => {
            let name = format_ident!("{name}");
            let default = union_default_value(argument);
            quote! {
                #fetch
                #default
                let #variable = #name::from_value(&ctx, value)?;
            }
        }
        ReturnType::Union(name, _) => {
            let name = format_ident!("{name}");
            let default = union_default_value(argument);
            quote! {
                #fetch
                #default
                let #variable = #name::from_value(&ctx, &value)?;
            }
        }
        ReturnType::NullableUnion(name, _) => {
            let name = format_ident!("{name}");
            quote! {
                #fetch
                let #variable = if value.is_null() || value.is_undefined() {
                    None
                } else {
                    Some(#name::from_value(&ctx, &value)?)
                };
            }
        }
        _ => unreachable!("converted arguments are object-family types"),
    }
}

fn operation_argument_at(
    index: &TokenStream,
    argument: &OperationArgument,
    variable: &proc_macro2::Ident,
) -> TokenStream {
    let fetch = quote! {
        let value = params.arg(#index).unwrap_or_else(|| Value::new_undefined(ctx.clone()));
    };
    match &argument.type_ {
        ReturnType::Any | ReturnType::PlatformObject => {
            let fetch = if argument.null_default {
                // https://webidl.spec.whatwg.org/#es-any
                quote! {
                    let value = params
                        .arg(#index)
                        .filter(|value| !value.is_undefined())
                        .unwrap_or_else(|| Value::new_null(ctx.clone()));
                }
            } else {
                fetch
            };
            converted_argument(argument, variable, &fetch)
        }
        ReturnType::Node
        | ReturnType::NullableNode
        | ReturnType::Callback
        | ReturnType::Dictionary(_)
        | ReturnType::Enumeration(_)
        | ReturnType::Union(..)
        | ReturnType::NullableUnion(..) => converted_argument(argument, variable, &fetch),
        ReturnType::String
        | ReturnType::UsvString
        | ReturnType::NullableString
        | ReturnType::NullableDocumentType
        | ReturnType::Boolean
        | ReturnType::UnsignedShort
        | ReturnType::Short
        | ReturnType::UnsignedLong
        | ReturnType::Long
        | ReturnType::Double
        | ReturnType::RestrictedDouble => operation_scalar_argument(argument, index, variable),
        _ => unreachable!("validated operation argument"),
    }
}

fn operation_scalar_argument(
    argument: &OperationArgument,
    index: &TokenStream,
    variable: &proc_macro2::Ident,
) -> TokenStream {
    match &argument.type_ {
        ReturnType::String => {
            // https://webidl.spec.whatwg.org/#es-DOMString
            let conversion = match (argument.arity, argument.legacy_null_to_empty) {
                (ArgumentArity::Optional, false) => {
                    quote! { host::optional_string_argument(params, #index)? }
                }
                (ArgumentArity::Optional, true) => quote! {
                    {
                        // https://webidl.spec.whatwg.org/#LegacyNullToEmptyString
                        let value = host::fetch(params, #index);
                        if value.is_undefined() {
                            None
                        } else if value.is_null() {
                            Some(rquickjs::String::from_str(ctx.clone(), "")?)
                        } else {
                            Some(host::string_argument(params, #index, None)?)
                        }
                    }
                },
                (_, true) => quote! { host::legacy_null_string_argument(params, #index)? },
                (_, false) => quote! { host::string_argument(params, #index, None)? },
            };
            quote! { let #variable = #conversion; }
        }
        ReturnType::UsvString => {
            let conversion = if argument.arity == ArgumentArity::Optional {
                quote! {
                    {
                        let value = host::fetch(params, #index);
                        if value.is_undefined() {
                            None
                        } else {
                            Some(host::usv_string_argument(params, #index)?)
                        }
                    }
                }
            } else {
                quote! { host::usv_string_argument(params, #index)? }
            };
            quote! { let #variable = #conversion; }
        }
        ReturnType::NullableString => quote! {
            let #variable = host::nullable_string_argument(params, #index)?;
        },
        ReturnType::NullableDocumentType => {
            // https://webidl.spec.whatwg.org/#es-any
            let fetch = if argument.null_default {
                quote! {
                    let value = params.arg(#index).unwrap_or_else(|| Value::new_null(ctx.clone()));
                }
            } else {
                quote! {
                    let value = params.arg(#index).unwrap_or_else(|| Value::new_undefined(ctx.clone()));
                }
            };
            quote! {
                #fetch
                let #variable = host::document_type_argument(&ctx, &value)?;
            }
        }
        ReturnType::Boolean => {
            // https://webidl.spec.whatwg.org/#es-boolean
            let conversion = match (argument.arity, argument.boolean_default) {
                (_, Some(default)) => {
                    quote! { host::boolean_argument_with_default(params, #index, #default)? }
                }
                (ArgumentArity::Optional, None) => {
                    quote! { host::optional_boolean_argument(params, #index)? }
                }
                (_, None) => quote! { host::boolean_argument(params, #index)? },
            };
            quote! { let #variable = #conversion; }
        }
        ReturnType::UnsignedShort => {
            ushort_argument_tokens(argument.ushort_default, index, variable)
        }
        ReturnType::Short => quote! {
            // https://webidl.spec.whatwg.org/#es-short
            let value = host::fetch(params, #index);
            let converted: rquickjs::Coerced<i32> = rquickjs::Coerced::from_js(&ctx, value)?;
            let #variable = u16::try_from(converted.0.rem_euclid(65536))
                .map(u16::cast_signed)
                .unwrap_or(0);
        },
        ReturnType::UnsignedLong => quote! {
            // https://webidl.spec.whatwg.org/#es-unsigned-long
            let #variable = host::ulong_argument(params, #index)?;
        },
        ReturnType::Long => quote! {
            // https://webidl.spec.whatwg.org/#es-long
            let #variable = host::long_argument(params, #index)?;
        },
        ReturnType::Double => quote! {
            // https://webidl.spec.whatwg.org/#es-double
            let #variable = host::double_argument(params, #index)?;
        },
        ReturnType::RestrictedDouble => quote! {
            let #variable = host::restricted_double_argument(params, #index)?;
        },
        _ => unreachable!("validated operation argument"),
    }
}

fn ushort_argument_tokens(
    default: Option<u16>,
    index: &TokenStream,
    variable: &proc_macro2::Ident,
) -> TokenStream {
    // https://webidl.spec.whatwg.org/#es-unsigned-short
    if let Some(default) = default {
        quote! {
            let #variable = if host::fetch(params, #index).is_undefined() {
                #default
            } else {
                host::ushort_argument(params, #index)?
            };
        }
    } else {
        quote! {
            let #variable = host::ushort_argument(params, #index)?;
        }
    }
}

fn constructor(interface: &Interface) -> (usize, TokenStream) {
    let Some(constructor) = &interface.constructor else {
        return (
            0,
            quote! { return Err(rquickjs::Exception::throw_type(&ctx, "Illegal constructor")); },
        );
    };
    let rust = &interface.rust;
    let payload = if interface.has_lifetime {
        quote! { #rust<'js> }
    } else {
        quote! { #rust }
    };
    let create = &constructor.rust;
    let construct = if interface.contract.is_some() {
        let contract = format_ident!("{}", interface.name);
        quote! { <#payload as #contract<'js>>::#create }
    } else {
        quote! { #rust::#create }
    };
    let required = constructor
        .arguments
        .iter()
        .take_while(|argument| argument.kind.is_required())
        .count();
    let argument_names: Vec<_> = (0..constructor.arguments.len())
        .map(|index| format_ident!("arg_{index}"))
        .collect();
    let arguments = constructor
        .arguments
        .iter()
        .enumerate()
        .map(|(index, argument)| {
            let variable = &argument_names[index];
            match &argument.kind {
                ConstructorArgumentKind::String { default } => {
                    let default = if let Some(default) = default {
                        quote! { Some(#default) }
                    } else {
                        quote! { None }
                    };
                    quote! { let #variable = host::string_argument(params, #index, #default)?; }
                }
                ConstructorArgumentKind::Callback => quote! {
                    let value = params.arg(#index).unwrap_or_else(|| Value::new_undefined(ctx.clone()));
                    let #variable = host::callback_argument(&ctx, &value)?;
                },
                ConstructorArgumentKind::Dictionary(name) => {
                    // https://webidl.spec.whatwg.org/#es-dictionary
                    let struct_name = format_ident!("{name}");
                    quote! {
                        let value = params.arg(#index).unwrap_or_else(|| Value::new_undefined(ctx.clone()));
                        let #variable = #struct_name::from_object(&ctx, &value)?;
                    }
                }
            }
        });
    (
        required,
        quote! {
            host::require_constructor(params)?;
            host::require_arguments(params, #required)?;
            #(#arguments)*
            let prototype = host::constructor_prototype::<#payload>(params)?;
            let result = #construct(&ctx, #(#argument_names),*)?;
            return rquickjs::Class::instance_proto(result, prototype).map(rquickjs::Class::into_value);
        },
    )
}

fn dictionary(dictionary: &Dictionary) -> TokenStream {
    let name = format_ident!("{}", dictionary.name);
    let has_required = dictionary.fields.iter().any(|field| {
        matches!(
            &field.type_,
            DictionaryFieldType::Boolean { required: true, .. }
                | DictionaryFieldType::Enumeration { required: true, .. }
        )
    });
    let null_branch = if has_required {
        quote! {
            // https://webidl.spec.whatwg.org/#es-dictionary
            return Err(rquickjs::Exception::throw_type(
                ctx,
                "required dictionary member is missing",
            ));
        }
    } else {
        let defaults = dictionary.fields.iter().map(dictionary_default);
        quote! { return Ok(Self { #(#defaults),* }); }
    };
    let fields = dictionary.fields.iter().map(dictionary_field);
    let conversions = dictionary.fields.iter().map(dictionary_conversion);
    let bool_fields = dictionary
        .fields
        .iter()
        .filter(|field| dictionary_field_is_bool(field))
        .count();
    let excessive_bools = (bool_fields > 3).then(|| {
        quote! {
            #[expect(
                clippy::struct_excessive_bools,
                reason = "a WebIDL dictionary carries its members individually; a bitmask would obscure the IDL mapping"
            )]
        }
    });
    quote! {
        // https://webidl.spec.whatwg.org/#es-dictionary
        #[allow(dead_code, reason = "generated from IDL; algorithms may read a subset")]
        #excessive_bools
        pub(crate) struct #name {
            #(#fields),*
        }
        impl #name {
            pub(crate) fn from_object<'js>(ctx: &Ctx<'js>, value: &Value<'js>) -> Result<Self> {
                if value.is_null() || value.is_undefined() {
                    #null_branch
                }
                let object: rquickjs::Object = value.clone().into_object().ok_or_else(|| {
                    rquickjs::Exception::throw_type(ctx, "dictionary must be an object")
                })?;
                Ok(Self {
                    #(#conversions),*
                })
            }
        }
    }
}

fn dictionary_field_is_bool(field: &DictionaryField) -> bool {
    matches!(
        &field.type_,
        DictionaryFieldType::Boolean {
            default: Some(_),
            ..
        } | DictionaryFieldType::Boolean { required: true, .. }
    )
}

fn dictionary_field(field: &DictionaryField) -> TokenStream {
    let rust = &field.rust;
    if dictionary_field_is_bool(field) {
        return quote! { pub(crate) #rust: bool };
    }
    match &field.type_ {
        DictionaryFieldType::Boolean { .. } => {
            quote! { pub(crate) #rust: Option<bool> }
        }
        DictionaryFieldType::Enumeration {
            name: enum_name,
            nullable: false,
            default: Some(_),
            ..
        }
        | DictionaryFieldType::Enumeration {
            name: enum_name,
            nullable: false,
            required: true,
            ..
        } => {
            let enum_name = format_ident!("{enum_name}");
            quote! { pub(crate) #rust: #enum_name }
        }
        DictionaryFieldType::Enumeration {
            name: enum_name, ..
        } => {
            let enum_name = format_ident!("{enum_name}");
            quote! { pub(crate) #rust: Option<#enum_name> }
        }
        DictionaryFieldType::StringSequence => {
            quote! { pub(crate) #rust: Option<Vec<Vec<u16>>> }
        }
        DictionaryFieldType::DomString => {
            quote! { pub(crate) #rust: Option<String> }
        }
        DictionaryFieldType::Interface { .. } => {
            quote! { pub(crate) #rust: Option<rquickjs::Persistent<rquickjs::Object<'static>>> }
        }
    }
}

fn dictionary_default(field: &DictionaryField) -> TokenStream {
    let rust = &field.rust;
    match &field.type_ {
        DictionaryFieldType::Boolean {
            default: Some(default),
            ..
        } => quote! { #rust: #default },
        DictionaryFieldType::Boolean { .. } => quote! { #rust: None },
        DictionaryFieldType::Enumeration {
            name: enum_name,
            default: Some(default),
            nullable: false,
            ..
        } => {
            let enum_name = format_ident!("{enum_name}");
            quote! { #rust: #enum_name::#default }
        }
        DictionaryFieldType::Enumeration {
            name: enum_name,
            default: Some(default),
            nullable: true,
            ..
        } => {
            let enum_name = format_ident!("{enum_name}");
            quote! { #rust: Some(#enum_name::#default) }
        }
        DictionaryFieldType::Enumeration { .. }
        | DictionaryFieldType::StringSequence
        | DictionaryFieldType::DomString
        | DictionaryFieldType::Interface { .. } => {
            quote! { #rust: None }
        }
    }
}

fn dictionary_conversion(field: &DictionaryField) -> TokenStream {
    let rust = &field.rust;
    let key = &field.name;
    match &field.type_ {
        DictionaryFieldType::Boolean { required: true, .. } => quote! {
            #rust: host::dict_flag(ctx, &object, #key)?.ok_or_else(|| {
                rquickjs::Exception::throw_type(ctx, "required dictionary member is missing")
            })?
        },
        DictionaryFieldType::Boolean {
            default: Some(default),
            ..
        } => quote! {
            #rust: host::dict_flag(ctx, &object, #key)?.unwrap_or(#default)
        },
        DictionaryFieldType::Boolean { .. } => quote! {
            #rust: host::dict_flag(ctx, &object, #key)?
        },
        DictionaryFieldType::Enumeration { .. } => dictionary_enum_conversion(field),
        DictionaryFieldType::StringSequence => quote! {
            #rust: host::dict_string_sequence(ctx, &object, #key)?
        },
        DictionaryFieldType::DomString => quote! {
            #rust: {
                let value: Value = object.get(#key)?;
                if value.is_undefined() {
                    None
                } else {
                    let converted: rquickjs::Coerced<rquickjs::String> =
                        rquickjs::FromJs::from_js(ctx, value)?;
                    Some(converted.0.to_string()?)
                }
            }
        },
        DictionaryFieldType::Interface { nullable } => quote! {
            // https://webidl.spec.whatwg.org/#es-dictionary
            #rust: host::dict_object(ctx, &object, #key, #nullable)?
        },
    }
}

fn dictionary_enum_conversion(field: &DictionaryField) -> TokenStream {
    let rust = &field.rust;
    let key = &field.name;
    let DictionaryFieldType::Enumeration {
        name: enum_name,
        nullable,
        default,
        required,
    } = &field.type_
    else {
        unreachable!("enumeration conversion takes enumeration fields")
    };
    let enum_name = format_ident!("{enum_name}");
    if *required {
        return quote! {
            #rust: {
                let value: Value = object.get(#key)?;
                if value.is_undefined() {
                    return Err(rquickjs::Exception::throw_type(
                        ctx,
                        "required dictionary member is missing",
                    ));
                }
                #enum_name::from_value(ctx, value)?
            }
        };
    }
    match (nullable, default) {
        (false, Some(default)) => quote! {
            #rust: {
                let value: Value = object.get(#key)?;
                if value.is_undefined() {
                    #enum_name::#default
                } else {
                    #enum_name::from_value(ctx, value)?
                }
            }
        },
        (true, Some(default)) => quote! {
            #rust: {
                let value: Value = object.get(#key)?;
                if value.is_undefined() {
                    Some(#enum_name::#default)
                } else if value.is_null() {
                    None
                } else {
                    Some(#enum_name::from_value(ctx, value)?)
                }
            }
        },
        (_, None) => quote! {
            #rust: {
                let value: Value = object.get(#key)?;
                if value.is_null() || value.is_undefined() {
                    None
                } else {
                    Some(#enum_name::from_value(ctx, value)?)
                }
            }
        },
    }
}

fn enumeration(enumeration: &Enumeration) -> TokenStream {
    let name = format_ident!("{}", enumeration.name);
    let variants = enumeration.values.iter().map(|(_, variant)| variant);
    let conversions = enumeration
        .values
        .iter()
        .map(|(text, variant)| quote! { #text => Ok(Self::#variant) });
    let strings = enumeration
        .values
        .iter()
        .map(|(text, variant)| quote! { Self::#variant => #text });
    quote! {
        #[derive(Clone, Copy)]
        pub(crate) enum #name { #(#variants),* }

        impl #name {
            // https://webidl.spec.whatwg.org/#es-enumeration
            #[allow(dead_code, reason = "generated from IDL; conversion is used on demand")]
            fn from_value<'js>(ctx: &Ctx<'js>, value: Value<'js>) -> Result<Self> {
                let string: rquickjs::Coerced<rquickjs::String> = rquickjs::FromJs::from_js(ctx, value)?;
                let string = match string.0.to_string() {
                    Ok(string) => string,
                    Err(rquickjs::Error::Utf8(_)) => return Err(rquickjs::Exception::throw_type(ctx, "invalid enumeration value")),
                    Err(error) => return Err(error),
                };
                match string.as_str() {
                    #(#conversions,)*
                    _ => Err(rquickjs::Exception::throw_type(ctx, "invalid enumeration value")),
                }
            }

            #[allow(dead_code, reason = "generated from IDL; serialization is used on demand")]
            pub(crate) fn as_str(self) -> &'static str {
                match self { #(#strings),* }
            }
        }
    }
}

fn unscopables(interface: &Interface) -> TokenStream {
    let names: Vec<&str> = interface
        .operations
        .iter()
        .filter(|operation| operation.unscopable)
        .map(|operation| operation.name.as_str())
        .chain(
            interface
                .attributes
                .iter()
                .filter(|attribute| attribute.unscopable)
                .map(|attribute| attribute.name.as_str()),
        )
        .collect();
    quote! {
        const UNSCOPABLES: &[&str] = &[#(#names),*];
    }
}

fn module_name(name: &str) -> proc_macro2::Ident {
    let snake = crate::names::snake_case(name);
    format_ident!("{snake}_generated")
}

// https://webidl.spec.whatwg.org/#es-union
fn union(union: &Union) -> TokenStream {
    let name = format_ident!("{}", union.name);
    let variants = union.members.iter().map(|member| {
        let variant = &member.variant;
        let type_ = match &member.type_ {
            UnionMemberType::Interface { node: true, .. } => quote! { host::NodeReference },
            UnionMemberType::Interface { node: false, .. } => quote! { Value<'js> },
            UnionMemberType::String => quote! { rquickjs::String<'js> },
            UnionMemberType::Boolean => quote! { bool },
            UnionMemberType::Long => quote! { i32 },
            UnionMemberType::Dictionary(name) => {
                let name = format_ident!("{name}");
                quote! { #name }
            }
        };
        quote! { #variant(#type_) }
    });
    let mut trials = Vec::new();
    let mut ordered: Vec<_> = union.members.iter().collect();
    ordered.sort_by_key(|member| match &member.type_ {
        UnionMemberType::Interface { .. } => 0,
        UnionMemberType::Dictionary(_) => 1,
        UnionMemberType::Boolean => 2,
        UnionMemberType::Long => 3,
        UnionMemberType::String => 4,
    });
    for member in ordered {
        let variant = &member.variant;
        match &member.type_ {
            UnionMemberType::Interface {
                name: interface,
                node: true,
            } => trials.push(quote! {
                if host::is_interface(ctx, value, #interface) {
                    return host::node_argument(ctx, value).map(Self::#variant);
                }
            }),
            UnionMemberType::Interface {
                node: false,
                name: interface,
            } => trials.push(quote! {
                if host::is_interface(ctx, value, #interface) {
                    return Ok(Self::#variant(value.clone()));
                }
            }),
            UnionMemberType::Dictionary(name) => {
                let name = format_ident!("{name}");
                trials.push(quote! {
                    // https://webidl.spec.whatwg.org/#es-union
                    if value.is_null() || value.is_undefined() || value.is_object() {
                        return #name::from_object(ctx, value).map(Self::#variant);
                    }
                });
            }
            UnionMemberType::Boolean => trials.push(quote! {
                if let Some(boolean) = value.as_bool() {
                    return Ok(Self::#variant(boolean));
                }
            }),
            UnionMemberType::Long => trials.push(quote! {
                // https://webidl.spec.whatwg.org/#es-long
                if value.is_number() {
                    let converted: rquickjs::Coerced<i32> =
                        rquickjs::FromJs::from_js(ctx, value.clone())?;
                    return Ok(Self::#variant(converted.0));
                }
            }),
            UnionMemberType::String => trials.push(quote! {
                // https://webidl.spec.whatwg.org/#es-DOMString
                rquickjs::FromJs::from_js(ctx, value.clone())
                    .map(|string: rquickjs::Coerced<rquickjs::String>| Self::#variant(string.0))
            }),
        }
    }
    let mismatch = union_fallback(union);
    let generic = if union
        .members
        .iter()
        .any(|member| member.type_.needs_lifetime())
    {
        quote! { <'js> }
    } else {
        quote! {}
    };
    let into_js = union_into_js(union, &name, &generic);
    quote! {
        #[allow(dead_code, reason = "generated from IDL; algorithms may read a subset")]
        pub(crate) enum #name #generic {
            #(#variants),*
        }

        impl<'js> #name #generic {
            #[allow(dead_code, reason = "generated from IDL; return-only unions do not convert from JS")]
            fn from_value(ctx: &Ctx<'js>, value: &Value<'js>) -> Result<Self> {
                #(#trials)*
                #mismatch
            }
        }

        #into_js
    }
}

fn union_into_js(union: &Union, name: &proc_macro2::Ident, generic: &TokenStream) -> TokenStream {
    let convertible = union.members.iter().all(|member| {
        matches!(
            member.type_,
            UnionMemberType::Interface { node: false, .. }
                | UnionMemberType::String
                | UnionMemberType::Boolean
                | UnionMemberType::Long
        )
    });
    if !convertible {
        return quote! {};
    }
    let uses_ctx = union.members.iter().any(|member| {
        matches!(
            member.type_,
            UnionMemberType::Boolean | UnionMemberType::Long
        )
    });
    let ctx_name = if uses_ctx {
        quote! { ctx }
    } else {
        quote! { _ctx }
    };
    let mut grouped: Vec<(String, proc_macro2::TokenStream, Vec<&syn::Ident>)> = Vec::new();
    for member in &union.members {
        let variant = &member.variant;
        let body = match member.type_ {
            UnionMemberType::Interface { .. } => quote! { Ok(value) },
            UnionMemberType::String => quote! { Ok(value.into_value()) },
            UnionMemberType::Boolean | UnionMemberType::Long => {
                quote! { rquickjs::IntoJs::into_js(value, &#ctx_name) }
            }
            UnionMemberType::Dictionary(_) => continue,
        };
        let key = match member.type_ {
            UnionMemberType::Boolean => format!("bool:{body}"),
            UnionMemberType::Long => format!("i32:{body}"),
            _ => body.to_string(),
        };
        match grouped.iter_mut().find(|(known, _, _)| *known == key) {
            Some((_, _, variants)) => variants.push(variant),
            None => grouped.push((key, body, vec![variant])),
        }
    }
    let arms = grouped.iter().map(|(_, body, variants)| {
        quote! { #(Self::#variants(value))|* => #body, }
    });
    quote! {
        impl<'js> rquickjs::IntoJs<'js> for #name #generic {
            fn into_js(self, #ctx_name: &Ctx<'js>) -> Result<Value<'js>> {
                match self {
                    #(#arms)*
                }
            }
        }
    }
}

// https://webidl.spec.whatwg.org/#es-union
fn union_fallback(union: &Union) -> TokenStream {
    if union
        .members
        .iter()
        .any(|member| matches!(member.type_, UnionMemberType::String))
    {
        return quote! {};
    }
    if let Some(member) = union
        .members
        .iter()
        .find(|member| matches!(member.type_, UnionMemberType::Boolean))
    {
        let variant = &member.variant;
        return quote! {
            rquickjs::FromJs::from_js(ctx, value.clone())
                .map(|converted: rquickjs::Coerced<bool>| Self::#variant(converted.0))
        };
    }
    quote! { Err(rquickjs::Exception::throw_type(ctx, "value does not match the union")) }
}
