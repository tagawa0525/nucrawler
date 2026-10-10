# embedding のサーバーを常駐させず、使う間だけ動かす

この計画は、何をなぜ決めたかを残す。競合や端のケースの扱いは、テストとともに決める。

## 前提

r995 の計算資源は、xlc や OpenMC などの計算に使う。補助のサーバーに常時の資源を割かない。

embedding のサーバー（`embeddingServer.enable`、text-embeddings-inference の CPU 版）は、次のとおり使う時間に対して持つ
資源が大きい（r995 で 2026-10-10 に実測）。

- 起動から 2 日 20 時間で CPU 時間は合計 40 分 44 秒（平均 1% 弱）。メモリは 4.2 GB で、この機械で最大のプロセスだった
- 直近の crawl は全体で実時間 55 秒・CPU 時間 2 秒。新着が 0〜9 件のとき、embed ステージは 20 ms ほどで終わる
- 実際に動くのは 1 日に数秒で、残りは 4 GB を持ったまま待機していた

xlc の計算が、サーバーの有無で遅くなるかは測っていない。使う時間に対して持つ資源が大きいことを理由にし、
「影響は小さい」とは言わない。

embedding はクラウドに出すこともありうる。`[embedding]` は OpenAI 互換の API ならどれでも受けるので（計画 010）、
url・model・認証を替えて、`embeddingServer.enable` を外すだけで移れる。クラウドに出せば、ローカルのサーバーの
起動・停止の仕組みが丸ごと要らなくなる。この計画の決定は、ローカルで動かす間のものであり、クラウドに出す決定が
あれば置き換わる。そのため、仕組みは `nix/home-manager.nix` に閉じ、Rust には足さない。

## 呼ぶ場面

embedding を呼ぶのは次の場面である。

- `crawl` の embed ステージ（全体の crawl）
- `crawl` と `crawl --requests-only` の review ステージ。`--requests-only` は、プロファイルの見直しを頼まれている
  ときだけ呼ぶ（`review_profiles` の `manual`）。15 分ごとの実行のほとんどは呼ばない
- 手で実行する `eval --profile` と `crawl --only embed`

`embed rebuild` は保存済みの embedding を消すだけで、呼ばない（作り直しは次の crawl）。Web UI と
`crawl --until extract` も呼ばない。

## 決めたこと

systemd の参照カウントで動かす。サーバーの unit（`nucrawler-embedding.service`）は `StopWhenUnneeded=true` にして、
使う unit が全部終わると止める。ログイン時には起動しない（`WantedBy` を外す）。

- **全体の crawl**：`Wants/After` でサーバーを起動し、応答を待つ（`ExecStartPre`）。サーバーが起動できなくても
  crawl は続け、embed の失敗として報告する。終わると、ほかに使う unit が無ければ systemd が止める
- **requests**：実行の前に、DB の `profile_review_requests` が空かを見る（`startEmbeddingForRequests`）。空でなければ
  `nucrawler-embedding-hold-requests.service` を start してサーバーを起動し、終わりに `--no-block` で stop する。
  読めないときは、頼まれているものとして起動する（起動し損ねて頼みが残り続けるより、無駄に起動するほうがよい）
- **手動**：`nucrawler-embedding-hold.service` を `start` する（応答するまで待つ）。使い終わったら `stop` する。
  保持の unit を使う側ごとに分けるので、ある側が終わっても、ほかの側が使っているサーバーは止まらない

## 検討した案

どれも、transient unit で挙動を確かめた（2026-10-10）。

- **終わりに `ExecStopPost` でサーバーを無条件に止める**：重なって使う側（手動、requests）のサーバーを止める。
  また、crawl を停止ジョブで止める（`systemctl stop`、シャットダウン）と、`After=` により embedding の停止が
  crawl の停止完了の後に並び、同期の `stop` は互いを待って `TimeoutStopSec` まで詰まる（同期は 8 秒の上限
  ちょうど、`--no-block` は 0.02 秒）。採らない
- **`StopWhenUnneeded` だけにする**：手で単独に `start` したサーバーは、使う unit が無いのですぐ止まる。
  手動で使えないので、保持用の unit を足した
- **requests も毎回サーバーを起動する**：requests は 15 分ごとで、1 日に 96 回モデルを読み込む。常駐より CPU の
  使用が増え、時間測定への干渉も増える。採らない
- **見直し依頼の有無を返す CLI を Rust に足す**：クラウドに出すと不要になる機能が Rust に残る。DB の表を直接見る
  Nix のスクリプトなら、仕組みごと消せる。採らない（表名の変更に追従できるよう、読めないときは起動する側に倒した）
- **crawl 全体を Slurm（`sbatch --exclusive`）で流す**：crawl には数十分かかる LLM の段階があり、その間ずっと
  ノードを占有して他の計算を止める。採らない
- **embedding の段階だけを Slurm のジョブに切り出す**：review が story の前に動くなど段階の順序が変わる。
  待ち行列に並ぶので、先行するジョブの後ろで点数が付くまで数時間遅れうる。定常の crawl の embedding は数秒で、
  「全コアを使う、または 1 分以上」の基準に当たらないので割に合わない。採らない
- **常駐のまま `Nice=`・`CPUQuota=` で抑える**：待機中の 4 GB は減らない。採らない

## 未確認

- 毎回のモデルの読み込みにかかる時間。モジュールのコメントは「数分」とするが、`~/.cache/huggingface` に
  キャッシュがあるときの実測値は無い。待つ上限は従来どおり 900 秒
- xlc の計算が、サーバーの有無で遅くなるか
- 実機での通し（timer での crawl・requests、手動の保持）。rebuild の後に確かめる
- 大量の embedding（`embed rebuild` の次の crawl の embed ステージ）は、重い計算の基準に当たりうる。そのときは
  `sbatch --exclusive --wait` の中で、保持用の unit を start してから流す運用とし、仕組みは作らない
