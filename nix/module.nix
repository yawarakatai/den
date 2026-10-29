{ packageFor }:
{
  config,
  lib,
  pkgs,
  ...
}:

let
  cfg = config.services.den;
in
{
  options.services.den = {
    enable = lib.mkEnableOption "the den battery history recorder";

    package = lib.mkOption {
      type = lib.types.package;
      default = packageFor pkgs.stdenv.hostPlatform.system;
      defaultText = lib.literalExpression "inputs.den.packages.\${pkgs.stdenv.hostPlatform.system}.den";
      description = "The den package to use for both the recorder and terminal viewer.";
    };

    sampleInterval = lib.mkOption {
      type = lib.types.ints.between 1 86400;
      default = 30;
      description = "Measurement interval in seconds.";
    };

    retentionDays = lib.mkOption {
      type = lib.types.ints.between 1 36500;
      default = 90;
      description = "Number of days of raw measurements to retain.";
    };

    readers = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = [ ];
      example = [ "alice" ];
      description = ''
        Users allowed to read the machine-wide battery history via the den group.
        A new login session is needed after changing group membership.
        History may reveal the machine's usage patterns.
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    environment.systemPackages = [ cfg.package ];
    users.groups.den.members = cfg.readers;
    users.users.den = {
      isSystemUser = true;
      group = "den";
      description = "Battery history recorder";
    };

    systemd.services.den = {
      description = "Battery history recorder";
      wantedBy = [ "multi-user.target" ];
      after = [ "local-fs.target" ];
      serviceConfig = {
        Type = "simple";
        User = "den";
        Group = "den";
        ExecStart = lib.escapeShellArgs [
          "${cfg.package}/bin/den"
          "--database"
          "/var/lib/den/history.sqlite3"
          "daemon"
          "--interval"
          (toString cfg.sampleInterval)
          "--retention-days"
          (toString cfg.retentionDays)
        ];
        Restart = "on-failure";
        RestartSec = "5s";
        StateDirectory = "den";
        StateDirectoryMode = "0750";
        UMask = "0027";

        NoNewPrivileges = true;
        ProtectSystem = "strict";
        ProtectHome = true;
        PrivateTmp = true;
        PrivateDevices = true;
        ProtectKernelTunables = true;
        ProtectKernelModules = true;
        ProtectKernelLogs = true;
        ProtectControlGroups = true;
        ProtectClock = true;
        RestrictSUIDSGID = true;
        RestrictRealtime = true;
        RestrictNamespaces = true;
        LockPersonality = true;
        MemoryDenyWriteExecute = true;
        CapabilityBoundingSet = "";
        RestrictAddressFamilies = [ "AF_UNIX" ];
        IPAddressDeny = "any";
        SystemCallArchitectures = "native";
      };
    };
  };
}
