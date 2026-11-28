use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::wrapper::Wrapper;

pub use wpt_report::Kind;

#[derive(Clone, Debug)]
pub struct TestCase {
    pub relative: String,
    pub source: String,
    pub kind: Kind,
    pub bytes: Vec<u8>,
    pub content_type: String,
    pub long_timeout: bool,
    pub refs: Vec<Reference>,
    pub fuzzy: Option<Fuzzy>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reference {
    pub relative: String,
    pub source: String,
    pub relation: Relation,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Relation {
    Match,
    Mismatch,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fuzzy {
    pub max_difference: u8,
    pub total_pixels: u64,
}

impl TestCase {
    #[must_use]
    pub fn id(&self) -> String {
        format!("/{}", self.relative)
    }
}

pub struct Selection {
    pub offset: usize,
    pub limit: usize,
    pub shard: Option<(usize, usize)>,
    pub exclude_testdriver: bool,
    pub exclude: Vec<String>,
}

pub fn select(
    root: &Path,
    paths: &[String],
    content_types: &HashMap<String, String>,
    selection: &Selection,
) -> io::Result<Vec<TestCase>> {
    let manifest = read_manifest(root)?;
    let mut selected: Vec<Item> = Vec::new();
    for kind in ["testharness", "crashtest", "reftest"] {
        let Some(tree) = manifest.get("items").and_then(|items| items.get(kind)) else {
            continue;
        };
        walk(tree, "", &mut |path, leaf| {
            if kind == "reftest" {
                collect_reftest(root, path, leaf, paths, &mut selected);
            } else {
                collect_leaf(root, path, leaf, kind, paths, &mut selected);
            }
        });
    }
    selected.sort_by(|left, right| left.relative.cmp(&right.relative));
    selected.dedup_by(|left, right| left.relative == right.relative);
    if let Some((index, count)) = selection.shard {
        selected.retain(|item| chunk_of_source(&item.source, count) == index);
    }
    if selection.exclude_testdriver {
        selected.retain(|item| item.kind != Kind::Testdriver);
    }
    if selected.is_empty() {
        return Err(io::Error::other(format!(
            "no manifest test matches {}",
            paths.join(", ")
        )));
    }
    if !selection.exclude.is_empty() {
        selected.retain(|item| !excluded(&item.relative, &selection.exclude));
    }
    let mut cases = Vec::new();
    for item in selected
        .into_iter()
        .skip(selection.offset)
        .take(selection.limit)
    {
        cases.push(item.materialize(root, content_types)?);
    }
    Ok(cases)
}

fn excluded(relative: &str, prefixes: &[String]) -> bool {
    prefixes.iter().any(|prefix| {
        let prefix = prefix.trim_matches('/');
        relative == prefix || relative.starts_with(&format!("{prefix}/"))
    })
}

fn chunk_of_source(source: &str, count: usize) -> usize {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in source.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    usize::try_from(hash % u64::try_from(count).unwrap_or(u64::MAX)).unwrap_or(0)
}

struct Item {
    relative: String,
    source: String,
    kind: Kind,
    long_timeout: bool,
    wrapper: Option<Wrapper>,
    refs: Vec<Reference>,
    fuzzy: Option<Fuzzy>,
}

impl Item {
    fn materialize(
        self,
        root: &Path,
        content_types: &HashMap<String, String>,
    ) -> io::Result<TestCase> {
        let bytes = fs::read(root.join(&self.source)).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("cannot read {}: {error}", self.source),
            )
        })?;
        let (bytes, content_type) = if let Some(wrapper) = self.wrapper {
            let js_url = js_url(&self.relative);
            (
                wrapper.render(&bytes, &js_url).into_bytes(),
                "text/html".to_owned(),
            )
        } else {
            let content_type = content_type_for(Path::new(&self.source), content_types);
            (bytes, content_type)
        };
        Ok(TestCase {
            relative: self.relative,
            source: self.source,
            kind: self.kind,
            bytes,
            content_type,
            long_timeout: self.long_timeout,
            refs: self.refs,
            fuzzy: self.fuzzy,
        })
    }
}

fn read_manifest(root: &Path) -> io::Result<Value> {
    let path = root.join("MANIFEST.json");
    let bytes = fs::read(&path).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!(
                "cannot read {}: {error} (run `cli.py manifest` to build it)",
                path.display()
            ),
        )
    })?;
    serde_json::from_slice(&bytes).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{}: {error}", path.display()),
        )
    })
}

fn walk(node: &Value, prefix: &str, visit: &mut impl FnMut(&str, &Value)) {
    let Some(entries) = node.as_object() else {
        return;
    };
    for (name, value) in entries {
        let path = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        if value.is_object() {
            walk(value, &path, visit);
        } else {
            visit(&path, value);
        }
    }
}

fn collect_leaf(
    root: &Path,
    path: &str,
    leaf: &Value,
    kind: &str,
    paths: &[String],
    selected: &mut Vec<Item>,
) {
    let Some(entries) = leaf.as_array() else {
        return;
    };
    for entry in entries.iter().skip(1) {
        let Some(entry) = entry.as_array() else {
            continue;
        };
        if entry.len() < 2 {
            continue;
        }
        let empty = Map::new();
        let props = entry.last().and_then(Value::as_object).unwrap_or(&empty);
        if props.contains_key("jsshell") {
            continue;
        }
        let url = entry[0].as_str().unwrap_or(path);
        let url_path = url.split('?').next().unwrap_or(url);
        let (source, wrapper) = match Wrapper::for_url(url_path) {
            Some((wrapper, source)) => (source, Some(wrapper)),
            None => (url_path.trim_start_matches('/').to_owned(), None),
        };
        if !matches_requested(paths, &source, url_path) {
            continue;
        }
        if !root.join(&source).is_file() {
            continue;
        }
        let kind = match kind {
            "crashtest" => Kind::Crashtest,
            _ if props.get("testdriver").and_then(Value::as_bool) == Some(true) => Kind::Testdriver,
            _ => Kind::Testharness,
        };
        selected.push(Item {
            relative: url.trim_start_matches('/').to_owned(),
            source,
            kind,
            long_timeout: props.get("timeout").and_then(Value::as_str) == Some("long"),
            wrapper,
            refs: Vec::new(),
            fuzzy: None,
        });
    }
}

// https://web-platform-tests.org/writing-tests/reftests.html
fn collect_reftest(
    root: &Path,
    path: &str,
    leaf: &Value,
    paths: &[String],
    selected: &mut Vec<Item>,
) {
    let Some(entries) = leaf.as_array() else {
        return;
    };
    for entry in entries.iter().skip(1) {
        let Some(entry) = entry.as_array() else {
            continue;
        };
        if entry.len() < 2 {
            continue;
        }
        let extras = entry.get(2).and_then(Value::as_object);
        let long_timeout = entry.first().and_then(Value::as_str) == Some("long")
            || extras
                .and_then(|extras| extras.get("timeout"))
                .and_then(Value::as_str)
                == Some("long");
        let mut refs = Vec::new();
        for pair in entry[1].as_array().into_iter().flatten() {
            let Some(pair) = pair.as_array() else {
                continue;
            };
            if pair.len() < 2 {
                continue;
            }
            let Some(url) = pair[0].as_str() else {
                continue;
            };
            let Some(relation) = pair[1].as_str().and_then(|relation| match relation {
                "==" => Some(Relation::Match),
                "!=" => Some(Relation::Mismatch),
                _ => None,
            }) else {
                continue;
            };
            let source = url.trim_start_matches('/').to_owned();
            refs.push(Reference {
                relative: source.clone(),
                source,
                relation,
            });
        }
        if refs.is_empty() {
            continue;
        }
        if !matches_requested(paths, path, path) || !root.join(path).is_file() {
            continue;
        }
        let kind = if extras
            .and_then(|extras| extras.get("testdriver"))
            .and_then(Value::as_bool)
            == Some(true)
        {
            Kind::Testdriver
        } else {
            Kind::Reftest
        };
        selected.push(Item {
            relative: path.to_owned(),
            source: path.to_owned(),
            kind,
            long_timeout,
            wrapper: None,
            refs,
            fuzzy: extras.and_then(parse_fuzzy),
        });
    }
}

fn parse_fuzzy(extras: &Map<String, Value>) -> Option<Fuzzy> {
    for entry in extras.get("fuzzy")?.as_array()? {
        let Some(pair) = entry.as_array() else {
            continue;
        };
        if pair.len() < 2 || !pair[0].is_null() {
            continue;
        }
        let Some(ranges) = pair[1].as_array() else {
            continue;
        };
        if ranges.len() < 2 {
            continue;
        }
        let max_difference = u8::try_from(range_max(&ranges[0])?.min(255)).ok()?;
        return Some(Fuzzy {
            max_difference,
            total_pixels: range_max(&ranges[1])?,
        });
    }
    None
}

fn range_max(value: &Value) -> Option<u64> {
    match value {
        Value::Number(number) => number.as_u64(),
        Value::Array(items) => items.last().and_then(Value::as_u64),
        _ => None,
    }
}

fn matches_requested(paths: &[String], source: &str, url_path: &str) -> bool {
    let url_path = url_path.trim_start_matches('/');
    paths.iter().any(|request| {
        let request = request
            .trim_start_matches('/')
            .split('?')
            .next()
            .unwrap_or("")
            .trim_end_matches('/');
        request.is_empty()
            || [source, url_path].iter().any(|candidate| {
                *candidate == request || candidate.starts_with(&format!("{request}/"))
            })
    })
}

fn js_url(document: &str) -> String {
    let path = document
        .split('?')
        .next()
        .unwrap_or(document)
        .trim_start_matches('/');
    let base = path.strip_suffix(".html").unwrap_or(path);
    format!("/{base}.js")
}

pub(crate) fn content_type_for(path: &Path, content_types: &HashMap<String, String>) -> String {
    path.extension()
        .and_then(|extension| extension.to_str())
        .and_then(|extension| content_types.get(extension))
        .cloned()
        .unwrap_or_else(|| "application/octet-stream".to_owned())
}

#[must_use]
pub fn default_root() -> PathBuf {
    PathBuf::from("third_party/wpt")
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::fs;

    use super::{Kind, Relation, Selection, select};

    fn all() -> Selection {
        Selection {
            offset: 0,
            limit: usize::MAX,
            shard: None,
            exclude_testdriver: false,
            exclude: Vec::new(),
        }
    }

    fn checkout(name: &str) -> std::path::PathBuf {
        let root =
            std::env::temp_dir().join(format!("wpt-harness-suite-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("dom/nodes/resources")).expect("checkout");
        fs::create_dir_all(root.join("crashtests")).expect("crashtest directory");
        fs::create_dir_all(root.join("css")).expect("reftest directory");
        fs::write(
            root.join("dom/nodes/plain.html"),
            "<script src=/resources/testharness.js></script>",
        )
        .expect("plain test");
        fs::write(
            root.join("dom/nodes/multi.any.js"),
            "// META: global=window,worker\n// META: timeout=long\ntest(() => {}, 'x');\n",
        )
        .expect("multi source");
        fs::write(
            root.join("dom/nodes/window.window.js"),
            "test(() => {}, 'w');",
        )
        .expect("window source");
        fs::write(
            root.join("dom/nodes/driver.html"),
            "<script src=/resources/testharness.js></script>",
        )
        .expect("testdriver test");
        fs::write(
            root.join("dom/nodes/resources/helper.html"),
            "<p>helper</p>",
        )
        .expect("helper");
        fs::write(root.join("crashtests/boom.html"), "<p>boom</p>").expect("crashtest");
        fs::write(root.join("css/example.html"), "<p>test</p>").expect("reftest");
        fs::write(root.join("css/example-ref.html"), "<p>ref</p>").expect("reftest ref");
        fs::write(root.join("css/other-ref.html"), "<p>other</p>").expect("reftest other ref");
        let manifest = serde_json::json!({
            "version": 9,
            "url_base": "/",
            "items": {
                "support": {
                    "dom": {"nodes": {"resources": {
                        "helper.html": ["h", [null, {}]]
                    }}}
                },
                "testharness": {
                    "dom": {"nodes": {
                        "plain.html": ["p", [null, {}]],
                        "driver.html": ["d", [null, {"testdriver": true}]],
                        "multi.any.js": ["m",
                            [null, {"jsshell": true}],
                            ["dom/nodes/multi.any.html", {"timeout": "long"}],
                            ["dom/nodes/multi.any.worker.html", {}]
                        ],
                        "window.window.js": ["w", ["dom/nodes/window.window.html", {}]]
                    }}
                },
                "crashtest": {
                    "crashtests": {
                        "boom.html": ["c", ["crashtests/boom.html", {"timeout": "long", "testdriver": true}]]
                    }
                },
                "reftest": {
                    "css": {
                        "example.html": ["r", [null, [
                            ["/css/example-ref.html", "=="],
                            ["/css/other-ref.html", "!="]
                        ], {"fuzzy": [[null, [[0, 3], [0, 250_000]]]]}]]
                    }
                }
            }
        });
        fs::write(
            root.join("MANIFEST.json"),
            serde_json::to_vec(&manifest).expect("manifest"),
        )
        .expect("manifest written");
        root
    }

    fn types() -> HashMap<String, String> {
        HashMap::from([
            ("html".to_owned(), "text/html".to_owned()),
            ("js".to_owned(), "text/javascript".to_owned()),
        ])
    }

    #[test]
    fn the_manifest_decides_type_timeout_and_wrapper() {
        let root = checkout("all");
        let cases = select(&root, &["dom/nodes".into()], &types(), &all()).expect("selection");
        let ids: Vec<&str> = cases.iter().map(|case| case.relative.as_str()).collect();
        assert_eq!(
            ids,
            vec![
                "dom/nodes/driver.html",
                "dom/nodes/multi.any.html",
                "dom/nodes/plain.html",
                "dom/nodes/window.window.html",
            ],
            "the worker wrapper and the jsshell variant are not browser tests"
        );
        assert_eq!(cases[0].kind, Kind::Testdriver);
        let cases = &cases[1..];
        let multi = &cases[0];
        assert_eq!(multi.kind, Kind::Testharness);
        assert!(multi.long_timeout, "the manifest marks the long timeout");
        assert_eq!(multi.source, "dom/nodes/multi.any.js");
        assert_eq!(multi.content_type, "text/html");
        let html = String::from_utf8(multi.bytes.clone()).expect("wrapper is UTF-8");
        assert!(html.contains("self.GLOBAL"));
        assert!(html.contains("<meta name=\"timeout\" content=\"long\">"));
        assert!(html.contains("<script src=\"/dom/nodes/multi.any.js\"></script>"));

        assert_eq!(cases[1].kind, Kind::Testharness);
        assert_eq!(cases[1].content_type, "text/html");
        assert!(!cases[1].long_timeout);

        let window = String::from_utf8(cases[2].bytes.clone()).expect("wrapper is UTF-8");
        assert!(
            !window.contains("self.GLOBAL"),
            "window.js has its own wrapper"
        );
        assert!(window.contains("/dom/nodes/window.window.js"));

        let crash =
            select(&root, &["crashtests".into()], &types(), &all()).expect("crashtest selection");
        assert_eq!(crash.len(), 1);
        assert_eq!(crash[0].kind, Kind::Crashtest);
        assert!(crash[0].long_timeout);
        fs::remove_dir_all(&root).expect("checkout removed");
    }

    #[test]
    fn a_reftest_carries_its_references_and_fuzzy_tolerance() {
        let root = checkout("reftest");
        let cases = select(&root, &["css".into()], &types(), &all()).expect("selection");
        assert_eq!(cases.len(), 1);
        let case = &cases[0];
        assert_eq!(case.kind, Kind::Reftest);
        assert_eq!(case.relative, "css/example.html");
        assert_eq!(case.source, "css/example.html");
        assert_eq!(case.refs.len(), 2);
        assert_eq!(case.refs[0].source, "css/example-ref.html");
        assert_eq!(case.refs[0].relation, Relation::Match);
        assert_eq!(case.refs[1].relation, Relation::Mismatch);
        let fuzzy = case.fuzzy.expect("fuzzy tolerance");
        assert_eq!(fuzzy.max_difference, 3);
        assert_eq!(fuzzy.total_pixels, 250_000);
        fs::remove_dir_all(&root).expect("checkout removed");
    }

    #[test]
    fn a_testdriver_testharness_is_selected_and_skipped() {
        let root = checkout("testdriver");
        let manifest = serde_json::json!({
            "items": {"testharness": {"t.html": ["h", [null, {"testdriver": true}]]}}
        });
        fs::write(root.join("t.html"), "<script src=/resources/testdriver.js>").expect("test");
        fs::write(
            root.join("MANIFEST.json"),
            serde_json::to_vec(&manifest).expect("manifest"),
        )
        .expect("manifest written");
        let cases = select(&root, &["t.html".into()], &types(), &all()).expect("selection");
        assert_eq!(cases[0].kind, Kind::Testdriver);
        fs::remove_dir_all(&root).expect("checkout removed");
    }

    #[test]
    fn a_testdriver_reftest_belongs_to_the_webdriver_lane() {
        let root = checkout("driver-reftest");
        let manifest = serde_json::json!({
            "items": {"reftest": {"r.html": ["x", [null, [
                ["/r-ref.html", "=="]
            ], {"testdriver": true}]]}}
        });
        fs::write(root.join("r.html"), "<p>test</p>").expect("test");
        fs::write(root.join("r-ref.html"), "<p>ref</p>").expect("ref");
        fs::write(
            root.join("MANIFEST.json"),
            serde_json::to_vec(&manifest).expect("manifest"),
        )
        .expect("manifest written");
        let cases = select(&root, &["r.html".into()], &types(), &all()).expect("selection");
        assert_eq!(cases[0].kind, Kind::Testdriver);
        let without = select(
            &root,
            &["r.html".into()],
            &types(),
            &Selection {
                offset: 0,
                limit: usize::MAX,
                shard: None,
                exclude_testdriver: true,
                exclude: Vec::new(),
            },
        );
        assert!(
            without.is_err(),
            "the in-process lane must leave a testdriver reftest out"
        );
        fs::remove_dir_all(&root).expect("checkout removed");
    }

    #[test]
    fn chunks_partition_the_selection_and_testdriver_can_be_left_out() {
        let root = checkout("chunks");
        let paths = ["dom".to_string(), "css".into(), "crashtests".into()];
        let whole = select(&root, &paths, &types(), &all()).expect("selection");

        let mut seen = Vec::new();
        for index in 0..2 {
            let shard = Selection {
                offset: 0,
                limit: usize::MAX,
                shard: Some((index, 2)),
                exclude_testdriver: false,
                exclude: Vec::new(),
            };
            let cases = select(&root, &paths, &types(), &shard).expect("chunk");
            seen.extend(cases.into_iter().map(|case| case.relative));
        }
        seen.sort();
        let mut expected: Vec<String> = whole.iter().map(|case| case.relative.clone()).collect();
        expected.sort();
        assert_eq!(seen, expected);

        let mut without = select(
            &root,
            &paths,
            &types(),
            &Selection {
                offset: 0,
                limit: usize::MAX,
                shard: None,
                exclude_testdriver: true,
                exclude: Vec::new(),
            },
        )
        .expect("without testdriver");
        assert!(
            !without.iter().any(|case| case.kind == Kind::Testdriver),
            "the excluded kind is gone"
        );
        without.sort_by(|left, right| left.relative.cmp(&right.relative));
        assert!(
            expected
                .iter()
                .any(|relative| relative == "dom/nodes/driver.html")
        );
        fs::remove_dir_all(&root).expect("checkout removed");
    }

    #[test]
    fn paths_select_files_directories_and_sources() {
        let root = checkout("paths");
        assert_eq!(
            select(&root, &["dom/nodes/plain.html".into()], &types(), &all())
                .expect("file")
                .len(),
            1
        );
        assert_eq!(
            select(&root, &["dom/nodes/multi.any.js".into()], &types(), &all())
                .expect("source name")
                .len(),
            1
        );
        assert_eq!(
            select(
                &root,
                &["dom/nodes/multi.any.html".into()],
                &types(),
                &all()
            )
            .expect("test URL")
            .len(),
            1
        );
        let window = Selection {
            offset: 1,
            limit: 1,
            shard: None,
            exclude_testdriver: false,
            exclude: Vec::new(),
        };
        let sliced = select(&root, &["dom/nodes".into()], &types(), &window).expect("slice");
        assert_eq!(sliced.len(), 1);
        assert_eq!(sliced[0].relative, "dom/nodes/multi.any.html");
        let excluded = Selection {
            exclude: vec!["dom/nodes/multi.any.html".into()],
            ..all()
        };
        let kept =
            select(&root, &["dom/nodes".into()], &types(), &excluded).expect("excluded selection");
        assert!(
            kept.iter()
                .all(|case| case.relative != "dom/nodes/multi.any.html")
        );
        let excluded_dir = Selection {
            exclude: vec!["dom/nodes".into()],
            ..all()
        };
        assert!(
            select(&root, &["dom/nodes".into()], &types(), &excluded_dir)
                .expect("excluded directory")
                .is_empty()
        );
        assert!(select(&root, &["missing".into()], &types(), &all()).is_err());
        fs::remove_dir_all(&root).expect("checkout removed");
    }
}
