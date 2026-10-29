{ lib, rustPlatform }:

rustPlatform.buildRustPackage {
  pname = "den";
  version = "0.1.0";

  src = lib.fileset.toSource {
    root = ../.;
    fileset = lib.fileset.unions [
      ../Cargo.toml
      ../Cargo.lock
      ../src
      ../tests
    ];
  };
  cargoLock.lockFile = ../Cargo.lock;
  strictDeps = true;

  meta = {
    description = "A lightweight Linux battery recorder and terminal viewer";
    mainProgram = "den";
    platforms = lib.platforms.linux;
  };
}
