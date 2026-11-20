use std::io::Read as _;
use std::sync::OnceLock;

use rquickjs::{Ctx, Exception, Result};

pub(crate) fn decompress(
    ctx: &Ctx<'_>,
    deflated: &'static [u8],
    cache: &'static OnceLock<Box<str>>,
) -> Result<&'static str> {
    if let Some(cached) = cache.get() {
        return Ok(cached);
    }
    let mut out = String::new();
    flate2::read::DeflateDecoder::new(deflated)
        .read_to_string(&mut out)
        .map_err(|err| Exception::throw_internal(ctx, &err.to_string()))?;
    Ok(cache.get_or_init(|| out.into_boxed_str()))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::Read as _;
    use std::path::PathBuf;

    #[test]
    fn blobs_round_trip() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let idl: Vec<_> = fs::read_dir(root.join("../webidl-bindgen/idl"))
            .expect("read IDL corpus")
            .map(|entry| entry.expect("read IDL entry").path())
            .filter(|path| path.extension().is_some_and(|extension| extension == "idl"))
            .map(|path| {
                (
                    path.display().to_string(),
                    fs::read_to_string(path).expect("read IDL extract"),
                )
            })
            .collect();
        let idl: Vec<_> = idl
            .iter()
            .map(|(name, text)| webidl_bindgen::Source { name, text })
            .collect();
        let dir = PathBuf::from(env!("OUT_DIR")).join("js_blobs");
        let manifest = fs::read_to_string(dir.join("manifest.txt")).expect("read blob manifest");
        assert!(!manifest.trim().is_empty(), "blob manifest is empty");
        for line in manifest.lines() {
            let (blob, rest) = line.split_once('\t').expect("manifest blob field");
            let compressed = fs::read(dir.join(blob)).expect("read blob");
            let mut out = String::new();
            flate2::read::DeflateDecoder::new(&compressed[..])
                .read_to_string(&mut out)
                .expect("inflate blob");
            let (mode, payload) = rest.split_once('\t').expect("manifest mode field");
            let expected = match mode {
                "single" => fs::read_to_string(root.join(payload)).expect("read shim source"),
                "bundle" => {
                    let (wrap, paths) = payload.split_once('\t').expect("bundle manifest");
                    let (prefix, suffix) = wrap.split_once('|').expect("bundle wrapper");
                    let mut expected = String::from(prefix);
                    for path in paths.split(',') {
                        let source = fs::read_to_string(root.join(path)).expect("read shim source");
                        expected.push_str(&source);
                    }
                    expected.push_str(suffix);
                    expected
                }
                _ => panic!("unknown manifest mode {mode}"),
            };
            let generated = webidl_bindgen::compile_javascript(
                &idl,
                &webidl_bindgen::Source {
                    name: blob,
                    text: &expected,
                },
            )
            .expect("compile JS contracts");
            assert_eq!(
                out, generated.source,
                "{blob} drifted from its ordered sources and IDL contracts"
            );
        }
    }
}
