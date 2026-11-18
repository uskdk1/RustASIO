{
  description = "RustASIO - Wine ASIO driver backed by PipeWire";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, nixpkgs, rust-overlay }:
    let
      system = "x86_64-linux";
      pkgs = import nixpkgs {
        inherit system;
        overlays = [ rust-overlay.overlays.default ];
      };
      mingw = pkgs.pkgsCross.mingwW64;
      rust = pkgs.rust-bin.stable.latest.default.override {
        extensions = [ "rust-src" "rust-analyzer" "clippy" ];
        targets = [ "x86_64-unknown-linux-gnu" "x86_64-pc-windows-gnu" ];
      };
    in
    {
      devShells.${system}.default = pkgs.mkShell {
        nativeBuildInputs = [
          rust
          mingw.stdenv.cc
          pkgs.pkg-config
          pkgs.clang
          pkgs.patchelf
          pkgs.binutils
        ];
        buildInputs = [ pkgs.pipewire ];

        LIBCLANG_PATH = "${pkgs.libclang.lib}/lib";
        CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER = "${mingw.stdenv.cc}/bin/x86_64-w64-mingw32-gcc";
        CARGO_TARGET_X86_64_PC_WINDOWS_GNU_RUSTFLAGS =
          "-L native=${mingw.windows.pthreads}/lib";
      };
    };
}
