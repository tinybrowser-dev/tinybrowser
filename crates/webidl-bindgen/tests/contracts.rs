use quote::ToTokens;
use webidl_bindgen::{
    ReflectedEnum, Source, compile_contracts, compile_contracts_with_reflection,
    compile_javascript, parse_reflected_enumerated, validate_sources,
};

/// Generated token streams are whitespace-insensitive; assertions can be too.
fn squash(source: &str) -> String {
    source.chars().filter(|c| !c.is_whitespace()).collect()
}

fn generated_trait(rust: &str) -> String {
    let syntax = syn::parse_file(rust).expect("generated syntax");
    let [syn::Item::Mod(module)] = syntax.items.as_slice() else {
        panic!("expected a generated module");
    };
    let (_, items) = module.content.as_ref().expect("module body");
    let traits: Vec<_> = items
        .iter()
        .filter_map(|item| match item {
            syn::Item::Trait(item) => Some(item.to_token_stream().to_string()),
            _ => None,
        })
        .collect();
    traits.join("\n")
}

#[test]
fn an_included_generated_module_needs_no_hand_written_implementation() {
    let idl = [Source {
        name: "fixture.idl",
        text: r"
        [Exposed=*] interface Parent { readonly attribute DOMString label; };
        [Exposed=*] interface Child : Parent { };
    ",
    }];
    let rust = [Source {
        name: "fixture.rs",
        text: r#"
        impl<'js> parent_generated::Parent<'js> for Payload<'js> {
            fn get_label(&self, ctx: &Ctx<'js>) -> Result<String<'js>> { read(ctx) }
        }
        include!(concat!(env!("OUT_DIR"), "/Child.rs"));
    "#,
    }];
    let bindings = compile_contracts(&idl, &rust).expect("compile fixture");
    let child = bindings
        .iter()
        .find(|binding| binding.interface == "Child")
        .expect("the included module is generated");
    // The payload is the nearest implemented ancestor's, because the engine
    // object that implements `Parent` is the same object `Child` names.
    assert!(child.rust.contains("use super::Payload"));
    assert!(
        !child.rust.contains("trait Child"),
        "an interface with no members has no contract trait"
    );
}

#[test]
fn contracts_follow_implementations_and_resolved_declarations() {
    let idl = [Source {
        name: "fixture.idl",
        text: r#"
        typedef unsigned short Count;
        [Exposed=*] interface Sample {
            constructor(optional DOMString label = "fixture");
            readonly attribute DOMString label;
            readonly attribute DOMString unsupported;
        };
        partial interface Sample { readonly attribute Count count; };
        interface mixin Common { readonly attribute boolean enabled; };
        Sample includes Common;
    "#,
    }];
    let rust = [Source {
        name: "fixture.rs",
        text: r"
        impl<'js> sample_generated::Sample<'js> for Payload<'js> {
            fn constructor(ctx: &Ctx<'js>, label: String<'js>) -> Result<Self> { build(ctx, label) }
            fn get_label(&self, ctx: &Ctx<'js>) -> Result<String<'js>> { read(ctx) }
            fn get_count(&self, ctx: &Ctx<'js>) -> Result<u16> { count(ctx) }
            fn get_enabled(&self, ctx: &Ctx<'js>) -> Result<bool> { enabled(ctx) }
        }
    ",
    }];
    let bindings = compile_contracts(&idl, &rust).expect("compile fixture");
    assert_eq!(bindings.len(), 1);
    assert_eq!(bindings[0].interface, "Sample");
    assert_eq!(generated_trait(&bindings[0].rust).replace(' ', ""), quote::quote! {
        pub(crate) trait Sample<'js> {
            fn constructor(ctx: &Ctx<'js>, arg_0: rquickjs::String<'js>) -> Result<Self> where Self: Sized;
            fn get_label(&self, ctx: &Ctx<'js>) -> Result<rquickjs::String<'js>>;
            fn get_count(&self, ctx: &Ctx<'js>) -> Result<u16>;
            fn get_enabled(&self, ctx: &Ctx<'js>) -> Result<bool>;
        }
    }.to_string().replace(' ', ""));
}

#[test]
fn conflicting_providers_and_unknown_contract_methods_are_diagnostics() {
    let idl = [Source {
        name: "fixture.idl",
        text: "[Exposed=Window] interface Sample { readonly attribute DOMString label; };",
    }];
    let unknown = [Source {
        name: "fixture.rs",
        text: "impl<'js> sample_generated::Sample<'js> for Payload { fn typo(&self, ctx: Ctx<'js>) {} }",
    }];
    assert_eq!(
        compile_contracts(&idl, &unknown)
            .err()
            .expect("unknown method")
            .to_string(),
        "Sample: methods do not match a supported IDL contract: typo"
    );
    // Two payloads for one interface are the multi-payload case: one
    // contract, one installation, a dispatch arm per platform type. The
    // declared class named for the interface owns the prototype however
    // discovery ordered the implementations.
    let providers = [Source {
        name: "fixture.rs",
        text: "#[rquickjs::class(rename = \"Sample\")] struct B; impl<'js> sample_generated::Sample<'js> for A {} impl<'js> sample_generated::Sample<'js> for B {}",
    }];
    let bindings = compile_contracts(&idl, &providers).expect("multi-payload provider");
    let dispatch = bindings[0].rust.split_whitespace().collect::<String>();
    assert!(
        dispatch.contains("host::receiver::<B>"),
        "elected payload is not primary: {}",
        bindings[0].rust
    );
    assert!(
        dispatch.contains("crate::js::bindings::A"),
        "missing alternate payload arm: {}",
        bindings[0].rust
    );
    // Two class-less payloads with no name match have no prototype owner.
    let anonymous = [Source {
        name: "fixture.rs",
        text: "impl<'js> sample_generated::Sample<'js> for A {} impl<'js> sample_generated::Sample<'js> for B {}",
    }];
    assert_eq!(
        compile_contracts(&idl, &anonymous)
            .err()
            .expect("ambiguous owner")
            .to_string(),
        "(discovered): Sample has no prototype owner: several class-less payloads and none is named for the interface"
    );
    // The same platform type twice is still a conflict.
    let repeated = [Source {
        name: "fixture.rs",
        text: "impl<'js> sample_generated::Sample<'js> for A {} impl<'js> sample_generated::Sample<'js> for A {}",
    }];
    assert_eq!(
        compile_contracts(&idl, &repeated)
            .err()
            .expect("repeated payload")
            .to_string(),
        "fixture.rs: Sample is implemented twice for A"
    );
}

#[test]
fn getters_lower_to_property_hooks_and_operations() {
    let idl = [Source {
        name: "node_list.idl",
        text: r"
            [Exposed=Window] interface NodeList {
                getter Node? item(unsigned long index);
                readonly attribute unsigned long length;
                iterable<Node>;
            };
        ",
    }];
    let rust = [Source {
        name: "node_list.rs",
        text: r"
            impl<'js> node_list_generated::NodeList<'js> for Payload {
                fn get_length(&self, ctx: &Ctx<'js>) -> Result<usize> { length(ctx) }
                fn item(&self, ctx: Ctx<'js>, arg_0: u32) -> Result<Value<'js>> { item(ctx, arg_0) }
            }
        ",
    }];
    let bindings = compile_contracts(&idl, &rust).expect("compile fixture");
    assert_eq!(bindings[0].interface, "NodeList");
    for fragment in ["exotic_get_own_property", "fn get_length", "fn item"] {
        assert!(
            bindings[0].rust.contains(fragment),
            "missing {fragment}: {}",
            bindings[0].rust
        );
    }
    let named_only = [Source {
        name: "named.idl",
        text: "[Exposed=Window] interface Named { getter Node? item(DOMString name); };",
    }];
    let named_rust = [Source {
        name: "named.rs",
        text: "impl<'js> named_generated::Named<'js> for Payload { fn item(&self, ctx: Ctx<'js>, arg_0: rquickjs::String<'js>) -> Result<Value<'js>> { item(ctx) } }",
    }];
    assert!(compile_contracts(&named_only, &named_rust).is_err());

    let indexed_named = [Source {
        name: "html_collection.idl",
        text: "interface Element {}; [Exposed=Window, LegacyUnenumerableNamedProperties] interface HTMLCollection { readonly attribute unsigned long length; getter Element? item(unsigned long index); getter Element? namedItem(DOMString name); };",
    }];
    let indexed_named_rust = [Source {
        name: "html_collection.rs",
        text: "impl<'js> html_collection_generated::HTMLCollection<'js> for Payload { fn get_length(&self, ctx: &Ctx<'js>) -> Result<usize> { l(ctx) } fn item(&self, ctx: Ctx<'js>, arg_0: u32) -> Result<Value<'js>> { i(ctx) } fn named_item(&self, ctx: Ctx<'js>, arg_0: rquickjs::String<'js>) -> Result<Value<'js>> { n(ctx) } fn supported_names(&self, ctx: &Ctx<'js>) -> Result<Vec<String>> { s(ctx) } }",
    }];
    let bindings = compile_contracts(&indexed_named, &indexed_named_rust).expect("compile fixture");
    assert!(bindings[0].rust.contains("supported_names"));
}

#[test]
fn nullable_strings_and_platform_attributes_lower() {
    let idl = [Source {
        name: "mutation_record.idl",
        text: r"
            [Exposed=Window] interface MutationRecord {
                readonly attribute DOMString type;
                [SameObject] readonly attribute Node target;
                readonly attribute Node? previousSibling;
                readonly attribute DOMString? attributeName;
            };
        ",
    }];
    let rust = [Source {
        name: "mutation_record.rs",
        text: r"
            impl<'js> mutation_record_generated::MutationRecord<'js> for JsMutationRecord<'js> {
                fn get_type(&self, ctx: &Ctx<'js>) -> Result<rquickjs::String<'js>> { t(ctx) }
                fn get_target(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> { g(ctx) }
                fn get_previous_sibling(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> { p(ctx) }
                fn get_attribute_name(&self, ctx: &Ctx<'js>) -> Result<Option<rquickjs::String<'js>>> { a(ctx) }
            }
        ",
    }];
    let bindings = compile_contracts(&idl, &rust).expect("compile fixture");
    let trait_text = generated_trait(&bindings[0].rust)
        .replace(' ', "")
        .replace(",)", ")");
    for fragment in [
        "get_target(&self,ctx:&Ctx<'js>)->Result<Value<'js>>",
        "get_attribute_name(&self,ctx:&Ctx<'js>)->Result<Option<rquickjs::String<'js>>>",
    ] {
        assert!(
            trait_text.contains(fragment),
            "missing {fragment}: {trait_text}"
        );
    }
}

#[test]
fn callbacks_dictionaries_and_sequences_lower_with_receiver() {
    let idl = [Source {
        name: "mutation_observer.idl",
        text: r"
            interface MutationRecord {};
            callback MutationCallback = undefined (sequence<MutationRecord> records, MutationObserver observer);
            dictionary BaseOptions {
                boolean z = false;
                boolean a;
            };
            dictionary MutationObserverInit : BaseOptions {
                sequence<DOMString> attributeFilter;
                boolean attributes;
            };
            partial dictionary MutationObserverInit {
                boolean childList = false;
                boolean subtree = false;
                boolean useCapture = false;
            };
            [Exposed=Window] interface MutationObserver {
                constructor(MutationCallback callback);
                undefined observe(Node target, optional MutationObserverInit options = {});
                sequence<MutationRecord> takeRecords();
            };
        ",
    }];
    let rust = [Source {
        name: "mutation_observer.rs",
        text: r"
            impl<'js> mutation_observer_generated::MutationObserver<'js> for JsMutationObserver {
                fn constructor(ctx: &Ctx<'js>, arg_0: rquickjs::Function<'js>) -> Result<Self> { c(ctx) }
                fn observe(&self, ctx: Ctx<'js>, this: Object<'js>, arg_0: host::NodeReference, arg_1: mutation_observer_generated::MutationObserverInit) -> Result<()> { o(ctx) }
                fn take_records(&self, ctx: Ctx<'js>) -> Result<Vec<Value<'js>>> { t(ctx) }
            }
        ",
    }];
    let bindings = compile_contracts(&idl, &rust).expect("compile fixture");
    let syntax = syn::parse_file(&bindings[0].rust).expect("generated syntax");
    let [syn::Item::Mod(module)] = syntax.items.as_slice() else {
        panic!("expected a generated module");
    };
    let (_, items) = module.content.as_ref().expect("module body");
    let dictionary = items
        .iter()
        .find_map(|item| match item {
            syn::Item::Struct(item) if item.ident == "MutationObserverInit" => Some(item),
            _ => None,
        })
        .expect("generated dictionary");
    assert_eq!(
        dictionary
            .fields
            .iter()
            .map(|field| field.ident.as_ref().expect("named field").to_string())
            .collect::<Vec<_>>(),
        [
            "a",
            "z",
            "attribute_filter",
            "attributes",
            "child_list",
            "subtree",
            "use_capture"
        ]
    );
    // Four plain `bool` fields trip `clippy::struct_excessive_bools`; the
    // generated dictionary states once that the members come from IDL.
    let expectation = dictionary
        .attrs
        .iter()
        .find(|attribute| {
            attribute
                .to_token_stream()
                .to_string()
                .contains("struct_excessive_bools")
        })
        .expect("a four-bool dictionary expects the clippy lint");
    assert!(
        expectation
            .meta
            .to_token_stream()
            .to_string()
            .contains("reason"),
        "the expectation must carry a reason"
    );
    let source = bindings[0].rust.replace(' ', "");
    for fragment in [
        "structMutationObserverInit",
        "host::callback_argument",
        "this_object",
        "host::sequence",
    ] {
        assert!(
            source.contains(fragment),
            "missing {fragment}: {}",
            bindings[0].rust
        );
    }
}

#[test]
fn malformed_graphs_return_named_diagnostics() {
    for (text, diagnostic) in [
        (
            "partial interface Missing {};",
            "partial Missing has no complete definition",
        ),
        (
            "interface A : B {}; interface B : A {};",
            "inheritance cycle at A",
        ),
        (
            "interface A {}; A includes Missing;",
            "included Missing is not an interface mixin",
        ),
        (
            "interface A {}; dictionary A {};",
            "fixture.idl: duplicate definition A",
        ),
        (
            "interface A {}; partial dictionary A {};",
            "partial A has a different declaration kind",
        ),
    ] {
        assert_eq!(
            validate_sources(&[Source {
                name: "fixture.idl",
                text
            }])
            .expect_err("invalid graph")
            .to_string(),
            diagnostic
        );
    }
}

#[test]
fn implemented_unsupported_types_do_not_disappear() {
    let idl = [Source {
        name: "fixture.idl",
        text: "[Exposed=Window] interface Sample { readonly attribute object value; };",
    }];
    let rust = [Source {
        name: "fixture.rs",
        text: "impl<'js> sample_generated::Sample<'js> for Payload { fn get_value() {} }",
    }];
    assert!(compile_contracts(&idl, &rust).is_err());
}

#[test]
fn enum_attributes_lower_to_the_generated_enum() {
    let idl = [Source {
        name: "dom.idl",
        text: "enum Mode { \"open\", \"closed\" }; [Exposed=Window] interface Shadow { readonly attribute Mode mode; };",
    }];
    let rust = [Source {
        name: "shadow.rs",
        text: "impl<'js> shadow_generated::Shadow<'js> for Payload { fn get_mode(&self, ctx: &Ctx<'js>) -> Result<shadow_generated::Mode> { Ok(shadow_generated::Mode::Open) } }",
    }];
    let bindings = compile_contracts(&idl, &rust).expect("compile enum attribute");
    let trait_text = generated_trait(&bindings[0].rust).replace(' ', "");
    assert!(
        trait_text.contains("fnget_mode(&self,ctx:&Ctx<'js>)->Result<Mode>"),
        "{trait_text}"
    );
    let source = bindings[0].rust.replace(' ', "");
    assert!(source.contains("enumMode"), "missing generated enum");
    assert!(source.contains("as_str"), "missing enum string conversion");
}

#[test]
fn node_or_string_unions_convert_interfaces_before_strings() {
    let idl = [Source {
        name: "dom.idl",
        text: "[Exposed=Window] interface Node {}; [Exposed=Window] interface Sample { undefined append((Node or DOMString)... nodes); };",
    }];
    let rust = [Source {
        name: "sample.rs",
        text: "impl<'js> sample_generated::Sample<'js> for Payload { fn append(&self, ctx: Ctx<'js>, nodes: Vec<sample_generated::DOMStringOrNode>) -> Result<()> { Ok(()) } }",
    }];
    let bindings = compile_contracts(&idl, &rust).expect("compile union");
    let source = bindings[0].rust.replace(' ', "");
    for fragment in [
        "enumDOMStringOrNode",
        "Node(host::NodeReference)",
        "DOMString(rquickjs::String<'js>)",
        "node_argument",
        "Coerced<rquickjs::String>",
    ] {
        assert!(
            source.contains(&fragment.replace(' ', "")),
            "missing {fragment}"
        );
    }
    let node_trial = source.find("node_argument").expect("node trial");
    let string_trial = source
        .find("Coerced<rquickjs::String>")
        .expect("string trial");
    assert!(
        node_trial < string_trial,
        "interface trial must precede string coercion"
    );
}

#[test]
fn unsupported_unions_fail_the_build() {
    let rust = [Source {
        name: "sample.rs",
        text: "impl<'js> sample_generated::Sample<'js> for Payload { fn append(&self, ctx: Ctx<'js>) -> Result<()> { Ok(()) } }",
    }];
    for text in [
        "[Exposed=Window] interface Sample { undefined append((Node or long)... nodes); };",
        "[Exposed=Window] interface Sample { undefined append((Node or DOMString)?... nodes); };",
        "[Exposed=Window] interface Sample { undefined append(([LegacyNullToEmptyString] DOMString or Node)... nodes); };",
    ] {
        let idl = [Source {
            name: "fixture.idl",
            text,
        }];
        assert!(compile_contracts(&idl, &rust).is_err(), "accepted {text}");
    }
}

#[test]
fn unscopable_operations_list_in_unscopables() {
    let idl = [Source {
        name: "dom.idl",
        text: "[Exposed=Window] interface Sample { [Unscopable] undefined append(Node node); undefined keep(Node node); };",
    }];
    let rust = [Source {
        name: "sample.rs",
        text: "impl<'js> sample_generated::Sample<'js> for Payload { fn append(&self, ctx: Ctx<'js>, node: host::NodeReference) -> Result<()> { Ok(()) } fn keep(&self, ctx: Ctx<'js>, node: host::NodeReference) -> Result<()> { Ok(()) } }",
    }];
    let bindings = compile_contracts(&idl, &rust).expect("compile unscopable");
    let source = bindings[0].rust.replace(' ', "");
    assert!(
        source.contains("UNSCOPABLES:&[&str]=&[\"append\"]"),
        "missing unscopables table"
    );
    assert!(
        source.contains("install_unscopables"),
        "missing install call"
    );
}

#[test]
fn mixin_implementations_install_on_includers() {
    let idl = [Source {
        name: "dom.idl",
        text: "[Exposed=Window] interface Node {}; [Exposed=Window] interface Element {}; [Exposed=Window] interface Document {}; [Exposed=Window] interface Ghost {}; interface mixin Nodes { undefined append((Node or DOMString)... nodes); }; Element includes Nodes; Document includes Nodes; Ghost includes Nodes;",
    }];
    // Install targets are the implemented includers: an included interface
    // without an implementation has no global to install onto.
    let rust = [Source {
        name: "nodes.rs",
        text: "#[rquickjs::class] struct Payload; impl<'js> nodes_generated::Nodes<'js> for Payload { fn append(&self, ctx: Ctx<'js>, nodes: Vec<nodes_generated::DOMStringOrNode>) -> Result<()> { Ok(()) } } impl<'js> element_generated::Element<'js> for Payload {} impl<'js> document_generated::Document<'js> for Payload {}",
    }];
    let bindings = compile_contracts(&idl, &rust).expect("compile mixin");
    let binding = bindings
        .iter()
        .find(|binding| binding.interface == "Nodes")
        .expect("mixin binding");
    // Generated installers route through `host::install_interface`; whitespace
    // spanning the call's arguments is normalized for the match.
    let source: String = binding
        .rust
        .chars()
        .filter(|c| !matches!(c, ' ' | '\n' | '\t'))
        .collect();
    for target in ["Element", "Document"] {
        assert!(
            source.contains(&format!("host::install_interface(ctx,\"{target}\"")),
            "missing install target {target}"
        );
    }
    assert!(
        !source.contains("host::install_interface(ctx,\"Nodes\""),
        "mixin must not install on itself"
    );
    assert!(
        !source.contains("host::install_interface(ctx,\"Ghost\""),
        "mixin must not install on unimplemented includers"
    );
}

#[test]
fn reflect_attributes_install_without_implementation() {
    let idl = [Source {
        name: "html.idl",
        text: "[Exposed=Window] interface Group { [CEReactions, Reflect] attribute boolean disabled; [CEReactions, Reflect] attribute DOMString label; };",
    }];
    let rust = [Source {
        name: "group.rs",
        text: "#[rquickjs::class] struct Payload; impl<'js> group_generated::Group<'js> for Payload {}",
    }];
    let bindings = compile_contracts(&idl, &rust).expect("compile reflect");
    let source = bindings[0].rust.replace(' ', "");
    for fragment in [
        "reflect_bool",
        "reflect_string",
        "reflect_set_bool",
        "\"disabled\"",
        "\"label\"",
    ] {
        assert!(
            source.contains(&fragment.replace(' ', "")),
            "missing {fragment}"
        );
    }
}

#[test]
fn reflect_conflicts_and_unsupported_shapes() {
    // An implementation method for a reflected name fails the build.
    let idl = [Source {
        name: "html.idl",
        text: "[Exposed=Window] interface Group { [Reflect] attribute boolean disabled; };",
    }];
    let rust = [Source {
        name: "group.rs",
        text: "#[rquickjs::class] struct Payload; impl<'js> group_generated::Group<'js> for Payload { fn get_disabled(&self, ctx: &Ctx<'js>) -> Result<bool> { Ok(true) } }",
    }];
    assert!(compile_contracts(&idl, &rust).is_err());
    // Unsupported reflect shapes without implementation methods are absent,
    // like any unimplemented member.
    for text in [
        "[Exposed=Window] interface Group { [Reflect] attribute unsigned long span; };",
        "[Exposed=Window] interface Group { [CEReactions, ReflectURL] attribute DOMString src; };",
        "[Exposed=Window] interface Group { [Reflect] readonly attribute Element anchor; };",
    ] {
        let idl = [Source {
            name: "html.idl",
            text,
        }];
        let empty = [Source {
            name: "group.rs",
            text: "#[rquickjs::class] struct Payload; impl<'js> group_generated::Group<'js> for Payload {}",
        }];
        let bindings = compile_contracts(&idl, &empty).expect("unsupported reflect stays absent");
        assert!(
            !bindings[0].rust.contains("reflect_"),
            "unexpected reflection for {text}"
        );
    }
}

#[test]
fn reflect_numeric_falls_through_to_hand_implementation() {
    // Numeric `[Reflect]` is not auto-generated: a hand-written getter and
    // setter pair lowers as ordinary methods.
    let idl = [Source {
        name: "html.idl",
        text: "[Exposed=Window] interface Group { [CEReactions, Reflect] attribute unsigned long size; };",
    }];
    let rust = [Source {
        name: "group.rs",
        text: "#[rquickjs::class] struct Payload; impl<'js> group_generated::Group<'js> for Payload { fn get_size(&self, ctx: &Ctx<'js>) -> Result<usize> { Ok(0) } fn set_size(&self, ctx: &Ctx<'js>, value: u32) -> Result<()> { Ok(()) } }",
    }];
    let bindings = compile_contracts(&idl, &rust).expect("numeric reflect hand impl");
    let source = bindings[0].rust.replace(' ', "");
    for fragment in ["fnget_size", "fnset_size", "usize", "u32"] {
        assert!(
            source.contains(&fragment.replace(' ', "")),
            "missing {fragment}"
        );
    }
    assert!(
        !source.contains("reflect_set_ulong"),
        "numeric reflect must not use the generated setter"
    );
    // The trait carries both methods with the path-specific numeric types:
    // plain-method `unsigned long` getters answer `usize`.
    let trait_source = bindings[0].rust.clone();
    assert!(
        trait_source.contains("fn get_size(&self, ctx: &Ctx<'js>) -> Result<usize>;"),
        "missing exact getter signature"
    );
}

#[test]
fn reflect_legacy_null_rejects_non_strings() {
    // `[LegacyNullToEmptyString]` requires `DOMString`: booleans, numerics,
    // and `USVString` fail the build instead of silently dropping the flag.
    for text in [
        "[Exposed=Window] interface Group { [Reflect] attribute [LegacyNullToEmptyString] boolean flag; };",
        "[Exposed=Window] interface Group { [Reflect] attribute [LegacyNullToEmptyString] USVString ping; };",
        "[Exposed=Window] interface Group { [Reflect, LegacyNullToEmptyString] attribute unsigned long size; };",
        "[Exposed=Window] interface Group { [ReflectSetter, LegacyNullToEmptyString] attribute long index; };",
    ] {
        let idl = [Source {
            name: "html.idl",
            text,
        }];
        let rust = [Source {
            name: "group.rs",
            text: "#[rquickjs::class] struct Payload; impl<'js> group_generated::Group<'js> for Payload { fn get_flag(&self, ctx: &Ctx<'js>) -> Result<bool> { Ok(true) } fn get_ping(&self, ctx: &Ctx<'js>) -> Result<crate::dom_string::DomString> { Ok(\"\".into()) } fn get_size(&self, ctx: &Ctx<'js>) -> Result<usize> { Ok(0) } fn set_size(&self, ctx: &Ctx<'js>, value: u32) -> Result<()> { Ok(()) } fn get_index(&self, ctx: &Ctx<'js>) -> Result<i32> { Ok(0) } }",
        }];
        assert!(
            compile_contracts(&idl, &rust).is_err(),
            "legacy null on a non-DOMString must fail: {text}"
        );
    }
}

#[test]
fn reflect_url_special_shapes_stay_absent() {
    // `[ReflectURL]` with `SameObject` and an implementation fails: the
    // member stays absent (a plain method would lose the `SameObject`
    // semantics), and the orphaned implementation trips the build's
    // method-contract check instead of installing silently.
    let idl = [Source {
        name: "html.idl",
        text: "[Exposed=Window] interface Group { [SameObject, ReflectURL] attribute USVString base; };",
    }];
    let rust = [Source {
        name: "group.rs",
        text: "#[rquickjs::class] struct Payload; impl<'js> group_generated::Group<'js> for Payload { fn get_base(&self, ctx: &Ctx<'js>) -> Result<crate::dom_string::DomString> { Ok(\"\".into()) } }",
    }];
    assert!(compile_contracts(&idl, &rust).is_err());
}

#[test]
fn reflect_url_dom_string_falls_through_to_hand_implementation() {
    // Non-`USVString` `[ReflectURL]` (like `object.codeBase`) lowers as an
    // ordinary hand-written method pair.
    let idl = [Source {
        name: "html.idl",
        text: "[Exposed=Window] interface Group { [CEReactions, ReflectURL] attribute DOMString base; };",
    }];
    let rust = [Source {
        name: "group.rs",
        text: "#[rquickjs::class] struct Payload; impl<'js> group_generated::Group<'js> for Payload { fn get_base(&self, ctx: &Ctx<'js>) -> Result<rquickjs::String<'js>> { rquickjs::String::from_str(ctx.clone(), \"\") } fn set_base(&self, ctx: &Ctx<'js>, value: rquickjs::String<'js>) -> Result<()> { Ok(()) } }",
    }];
    let bindings = compile_contracts(&idl, &rust).expect("ReflectURL DOMString hand impl");
    assert!(
        bindings[0].rust.contains("fn get_base"),
        "missing hand-written ReflectURL getter"
    );
}

#[test]
fn reflect_setter_rejects_member_attributes() {
    // Unknown member attributes on the `[ReflectSetter]` path fail instead
    // of being silently ignored.
    let idl = [Source {
        name: "html.idl",
        text: "[Exposed=Window] interface Group { [ReflectSetter, Clamp] attribute long index; };",
    }];
    let rust = [Source {
        name: "group.rs",
        text: "#[rquickjs::class] struct Payload; impl<'js> group_generated::Group<'js> for Payload { fn get_index(&self, ctx: &Ctx<'js>) -> Result<i32> { Ok(0) } }",
    }];
    assert!(compile_contracts(&idl, &rust).is_err());
}

#[test]
fn reflect_parameter_values_are_validated() {
    // `ReflectDefault=foo` and any malformed `ReflectRange` fail the build.
    for text in [
        "[Exposed=Window] interface Group { [Reflect, ReflectDefault=foo] attribute unsigned long size; };",
        "[Exposed=Window] interface Group { [Reflect, ReflectRange=(1, 2, 3)] attribute unsigned long size; };",
        "[Exposed=Window] interface Group { [Reflect, ReflectRange=(one, two)] attribute unsigned long size; };",
    ] {
        let idl = [Source {
            name: "html.idl",
            text,
        }];
        let rust = [Source {
            name: "group.rs",
            text: "#[rquickjs::class] struct Payload; impl<'js> group_generated::Group<'js> for Payload { fn get_size(&self, ctx: &Ctx<'js>) -> Result<usize> { Ok(0) } fn set_size(&self, ctx: &Ctx<'js>, value: u32) -> Result<()> { Ok(()) } }",
        }];
        assert!(
            compile_contracts(&idl, &rust).is_err(),
            "invalid reflection parameter must fail: {text}"
        );
    }
}

#[test]
fn reflect_setter_unsigned_long_with_default_fails() {
    // A non-zero `ReflectDefault` on a generated `unsigned long` setter
    // would be silently ignored (the writer falls back to 0), so it fails
    // the build instead of emitting the wrong fallback.
    let idl = [Source {
        name: "html.idl",
        text: "[Exposed=Window] interface Group { [ReflectSetter, ReflectDefault=1] attribute unsigned long span; };",
    }];
    let rust = [Source {
        name: "group.rs",
        text: "#[rquickjs::class] struct Payload; impl<'js> group_generated::Group<'js> for Payload { fn get_span(&self, ctx: &Ctx<'js>) -> Result<u32> { Ok(0) } }",
    }];
    assert!(compile_contracts(&idl, &rust).is_err());
}

#[test]
fn mixin_without_implemented_includers_fails() {
    // A mixin included only by unimplemented interfaces has no install
    // target: fail loudly instead of emitting an empty installer.
    let idl = [Source {
        name: "dom.idl",
        text: "[Exposed=Window] interface Ghost {}; interface mixin Nodes { undefined append((Node or DOMString)... nodes); }; Ghost includes Nodes;",
    }];
    let rust = [Source {
        name: "nodes.rs",
        text: "#[rquickjs::class] struct Payload; impl<'js> nodes_generated::Nodes<'js> for Payload { fn append(&self, ctx: Ctx<'js>, nodes: Vec<nodes_generated::DOMStringOrNode>) -> Result<()> { Ok(()) } }",
    }];
    assert!(compile_contracts(&idl, &rust).is_err());
}

#[test]
fn usv_string_operation_argument_generates() {
    // `USVString` operation arguments lower with the USV conversion.
    let idl = [Source {
        name: "html.idl",
        text: "[Exposed=Window] interface Group { undefined send(USVString data); };",
    }];
    let rust = [Source {
        name: "group.rs",
        text: "#[rquickjs::class] struct Payload; impl<'js> group_generated::Group<'js> for Payload { fn send(&self, ctx: Ctx<'js>, data: rquickjs::String<'js>) -> Result<()> { Ok(()) } }",
    }];
    let bindings = compile_contracts(&idl, &rust).expect("USVString operation argument");
    assert!(
        bindings[0]
            .rust
            .replace(' ', "")
            .contains("usv_string_argument"),
        "missing USVString argument conversion"
    );
}

#[test]
fn reflect_legacy_null_generates() {
    let idl = [Source {
        name: "html.idl",
        text: "[Exposed=Window] interface Group { [CEReactions, Reflect] attribute [LegacyNullToEmptyString] DOMString border; };",
    }];
    let rust = [Source {
        name: "group.rs",
        text: "#[rquickjs::class] struct Payload; impl<'js> group_generated::Group<'js> for Payload {}",
    }];
    let bindings = compile_contracts(&idl, &rust).expect("legacy reflect");
    assert!(
        bindings[0]
            .rust
            .replace(' ', "")
            .contains("legacy_null_string_argument"),
        "missing legacy-null setter conversion"
    );
}

#[test]
fn reflect_usv_string_generates() {
    let idl = [Source {
        name: "html.idl",
        text: "[Exposed=Window] interface Group { [CEReactions, Reflect] attribute USVString ping; };",
    }];
    let rust = [Source {
        name: "group.rs",
        text: "#[rquickjs::class] struct Payload; impl<'js> group_generated::Group<'js> for Payload {}",
    }];
    let bindings = compile_contracts(&idl, &rust).expect("usv reflect");
    assert!(
        bindings[0]
            .rust
            .replace(' ', "")
            .contains("usv_string_argument"),
        "missing USVString setter conversion"
    );
}

#[test]
fn reflect_setter_numeric_types() {
    let idl = [Source {
        name: "html.idl",
        text: "[Exposed=Window] interface Group { [CEReactions, ReflectSetter] attribute long index; [CEReactions, ReflectSetter] attribute double score; };",
    }];
    let rust = [Source {
        name: "group.rs",
        text: "#[rquickjs::class] struct Payload; impl<'js> group_generated::Group<'js> for Payload { fn get_index(&self, ctx: &Ctx<'js>) -> Result<i32> { Ok(0) } fn get_score(&self, ctx: &Ctx<'js>) -> Result<f64> { Ok(0.0) } }",
    }];
    let bindings = compile_contracts(&idl, &rust).expect("numeric reflect setter");
    let source = bindings[0].rust.replace(' ', "");
    for fragment in ["i32", "f64", "reflect_set_long", "reflect_set_double"] {
        assert!(
            source.contains(&fragment.replace(' ', "")),
            "missing {fragment}"
        );
    }
}

#[test]
fn reflect_explicit_content_name() {
    let idl = [Source {
        name: "html.idl",
        text: "[Exposed=Window] interface Group { [Reflect=\"for\"] attribute DOMString target; };",
    }];
    let rust = [Source {
        name: "group.rs",
        text: "#[rquickjs::class] struct Payload; impl<'js> group_generated::Group<'js> for Payload {}",
    }];
    let bindings = compile_contracts(&idl, &rust).expect("compile named reflect");
    assert!(
        bindings[0].rust.replace(' ', "").contains("\"for\""),
        "missing explicit content name"
    );
}

#[test]
fn reflect_setter_splits_getter_and_generated_setter() {
    let idl = [Source {
        name: "html.idl",
        text: "[Exposed=Window] interface Button { [CEReactions, ReflectSetter] attribute USVString formAction; [CEReactions, ReflectSetter] attribute DOMString command; };",
    }];
    // Only the getter with an implementation installs; the setter generates.
    let rust = [Source {
        name: "button.rs",
        text: "#[rquickjs::class] struct Payload; impl<'js> button_generated::Button<'js> for Payload { fn get_form_action(&self, ctx: &Ctx<'js>) -> Result<dom::DomString> { Ok(\"x\".into()) } }",
    }];
    let bindings = compile_contracts(&idl, &rust).expect("compile reflect setter");
    let source = bindings[0].rust.replace(' ', "");
    assert!(
        source.contains("reflect_set_string"),
        "missing generated setter"
    );
    assert!(
        !source.contains("\"command\""),
        "unimplemented reflect setter member must stay absent"
    );
    // An implemented setter conflicts with the generated one.
    let conflict = [Source {
        name: "button.rs",
        text: "#[rquickjs::class] struct Payload; impl<'js> button_generated::Button<'js> for Payload { fn get_form_action(&self, ctx: &Ctx<'js>) -> Result<dom::DomString> { Ok(\"x\".into()) } fn set_form_action(&self, ctx: &Ctx<'js>, value: rquickjs::String<'js>) -> Result<()> { Ok(()) } }",
    }];
    assert!(compile_contracts(&idl, &conflict).is_err());
}

#[test]
fn unforgeable_and_scoped_native_members_fail_the_build() {
    let rust = [Source {
        name: "fixture.rs",
        text: "impl<'js> sample_generated::Sample<'js> for Payload { fn get_flag(&self, ctx: &Ctx<'js>) -> Result<bool> { Ok(true) } }",
    }];
    // `[LegacyUnforgeable]` is annotation-only on the contract: the
    // prototype accessor installs and the interface's own shim owns the
    // unforgeable instance shape, matching the legacy path.
    let unforgeable = [Source {
        name: "fixture.idl",
        text: "[Exposed=Window] interface Sample { [LegacyUnforgeable] readonly attribute boolean flag; };",
    }];
    compile_contracts(&unforgeable, &rust).expect("unforgeable annotation is accepted");
    // `[LegacyOverrideBuiltIns]` without a named getter changes nothing
    // observable, so the scope is accepted.
    let scoped = [Source {
        name: "fixture.idl",
        text: "[Exposed=Window] interface Sample {}; [LegacyOverrideBuiltIns] partial interface Sample { readonly attribute boolean flag; };",
    }];
    compile_contracts(&scoped, &rust).expect("inert scope is accepted");
}

#[test]
fn native_secure_context_resolves_from_every_declaration_site() {
    // One resolved exposure no matter where `[SecureContext]` sits: on the
    // member, on the interface, or on a partial interface or mixin scope
    // (<https://webidl.spec.whatwg.org/#SecureContext>).
    let rust = [Source {
        name: "fixture.rs",
        text: "impl<'js> sample_generated::Sample<'js> for Payload { fn get_flag(&self, ctx: &Ctx<'js>) -> Result<bool> { Ok(true) } fn get_open(&self, ctx: &Ctx<'js>) -> Result<bool> { Ok(true) } fn hidden(&self, ctx: &Ctx<'js>) -> Result<rquickjs::String<'js>> { rquickjs::String::from_str(ctx.clone(), \"\") } fn visible(&self, ctx: &Ctx<'js>) -> Result<rquickjs::String<'js>> { rquickjs::String::from_str(ctx.clone(), \"\") } }",
    }];
    let idl = [Source {
        name: "fixture.idl",
        text: "
        [Exposed=Window] interface Sample {
            [SecureContext] readonly attribute boolean flag;
            readonly attribute boolean open;
            [SecureContext] DOMString hidden();
            DOMString visible();
        };
    ",
    }];
    let bindings = compile_contracts(&idl, &rust).expect("compile secure members");
    let generated = bindings[0].rust.replace(' ', "");
    assert_eq!(
        generated.matches("secure_context:true").count(),
        2,
        "one gated attribute and one gated operation: {generated}"
    );
    assert_eq!(generated.matches("secure_context:false").count(), 2);

    // A `[SecureContext]` interface object and prototype are absent outside a
    // secure context, so both install hooks answer `None`.
    let interface = [Source {
        name: "fixture.idl",
        text: "[Exposed=Window, SecureContext] interface Sample { readonly attribute boolean flag; };",
    }];
    let interface_rust = [Source {
        name: "fixture.rs",
        text: "impl<'js> sample_generated::Sample<'js> for Payload { fn get_flag(&self, ctx: &Ctx<'js>) -> Result<bool> { Ok(true) } }",
    }];
    let bindings =
        compile_contracts(&interface, &interface_rust).expect("compile secure interface");
    let generated = bindings[0].rust.replace(' ', "");
    assert!(
        generated.matches("is_secure_context").count() >= 2,
        "prototype and constructor both gate: {}",
        &generated[generated.find("fnprototype").unwrap_or(0)..][..600.min(generated.len())]
    );

    // The same fact declared on a partial interface or a mixin scope reaches
    // the member.
    for text in [
        "[Exposed=Window] interface Sample { readonly attribute boolean open; }; [SecureContext] partial interface Sample { readonly attribute boolean flag; };",
        "[Exposed=Window] interface Sample { readonly attribute boolean open; }; [SecureContext] interface mixin Extra { readonly attribute boolean flag; }; Sample includes Extra;",
    ] {
        let idl = [Source {
            name: "fixture.idl",
            text,
        }];
        let scoped_rust = [Source {
            name: "fixture.rs",
            text: "impl<'js> sample_generated::Sample<'js> for Payload { fn get_flag(&self, ctx: &Ctx<'js>) -> Result<bool> { Ok(true) } fn get_open(&self, ctx: &Ctx<'js>) -> Result<bool> { Ok(true) } }",
        }];
        let bindings = compile_contracts(&idl, &scoped_rust).expect("compile scoped secure member");
        let generated = bindings[0].rust.replace(' ', "");
        let table = &generated[generated.find("constMEMBERS").unwrap_or(0)..];
        assert_eq!(
            generated.matches("secure_context:true").count(),
            1,
            "scoped member must be gated: {table}"
        );
        assert_eq!(
            generated.matches("secure_context:false").count(),
            1,
            "plain member must stay: {table}"
        );
    }
}

#[test]
fn javascript_secure_context_members_carry_their_exposure() {
    let idl = [Source {
        name: "fixture.idl",
        text: "[Exposed=Window] interface Sample { [SecureContext] readonly attribute DOMString secret; readonly attribute DOMString plain; [SecureContext] DOMString hidden(); DOMString visible(); };",
    }];
    let source = Source {
        name: "fixture.js",
        text: "
        __tbInstallInterface(class Sample {
            get secret() { return 's'; }
            get plain() { return 'p'; }
            hidden() { return 'h'; }
            visible() { return 'v'; }
        });
        ",
    };
    let output = compile_javascript(&idl, &source)
        .expect("compile JS secure members")
        .source;
    assert_eq!(
        output.matches("secureContext: true").count(),
        2,
        "one gated attribute and one gated operation: {output}"
    );
    assert_eq!(output.matches("secureContext: false").count(), 2);
}

#[test]
fn javascript_missing_members_need_an_explicit_unimplemented_list() {
    let idl = [Source {
        name: "fixture.idl",
        text: "[Exposed=Window] interface Sample { DOMString implemented(); DOMString pending(); };",
    }];
    let missing = Source {
        name: "fixture.js",
        text: "__tbInstallInterface(class Sample { implemented() { return 'x'; } })",
    };
    let error = compile_javascript(&idl, &missing)
        .err()
        .expect("missing IDL member must fail")
        .to_string();
    assert!(error.contains("pending"), "unexpected diagnostic: {error}");

    let listed = Source {
        name: "fixture.js",
        text: "__tbInstallInterface(class Sample { implemented() { return 'x'; } }, ['pending'])",
    };
    compile_javascript(&idl, &listed).expect("declared gap is accepted");

    let unknown = Source {
        name: "fixture.js",
        text: "__tbInstallInterface(class Sample { implemented() { return 'x'; } }, ['absent'])",
    };
    let error = compile_javascript(&idl, &unknown)
        .err()
        .expect("unimplemented name must name a member")
        .to_string();
    assert!(error.contains("absent"), "unexpected diagnostic: {error}");
}

const JS_ENCODER_IDL: &str = "[Exposed=Window] interface TextEncoder { readonly attribute DOMString encoding; Uint8Array encode(optional DOMString input = \"\"); };";

#[test]
fn javascript_contracts_insert_safe_reads_and_realm_results() {
    let idl = [Source {
        name: "encoding.idl",
        text: JS_ENCODER_IDL,
    }];
    let source = Source {
        name: "fixture.js",
        text: "globalThis.TextEncoder = __tbInstallInterface(class TextEncoder { get encoding() { return 'utf-8'; } encode(input) { return new Uint8Array(); } });",
    };
    let output = compile_javascript(&idl, &source)
        .expect("compile JS contract")
        .source;
    for fragment in [
        "args.length > 0 && args[0] !== undefined",
        "__tbIDLRealmUint8Array(value, false, realm)",
    ] {
        assert!(output.contains(fragment), "missing {fragment}: {output}");
    }
}

#[test]
fn javascript_unsupported_implementations_fail_the_build() {
    let idl = [Source {
        name: "encoding.idl",
        text: JS_ENCODER_IDL,
    }];
    for text in [
        "__tbInstallInterface(class TextEncoder { get encoding() { return 'x'; } encode(input) { return new Uint8Array(); } extra() {} })",
        "__tbInstallInterface(class TextEncoder { get encoding() { return 'x'; } encode() { return new Uint8Array(); } })",
        "__tbInstallInterface(class TextEncoder { get encoding() { return 'x'; } }); __tbInstallInterface(class TextEncoder { get encoding() { return 'x'; } })",
        "__tbInstallInterface(function() {}, 'TextEncoder', null, [])",
    ] {
        let source = Source {
            name: "fixture.js",
            text,
        };
        assert!(
            compile_javascript(&idl, &source).is_err(),
            "accepted {text}"
        );
    }
}

/// A `[SecureContext]` interface must use the installer form that owns its
/// global, so the global can be omitted on an insecure page.
#[test]
fn javascript_global_installation_hides_a_secure_only_interface() {
    let idl = [Source {
        name: "fixture.idl",
        text: "[Exposed=Window, SecureContext] interface Sample { readonly attribute DOMString value; };",
    }];
    let interface = Source {
        name: "fixture.js",
        text: "__tbInstallInterface(class Sample { get value() { return 'v'; } })",
    };
    let error = compile_javascript(&idl, &interface)
        .err()
        .expect("a secure-only interface cannot leave its global to the shim");
    assert!(error.to_string().contains("__tbInstallGlobal"), "{error}");
    let global = Source {
        name: "fixture.js",
        text: "__tbInstallGlobal('Sample', class Sample { get value() { return 'v'; } })",
    };
    let output = compile_javascript(&idl, &global)
        .expect("global installation")
        .source;
    assert!(
        output.contains("__tbInstallGlobal(class Sample"),
        "{output}"
    );
    assert!(
        output.contains(", true)"),
        "the secure bit gates the global: {output}"
    );
}

/// Structured JS types lower to their conversion helpers, including the
/// `Promise` fulfillment conversion.
#[test]
fn javascript_structured_types_convert_sequences_and_promises() {
    let idl = [Source {
        name: "fixture.idl",
        text: "[Exposed=Window] interface Sample { readonly attribute FrozenArray<DOMString> list; Promise<DOMString> run(sequence<DOMString> names); };",
    }];
    let source = Source {
        name: "fixture.js",
        text: "__tbInstallGlobal('Sample', class Sample { get list() { return []; } run(names) { return Promise.resolve(''); } })",
    };
    let output = compile_javascript(&idl, &source)
        .expect("structured types")
        .source;
    for fragment in [
        "__tbIDLSequence(args[0], item => __tbIDLString(item))",
        "__tbIDLPromise(value, item => __tbIDLResult(item, 'string'))",
        "__tbIDLFrozenArrayResult(value, item => __tbIDLResult(item, 'string'))",
    ] {
        assert!(output.contains(fragment), "missing {fragment}: {output}");
    }
}

#[test]
fn a_secure_context_constant_is_gated_where_it_is_installed() {
    // `[SecureContext]` const is rejected on the member itself, but a
    // `[SecureContext] partial interface` is a resolved scope, so the constant
    // must carry the gate to the installer
    // (<https://webidl.spec.whatwg.org/#SecureContext>).
    let idl = [Source {
        name: "dom.idl",
        text: "[Exposed=Window] interface Sample { readonly attribute boolean open; }; [SecureContext] partial interface Sample { const unsigned short SECRET = 7; };",
    }];
    let rust = [Source {
        name: "sample.rs",
        text: "impl<'js> sample_generated::Sample<'js> for Payload { fn get_open(&self, ctx: &Ctx<'js>) -> Result<bool> { Ok(true) } }",
    }];
    let bindings = compile_contracts(&idl, &rust).expect("compile secure constant");
    let source = squash(&bindings[0].rust);
    for fragment in [
        "CONSTANTS:&[host::Constant]=&[host::Constant{name:\"SECRET\",value:7u16,secure_context:true,},]",
        "host::Member{name:\"open\",secure_context:false",
    ] {
        assert!(
            source.contains(&squash(fragment)),
            "missing {fragment}: {source}"
        );
    }
}

#[test]
fn an_interface_secure_context_gates_the_interface_not_its_members() {
    // The interface object and prototype are gated whole, so a member of a
    // `[SecureContext] interface` carries no gate of its own: only a member or
    // an enclosing partial/mixin scope can gate one member
    // (<https://webidl.spec.whatwg.org/#SecureContext>).
    let idl = [Source {
        name: "dom.idl",
        text: "[Exposed=Window, SecureContext] interface Sample { readonly attribute boolean open; [SecureContext] undefined secret(); };",
    }];
    let rust = [Source {
        name: "sample.rs",
        text: "impl<'js> sample_generated::Sample<'js> for Payload { fn get_open(&self, ctx: &Ctx<'js>) -> Result<bool> { Ok(true) } fn secret(&self, ctx: &Ctx<'js>) -> Result<()> { Ok(()) } }",
    }];
    let bindings = compile_contracts(&idl, &rust).expect("compile secure interface");
    let source = squash(&bindings[0].rust);
    for fragment in [
        "if!host::is_secure_context(ctx)?{returnOk(None);}",
        "host::Member{name:\"open\",secure_context:false",
        "host::Member{name:\"secret\",secure_context:true",
    ] {
        assert!(
            source.contains(&squash(fragment)),
            "missing {fragment}: {source}"
        );
    }
}

fn reflected(
    interface: &str,
    attribute: &str,
    type_path: &str,
    content: &str,
    missing: Option<&str>,
    invalid: Option<&str>,
) -> ReflectedEnum {
    ReflectedEnum {
        interface: interface.to_owned(),
        attribute: attribute.to_owned(),
        content: content.to_owned(),
        type_path: type_path.to_owned(),
        missing: missing.map(str::to_owned),
        invalid: invalid.map(str::to_owned),
    }
}

/// A table entry is the only definition of the getter and setter: the trait
/// carries neither, and the generated dispatch names the keyword type and both
/// defaults. Exchanging the defaults changes the generated code, so the table
/// cannot silently swap them.
#[test]
fn reflected_enumerated_members_are_generated_from_the_table() {
    let idl = [Source {
        name: "fixture.idl",
        text: "[Exposed=Window] interface Sample { attribute DOMString mood; };",
    }];
    let rust = [Source {
        name: "fixture.rs",
        text: "impl<'js> sample_generated::Sample<'js> for Payload {}",
    }];
    let table = [reflected(
        "Sample",
        "mood",
        "Mood",
        "mood",
        Some("Happy"),
        Some("Sad"),
    )];
    let bindings = compile_contracts_with_reflection(&idl, &rust, &table).expect("compile");
    let source = squash(&bindings[0].rust);
    assert!(
        !generated_trait(&bindings[0].rust).contains("get_mood"),
        "a generated reflected getter is not a trait method"
    );
    for fragment in [
        "reflect_enumerated_attribute::<crate::js::bindings::enumerated::Mood",
        "\"mood\"",
        "missing:Some(crate::js::bindings::enumerated::Mood::Happy)",
        "invalid:Some(crate::js::bindings::enumerated::Mood::Sad)",
    ] {
        assert!(
            source.contains(&squash(fragment)),
            "missing {fragment}: {source}"
        );
    }
    let swapped = [reflected(
        "Sample",
        "mood",
        "Mood",
        "mood",
        Some("Sad"),
        Some("Happy"),
    )];
    let swapped = compile_contracts_with_reflection(&idl, &rust, &swapped).expect("compile");
    let swapped = squash(&swapped[0].rust);
    assert!(
        swapped.contains(&squash(
            "missing:Some(crate::js::bindings::enumerated::Mood::Sad)"
        )),
        "the table's missing default is the one emitted: {swapped}"
    );
}

/// A hand-written getter for a table member would compete with the generated
/// one, so the compiler refuses it instead of letting one silently win.
#[test]
fn a_hand_written_getter_for_a_table_member_is_rejected() {
    let idl = [Source {
        name: "fixture.idl",
        text: "[Exposed=Window] interface Sample { attribute DOMString mood; };",
    }];
    let rust = [Source {
        name: "fixture.rs",
        text: "impl<'js> sample_generated::Sample<'js> for Payload { fn get_mood(&self, ctx: &Ctx<'js>) -> Result<String<'js>> { read(ctx) } }",
    }];
    let table = [reflected("Sample", "mood", "Mood", "mood", None, None)];
    let error = compile_contracts_with_reflection(&idl, &rust, &table)
        .err()
        .expect("hand-written getter");
    assert!(
        error.to_string().contains("remove the hand-written one"),
        "{error}"
    );
}

/// The table is one line per (interface, attribute); a duplicate is an error
/// rather than a silent last-one-wins.
#[test]
fn the_reflected_table_rejects_a_duplicate_member() {
    let source = Source {
        name: "reflected.txt",
        text: "Sample mood Mood mood - -\nSample mood Mood mood - -\n",
    };
    let error = parse_reflected_enumerated(&source)
        .err()
        .expect("duplicate entry");
    assert!(error.to_string().contains("listed twice"), "{error}");
    let malformed = Source {
        name: "reflected.txt",
        text: "Sample mood Mood\n",
    };
    assert!(parse_reflected_enumerated(&malformed).is_err());
}
