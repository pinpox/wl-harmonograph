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

          pythonEnv = pkgs.python3.withPackages (ps: [
            ps.pygobject3
            ps.pycairo
          ]);

          runtimeDeps = with pkgs; [
            gtk3
            gtk-layer-shell
            gobject-introspection
          ];
        in {
          default = pkgs.stdenv.mkDerivation {
            pname = "wl-harmonograph";
            inherit version;

            src = ./.;

            nativeBuildInputs = [ pkgs.wrapGAppsHook3 pkgs.gobject-introspection pkgs.makeWrapper ];

            buildInputs = [ pythonEnv ] ++ runtimeDeps;

            dontBuild = true;

            installPhase = ''
              mkdir -p $out/bin $out/share/wl-harmonograph
              cp wl-harmonograph.py $out/share/wl-harmonograph/wl-harmonograph.py

              makeWrapper ${pythonEnv}/bin/python3 $out/bin/wl-harmonograph \
                --add-flags "$out/share/wl-harmonograph/wl-harmonograph.py"
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
