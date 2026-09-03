{
  description = "Rust development environment";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay.url = "github:oxalica/rust-overlay";
    crane.url = "github:ipetkov/crane";
  };

  outputs = { self, nixpkgs, flake-utils, rust-overlay, crane }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs {
          inherit system overlays;
        };
        rust-toolchain = pkgs.rust-bin.stable.latest.default.override {
          extensions = [ "rust-src" "rust-analyzer" ];
        };
        craneLib = (crane.mkLib pkgs).overrideToolchain (p: rust-toolchain);
      in
      {
        devShells.default = pkgs.mkShell {
          buildInputs = with pkgs; [
            rust-toolchain
            cargo-edit
            rustfmt
            clippy
            rust-analyzer
            just
            sqlite
            python3
            (diesel-cli.override { postgresqlSupport = false; sqliteSupport = true; })
          ];

          shellHook = ''
            export RUST_SRC_PATH="${rust-toolchain}/lib/rustlib/src/rust/library"
            export RUST_BACKTRACE=1
          '';
        };

        packages.default = craneLib.buildPackage {
          pname = "lnaddrd";
          version = "0.1.0";
          src = pkgs.lib.fileset.toSource {
            root = ./.;
            fileset = pkgs.lib.fileset.unions [
              (craneLib.fileset.commonCargoSources ./.)
              ./migrations
              ./assets
            ];
          };
          cargoLock = ./Cargo.lock;
          nativeBuildInputs = [ pkgs.pkg-config ];
          buildInputs = [];
        };

        packages.postgres-import = craneLib.buildPackage {
          pname = "lnaddrd-postgres-import";
          version = "0.1.0";
          src = pkgs.lib.fileset.toSource {
            root = ./.;
            fileset = pkgs.lib.fileset.unions [
              (craneLib.fileset.commonCargoSources ./.)
              ./migrations
              ./assets
            ];
          };
          cargoLock = ./Cargo.lock;
          cargoExtraArgs = "--features postgres-import";
          nativeBuildInputs = [ pkgs.pkg-config ];
          buildInputs = [];
        };

        nixosModules.lnaddrd = { config, lib, pkgs, system, ... }: with lib; {
          options.services.lnaddrd = {
            enable = mkOption {
              type = types.bool;
              default = false;
              description = "Whether to enable the lnaddrd service.";
            };
            domains = mkOption {
              type = with types; listOf str;
              default = [];
              description = ''
                One or more domain names to serve. Specify multiple times for multiple domains.
              '';
            };
            bind = mkOption {
              type = types.str;
              default = "127.0.0.1:8080";
              description = "The address to bind the server to.";
            };
            databasePath = mkOption {
              type = types.str;
              default = "/var/lib/lnaddrd/lnaddrd.sqlite3";
              description = "Path to the SQLite database.";
            };
            nostrRelays = mkOption {
              type = with types; listOf str;
              default = [];
              description = "Nostr relays used for encrypted state backup.";
            };
            publicBaseUrl = mkOption {
              type = types.nullOr types.str;
              default = null;
              description = "Canonical public HTTPS origin used for service announcements.";
            };
            serviceName = mkOption {
              type = types.str;
              default = "lnaddrd";
              description = "Human-readable service announcement name.";
            };
            package = mkOption {
              type = types.nullOr types.package;
              default = self.packages.${system}.default;
              description = "The package to use for the lnaddrd service. Defaults to the flake's default package if available.";
            };
            warning = mkOption {
              type = types.nullOr types.str;
              default = null;
              description = "Warning displayed on registration page.";
            };
          };

          config = mkIf config.services.lnaddrd.enable {
            users.groups.lnaddrd = {};
            users.users.lnaddrd = {
              isSystemUser = true;
              group = "lnaddrd";
              home = "/var/empty";
              shell = "/run/current-system/sw/bin/nologin";
              description = "User for lnaddrd service";
            };

            systemd.services.lnaddrd = {
              description = "lnaddrd Lightning Address Service";
              after = [ "network.target" ];
              wantedBy = [ "multi-user.target" ];
              environment = {
                LNADDRD_DOMAINS = concatStringsSep "," config.services.lnaddrd.domains;
                LNADDRD_BIND = config.services.lnaddrd.bind;
                LNADDRD_DATABASE_PATH = config.services.lnaddrd.databasePath;
                LNADDRD_ROOT_SECRET_FILE = "/var/lib/lnaddrd/root-secret";
                LNADDRD_ADMIN_PASSWORD_FILE = "/var/lib/lnaddrd/admin-password";
                LNADDRD_NOSTR_RELAYS = concatStringsSep "," config.services.lnaddrd.nostrRelays;
                LNADDRD_SERVICE_NAME = config.services.lnaddrd.serviceName;
              } // optionalAttrs (config.services.lnaddrd.publicBaseUrl != null) {
                LNADDRD_PUBLIC_BASE_URL = config.services.lnaddrd.publicBaseUrl;
              } // optionalAttrs (config.services.lnaddrd.warning != null) {
                LNADDRD_WARNING = config.services.lnaddrd.warning;
              };
              serviceConfig = {
                ExecStart = "${config.services.lnaddrd.package}/bin/lnaddrd";
                Restart = "on-failure";
                User = "lnaddrd";
                Group = "lnaddrd";
                StateDirectory = "lnaddrd";
                StateDirectoryMode = "0700";
              };
            };
          };
        };
      }
    );
}
