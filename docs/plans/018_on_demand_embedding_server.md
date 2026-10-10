# embedding のサーバーを常駐させず、crawl の間だけ動かす

この計画は、何をなぜ決めたかを残す。

## 背景

`embeddingServer.enable` のサーバー（text-embeddings-inference、CPU 版）は、ログイン時から常駐していた。
embedding を呼ぶのは `crawl` の embed・review ステージと、手で実行する `eval --profile`・`embed rebuild` だけで、
Web UI と `crawl --requests-only`・`--until extract` は呼ばない。

r995 での実測（2026-10-10）：起動から 2 日 20 時間で CPU 時間は合計 40 分 44 秒（平均 1% 弱）、メモリは 4.2 GB。
直近の crawl は全体で実時間 55 秒・CPU 時間 2 秒で、新着が 0〜9 件のとき embed ステージは 20 ms ほどで終わる。
つまり実際に動くのは 1 日に数秒で、残りは待機のまま 4 GB を持っていた。r995 は xlc などの計算も走らせるので、
計算の時間測定と重なる常駐プロセスは減らしたい。

## 決めたこと

- サーバーの unit（`nucrawler-embedding.service`）は `WantedBy` を外し、ログイン時に起動しない
- `nucrawler-crawl` は従来どおり `Wants/After` でサーバーを起動して応答を待ち、終わり（成否を問わない）に
  `ExecStopPost` で止める。サーバーが起動できなくても crawl は続け、embed の失敗として報告する
- crawl の外で使うとき（`eval --profile`、`embed rebuild` の後の全件の embedding）は、手で
  `systemctl --user start nucrawler-embedding` し、終わったら `stop` する

## 検討した案

- **crawl 全体を Slurm（`sbatch --exclusive`）で流す**：crawl には数十分かかる LLM の段階があり、その間ずっと
  ノードを占有して他の計算を止める。採らない
- **embedding の段階だけを Slurm のジョブに切り出す**（`crawl --until digest`、ジョブ内でサーバー起動と
  `--only embed`・`--only review`、残りの段階を個別に実行）：review が story の前に動くなど段階の順序が変わる。
  待ち行列に並ぶので、先行するジョブの後ろで点数が付くまで数時間遅れうる。定常の crawl の embedding は数秒で、
  「全コアを使う、または 1 分以上」の基準に当たらないので割に合わない。採らない
- **常駐のまま `Nice=`・`CPUQuota=` で抑える**：待機中の 4 GB は減らない。採らない
- **`StopWhenUnneeded=true`**：手で起動した unit も、必要とする unit が無いのでほどなく止まり、crawl の外で使えない。
  採らない

## 未確認

- 毎回のモデルの読み込みにかかる時間。モジュールのコメントは「数分」とするが、`~/.cache/huggingface` に
  キャッシュがあるときの実測値は無い。待つ上限は従来どおり 900 秒
- 大量の embedding（`embed rebuild` の後）は、重い計算の基準に当たりうる。そのときは
  `sbatch --exclusive --wait` の中でサーバーを起動して流す運用とし、仕組みは作らない
