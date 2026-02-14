{
  description = "pail — Personal AI Lurker";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    crane.url = "github:ipetkov/crane";
    fenix = {
      url = "github:nix-community/fenix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    opencode = {
      url = "github:anomalyco/opencode";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = inputs @ {
    nixpkgs,
    crane,
    ...
  }: let
    systems = ["x86_64-linux" "aarch64-linux"];
    forEachSystem = fn:
      nixpkgs.lib.genAttrs systems (system:
        fn {
          pkgs = import nixpkgs {inherit system;};
          opencodePkg = inputs.opencode.packages.${system}.default;
          fenixPkgs = inputs.fenix.packages.${system};
        });
  in {
    packages = forEachSystem ({
      pkgs,
      opencodePkg,
      fenixPkgs,
    }: let
      craneLib =
        (crane.mkLib pkgs).overrideToolchain
        fenixPkgs.minimal.toolchain;
      pail = craneLib.buildPackage {
        src = ./.;
        nativeBuildInputs = [pkgs.pkg-config];
        buildInputs = [pkgs.openssl];
      };
    in {
      default = pail;
      docker = pkgs.dockerTools.buildImage {
        name = "pail";
        tag = "0.1.0";
        copyToRoot = pkgs.buildEnv {
          name = "image-root";
          paths = [pail opencodePkg pkgs.cacert];
        };
        config = {
          Entrypoint = ["${pail}/bin/pail" "--config" "/etc/pail/config.toml"];
          Env = [
            "SSL_CERT_FILE=${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt"
          ];
          ExposedPorts = {"8080/tcp" = {};};
        };
      };
    });

    devShells = forEachSystem ({
      pkgs,
      opencodePkg,
      fenixPkgs,
    }: {
      default = pkgs.mkShell {
        buildInputs = [
          # Rust toolchain (complete — includes rust-analyzer, rust-src)
          fenixPkgs.complete.toolchain

          # Native deps
          pkgs.pkg-config
          pkgs.openssl
          pkgs.sqlite

          # Tools
          pkgs.alejandra
          opencodePkg
          pkgs.gh
        ];
      };
    });
  };
}
