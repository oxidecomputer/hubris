{ pkgs ? import <nixpkgs> { } }:

pkgs.mkShell {
  nativeBuildInputs = with pkgs; [
    pkg-config
  ];
  buildInputs = with pkgs; [
    udev
    claude-code
  ];

  PKG_CONFIG_PATH = "${pkgs.udev.dev}/lib/pkgconfig";
}
