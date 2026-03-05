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
      docker = let
        uid = "1000";
        gid = "1000";
        passwd = pkgs.writeTextDir "etc/passwd" "pail:x:${uid}:${gid}:pail:/home/pail:/bin/false\n";
        group = pkgs.writeTextDir "etc/group" "pail:x:${gid}:\n";
      in
        pkgs.dockerTools.buildLayeredImage {
          name = "pail";
          tag = "0.1.0";
          contents = [pail opencodePkg pkgs.cacert pkgs.tini passwd group];
          # fakeRootCommands runs under fakeroot so chown works in the Nix sandbox
          fakeRootCommands = ''
            mkdir -p tmp home/pail var/lib/pail home/pail/.local/share/opencode home/pail/.config/opencode
            chmod 1777 tmp
            chown -R ${uid}:${gid} home/pail var/lib/pail
          '';
          config = {
            Entrypoint = ["${pkgs.tini}/bin/tini" "--" "${pail}/bin/pail" "--config" "/etc/pail/config.toml"];
            User = "${uid}:${gid}";
            # @NOTE: SENTRY_DSN and GIT_SHA use builtins.getEnv — requires `--impure` flag.
            # CI sets these from secrets/context; local builds without --impure get empty
            # strings (Sentry becomes a no-op, release is unset). See docs/observability.md.
            Env =
              [
                "HOME=/home/pail"
                "SSL_CERT_FILE=${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt"
                "SENTRY_ENVIRONMENT=production"
              ]
              ++ (let
                dsn = builtins.getEnv "SENTRY_DSN";
              in
                if dsn != ""
                then ["SENTRY_DSN=${dsn}"]
                else [])
              ++ (let
                sha = builtins.getEnv "GIT_SHA";
              in
                if sha != ""
                then ["GIT_SHA=${sha}"]
                else []);
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
