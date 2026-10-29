# Initial tools: Rust
# Package source: github:NixOS/nixpkgs/nixos-unstable (pinned in flake.lock)
# Find package names: https://search.nixos.org/packages?channel=unstable

{
  systems = [ "x86_64-linux" ];

  packages = [
    "cargo"
    "clippy"
    "rust-analyzer"
    "rustc"
    "rustfmt"
  ];
}
