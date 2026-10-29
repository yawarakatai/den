{
  self,
  nixpkgs,
  system,
}:

let
  pkgs = nixpkgs.legacyPackages.${system};
  evaluate =
    enabled:
    (nixpkgs.lib.nixosSystem {
      inherit system;
      modules = [
        self.nixosModules.default
        {
          system.stateVersion = "26.05";
          services.den = {
            enable = enabled;
            readers = [ "alice" ];
            sampleInterval = 15;
            retentionDays = 7;
          };
          users.users.alice.isNormalUser = true;
        }
      ];
    }).config;
  enabled = evaluate true;
  disabled = evaluate false;
  service = enabled.systemd.services.den;
in
assert service.serviceConfig.User == "den";
assert service.serviceConfig.Group == "den";
assert service.serviceConfig.StateDirectoryMode == "0750";
assert enabled.users.groups.den.members == [ "alice" ];
assert builtins.elem "multi-user.target" service.wantedBy;
assert nixpkgs.lib.hasInfix "--interval 15 --retention-days 7" service.serviceConfig.ExecStart;
assert !(builtins.hasAttr "den" disabled.systemd.services);
pkgs.runCommand "den-module-evaluation" { } ''
  touch $out
''
