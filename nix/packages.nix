{ inputs, ... }: {
  perSystem = { inputs', system, config, lib, pkgs, ... }: {
    packages = let
      naerskBuildPackageArgs = rec {
        pname = "amaru-debug-tools";

        strictDeps = true;

        src = with lib.fileset; toSource {
          root = ./..;
          fileset = unions [
            ../Cargo.lock
            ../Cargo.toml
            ../src
            ../tests
          ];
        };

        buildInputs = with pkgs; [
          pkg-config
          openssl
          zlib
        ];

        nativeBuildInputs = with pkgs; [
          cmake # needed by tests in randomx-rs build script
        ];

        doCheck = true;

        meta = {
          mainProgram = pname;
          maintainers = with lib.maintainers; [
            disassembler
            dermetfan
          ];
          license = with lib.licenses; [
            asl20
            mit
          ];
        };
      };
    in {
      amaru-debug-tools = inputs.naersk.lib.${system}.buildPackage naerskBuildPackageArgs;

      amaru-debug-tools-x86_64-pc-windows-gnu = (let
        toolchain = with inputs'.fenix.packages;
          combine [
            minimal.rustc
            minimal.cargo
            targets.x86_64-pc-windows-gnu.latest.rust-std
          ];
      in inputs.naersk.lib.${system}.override {
        cargo = toolchain;
        rustc = toolchain;
      }).buildPackage (naerskBuildPackageArgs // {
        depsBuildBuild = with pkgs.pkgsCross.mingwW64; naerskBuildPackageArgs.depsBuildBuild or [] ++ [
          stdenv.cc
          windows.pthreads
        ];

        doCheck = false;

        CARGO_BUILD_TARGET = "x86_64-pc-windows-gnu";
      });

      default = config.packages.amaru-debug-tools;
    };
  };
}
