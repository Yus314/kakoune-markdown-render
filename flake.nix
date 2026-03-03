{
  description = "kakoune-markdown-render: WYSIWYG Markdown rendering for Kakoune";

  inputs.nixpkgs.url = "nixpkgs";

  outputs = { self, nixpkgs }:
    let
      forAllSystems = nixpkgs.lib.genAttrs [
        "x86_64-linux"
        "aarch64-linux"
        "x86_64-darwin"
        "aarch64-darwin"
      ];
      pkgsFor = system: nixpkgs.legacyPackages.${system};
    in
    {
      packages = forAllSystems (system:
        let pkgs = pkgsFor system; in {
          default = pkgs.rustPlatform.buildRustPackage {
            pname = "mkdr";
            version = "0.1.0";
            src = ./.;
            cargoLock.lockFile = ./Cargo.lock;
            meta = {
              description = "WYSIWYG Markdown rendering for Kakoune editor";
              mainProgram = "mkdr";
              license = pkgs.lib.licenses.mit;
            };
          };
        });

      devShells = forAllSystems (system:
        let pkgs = pkgsFor system; in {
          default = pkgs.mkShell {
            packages = with pkgs; [
              rustc
              cargo
              clippy
              rustfmt
              rust-analyzer
            ];
            RUST_BACKTRACE = "1";
          };
        });
    };
}
