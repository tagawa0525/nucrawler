{
  lib,
  rustPlatform,
  cacert,
  pkg-config,
  aws-lc,
  sqlite,
}:

rustPlatform.buildRustPackage {
  pname = "nucrawler";
  version = "0.1.0";

  src = lib.cleanSource ./..;

  cargoLock.lockFile = ../Cargo.lock;

  # nixpkgs の cargo-setup-hook は AWS_LC_SYS_USE_SYSTEM=1 と LIBSQLITE3_SYS_USE_PKG_CONFIG=1 を設定し、
  # aws-lc-sys（rustls 経由）と libsqlite3-sys に system のライブラリを使わせる。見つけられるよう、ここで渡す
  nativeBuildInputs = [ pkg-config ];
  buildInputs = [
    aws-lc
    sqlite
  ];

  # reqwest はクライアントを作るときにシステムの CA 証明書を読むので、サンドボックスの
  # テストにも渡す（テストは外部に接続しない）
  nativeCheckInputs = [ cacert ];

  # 設定の例（home-manager モジュールの sources.toml の既定値にも使う）
  postInstall = ''
    install -Dm644 -t $out/share/nucrawler examples/*.toml
  '';

  meta = with lib; {
    description = "Crawl nuclear (LWR) news, summarize and translate into Japanese, and recommend";
    homepage = "https://github.com/tagawa0525/nucrawler";
    platforms = platforms.linux;
    mainProgram = "nucrawler";
  };
}
