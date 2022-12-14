{ pkgs ? import <nixpkgs> { } }:

let

  # for each child shell
  t1 = import ./inix/template/shell.nix { };
  t2 = import ./inix/othertemplate/shell.nix { };

in
pkgs.mkShell {
  buildInputs =
    t1.buildInputs
    ++ t2.buildInputs
    ++ [
      pkgs.hello
    ];
}
