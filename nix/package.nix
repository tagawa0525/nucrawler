{
  lib,
  rustPlatform,
  cacert,
}:

rustPlatform.buildRustPackage {
  pname = "nucrawler";
  version = "0.1.0";

  src = lib.cleanSource ./..;

  cargoLock.lockFile = ../Cargo.lock;

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
