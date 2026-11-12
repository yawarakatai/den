{
  lib,
  rustPlatform,
  pkg-config,
  sqlite,
}:

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

  # nixpkgs' cargoUseSystemLibs makes libsqlite3-sys link the system SQLite
  # even with rusqlite's `bundled` feature.
  nativeBuildInputs = [ pkg-config ];
  buildInputs = [ sqlite ];

  meta = {
    description = "A lightweight Linux battery recorder and terminal viewer";
    homepage = "https://github.com/yawarakatai/den";
    license = lib.licenses.mit;
    mainProgram = "den";
    platforms = lib.platforms.linux;
  };
}
