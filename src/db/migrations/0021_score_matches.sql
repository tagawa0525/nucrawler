-- 採点が当たったプロファイルの語：関心分野（interest の topic）と推薦しない話題（exclude）。
-- 点数の理由を、LLM の自由な文ではなく判断の根拠であるプロファイルの項目で示すために残す。
-- 分野での絞り込みや集計ができるよう、JSON の列ではなく行で持つ。
CREATE TABLE score_matches (
    score_id INTEGER NOT NULL REFERENCES scores (id) ON DELETE CASCADE,
    kind     TEXT NOT NULL CHECK (kind IN ('interest', 'exclude')),
    topic    TEXT NOT NULL,
    PRIMARY KEY (score_id, kind, topic)
) WITHOUT ROWID;
