{
  config,
  lib,
  pkgs,
  ...
}:

let
  cfg = config.services.nucrawler;
  toml = pkgs.formats.toml { };
  bin = lib.getExe cfg.package;

  # claude は利用者のプロファイルに入っているものを使う（サブスクリプションの認証は ~/.claude）
  path = lib.concatStringsSep ":" (
    map (p: "${p}/bin") cfg.extraPackages
    ++ [
      "/etc/profiles/per-user/${config.home.username}/bin"
      "${config.home.homeDirectory}/.nix-profile/bin"
      "/run/current-system/sw/bin"
    ]
  );

  crawlService = description: args: {
    Unit = {
      Description = description;
      # 設定の反映（home-manager switch）で止めたり起動したりしない。oneshot なので起動すると
      # 巡回が終わるまで反映が待たされ（LLM を使うと数十分）、実行中の巡回も中断される。
      # 新しい定義は次に timer で起動したときから使われる
      X-SwitchMethod = "keep-old";
    };
    Service = {
      Type = "oneshot";
      ExecStart = lib.concatStringsSep " " (
        [
          bin
          "crawl"
          # 停止明けに Persistent の timer が同時に動いても、見送らずに順に実行する
          "--wait-lock"
        ]
        ++ args
      );
      Environment = [ "PATH=${path}" ];
    };
  };

  timer = description: onCalendar: {
    Unit.Description = description;
    Timer = {
      OnCalendar = onCalendar;
      # 止まっていた間の実行を、起動後に取り戻す
      Persistent = true;
    };
    Install.WantedBy = [ "timers.target" ];
  };
in
{
  options.services.nucrawler = {
    enable = lib.mkEnableOption "nucrawler (crawl timers and the web ui)";

    package = lib.mkOption {
      type = lib.types.package;
      description = "The nucrawler package to use.";
    };

    settings = lib.mkOption {
      inherit (toml) type;
      default = { };
      example = lib.literalExpression ''
        {
          web.bind = "100.64.0.1:8080";
          llm.translate_min_score = 85;
        }
      '';
      description = ''
        config.toml の内容。省略した項目は既定値になる（examples/config.toml を参照）。
      '';
    };

    sourcesFile = lib.mkOption {
      type = lib.types.path;
      default = "${cfg.package}/share/nucrawler/sources.toml";
      defaultText = lib.literalExpression ''"''${cfg.package}/share/nucrawler/sources.toml"'';
      description = "巡回するソースの一覧（sources.toml）。";
    };

    extraPackages = lib.mkOption {
      type = lib.types.listOf lib.types.package;
      default = [ ];
      example = lib.literalExpression "[ pkgs.claude-code ]";
      description = ''
        crawl の PATH に加えるパッケージ。claude を利用者のプロファイルに入れていないときに指定する。
      '';
    };

    serve.enable = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = "Web UI（nucrawler serve）を常駐させる。";
    };

    schedule = {
      crawl = lib.mkOption {
        type = lib.types.listOf lib.types.str;
        default = [
          "*-*-* 03:00:00"
          "*-*-* 10:00:00"
          "*-*-* 16:00:00"
        ];
        description = ''
          取得から要約・採点・和訳までを実行する時刻（systemd の OnCalendar）。
          LLM を使う量は config.toml の [quota] の時間帯ごとの上限で決まる。
        '';
      };

      fetch = lib.mkOption {
        type = lib.types.listOf lib.types.str;
        default = [ "*-*-* 22:00:00" ];
        description = "LLM を使わず、取得と本文の抽出だけを実行する時刻。";
      };

      requests = lib.mkOption {
        type = lib.types.str;
        default = "*:0/15";
        description = "Web UI から依頼された和訳を処理する間隔。";
      };
    };
  };

  config = lib.mkIf cfg.enable {
    home.packages = [ cfg.package ];

    xdg.configFile = {
      "nucrawler/config.toml".source = toml.generate "nucrawler-config.toml" cfg.settings;
      "nucrawler/sources.toml".source = cfg.sourcesFile;
    };

    systemd.user.services = {
      nucrawler-crawl = crawlService "nucrawler: fetch, extract, digest, score and translate" [ ];
      nucrawler-fetch = crawlService "nucrawler: fetch and extract only" [
        "--until"
        "extract"
      ];
      nucrawler-requests = crawlService "nucrawler: translate requested articles" [ "--requests-only" ];
    }
    // lib.optionalAttrs cfg.serve.enable {
      nucrawler-serve = {
        Unit.Description = "nucrawler web ui";
        Service = {
          ExecStart = "${bin} serve";
          # Tailscale の IP で待ち受けるとき、起動直後はまだアドレスが無いことがある
          Restart = "on-failure";
          RestartSec = 10;
        };
        Install.WantedBy = [ "default.target" ];
      };
    };

    systemd.user.timers = {
      nucrawler-crawl = timer "nucrawler: full crawl" cfg.schedule.crawl;
      nucrawler-fetch = timer "nucrawler: fetch only" cfg.schedule.fetch;
      nucrawler-requests = timer "nucrawler: requested translations" cfg.schedule.requests;
    };
  };
}
