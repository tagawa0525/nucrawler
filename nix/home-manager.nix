{
  config,
  lib,
  pkgs,
  ...
}:

let
  cfg = config.services.nucrawler;
  emb = cfg.embeddingServer;

  # crawl の前に、embedding の API が応答するまで待つ（モデルの読み込みに数分、初回は取得も加わる）。
  # embedding の unit 自身の起動処理（ExecStartPost）では待たない。home-manager の切り替え（sd-switch）は
  # unit の起動完了を 120 秒しか待たず、それを超えると切り替え全体が失敗するため。
  # unit が止まっている（失敗した）ときは待たずに諦め、crawl は embed の失敗として報告して続ける
  waitForEmbedding = pkgs.writeShellScript "nucrawler-wait-embedding" ''
    for _ in $(seq 1 900); do
      ${lib.getExe pkgs.curl} --silent --fail --output /dev/null http://127.0.0.1:${toString emb.port}/health && exit 0
      case "$(${lib.getExe' pkgs.systemd "systemctl"} --user show --property=ActiveState --value nucrawler-embedding.service)" in
        active | activating | reloading) ;;
        *)
          echo "the embedding server is not running" >&2
          exit 1
          ;;
      esac
      sleep 1
    done
    echo "the embedding server did not become ready" >&2
    exit 1
  '';
  systemctl = "${lib.getExe' pkgs.systemd "systemctl"} --user";

  # nucrawler-requests は、プロファイルの見直しを頼まれているときだけ embedding を呼ぶ（15 分ごとの実行の
  # ほとんどは呼ばない）ので、頼まれているときだけサーバーを起動する。頼みの有無は DB の
  # profile_review_requests（src/db/profile_suggestions.rs）で見る。読めないときは頼まれているものとして
  # 起動する（起動し損ねて頼みが残り続けるより、無駄に起動するほうがよい）
  startEmbeddingForRequests = pkgs.writeShellScript "nucrawler-start-embedding-for-requests" ''
    db="''${XDG_DATA_HOME:-$HOME/.local/share}/nucrawler/nucrawler.db"
    pending=$(${lib.getExe pkgs.sqlite} -readonly "$db" 'SELECT count(*) FROM profile_review_requests') || pending=unknown
    [ "$pending" = 0 ] && exit 0
    ${systemctl} start nucrawler-embedding-hold@requests.service
  '';
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
      # 停止（再起動・シャットダウン）の SIGTERM は nucrawler にだけ送る。nucrawler が処理中の
      # claude を止めて、途中の呼び出しを失敗として記録せずに終わる（残りは次回）。
      # 既定の control-group だと claude も同時に殺され、要約の失敗として記録されていた
      KillMode = "mixed";
      # 止められて途中で終わった（130）のは失敗ではない
      SuccessExitStatus = 130;
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

    embeddingServer = {
      enable = lib.mkEnableOption ''
        記事と好みの embedding を作るローカルのサーバー（Hugging Face の text-embeddings-inference を
        Podman で動かす）。config.toml の [embedding] も、このサーバーを呼ぶよう既定で設定する
      '';

      image = lib.mkOption {
        type = lib.types.str;
        default = "ghcr.io/huggingface/text-embeddings-inference:cpu-1.8";
        description = "text-embeddings-inference のイメージ。";
      };

      model = lib.mkOption {
        type = lib.types.str;
        default = "cl-nagoya/ruri-v3-310m";
        description = ''
          Hugging Face のモデル。替えたら `nucrawler embed rebuild` で作り直す（替えたまま crawl すると embed が止まる）。
        '';
      };

      port = lib.mkOption {
        type = lib.types.port;
        default = 8090;
        description = "127.0.0.1 で待ち受けるポート。";
      };

      queryPrefix = lib.mkOption {
        type = lib.types.str;
        default = "検索クエリ: ";
        description = "好み（クエリ）の文の前に付ける接頭辞（ruri の作法）。";
      };

      documentPrefix = lib.mkOption {
        type = lib.types.str;
        default = "検索文書: ";
        description = "記事（文書）の文の前に付ける接頭辞（ruri の作法）。";
      };

      podman = lib.mkOption {
        type = lib.types.str;
        default = "/run/current-system/sw/bin/podman";
        description = "podman のパス（rootless で動かす）。";
      };
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

    services.nucrawler.settings = lib.mkIf emb.enable {
      embedding = lib.mapAttrs (_: lib.mkDefault) {
        url = "http://127.0.0.1:${toString emb.port}/v1/embeddings";
        inherit (emb) model;
        query_prefix = emb.queryPrefix;
        document_prefix = emb.documentPrefix;
      };
    };

    xdg.configFile = {
      "nucrawler/config.toml".source = toml.generate "nucrawler-config.toml" cfg.settings;
      "nucrawler/sources.toml".source = cfg.sourcesFile;
    };

    systemd.user.services = {
      # embed ステージを含む全体の crawl は、embedding のサーバーを起動して待つ。サーバーは、使う unit が
      # 全部終わると systemd が止める（常駐させない。計画 018）。
      # サーバーが起動できなくても crawl は続き、embed の失敗として報告する
      nucrawler-crawl =
        lib.recursiveUpdate (crawlService "nucrawler: fetch, extract, digest, score and translate" [ ])
          {
            Unit = lib.optionalAttrs emb.enable {
              Wants = [ "nucrawler-embedding.service" ];
              After = [ "nucrawler-embedding.service" ];
            };
            # 失敗しても（- を付けて）crawl は続ける
            Service = lib.optionalAttrs emb.enable { ExecStartPre = "-${waitForEmbedding}"; };
          };
      nucrawler-fetch = crawlService "nucrawler: fetch and extract only" [
        "--until"
        "extract"
      ];
      nucrawler-requests =
        lib.recursiveUpdate (crawlService "nucrawler: translate requested articles" [ "--requests-only" ])
          {
            Service = lib.optionalAttrs emb.enable {
              ExecStartPre = "-${startEmbeddingForRequests}";
              # 保持の unit は requests の後ろに並ばないので待っても循環しないが、サーバーの停止（podman stop）で
              # requests の終わりを遅らせない
              ExecStopPost = "-${systemctl} --no-block stop nucrawler-embedding-hold@requests.service";
            };
          };
    }
    // lib.optionalAttrs emb.enable {
      nucrawler-embedding = {
        Unit = {
          Description = "nucrawler embedding server (text-embeddings-inference)";
          # 使う unit（crawl、下の保持用）が全部終わると止まる。手で単独で start しても、使う unit が無いので
          # すぐ止まる。手で使うときは nucrawler-embedding-hold@<名前> を start する
          StopWhenUnneeded = true;
        };
        Service = {
          # モデルは ~/.cache/huggingface に置き、起動のたびに取得し直さない
          ExecStartPre = "-${emb.podman} rm -f nucrawler-embedding";
          ExecStart = lib.concatStringsSep " " [
            emb.podman
            "run --rm --name nucrawler-embedding"
            "-p 127.0.0.1:${toString emb.port}:80"
            "-v %h/.cache/huggingface:/data"
            emb.image
            "--model-id ${emb.model}"
          ];
          # 起動の直後（コンテナを作る前）に止められると stop の対象が無いので、失敗を無視する。
          # 使う unit が無い状態で home-manager の切り替えが起動したときに起こる
          ExecStop = "-${emb.podman} stop nucrawler-embedding";
          # rootless の podman は newuidmap（/run/wrappers/bin）を使う
          Environment = [ "PATH=/run/wrappers/bin:/run/current-system/sw/bin" ];
          Restart = "on-failure";
          RestartSec = 30;
        };
        # ログイン時には起動しない。起動は使う unit の Wants（nucrawler-crawl、保持用）から
      };
    }
    // lib.optionalAttrs emb.enable {
      # embedding のサーバーを使っている間だけ残る unit。使う側ごとに別のインスタンス名で start/stop する
      # （同じ名前を共有すると、start しても参照は増えず、先に終えた側の stop で、使っている側のサーバーが止まる）。
      # start は応答するまで待つので、そのあとに使える
      "nucrawler-embedding-hold@" = {
        Unit = {
          Description = "keep the embedding server running for %i";
          Wants = [ "nucrawler-embedding.service" ];
          After = [ "nucrawler-embedding.service" ];
        };
        Service = {
          Type = "oneshot";
          RemainAfterExit = true;
          ExecStart = waitForEmbedding;
          TimeoutStartSec = 960;
        };
      };
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
