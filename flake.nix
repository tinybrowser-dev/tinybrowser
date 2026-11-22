{
  description = "Pinned rustc and OpenSSL for native-tls builds";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    # mbx (mr boxington) shares compiler outputs across checkouts. Its cargo
    # wrapper goes first on PATH below, so plain `cargo` builds through it.
    mbx = {
      url = "github:jdx/mr-boxington";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    {
      nixpkgs,
      rust-overlay,
      mbx,
      ...
    }:
    let
      system = "x86_64-linux";
      pkgs = import nixpkgs {
        inherit system;
        overlays = [ rust-overlay.overlays.default ];
      };
      rustToolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
      mbxPackage = mbx.packages.${system}.mbx;
      # Pinned nightly (rust-toolchain.toml): `[profile.release] rustflags`
      # (unwind tables off) and `.cargo/config.toml`'s in-tree std both need a
      # nightly toolchain with `rust-src`.
      runtimeLibs = pkgs.lib.makeLibraryPath [
        pkgs.openssl
        pkgs.stdenv.cc.cc
      ];
      # cargo test binaries are unwrapped, so libssl and libstdc++ must be
      # on the loader path at runtime. Prepend so the NixOS session path
      # (gcc, GL) stays behind this shell.
      # mise rust=1 shims must not shadow the overlay toolchain.
      # Build outputs are mbx's: it keeps a managed target directory per
      # checkout under /mnt/ssd/mbx and a `target` symlink here, collected
      # under a disk budget. Do not set CARGO_TARGET_DIR; that turns managed
      # targets off. mbx's cargo wrapper must come before the nix toolchain's
      # cargo so plain `cargo` builds through the shared cache.
      shellHook = ''
        export LD_LIBRARY_PATH="${runtimeLibs}''${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
        export MISE_DISABLE_TOOLS="rust''${MISE_DISABLE_TOOLS:+,$MISE_DISABLE_TOOLS}"
        export PATH="${mbxPackage}/libexec/mbx:$PATH"
      '';
    in
    {
      devShells.${system} = {
        default = pkgs.mkShell {
          strictDeps = true;
          nativeBuildInputs = [
            rustToolchain
            mbxPackage
            pkgs.pkg-config
            pkgs.nodejs_24
            pkgs.valgrind
            (pkgs.python3.withPackages (ps: [ ps.mako ]))
          ];
          buildInputs = [
            pkgs.openssl
          ];
          env = {
            OPENSSL_NO_VENDOR = "1";
          };
          inherit shellHook;
        };
      };
    };
}
