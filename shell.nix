{ pkgs ? import <nixpkgs> {
  overlays = [
    (import (builtins.fetchTarball
      "https://github.com/mozilla/nixpkgs-mozilla/archive/master.tar.gz"))
  ];
} }:
with pkgs;
let

  rust = pkgs.latest.rustChannels.stable.rust.override {
    extensions = [
      "rust-src"
    ];
  };

  in

pkgs.mkShell {
  buildInputs = [
    cargo-watch
    rust

    # keep this line if you use bash
    pkgs.bashInteractive
  ];
}
