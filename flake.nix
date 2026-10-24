{
  description = "A composition layer over Apple's container runtime";

  # The org toolchain, inherited rather than re-pinned. `defrag-nix` already assembles
  # fenix stable with the targets the siblings build for, and its lock is the one that is
  # realised in the store; a second fenix pin here would be a second rustc to bump and,
  # as it turned out, a rustc built from source.
  inputs.defrag-nix.url = "github:defrag-au/defrag-nix";

  outputs =
    { defrag-nix, ... }:
    let
      systems = [
        "aarch64-darwin"
        "x86_64-darwin"
        "x86_64-linux"
        "aarch64-linux"
      ];
    in
    {
      # `rust-stable` is the native shell: rustc, cargo, clippy, rustfmt, plus the
      # shared CLI tools and the agent inspectors. Nothing here builds for wasm.
      devShells = builtins.listToAttrs (
        map (system: {
          name = system;
          value = {
            default = defrag-nix.devShells.${system}.rust-stable;
          };
        }) systems
      );
    };
}
