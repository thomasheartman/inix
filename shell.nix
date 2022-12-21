{ pkgs ? import <nixpkgs> {
    overlays = [
      (import (builtins.fetchTarball
        "https://github.com/mozilla/nixpkgs-mozilla/archive/master.tar.gz"))
    ];
  }
}:
with pkgs;
let

  rust = pkgs.latest.rustChannels.stable.rust.override {
    extensions = [
      "rust-src"
    ];
  };

  os = builtins.currentSystem;
  platformSpecificInputs = if lib.hasInfix "darwin" os then [ ] else [ cargo-watch ];


in

pkgs.mkShell {
  buildInputs =
    platformSpecificInputs ++
    [
      rust

      taplo-cli # add taplo for LSP support for toml files

      # keep this line if you use bash
      pkgs.bashInteractive
    ];
}
