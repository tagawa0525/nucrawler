-- 作業の予約。LLM を呼ぶ処理が同じ記事・ステージ・モデルを同時に処理しないよう、対象を選んだら予約し、
-- 処理を終えたら外す。キーは stage_errors と同じ。期限（expires_at）を過ぎた予約は、落ちたプロセスが
-- 残したものとして、ほかの実行が取り直せる。token は予約ごとに違う値で、外すときに照らし合わせる
-- （期限切れの予約を持っていた実行が、後から取り直した実行の予約を消さないように）。
CREATE TABLE work_claims (
    article_id INTEGER NOT NULL REFERENCES articles (id) ON DELETE CASCADE,
    stage      TEXT NOT NULL,
    backend    TEXT NOT NULL,
    model      TEXT NOT NULL,
    token      TEXT NOT NULL,
    expires_at TEXT NOT NULL,
    PRIMARY KEY (article_id, stage, backend, model)
) WITHOUT ROWID;
