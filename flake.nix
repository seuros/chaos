{
  description = "chaos — a provider-agnostic AI agent operating system";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    # The project pins its toolchain in rust-toolchain.toml (currently 1.98.0),
    # which is newer than nixpkgs' rustc. The rust overlay builds with the
    # exact pinned compiler; see `makeRustPlatform` below.
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    {
      self,
      nixpkgs,
      rust-overlay,
    }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "x86_64-darwin"
        "aarch64-darwin"
      ];
      forAllSystems = nixpkgs.lib.genAttrs systems;
    in
    {
      packages = forAllSystems (
        system:
        let
          pkgs = import nixpkgs {
            inherit system;
            overlays = [ rust-overlay.overlays.default ];
          };
          rustBin = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
          rustPlatform = pkgs.makeRustPlatform {
            cargo = rustBin;
            rustc = rustBin;
          };
        in
        {
          chaos = pkgs.callPackage ./package.nix { inherit rustPlatform; };
          default = self.packages.${system}.chaos;
        }
      );

      devShells = forAllSystems (
        system:
        let
          pkgs = import nixpkgs {
            inherit system;
            overlays = [ rust-overlay.overlays.default ];
          };
          rustBin = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
        in
        {
          default = pkgs.mkShell {
            packages = [
              rustBin
              pkgs.just
              pkgs.git
              pkgs.pkg-config # libdbus-sys / libsqlite3-sys discovery
              pkgs.clang # bindgen (rama-dns) needs libclang
              pkgs.perl # vendored openssl-sys build
              pkgs.dbus # arboard clipboard backend links libdbus-1
              pkgs.cargo-nextest
            ];
            # The pkg-config wrapper execs the real pkg-config with the
            # role-suffixed variable only, and stdenv initializes it to "."
            # in dev shells — so build scripts probing via the pkg_config
            # crate (libdbus-sys) see no search path at all. Mirror the
            # plain variable into the role variable after all hooks ran.
            shellHook = ''
              export PKG_CONFIG_PATH_x86_64_unknown_linux_gnu="''${PKG_CONFIG_PATH:-}"
            '';
            env.LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";
          };
        }
      );
    };
}
