{
  description = "Development environment for agent-rs (production-shaped LLM agent framework)";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, flake-utils }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs {
          inherit system;
        };
      in
      {
        devShells.default = pkgs.mkShell {
          packages = with pkgs; [
            rustc
            cargo
            clippy
            rustfmt
            rust-analyzer
            rustPlatform.rustLibSrc
          ];

          shellHook = ''
            export RUST_BACKTRACE=1
            export CARGO_HOME="$PWD/.cargo"
            export RUST_SRC_PATH="${pkgs.rustPlatform.rustLibSrc}"
            echo "agent-rs dev shell ready. Run: make check"
          '';
        };
      });
}
