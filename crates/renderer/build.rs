//! Compresses the renderer's embedded JS shims at compile time.
//!
//! The Web API shims ship inside the binary (~323 KB raw, ~72 KB deflated),
//! so they are stored deflated under `OUT_DIR/js_blobs` and inflated once per
//! process (see `js::blob`). The web bundle order lives in
//! `src/js/scripts/web/order.txt`: that file is the single source of truth,
//! read here and by the round-trip test, so adding a shim cannot silently
//! diverge from what realms evaluate.

use std::collections::HashSet;
use std::env;
use std::fmt::Write as _;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

/// (blob file name, shim path relative to the crate root).
const SINGLES: &[(&str, &str)] = &[
    ("intl.deflate", "src/js/scripts/intl.js"),
    ("brands.deflate", "src/js/scripts/brands.js"),
    ("collections.deflate", "src/js/scripts/collections.js"),
    (
        "dom_parser_ctor.deflate",
        "src/js/scripts/parsing/dom_parser_ctor.js",
    ),
    (
        "event_target_ctor.deflate",
        "src/js/scripts/events/event_target_ctor.js",
    ),
    ("abort.deflate", "src/js/scripts/events/abort.js"),
    ("event_ctor.deflate", "src/js/scripts/events/event_ctor.js"),
    (
        "custom_event.deflate",
        "src/js/scripts/events/custom_event.js",
    ),
    (
        "security_policy_violation_event.deflate",
        "src/js/scripts/events/security_policy_violation_event.js",
    ),
];

/// Wrapper around the concatenated web bundle, matching the previous
/// `concat!("(function(){", ..., "})();")` shape in `js::JsRealm::install`.
const WEB_PREFIX: &str = "(function(){";
const WEB_SUFFIX: &str = "})();";

fn main() {
    let mut contracts = generate_bindings();
    let root = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is set"));
    let out = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR is set")).join("js_blobs");
    fs::create_dir_all(&out).expect("create js_blobs dir");

    let mut manifest = String::new();

    for (blob, order) in [
        ("web_bundle.deflate", "order.txt"),
        ("worker_bundle.deflate", "worker_order.txt"),
    ] {
        let order_path = root.join("src/js/scripts/web").join(order);
        println!("cargo:rerun-if-changed={}", order_path.display());
        let mut bundle = WEB_PREFIX.to_owned();
        let mut bundle_inputs = Vec::new();
        let mut seen = HashSet::new();
        let order =
            fs::read_to_string(&order_path).unwrap_or_else(|err| panic!("read {order}: {err}"));
        for file in order.lines() {
            let file = file.trim();
            if file.is_empty() || file.starts_with('#') {
                continue;
            }
            assert!(
                file.chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')),
                "{order} entry {file:?} must be a plain file name"
            );
            assert!(
                seen.insert(file.to_owned()),
                "duplicate {order} entry {file}"
            );
            let rel = format!("src/js/scripts/web/{file}");
            println!("cargo:rerun-if-changed={}", root.join(&rel).display());
            bundle.push_str(
                &fs::read_to_string(root.join(&rel))
                    .unwrap_or_else(|err| panic!("read web shim {rel}: {err}")),
            );
            bundle_inputs.push(rel);
        }
        assert!(
            !bundle_inputs.is_empty(),
            "{order} lists no shims; refusing to ship an empty bundle"
        );
        bundle.push_str(WEB_SUFFIX);
        let name = blob.trim_end_matches(".deflate");
        // The bundles share interfaces on purpose: the worker bundle is the
        // document bundle minus DOM and Window. Compile it against a fresh
        // provider table so sharing a name is not a conflict.
        let bundle = if name == "web_bundle" {
            contracts.javascript(name, &bundle)
        } else {
            let mut worker = Contracts {
                idl: contracts.idl.clone(),
                providers: HashSet::new(),
            };
            worker.javascript(name, &bundle)
        };
        write_blob(&out, blob, bundle.as_bytes());
        writeln!(
            manifest,
            "{blob}\tbundle\t{WEB_PREFIX}|{WEB_SUFFIX}\t{}",
            bundle_inputs.join(",")
        )
        .expect("write manifest line");
    }

    for (blob, input) in SINGLES {
        println!("cargo:rerun-if-changed={}", root.join(input).display());
        let text = fs::read_to_string(root.join(input))
            .unwrap_or_else(|err| panic!("read shim {input}: {err}"));
        let text = contracts.javascript(input, &text);
        write_blob(&out, blob, text.as_bytes());
        writeln!(manifest, "{blob}\tsingle\t{input}").expect("write manifest line");
    }
    fs::write(out.join("manifest.txt"), manifest).expect("write manifest");
}

/// Deflate-compresses `bytes` at maximum level; output is deterministic for
/// one `flate2` version, which `Cargo.lock` pins.
fn write_blob(out: &Path, name: &str, bytes: &[u8]) {
    let mut encoder = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::best());
    encoder.write_all(bytes).expect("deflate shim");
    let compressed = encoder.finish().expect("finish deflate");
    fs::write(out.join(name), compressed).expect("write blob");
}

/// Compiles only the declared native surface. Unsupported semantics fail the
/// build instead of producing bindings that merely look like Web IDL.
struct Contracts {
    idl: Vec<(String, String)>,
    providers: HashSet<String>,
}

impl Contracts {
    fn javascript(&mut self, name: &str, text: &str) -> String {
        let idl: Vec<_> = self
            .idl
            .iter()
            .map(|(name, text)| webidl_bindgen::Source { name, text })
            .collect();
        let binding =
            webidl_bindgen::compile_javascript(&idl, &webidl_bindgen::Source { name, text })
                .unwrap_or_else(|error| panic!("JS IDL contracts: {error}"));
        for interface in binding.interfaces {
            assert!(
                self.providers.insert(interface.clone()),
                "conflicting binding implementations of {interface}"
            );
        }
        binding.source
    }
}

fn generate_bindings() -> Contracts {
    let root = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR"));
    let mut contracts = Contracts {
        idl: Vec::new(),
        providers: HashSet::new(),
    };
    binding_sources(
        &root.join("../webidl-bindgen/idl"),
        "idl",
        &mut contracts.idl,
    );
    let mut implementations = Vec::new();
    binding_sources(&root.join("src/js"), "rs", &mut implementations);
    let extracts: Vec<_> = contracts
        .idl
        .iter()
        .map(|(name, text)| webidl_bindgen::Source { name, text })
        .collect();
    let implementations: Vec<_> = implementations
        .iter()
        .map(|(name, text)| webidl_bindgen::Source { name, text })
        .collect();
    let table_path = root.join("src/js/bindings/reflected-enumerated.txt");
    println!("cargo:rerun-if-changed={}", table_path.display());
    let table =
        fs::read_to_string(&table_path).expect("read the reflected-enumerated metadata table");
    let reflected = webidl_bindgen::parse_reflected_enumerated(&webidl_bindgen::Source {
        name: &table_path.display().to_string(),
        text: &table,
    })
    .unwrap_or_else(|error| panic!("reflected-enumerated metadata: {error}"));
    let bindings =
        webidl_bindgen::compile_contracts_with_reflection(&extracts, &implementations, &reflected)
            .unwrap_or_else(|error| panic!("upstream IDL contracts: {error}"));
    for binding in bindings {
        contracts.providers.insert(binding.interface.clone());
        fs::write(
            out.join(&binding.interface).with_extension("rs"),
            binding.rust,
        )
        .expect("write generated contract");
    }
    contracts
}

fn binding_sources(directory: &Path, extension: &str, sources: &mut Vec<(String, String)>) {
    // The generated contracts depend on each implementation's method names,
    // so a nested edit must rerun the build script. `rerun-if-changed` on a
    // directory tracks the directory entry only, not files below it.
    println!("cargo:rerun-if-changed={}", directory.display());
    let mut paths: Vec<_> = fs::read_dir(directory)
        .expect("read binding input directory")
        .map(|entry| entry.expect("read binding input entry").path())
        .collect();
    paths.sort();
    for path in paths {
        if path.is_dir() {
            binding_sources(&path, extension, sources);
        } else if path.extension().is_some_and(|item| item == extension) {
            println!("cargo:rerun-if-changed={}", path.display());
            sources.push((
                path.display().to_string(),
                fs::read_to_string(&path).expect("read binding input"),
            ));
        }
    }
}
