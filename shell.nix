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

  systemNameContains = (lib.flip lib.hasInfix) builtins.currentSystem;
  platformSpecificInputs =
    if systemNameContains "darwin" then [
      libiconv # seems macOS needs this as an extra dependency for some reason.
    ] else [ cargo-watch ];


in

pkgs.mkShell {
  buildInputs =
    platformSpecificInputs ++
    [
      rust

      taplo-cli # add taplo for LSP support for toml files

      # keep this line if you use bash
      pkgs.bashInteractive

      (pkgs.writeShellApplication
        {
          name = "run";
          text = ''cargo run -- "$@"'';
        })
    ];
}
