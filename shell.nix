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

  osSpecific =
    if systemNameContains "darwin" then { packages = [ ]; buildInputs = [ libiconv ]; }
    else { packages = [ cargo-watch ]; buildInputs = [ ]; };


  packages = osSpecific.packages ++
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

  inputsFrom = osSpecific.buildInputs;


in

pkgs.mkShell {
  inherit inputsFrom packages;
}
