{
  description = "Animated harmonograph wallpaper for Sway/Wayland";

  inputs.nixpkgs.url = "nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      lastModifiedDate = self.lastModifiedDate or self.lastModified or "19700101";
      version = builtins.substring 0 8 lastModifiedDate;
      forAllSystems = nixpkgs.lib.genAttrs nixpkgs.lib.systems.flakeExposed;
      nixpkgsFor = forAllSystems (system: import nixpkgs { inherit system; });
    in {
      packages = forAllSystems (system:
        let
          pkgs = nixpkgsFor.${system};
        in {
          default = pkgs.rustPlatform.buildRustPackage {
            pname = "wl-harmonograph";
            inherit version;

            src = ./.;

            cargoHash = "sha256-jvCq3NQBIoK0ZctDeTeFi9eXIi7mbZYDF6RoiWCk7JY=";

            nativeBuildInputs = with pkgs; [
              pkg-config
            ];

            buildInputs = with pkgs; [
              wayland
              libGL
              libglvnd
            ];

            # khronos-egl with "static" feature links EGL statically,
            # but still needs the GL driver at runtime.
            postFixup = ''
              patchelf --add-rpath ${pkgs.lib.makeLibraryPath [
                pkgs.libglvnd
                pkgs.mesa
              ]} $out/bin/wl-harmonograph
            '';

            meta = with pkgs.lib; {
              description = "Animated harmonograph wallpaper for Sway/Wayland";
              license = licenses.mit;
              platforms = platforms.linux;
              mainProgram = "wl-harmonograph";
            };
          };
        });
    };
}
