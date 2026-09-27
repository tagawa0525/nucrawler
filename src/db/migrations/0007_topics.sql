-- トピックの語彙。LLM が自由に付けると表記が揺れる（「新設」「新設炉」「新規建設」など）ので、
-- 要約には語彙の中から選ばせる。以後の編集は `topics export` / `topics import` で行う。
-- 発電所名などの固有名は語彙に入れず、全文検索で引く。
CREATE TABLE topics (
    id    INTEGER PRIMARY KEY,
    name  TEXT NOT NULL UNIQUE,
    facet TEXT NOT NULL CHECK (facet IN ('分野', '炉型', '地域', '組織'))
);

-- 要約の版ごとのトピック。版の閲覧資格は artifacts 側で判定する。
-- 付いている語を語彙から消すと付与が黙って失われるので、削除させない（RESTRICT）。
CREATE TABLE artifact_topics (
    artifact_id INTEGER NOT NULL REFERENCES artifacts (id) ON DELETE CASCADE,
    topic_id    INTEGER NOT NULL REFERENCES topics (id) ON DELETE RESTRICT,
    PRIMARY KEY (artifact_id, topic_id)
) WITHOUT ROWID;
CREATE INDEX artifact_topics_by_topic ON artifact_topics (topic_id);

INSERT INTO topics (name, facet) VALUES
    ('規制・審査', '分野'),
    ('再稼働', '分野'),
    ('運転・保守', '分野'),
    ('高経年化', '分野'),
    ('出力向上', '分野'),
    ('設備トラブル・不適合', '分野'),
    ('安全解析', '分野'),
    ('耐震・自然災害', '分野'),
    ('防災・緊急時対応', '分野'),
    ('放射線防護', '分野'),
    ('燃料', '分野'),
    ('ウラン・濃縮', '分野'),
    ('燃料サイクル・再処理', '分野'),
    ('使用済燃料・貯蔵', '分野'),
    ('放射性廃棄物', '分野'),
    ('廃止措置', '分野'),
    ('新設・建設', '分野'),
    ('サプライチェーン', '分野'),
    ('政策・市場', '分野'),
    ('人材・安全文化', '分野'),
    ('不祥事・ガバナンス', '分野'),
    ('研究開発', '分野'),
    ('デジタル・AI', '分野'),
    ('核不拡散・核セキュリティ', '分野'),
    ('国際協力', '分野'),
    ('立地地域・広報', '分野'),
    ('非発電利用', '分野'),
    ('PWR', '炉型'),
    ('BWR', '炉型'),
    ('SMR', '炉型'),
    ('マイクロ炉', '炉型'),
    ('重水炉', '炉型'),
    ('高速炉', '炉型'),
    ('高温ガス炉', '炉型'),
    ('溶融塩炉', '炉型'),
    ('研究炉', '炉型'),
    ('核融合', '炉型'),
    ('日本', '地域'),
    ('米国', '地域'),
    ('カナダ', '地域'),
    ('英国', '地域'),
    ('フランス', '地域'),
    ('欧州', '地域'),
    ('ロシア・東欧', '地域'),
    ('中国', '地域'),
    ('韓国', '地域'),
    ('インド', '地域'),
    ('東南アジア', '地域'),
    ('中東・アフリカ', '地域'),
    ('中南米', '地域'),
    ('IAEA', '組織'),
    ('OECD/NEA', '組織'),
    ('NRC', '組織'),
    ('NRA', '組織'),
    ('DOE', '組織');

-- 語彙を入れる前の要約のうち、語彙と同じ名前のトピックは付与として移す。
-- 語彙に無い名前（表記の揺れ）は payload に残るだけで、要約を作り直すまで付与は無い。
INSERT OR IGNORE INTO artifact_topics (artifact_id, topic_id)
SELECT a.id, t.id
FROM artifacts AS a, json_each(a.payload, '$.topics') AS j
JOIN topics AS t ON t.name = j.value
WHERE a.kind = 'digest';
