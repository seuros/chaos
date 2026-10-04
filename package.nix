# Nix packaging for chaos.
#
# Builds the same four binaries the GitHub release ships (see install.sh
# and `just dist`): chaos, alcatraz, chaos_journald, chaos-forkve-wrapper.
#
# Native build prerequisites mirror `just install`:
#   - clang   -> libclang for bindgen (rama-dns)
#   - pkgconf -> pkg-config for libdbus-sys
#   - dbus    -> libdbus-1 for arboard's clipboard backend (Linux)
#   - perl    -> vendored openssl-sys (openssl-src) compiles OpenSSL
{
  lib,
  rustPlatform,
  clang,
  llvmPackages,
  pkg-config,
  perl,
  dbus,
}:

rustPlatform.buildRustPackage {
  pname = "chaos";
  version = "47.11.0";

  # Restrict src to what cargo actually needs so flake/tooling edits don't
  # invalidate the build. Keep the list minimal: sys/kern/kern/build.rs
  # embeds a catalog from man/, and embed macros may reference other
  # root-level files, so docs/ and the markdown files stay in.
  src =
    let
      excluded = [
        "target"
        ".tmp" # with-local-qa-tmp.sh scratch dir
        ".github"
        ".idea"
        "drivers" # git submodules, excluded from the workspace
        "flake.nix"
        "flake.lock"
        "package.nix"
        "install.sh"
        "justfile"
        "mise.toml"
        ".gitignore"
      ];
    in
    lib.cleanSourceWith {
      src = lib.cleanSource ./.;
      filter =
        path: type:
        !(builtins.elem (baseNameOf path) excluded);
    };

  cargoHash = "sha256-y/yPoo0ZB60qB1R0FmHYYuMBVpc84YYSaVjm5IGwDb4=";

  # The workspace test suites drive agents, PTYs, and (for the regress set)
  # a live Postgres — none of that survives the build sandbox. Run them via
  # `nix develop` (cargo-nextest is in the shell).
  doCheck = false;

  nativeBuildInputs = [
    clang
    pkg-config
    perl
  ];
  buildInputs = [
    dbus
  ];

  env.LIBCLANG_PATH = "${llvmPackages.libclang.lib}/lib";

  # Restrict the build to the release binary set; the install hook copies
  # every executable it finds in target/, and the workspace also contains
  # dev-only bins (apply_patch, clamp-test-peer) we don't ship.
  cargoBuildFlags = [
    "-p"
    "chaos-cli"
    "-p"
    "alcatraz"
    "-p"
    "chaos_journald"
    "-p"
    "chaos-doas"
    "--bin"
    "chaos"
    "--bin"
    "alcatraz"
    "--bin"
    "chaos_journald"
    "--bin"
    "chaos-forkve-wrapper"
  ];

  meta = {
    description = "Provider-agnostic AI agent operating system (Codex CLI fork)";
    longDescription = ''
      chaos ("Chat OS") is an Apache-2.0 fork of OpenAI's Codex CLI made
      provider-agnostic: it speaks to OpenAI, Anthropic, and local models.
      Structured like a BSD system: a Kernel for LLM communication, Modules
      for capabilities (voice, sandboxing), and Drivers (MCP servers) for
      external tools.
    '';
    homepage = "https://github.com/seuros/chaos";
    changelog = "https://github.com/seuros/chaos/blob/master/CHANGELOG.md";
    license = lib.licenses.asl20;
    mainProgram = "chaos";
    platforms = lib.platforms.unix;
  };
}
