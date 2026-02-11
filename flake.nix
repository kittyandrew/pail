{
  description = "pail — Personal AI Lurker";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    opencode = {
      url = "github:anomalyco/opencode";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = {
    nixpkgs,
    rust-overlay,
    opencode,
    ...
  }: let
    systems = ["x86_64-linux" "aarch64-linux"];
    forEachSystem = fn:
      nixpkgs.lib.genAttrs systems (system:
        fn {
          pkgs = import nixpkgs {
            inherit system;
            overlays = [(import rust-overlay)];
          };
          opencodePkg = opencode.packages.${system}.default;
        });
  in {
    devShells = forEachSystem ({
      pkgs,
      opencodePkg,
    }: {
      default = pkgs.mkShell {
        buildInputs = with pkgs; [
          # Rust toolchain
          (rust-bin.stable.latest.default.override {
            extensions = ["rust-src" "rust-analyzer"];
          })

          # Native deps
          pkg-config
          openssl
          sqlite

          # Tools
          alejandra
          opencodePkg
        ];
      };
    });
  };
}
