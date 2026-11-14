{
  description = "asterofix - launch Steam Fix / Online Fix games through Proton with the Steam overlay";

  # Prebuilt by CI (.github/workflows/nix.yml). Nix only offers it to `nix run` and the like on
  # this flake; a system that takes asterofix as an input adds the cache itself (README).
  nixConfig = {
    extra-substituters = [ "https://asterofix.cachix.org" ];
    extra-trusted-public-keys = [ "asterofix.cachix.org-1:eSwsKEg8DvMbePlRof4JgPYVxjrv+sBbG11Evomy008=" ];
  };

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    {
      self,
      nixpkgs,
      rust-overlay,
    }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      forAll =
        f:
        nixpkgs.lib.genAttrs systems (
          s:
          f (
            import nixpkgs {
              system = s;
              overlays = [ rust-overlay.overlays.default ];
            }
          )
        );
      # Rune pins this toolchain (its rust-toolchain.toml). The package builds with its minimal
      # profile; this one, with sources and tools, is the dev shell's.
      toolchain =
        pkgs:
        pkgs.rust-bin.stable."1.99.1".default.override {
          extensions = [
            "rust-src"
            "clippy"
            "rustfmt"
          ];
        };
      # Linked at build time (xkbcommon for layer-shell, the Wayland client).
      buildLibs =
        pkgs: with pkgs; [
          libxkbcommon
          wayland
        ];
      # dlopen()ed at run time by winit and wgpu.
      runtimeLibs =
        pkgs: with pkgs; [
          libGL
          libxkbcommon
          vulkan-loader
          wayland
          libx11
          libxcursor
          libxi
          libxrandr
        ];
    in
    {
      packages = forAll (
        pkgs:
        let
          # Without rust-src: with it, rustc writes its real path into std's panic locations, and
          # the whole toolchain (1.6 GB) became a runtime dependency.
          rust = pkgs.rust-bin.stable."1.99.1".minimal;
          platform = pkgs.makeRustPlatform {
            cargo = rust;
            rustc = rust;
          };
        in
        {
          default = platform.buildRustPackage {
            pname = "asterofix";
            version = "0.2.1";
            src = pkgs.lib.fileset.toSource {
              root = ./.;
              fileset = pkgs.lib.fileset.unions [
                ./Cargo.toml
                ./Cargo.lock
                ./src
                ./fonts
                ./packaging
              ];
            };
            cargoLock = {
              lockFile = ./Cargo.lock;
              # Rune's commit, fetched alone. Without a hash nixpkgs falls back to
              # builtins.fetchGit with allRefs, which clones every branch and pull request
              # (160 MB). Update it with the rev in Cargo.toml.
              outputHashes."rune-0.0.1" = "sha256-sdskmmer1AdrlLLKN4ga7D/W02D01lztof5UloWCwUY=";
            };
            # Tests need a writable HOME and some spawn processes; `nix develop -c cargo test` runs
            # them, and CI does in .github/workflows/test.yml.
            doCheck = false;
            nativeBuildInputs = [
              pkgs.pkg-config
              pkgs.makeWrapper
            ];
            buildInputs = buildLibs pkgs;
            disallowedReferences = [ rust ];
            # The app menu entry and its icon, as the AppImage carries them.
            postInstall = ''
              install -Dm644 packaging/asterofix.desktop -t $out/share/applications
              install -Dm644 packaging/asterofix.svg -t $out/share/icons/hicolor/scalable/apps
            '';
            postFixup = ''
              wrapProgram $out/bin/asterofix \
                --prefix LD_LIBRARY_PATH : ${pkgs.lib.makeLibraryPath (runtimeLibs pkgs)}
            '';
            meta.mainProgram = "asterofix";
          };
        }
      );

      # The package shows up in app menus: a valid desktop entry whose command and icon it ships.
      checks = forAll (pkgs: {
        desktop-entry =
          let
            app = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
          in
          pkgs.runCommand "asterofix-desktop-entry" { nativeBuildInputs = [ pkgs.desktop-file-utils ]; } ''
            entry=${app}/share/applications/asterofix.desktop
            desktop-file-validate "$entry"
            grep -qx 'Exec=asterofix' "$entry"
            test -x ${app}/bin/asterofix
            grep -qx 'Icon=asterofix' "$entry"
            test -s ${app}/share/icons/hicolor/scalable/apps/asterofix.svg
            touch $out
          '';
      });

      devShells = forAll (pkgs: {
        default = pkgs.mkShell {
          packages = [
            (toolchain pkgs)
            pkgs.pkg-config
            pkgs.mold
          ]
          ++ buildLibs pkgs;
          LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath (runtimeLibs pkgs);
          # mold links the (large) binary several times faster than ld.
          CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTFLAGS = "-C link-arg=-fuse-ld=mold";
          # Interactive `nix develop` drops into fish; `nix develop -c ...` is untouched.
          shellHook = ''
            if [[ $- == *i* && -z "$ASTEROFIX_IN_FISH" ]] && command -v fish >/dev/null; then
              export ASTEROFIX_IN_FISH=1
              exec fish
            fi
          '';
        };
      });
    };
}
