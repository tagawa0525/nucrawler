-- 訳語集。要約と和訳の system prompt に、記事に原語が出てくる語だけを載せて訳を揃える。
-- 1 つの訳語に原語を複数結び付け、表記の揺れや略語をまとめて同じ訳にする。
CREATE TABLE glossary_terms (
    id     INTEGER PRIMARY KEY,
    target TEXT NOT NULL UNIQUE CHECK (trim(target) <> ''),
    -- 訳語に添える略語。初出は「訳語（略語）」と書かせる
    abbr   TEXT UNIQUE CHECK (trim(abbr) <> ''),
    note   TEXT CHECK (trim(note) <> '')
);

-- 原語は大文字小文字を問わず 1 つの訳語にしか結び付けない。並びは登録順（rowid）。
CREATE TABLE glossary_sources (
    source  TEXT NOT NULL PRIMARY KEY COLLATE NOCASE CHECK (trim(source) <> ''),
    term_id INTEGER NOT NULL REFERENCES glossary_terms (id) ON DELETE CASCADE
);
CREATE INDEX glossary_sources_by_term ON glossary_sources (term_id);

-- 以前はコードに持っていた訳語集を移す。
INSERT INTO glossary_terms (id, target, abbr) VALUES
    (1, '燃料取替停止（定期検査）', NULL),
    (2, 'スクラム（原子炉緊急停止）', NULL),
    (3, '運転認可更新', NULL),
    (4, '2 回目の運転認可更新', 'SLR'),
    (5, '出力向上', NULL),
    (6, '事故耐性燃料', 'ATF'),
    (7, '高燃焼度', NULL),
    (8, '確率論的リスク評価', 'PRA'),
    (9, '小型モジュール炉', 'SMR'),
    (10, '使用済燃料', NULL),
    (11, '廃止措置', NULL),
    (12, '加圧水型軽水炉', 'PWR'),
    (13, '沸騰水型軽水炉', 'BWR'),
    (14, '米国原子力規制委員会', 'NRC'),
    (15, '原子力規制委員会', 'NRA');

INSERT INTO glossary_sources (term_id, source) VALUES
    (1, 'refueling outage'),
    (2, 'scram'),
    (3, 'license renewal'),
    (4, 'subsequent license renewal'),
    (4, 'SLR'),
    (5, 'power uprate'),
    (6, 'accident tolerant fuel'),
    (6, 'accident-tolerant fuel'),
    (6, 'ATF'),
    (7, 'high burnup'),
    (8, 'probabilistic risk assessment'),
    (8, 'PRA'),
    (9, 'small modular reactor'),
    (9, 'SMR'),
    (10, 'spent fuel'),
    (10, 'used fuel'),
    (11, 'decommissioning'),
    (12, 'pressurized water reactor'),
    (12, 'PWR'),
    (13, 'boiling water reactor'),
    (13, 'BWR'),
    (14, 'Nuclear Regulatory Commission'),
    (14, 'NRC'),
    (15, 'Nuclear Regulation Authority'),
    (15, 'NRA'),
    (15, '原子力規制委員会');
