//! Link-time size knobs for the shipping binary.
//!
//! These lived in `.cargo/config.toml` as global `rustflags`. They are scoped
//! to the release binary instead: they only shrink the shipping artifact.
//! Every link uses the toolchain's default rust-lld (the rustc 1.98.1
//! `unknown relocation (1875)` DWARF bug that forced bfd onto dev/test links
//! is gone on the pinned nightly). Nothing unwinds in release (`panic =
//! "abort"` from the profile; QuickJS-ng uses setjmp/longjmp), so
//! `[profile.release] rustflags` disables unwind-table emission outright
//! (`force-unwind-tables=no`): the shipping binary carries no `.eh_frame`
//! bodies beyond the CRT startup objects' 48-byte CIE. No post-link step is
//! left; `cargo build --release` is the shipped artifact.
//!
//! Sizes and method: `docs/size.md`.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("PROFILE").as_deref() != Ok("release") {
        return;
    }
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();

    match target_os.as_str() {
        "linux" => {
            for arg in [
                "-fuse-ld=lld",
                "-Wl,--icf=all",
                "-Wl,--build-id=none",
                "-Wl,--gc-sections",
                "-Wl,-O2",
                "-Wl,-z,pack-relative-relocs",
                "-Wl,--no-eh-frame-hdr",
            ] {
                println!("cargo:rustc-link-arg-bins={arg}");
            }
        }
        "macos" => {
            println!("cargo:rustc-link-arg-bins=-Wl,-dead_strip");
        }
        "windows" if target_env == "msvc" => {
            for arg in ["/OPT:REF", "/OPT:ICF"] {
                println!("cargo:rustc-link-arg-bins={arg}");
            }
        }
        _ => {}
    }
}
