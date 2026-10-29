{ lib, rustPlatform }:

rustPlatform.buildRustPackage {
  pname = "den";
  version = "0.1.0";

  src = lib.fileset.toSource {
    root = ../.;
    fileset = lib.fileset.unions [
      ../Cargo.toml
      ../Cargo.lock
      ../README.md
      ../LICENSE
      ../src
      ../tests
    ];
  };
  cargoLock.lockFile = ../Cargo.lock;
  strictDeps = true;

  meta = {
    description = "A lightweight Linux battery recorder and terminal viewer";
    homepage = "https://github.com/yawarakatai/den";
    license = lib.licenses.mit;
    mainProgram = "den";
    platforms = lib.platforms.linux;
  };
}
