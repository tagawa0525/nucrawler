-- 時刻はすべて UTC の RFC 3339 文字列（例 2026-09-27T01:23:45.678Z）。

-- ユーザーと会員資格 ---------------------------------------------------------

CREATE TABLE users (
    id           INTEGER PRIMARY KEY,
    login        TEXT NOT NULL UNIQUE,           -- Tailscale-User-Login
    display_name TEXT NOT NULL,
    is_owner     INTEGER NOT NULL DEFAULT 0 CHECK (is_owner IN (0, 1))
);
-- オーナー（サブスクで LLM を動かす本人）は 1 人だけ。
CREATE UNIQUE INDEX users_single_owner ON users (is_owner) WHERE is_owner = 1;

CREATE TABLE memberships (
    id   INTEGER PRIMARY KEY,
    code TEXT NOT NULL UNIQUE,                   -- 'aesj' など
    name TEXT NOT NULL
);

-- 自己申告の会員資格。
CREATE TABLE user_memberships (
    user_id       INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    membership_id INTEGER NOT NULL REFERENCES memberships (id),
    PRIMARY KEY (user_id, membership_id)
) WITHOUT ROWID;

CREATE TABLE profiles (
    user_id    INTEGER PRIMARY KEY REFERENCES users (id) ON DELETE CASCADE,
    interests  TEXT NOT NULL CHECK (json_valid(interests)),
    excludes   TEXT NOT NULL CHECK (json_valid(excludes)),
    hash       TEXT NOT NULL,                    -- 採点の再計算判定に使う
    updated_at TEXT NOT NULL
);

-- 記事と本文 -------------------------------------------------------------------

CREATE TABLE articles (
    id           INTEGER PRIMARY KEY,
    source_id    TEXT NOT NULL,
    url          TEXT NOT NULL UNIQUE,           -- 正規化済み
    title        TEXT NOT NULL,
    lang         TEXT NOT NULL CHECK (lang IN ('en', 'ja')),
    published_at TEXT,
    fetched_at   TEXT NOT NULL
);
CREATE INDEX articles_by_source ON articles (source_id, published_at);

-- 原文を読むのに必要な会員資格（🔒 表示用）。
CREATE TABLE article_access (
    article_id    INTEGER NOT NULL REFERENCES articles (id) ON DELETE CASCADE,
    membership_id INTEGER NOT NULL REFERENCES memberships (id),
    PRIMARY KEY (article_id, membership_id)
) WITHOUT ROWID;

-- 本文の部分。access_membership_id が NULL なら公開。
CREATE TABLE contents (
    id                   INTEGER PRIMARY KEY,
    article_id           INTEGER NOT NULL REFERENCES articles (id) ON DELETE CASCADE,
    kind                 TEXT NOT NULL CHECK (kind IN ('lead', 'body', 'abstract', 'fulltext')),
    access_membership_id INTEGER REFERENCES memberships (id),
    text                 TEXT NOT NULL,
    origin               TEXT NOT NULL CHECK (origin IN ('feed', 'page', 'pdf', 'upload', 'login')),
    fetched_at           TEXT NOT NULL
);
CREATE INDEX contents_by_article ON contents (article_id);

-- LLM などの成果物 -----------------------------------------------------------

-- digest / translation / judgment。モデル・プロンプト版ごとに行を残し、上書きしない。
-- input_scope は入力に使った本文の公開範囲：'public'、または必要な会員資格の code を
-- ソートして '+' で連結したもの（例 'aesj'）。
CREATE TABLE artifacts (
    id             INTEGER PRIMARY KEY,
    article_id     INTEGER NOT NULL REFERENCES articles (id) ON DELETE CASCADE,
    kind           TEXT NOT NULL CHECK (kind IN ('digest', 'translation', 'judgment')),
    backend        TEXT NOT NULL,                -- claude-cli / anthropic-api / jev ...
    model          TEXT NOT NULL,
    prompt_version INTEGER NOT NULL,
    input_scope    TEXT NOT NULL,
    payload        TEXT NOT NULL CHECK (json_valid(payload)),
    created_at     TEXT NOT NULL,
    title_ja       TEXT GENERATED ALWAYS AS (json_extract(payload, '$.title_ja')) VIRTUAL,
    summary_ja     TEXT GENERATED ALWAYS AS (json_extract(payload, '$.summary_ja')) VIRTUAL,
    UNIQUE (article_id, kind, backend, model, prompt_version, input_scope)
);

CREATE TABLE artifact_inputs (
    artifact_id INTEGER NOT NULL REFERENCES artifacts (id) ON DELETE CASCADE,
    content_id  INTEGER NOT NULL REFERENCES contents (id),
    PRIMARY KEY (artifact_id, content_id)
) WITHOUT ROWID;

-- 閲覧に必要な会員資格（入力の資格の和集合）。行が無ければ公開。
CREATE TABLE artifact_access (
    artifact_id   INTEGER NOT NULL REFERENCES artifacts (id) ON DELETE CASCADE,
    membership_id INTEGER NOT NULL REFERENCES memberships (id),
    PRIMARY KEY (artifact_id, membership_id)
) WITHOUT ROWID;

-- 推薦 -----------------------------------------------------------------------

CREATE TABLE scores (
    id           INTEGER PRIMARY KEY,
    user_id      INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    artifact_id  INTEGER NOT NULL REFERENCES artifacts (id) ON DELETE CASCADE,
    profile_hash TEXT NOT NULL,
    backend      TEXT NOT NULL,
    model        TEXT NOT NULL,
    score        INTEGER NOT NULL CHECK (score BETWEEN 0 AND 100),
    reason       TEXT,                           -- 理由を返さない判定器もある
    created_at   TEXT NOT NULL,
    UNIQUE (user_id, artifact_id, profile_hash, backend, model)
);

CREATE TABLE events (
    id         INTEGER PRIMARY KEY,
    user_id    INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    article_id INTEGER NOT NULL REFERENCES articles (id) ON DELETE CASCADE,
    kind       TEXT NOT NULL CHECK (kind IN ('open_detail', 'open_translation', 'up', 'down')),
    created_at TEXT NOT NULL
);
CREATE INDEX events_by_user ON events (user_id, created_at);

CREATE TABLE translation_requests (
    user_id      INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    article_id   INTEGER NOT NULL REFERENCES articles (id) ON DELETE CASCADE,
    requested_at TEXT NOT NULL,
    done_at      TEXT,
    PRIMARY KEY (user_id, article_id)
) WITHOUT ROWID;

-- 運用 -----------------------------------------------------------------------

-- LLM を使わないステージは backend と model を '' にする。
CREATE TABLE stage_errors (
    article_id    INTEGER NOT NULL REFERENCES articles (id) ON DELETE CASCADE,
    stage         TEXT NOT NULL,
    backend       TEXT NOT NULL,
    model         TEXT NOT NULL,
    attempts      INTEGER NOT NULL,
    last_error    TEXT NOT NULL,
    next_retry_at TEXT NOT NULL,
    PRIMARY KEY (article_id, stage, backend, model)
) WITHOUT ROWID;

CREATE TABLE llm_calls (
    id          INTEGER PRIMARY KEY,
    at          TEXT NOT NULL,
    stage       TEXT NOT NULL,
    backend     TEXT NOT NULL,
    model       TEXT NOT NULL,
    n_items     INTEGER NOT NULL,
    ok          INTEGER NOT NULL CHECK (ok IN (0, 1)),
    duration_ms INTEGER NOT NULL,
    error       TEXT,
    rate_limit  TEXT CHECK (rate_limit IS NULL OR json_valid(rate_limit))
);
CREATE INDEX llm_calls_by_time ON llm_calls (at);

CREATE TABLE source_state (
    source_id       TEXT PRIMARY KEY,
    last_success_at TEXT,
    last_error      TEXT,
    last_error_at   TEXT
);

-- 初期データ -------------------------------------------------------------------

INSERT INTO users (login, display_name, is_owner) VALUES ('owner', 'owner', 1);
INSERT INTO memberships (code, name) VALUES ('aesj', '日本原子力学会');
