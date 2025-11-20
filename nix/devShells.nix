{
  perSystem = { config, pkgs, ... }: {
    devShells.default = with pkgs; mkShell {
      packages = [
        cargo
        cmake
        rust-bin.nightly.latest.default
        pkg-config
        openssl
        zlib
        rust-analyzer
        rustfmt
        clippy
      ];
    };
  };
}
