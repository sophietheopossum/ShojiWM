{
  description = "ShojiWM, a TypeScript-configured Wayland compositor";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  };

  outputs =
    { self, nixpkgs }:
    let
      lib = nixpkgs.lib;
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      forAllSystems = lib.genAttrs systems;
      pkgsFor = system: import nixpkgs { inherit system; };
    in
    {
      packages = forAllSystems (
        system:
        let
          pkgs = pkgsFor system;
          libgbm = pkgs.libgbm or pkgs.mesa;
          xwayland = pkgs.xwayland or (pkgs.xorg.xwayland or null);
          xwaylandSatellite = pkgs.xwayland-satellite or null;
        in
        rec {
          shojiwm = pkgs.callPackage ./nix/package.nix {
            inherit libgbm xwayland xwaylandSatellite;
          };
          default = shojiwm;
        }
      );

      apps = forAllSystems (
        system:
        let
          package = self.packages.${system}.default;
        in
        {
          default = {
            type = "app";
            program = "${package}/bin/shoji_wm";
          };
          init-config = {
            type = "app";
            program = "${package}/bin/shojiwm-init-config";
          };
        }
      );

      devShells = forAllSystems (
        system:
        let
          pkgs = pkgsFor system;
          libgbm = pkgs.libgbm or pkgs.mesa;
          xwayland = pkgs.xwayland or (pkgs.xorg.xwayland or null);
          xwaylandSatellite = pkgs.xwayland-satellite or null;
        in
        {
          default = pkgs.mkShell {
            packages =
              with pkgs;
              [
                cargo
                rustc
                rustfmt
                clippy
                nodejs_22
                pkg-config
                wayland
                wayland-protocols
                libxkbcommon
                systemd
                libinput
                mesa
                libgbm
                pixman
                seatd
                pipewire
                libdrm
                dbus
              ]
              ++ lib.optional (xwayland != null) xwayland
              ++ lib.optional (xwaylandSatellite != null) xwaylandSatellite;

            SHOJI_XWAYLAND_SATELLITE_PATH = lib.optionalString (
              xwaylandSatellite != null
            ) "${xwaylandSatellite}/bin/xwayland-satellite";

            shellHook = ''
              echo "ShojiWM development shell"
              echo "Run: npm ci"
              echo "Then: cargo run --release -p shoji_wm -- --dev"
            '';
          };
        }
      );

      nixosModules.default = import ./nix/nixos-module.nix { inherit self; };
    };
}
