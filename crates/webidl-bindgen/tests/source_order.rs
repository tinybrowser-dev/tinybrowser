use webidl_bindgen::{Source, compile_contracts, compile_javascript};

#[test]
fn source_order_does_not_change_the_generated_output() {
    let base = Source {
        name: "alpha.idl",
        text: "[Exposed=Window] interface Sample { readonly attribute DOMString first; };",
    };
    let partial = Source {
        name: "beta.idl",
        text: "partial interface Sample { readonly attribute DOMString second; };",
    };
    let rust = Source {
        name: "sample.rs",
        text: "impl<'js> sample_generated::Sample<'js> for Payload { fn get_first(&self, ctx: &Ctx<'js>) -> Result<String<'js>> { read(ctx) } fn get_second(&self, ctx: &Ctx<'js>) -> Result<String<'js>> { read(ctx) } }",
    };
    let javascript = Source {
        name: "sample.js",
        text: "__tbInstallGlobal('Sample', class Sample { get first() { return 'a'; } get second() { return 'b'; } })",
    };

    let forward = [
        Source {
            name: base.name,
            text: base.text,
        },
        Source {
            name: partial.name,
            text: partial.text,
        },
    ];
    let reverse = [
        Source {
            name: partial.name,
            text: partial.text,
        },
        Source {
            name: base.name,
            text: base.text,
        },
    ];

    let contracts_forward = compile_contracts(
        &forward,
        &[Source {
            name: rust.name,
            text: rust.text,
        }],
    )
    .expect("compile contracts");
    let contracts_reverse = compile_contracts(
        &reverse,
        &[Source {
            name: rust.name,
            text: rust.text,
        }],
    )
    .expect("compile contracts");
    assert_eq!(
        contracts_forward[0].rust, contracts_reverse[0].rust,
        "Rust bindings depend on source order"
    );

    let js_forward = compile_javascript(
        &forward,
        &Source {
            name: javascript.name,
            text: javascript.text,
        },
    )
    .expect("compile JavaScript");
    let js_reverse = compile_javascript(
        &reverse,
        &Source {
            name: javascript.name,
            text: javascript.text,
        },
    )
    .expect("compile JavaScript");
    assert_eq!(
        js_forward.source, js_reverse.source,
        "generated JavaScript depends on source order"
    );
}
