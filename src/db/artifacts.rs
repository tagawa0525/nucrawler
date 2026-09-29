//! 要約などの成果物の登録と、要約待ちの記事。

use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactKind {
    Digest,
    Translation,
    Judgment,
    /// 本文が無く要約できない記事の見出しの和訳
    Title,
}

impl ArtifactKind {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Digest => "digest",
            Self::Translation => "translation",
            Self::Judgment => "judgment",
            Self::Title => "title",
        }
    }
}

/// 登録する成果物。`inputs` は元にした本文の部分（contents.id）で、空は許さない（閲覧できる範囲を
/// 入力の本文から決めるため）。見出しの和訳だけは、公開の見出しから作るので入力を持たせない
/// （表示のときに閲覧の制限を確かめないため、本文を入力にしたものは拒む）。
#[derive(Debug)]
pub struct NewArtifact<'a> {
    pub article_id: i64,
    pub kind: ArtifactKind,
    pub backend: &'a str,
    pub model: &'a str,
    pub prompt_version: i64,
    pub payload: &'a serde_json::Value,
    pub inputs: &'a [i64],
    /// 使った訳語集の時点（[`crate::glossary::Relevant::glossary_at`]）
    pub glossary_at: Option<&'a str>,
}

/// 見出しを和訳する記事。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TitleInput {
    pub article_id: i64,
    pub title: String,
}

/// 要約の入力にする記事と、その公開の本文の部分。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DigestInput {
    pub article_id: i64,
    pub source_id: String,
    pub title: String,
    pub lang: String,
    pub contents: Vec<InputContent>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputContent {
    pub id: i64,
    pub kind: String,
    pub text: String,
}

impl Db {
    /// 成果物と、その入力（artifact_inputs）を 1 つのトランザクションで登録する。
    /// `input_scope` は入力の会員資格から導出する（会員限定の部分が無ければ "public"）。
    pub fn insert_artifact(
        &self,
        a: &NewArtifact,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<i64, DbError> {
        let tx = self.conn.unchecked_transaction()?;
        let id = write_artifact(&tx, a, now)?;
        tx.commit()?;
        Ok(id)
    }

    /// まだ digest が 1 つも無い記事を、新しい順に最大 `limit` 件、公開の入力とともに返す
    /// （会員限定の本文は、ログイン取得を実装するまで扱わない）。
    /// 本文（body/fulltext）がある記事に加え、抽出を断念して概要（lead/abstract）しか無い記事も含める。
    /// 抽出の再試行待ちの記事は、本文が取れるのを待つので含めない。
    /// `backend`/`model` の digest の失敗で再試行待ち・断念済みの記事も含めない。
    pub fn pending_digest(
        &self,
        cutoff: chrono::DateTime<chrono::Utc>,
        now: chrono::DateTime<chrono::Utc>,
        backend: &str,
        model: &str,
        limit: usize,
    ) -> Result<Vec<DigestInput>, DbError> {
        let mut stmt = self.conn.prepare(
            "SELECT a.id, a.source_id, a.title, a.lang FROM articles AS a
             WHERE coalesce(a.published_at, a.fetched_at) >= ?1
               AND NOT EXISTS (
                 SELECT 1 FROM artifacts AS r WHERE r.article_id = a.id AND r.kind = 'digest')
               AND (
                 EXISTS (
                   SELECT 1 FROM contents AS c
                   WHERE c.article_id = a.id AND c.kind IN ('body', 'fulltext')
                     AND c.access_membership_id IS NULL)
                 OR (
                   EXISTS (
                     SELECT 1 FROM contents AS c
                     WHERE c.article_id = a.id AND c.kind IN ('lead', 'abstract')
                       AND c.access_membership_id IS NULL)
                   AND EXISTS (
                     SELECT 1 FROM stage_errors AS e
                     WHERE e.article_id = a.id AND e.stage = 'extract'
                       AND e.backend = '' AND e.model = '' AND e.attempts >= ?2)))
               AND NOT EXISTS (
                 SELECT 1 FROM stage_errors AS e
                 WHERE e.article_id = a.id AND e.stage = 'digest'
                   AND e.backend = ?3 AND e.model = ?4
                   AND (e.attempts >= ?2 OR e.next_retry_at > ?5))
               AND NOT EXISTS (
                 SELECT 1 FROM work_claims AS w
                 WHERE w.article_id = a.id AND w.stage = 'digest'
                   AND w.backend = ?3 AND w.model = ?4 AND w.expires_at > ?5)
             ORDER BY coalesce(a.published_at, a.fetched_at) DESC, a.id DESC
             LIMIT ?6",
        )?;
        let articles = stmt
            .query_map(
                rusqlite::params![
                    timestamp(cutoff),
                    MAX_ATTEMPTS,
                    backend,
                    model,
                    timestamp(now),
                    i64::try_from(limit).unwrap_or(i64::MAX),
                ],
                |r| Ok((r.get::<_, i64>(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )?
            .collect::<Result<Vec<(i64, String, String, String)>, _>>()?;
        articles
            .into_iter()
            .map(|(article_id, source_id, title, lang)| {
                let contents = self.public_contents(article_id, ContentSet::All)?;
                Ok(DigestInput {
                    article_id,
                    source_id,
                    title,
                    lang,
                    contents,
                })
            })
            .collect()
    }

    /// 見出しを和訳する英語記事（新しい順）：要約も見出しの和訳も無く、公開の本文（body/fulltext）が
    /// 無いもの。本文のある記事は要約で見出しが付く。抽出の再試行待ちの記事も、見出しは先に訳しておく。
    /// 見出しだけの記事は数が限られるので、期間では絞らない（絞ると古い記事が英語のまま残る）。
    /// `backend`/`model` の title の失敗で再試行待ち・断念済みの記事は含めない。
    pub fn pending_titles(
        &self,
        now: chrono::DateTime<chrono::Utc>,
        backend: &str,
        model: &str,
        limit: usize,
    ) -> Result<Vec<TitleInput>, DbError> {
        let mut stmt = self.conn.prepare(
            "SELECT a.id, a.title FROM articles AS a
             WHERE a.lang = 'en'
               AND NOT EXISTS (
                 SELECT 1 FROM artifacts AS r
                 WHERE r.article_id = a.id AND r.kind IN ('digest', 'title'))
               AND NOT EXISTS (
                 SELECT 1 FROM contents AS c
                 WHERE c.article_id = a.id AND c.kind IN ('body', 'fulltext')
                   AND c.access_membership_id IS NULL)
               AND NOT EXISTS (
                 SELECT 1 FROM stage_errors AS e
                 WHERE e.article_id = a.id AND e.stage = 'title'
                   AND e.backend = ?2 AND e.model = ?3
                   AND (e.attempts >= ?1 OR e.next_retry_at > ?4))
               AND NOT EXISTS (
                 SELECT 1 FROM work_claims AS w
                 WHERE w.article_id = a.id AND w.stage = 'title'
                   AND w.backend = ?2 AND w.model = ?3 AND w.expires_at > ?4)
             ORDER BY coalesce(a.published_at, a.fetched_at) DESC, a.id DESC
             LIMIT ?5",
        )?;
        let rows = stmt.query_map(
            rusqlite::params![
                MAX_ATTEMPTS,
                backend,
                model,
                timestamp(now),
                i64::try_from(limit).unwrap_or(i64::MAX),
            ],
            |r| {
                Ok(TitleInput {
                    article_id: r.get(0)?,
                    title: r.get(1)?,
                })
            },
        )?;
        Ok(rows.collect::<Result<_, _>>()?)
    }
}

/// 成果物と入力を、呼び出し側のトランザクションの中で書く。
pub(super) fn write_artifact(
    tx: &Connection,
    a: &NewArtifact,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<i64, DbError> {
    match (a.kind, a.inputs.is_empty()) {
        (ArtifactKind::Title, false) => {
            return Err(DbError::TitleWithInputs {
                article_id: a.article_id,
            });
        }
        (ArtifactKind::Title, true) | (_, false) => {}
        (_, true) => {
            return Err(DbError::NoArtifactInputs {
                article_id: a.article_id,
            });
        }
    }
    let mut codes = std::collections::BTreeSet::new();
    for &content_id in a.inputs {
        let code: Option<String> = tx.query_row(
            "SELECT m.code FROM contents AS c
             LEFT JOIN memberships AS m ON m.id = c.access_membership_id
             WHERE c.id = ?1",
            [content_id],
            |r| r.get(0),
        )?;
        codes.extend(code);
    }
    let input_scope = if codes.is_empty() {
        "public".to_string()
    } else {
        codes.into_iter().collect::<Vec<_>>().join("+")
    };
    let id: i64 = tx.query_row(
        "INSERT INTO artifacts
           (article_id, kind, backend, model, prompt_version, input_scope, payload, created_at,
            glossary_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9) RETURNING id",
        rusqlite::params![
            a.article_id,
            a.kind.as_str(),
            a.backend,
            a.model,
            a.prompt_version,
            input_scope,
            a.payload.to_string(),
            timestamp(now),
            a.glossary_at,
        ],
        |r| r.get(0),
    )?;
    for &content_id in a.inputs {
        tx.execute(
            "INSERT INTO artifact_inputs (artifact_id, article_id, content_id)
             VALUES (?1, ?2, ?3)",
            [id, a.article_id, content_id],
        )?;
    }
    if a.kind == ArtifactKind::Digest {
        link_digest_topics(tx, id, a.payload, now)?;
    }
    Ok(id)
}

/// 要約の `new_topics` を語彙に加え、`topics` の語を付与として書く。語彙に無い語があれば失敗する
/// （呼び出し側のトランザクションごと取り消される）。
fn link_digest_topics(
    tx: &Connection,
    artifact_id: i64,
    payload: &serde_json::Value,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<(), DbError> {
    use rusqlite::OptionalExtension;
    let new_topics: Vec<crate::topics::Topic> = match payload.get("new_topics") {
        Some(v) => serde_json::from_value(v.clone())?,
        None => Vec::new(),
    };
    // 統合済みの語（別名）が提案されても語彙に戻さず、下で統合先に付ける
    for t in &new_topics {
        tx.execute(
            "INSERT INTO topics (name, facet, added_at)
             SELECT ?1, ?2, ?3 WHERE NOT EXISTS (SELECT 1 FROM topic_aliases WHERE alias = ?1)
             ON CONFLICT (name) DO NOTHING",
            [&t.name, t.facet.as_str(), &timestamp(now)],
        )?;
    }
    let names: Vec<String> = match payload.get("topics") {
        Some(v) => serde_json::from_value(v.clone())?,
        None => Vec::new(),
    };
    for name in names {
        let topic_id: Option<i64> = tx
            .query_row(
                "SELECT id FROM topics WHERE name = ?1
                 UNION ALL
                 SELECT topic_id FROM topic_aliases WHERE alias = ?1
                 LIMIT 1",
                [&name],
                |r| r.get(0),
            )
            .optional()?;
        let topic_id = topic_id.ok_or(DbError::UnknownTopic(name))?;
        tx.execute(
            "INSERT OR IGNORE INTO artifact_topics (artifact_id, topic_id) VALUES (?1, ?2)",
            [artifact_id, topic_id],
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_support::linked_topics;
    use crate::db::test_support::*;

    fn digest_payload() -> serde_json::Value {
        serde_json::json!({"title_ja": "題", "summary_ja": "要約"})
    }

    #[test]
    fn insert_artifact_links_inputs_and_derives_scope() {
        let db = Db::open_in_memory().unwrap();
        let a = page_article(&db, "https://e.com/a", "2026-09-20T00:00:00.000Z");
        let lead = db
            .insert_content(a, ContentKind::Lead, ContentOrigin::Feed, "lead")
            .unwrap();
        let body = db
            .insert_content(a, ContentKind::Body, ContentOrigin::Page, "body")
            .unwrap();
        let payload = digest_payload();
        let id = db
            .insert_artifact(
                &NewArtifact {
                    article_id: a,
                    kind: ArtifactKind::Digest,
                    backend: "claude-cli",
                    model: "sonnet",
                    prompt_version: 1,
                    payload: &payload,
                    inputs: &[lead, body],
                    glossary_at: None,
                },
                t("2026-09-27T00:00:00Z"),
            )
            .unwrap();
        let row = db
            .query_strings(&format!(
                "SELECT kind || '|' || input_scope || '|' || title_ja || '|' || created_at
                 FROM artifacts WHERE id = {id}"
            ))
            .unwrap();
        assert_eq!(row, ["digest|public|題|2026-09-27T00:00:00.000Z"]);
        assert_eq!(
            db.query_i64(&format!(
                "SELECT count(*) FROM artifact_inputs WHERE artifact_id = {id}"
            ))
            .unwrap(),
            2
        );
    }

    #[test]
    fn insert_artifact_scope_reflects_gated_inputs() {
        let db = Db::open_in_memory().unwrap();
        let a = page_article(&db, "https://e.com/a", "2026-09-20T00:00:00.000Z");
        let aesj: i64 = db
            .conn()
            .query_row("SELECT id FROM memberships WHERE code = 'aesj'", [], |r| {
                r.get(0)
            })
            .unwrap();
        let gated = insert_content(&db, a, Some(aesj));
        let payload = digest_payload();
        let id = db
            .insert_artifact(
                &NewArtifact {
                    article_id: a,
                    kind: ArtifactKind::Digest,
                    backend: "claude-cli",
                    model: "sonnet",
                    prompt_version: 1,
                    payload: &payload,
                    inputs: &[gated],
                    glossary_at: None,
                },
                t("2026-09-27T00:00:00Z"),
            )
            .unwrap();
        assert_eq!(
            db.query_strings(&format!(
                "SELECT input_scope FROM artifacts WHERE id = {id}"
            ))
            .unwrap(),
            ["aesj"]
        );
        assert_eq!(access_of(&db, id), vec![aesj]);
    }

    fn add_title(db: &Db, article_id: i64, title_ja: &str) -> i64 {
        db.insert_artifact(
            &NewArtifact {
                article_id,
                kind: ArtifactKind::Title,
                backend: "claude-cli",
                model: "sonnet",
                prompt_version: 1,
                payload: &serde_json::json!({ "title_ja": title_ja }),
                inputs: &[],
                glossary_at: None,
            },
            t("2026-09-27T00:00:00Z"),
        )
        .unwrap()
    }

    /// 見出しの和訳は本文を入力にしない（見出しは公開）ので、入力が無くても保存でき、誰でも見られる。
    #[test]
    fn insert_artifact_accepts_titles_without_inputs() {
        let db = Db::open_in_memory().unwrap();
        let a = page_article(&db, "https://e.com/a", "2026-09-20T00:00:00.000Z");
        let id = add_title(&db, a, "見出し");
        assert_eq!(
            db.query_strings(&format!(
                "SELECT kind || '|' || input_scope || '|' || title_ja FROM artifacts WHERE id = {id}"
            ))
            .unwrap(),
            ["title|public|見出し"]
        );
        assert_eq!(access_of(&db, id), Vec::<i64>::new());
    }

    /// 見出しの和訳は公開のものとして閲覧の制限を確かめずに表示するので、本文を入力にしたものは拒む。
    #[test]
    fn insert_artifact_rejects_titles_with_inputs() {
        let db = Db::open_in_memory().unwrap();
        let a = page_article(&db, "https://e.com/a", "2026-09-20T00:00:00.000Z");
        let m = insert_membership(&db);
        let gated = insert_content(&db, a, Some(m));
        let err = db
            .insert_artifact(
                &NewArtifact {
                    article_id: a,
                    kind: ArtifactKind::Title,
                    backend: "claude-cli",
                    model: "sonnet",
                    prompt_version: 1,
                    payload: &serde_json::json!({ "title_ja": "会員限定の見出し" }),
                    inputs: &[gated],
                    glossary_at: None,
                },
                t("2026-09-27T00:00:00Z"),
            )
            .unwrap_err();
        assert!(matches!(err, DbError::TitleWithInputs { .. }), "{err}");
        assert_eq!(db.query_i64("SELECT count(*) FROM artifacts").unwrap(), 0);
    }

    fn title_ids(db: &Db, now: &str) -> Vec<i64> {
        db.pending_titles(t(now), "claude-cli", "sonnet", 10)
            .unwrap()
            .into_iter()
            .map(|i| i.article_id)
            .collect()
    }

    /// 要約できない（公開の本文が無い）英語記事の見出しを訳す。期間では絞らない。
    #[test]
    fn pending_titles_selects_english_articles_without_body() {
        let db = Db::open_in_memory().unwrap();
        let now = "2026-09-27T00:00:00Z";
        // 本文も概要も無い → 対象
        let empty = page_article(&db, "https://e.com/empty", "2026-09-26T00:00:00.000Z");
        // 概要だけで抽出の再試行待ち → 見出しは先に訳す
        let lead_only = page_article(&db, "https://e.com/lead", "2026-09-25T00:00:00.000Z");
        db.insert_content(lead_only, ContentKind::Lead, ContentOrigin::Feed, "lead")
            .unwrap();
        // 会員限定の本文しか無い → 要約できないので対象
        let aesj: i64 = db
            .conn()
            .query_row("SELECT id FROM memberships WHERE code = 'aesj'", [], |r| {
                r.get(0)
            })
            .unwrap();
        let member_only = page_article(&db, "https://e.com/member", "2026-09-24T00:00:00.000Z");
        insert_content(&db, member_only, Some(aesj));
        // 何年も前の記事 → 期間では絞らない
        let old = page_article(&db, "https://e.com/old", "2020-01-01T00:00:00.000Z");
        // 公開の本文がある → 要約で見出しが付く
        let with_body = page_article(&db, "https://e.com/body", "2026-09-23T00:00:00.000Z");
        db.insert_content(with_body, ContentKind::Body, ContentOrigin::Page, "body")
            .unwrap();
        // 要約済み
        let digested = page_article(&db, "https://e.com/digested", "2026-09-22T00:00:00.000Z");
        add_digest(&db, digested, "sonnet", "題", true, now);
        // 訳済み（どのモデルでも）
        let titled = page_article(&db, "https://e.com/titled", "2026-09-21T00:00:00.000Z");
        add_title(&db, titled, "訳済み");
        // 日本語の記事
        let ja = db
            .insert_article(&NewArticle {
                lang: Lang::Ja,
                ..article("https://e.com/ja")
            })
            .unwrap()
            .unwrap();
        let _ = ja;
        assert_eq!(title_ids(&db, now), [empty, lead_only, member_only, old]);

        let inputs = db
            .pending_titles(t(now), "claude-cli", "sonnet", 1)
            .unwrap();
        assert_eq!(
            inputs,
            [TitleInput {
                article_id: empty,
                title: "t".into()
            }]
        );
    }

    /// ほかの実行が予約している記事は選ばない（期限を過ぎれば選ぶ）。
    #[test]
    fn pending_digest_and_titles_skip_claimed_articles() {
        let db = Db::open_in_memory().unwrap();
        let now = "2026-09-27T00:00:00Z";
        let with_body = page_article(&db, "https://e.com/body", "2026-09-26T00:00:00.000Z");
        db.insert_content(with_body, ContentKind::Body, ContentOrigin::Page, "body")
            .unwrap();
        let no_body = page_article(&db, "https://e.com/none", "2026-09-26T00:00:00.000Z");
        let key = |stage| ClaimKey {
            stage,
            backend: "claude-cli",
            model: "sonnet",
        };
        let _digest = db
            .claim(
                key("digest"),
                &[with_body],
                t(now),
                chrono::Duration::minutes(10),
            )
            .unwrap();
        let _title = db
            .claim(
                key("title"),
                &[no_body],
                t(now),
                chrono::Duration::minutes(10),
            )
            .unwrap();
        assert!(digest_ids(&db, now).is_empty());
        assert!(title_ids(&db, now).is_empty());
        let later = "2026-09-27T00:10:00Z";
        assert_eq!(digest_ids(&db, later), [with_body]);
        assert_eq!(title_ids(&db, later), [no_body]);
    }

    #[test]
    fn pending_titles_skips_articles_backing_off_for_this_model() {
        let db = Db::open_in_memory().unwrap();
        let a = page_article(&db, "https://e.com/a", "2026-09-26T00:00:00.000Z");
        let key = StageKey {
            article_id: a,
            stage: "title",
            backend: "claude-cli",
            model: "sonnet",
        };
        db.record_stage_failure(key, "bad output", t("2026-09-27T00:00:00Z"), false)
            .unwrap();
        assert!(title_ids(&db, "2026-09-27T00:30:00Z").is_empty());
        assert_eq!(title_ids(&db, "2026-09-27T01:00:00Z"), [a]);
    }

    #[test]
    fn insert_artifact_rejects_empty_inputs() {
        let db = Db::open_in_memory().unwrap();
        let a = page_article(&db, "https://e.com/a", "2026-09-20T00:00:00.000Z");
        let payload = digest_payload();
        let err = db
            .insert_artifact(
                &NewArtifact {
                    article_id: a,
                    kind: ArtifactKind::Digest,
                    backend: "claude-cli",
                    model: "sonnet",
                    prompt_version: 1,
                    payload: &payload,
                    inputs: &[],
                    glossary_at: None,
                },
                t("2026-09-27T00:00:00Z"),
            )
            .unwrap_err();
        assert!(matches!(err, DbError::NoArtifactInputs { .. }), "{err}");
        assert_eq!(db.query_i64("SELECT count(*) FROM artifacts").unwrap(), 0);
    }

    fn digest_ids(db: &Db, now: &str) -> Vec<i64> {
        db.pending_digest(
            t("2026-09-10T00:00:00Z"),
            t(now),
            "claude-cli",
            "sonnet",
            10,
        )
        .unwrap()
        .into_iter()
        .map(|d| d.article_id)
        .collect()
    }

    #[test]
    fn pending_digest_selects_articles_ready_for_summary() {
        let db = Db::open_in_memory().unwrap();
        let now = "2026-09-27T00:00:00Z";
        let extract_key = |article_id| StageKey {
            article_id,
            stage: "extract",
            backend: "",
            model: "",
        };
        // 本文あり → 対象
        let with_body = page_article(&db, "https://e.com/body", "2026-09-26T00:00:00.000Z");
        db.insert_content(with_body, ContentKind::Lead, ContentOrigin::Feed, "lead")
            .unwrap();
        db.insert_content(with_body, ContentKind::Body, ContentOrigin::Page, "body")
            .unwrap();
        // 概要だけで抽出を断念 → 対象
        let lead_only = page_article(&db, "https://e.com/lead", "2026-09-25T00:00:00.000Z");
        db.insert_content(lead_only, ContentKind::Lead, ContentOrigin::Feed, "lead")
            .unwrap();
        db.record_stage_failure(extract_key(lead_only), "403", t(now), true)
            .unwrap();
        // 概要だけで抽出の再試行待ち → 本文を待つ
        let waiting = page_article(&db, "https://e.com/wait", "2026-09-24T00:00:00.000Z");
        db.insert_content(waiting, ContentKind::Lead, ContentOrigin::Feed, "lead")
            .unwrap();
        db.record_stage_failure(extract_key(waiting), "500", t(now), false)
            .unwrap();
        // 本文なし・概要なし → 入力が無い
        let _empty = page_article(&db, "https://e.com/empty", "2026-09-23T00:00:00.000Z");
        // 期間外
        let old = page_article(&db, "https://e.com/old", "2026-09-01T00:00:00.000Z");
        db.insert_content(old, ContentKind::Body, ContentOrigin::Page, "body")
            .unwrap();
        // digest 済み
        let done = page_article(&db, "https://e.com/done", "2026-09-22T00:00:00.000Z");
        let c = db
            .insert_content(done, ContentKind::Body, ContentOrigin::Page, "body")
            .unwrap();
        let payload = digest_payload();
        db.insert_artifact(
            &NewArtifact {
                article_id: done,
                kind: ArtifactKind::Digest,
                backend: "claude-cli",
                model: "haiku",
                prompt_version: 1,
                payload: &payload,
                inputs: &[c],
                glossary_at: None,
            },
            t(now),
        )
        .unwrap();

        assert_eq!(digest_ids(&db, now), [with_body, lead_only]);

        let inputs = db
            .pending_digest(t("2026-09-10T00:00:00Z"), t(now), "claude-cli", "sonnet", 1)
            .unwrap();
        assert_eq!(inputs.len(), 1);
        let kinds: Vec<_> = inputs[0].contents.iter().map(|c| c.kind.as_str()).collect();
        assert_eq!(kinds, ["lead", "body"]);
        assert_eq!(inputs[0].lang, "en");
        assert_eq!(inputs[0].source_id, "s");
    }

    /// 公開の本文が無ければ（会員限定の本文しか無ければ）、入力が作れないので選ばない。
    #[test]
    fn pending_digest_ignores_member_only_contents() {
        let db = Db::open_in_memory().unwrap();
        let aesj: i64 = db
            .conn()
            .query_row("SELECT id FROM memberships WHERE code = 'aesj'", [], |r| {
                r.get(0)
            })
            .unwrap();
        let a = page_article(&db, "https://e.com/a", "2026-09-26T00:00:00.000Z");
        insert_content(&db, a, Some(aesj));
        assert!(digest_ids(&db, "2026-09-27T00:00:00Z").is_empty());
    }

    /// 抽出の断念は、抽出ステージのキー（backend と model が空）の記録だけで判断する。
    #[test]
    fn pending_digest_checks_extract_failures_by_exact_key() {
        let db = Db::open_in_memory().unwrap();
        let a = page_article(&db, "https://e.com/a", "2026-09-26T00:00:00.000Z");
        db.insert_content(a, ContentKind::Lead, ContentOrigin::Feed, "lead")
            .unwrap();
        let other = StageKey {
            article_id: a,
            stage: "extract",
            backend: "chromium",
            model: "",
        };
        db.record_stage_failure(other, "x", t("2026-09-27T00:00:00Z"), true)
            .unwrap();
        assert!(digest_ids(&db, "2026-09-27T00:00:00Z").is_empty());
    }

    #[test]
    fn pending_digest_skips_articles_backing_off_for_this_model() {
        let db = Db::open_in_memory().unwrap();
        let a = page_article(&db, "https://e.com/a", "2026-09-26T00:00:00.000Z");
        db.insert_content(a, ContentKind::Body, ContentOrigin::Page, "body")
            .unwrap();
        let key = StageKey {
            article_id: a,
            stage: "digest",
            backend: "claude-cli",
            model: "sonnet",
        };
        db.record_stage_failure(key, "bad output", t("2026-09-27T00:00:00Z"), false)
            .unwrap();
        assert!(digest_ids(&db, "2026-09-27T00:30:00Z").is_empty());
        assert_eq!(digest_ids(&db, "2026-09-27T01:00:00Z"), [a]);
    }

    #[test]
    fn insert_digest_links_topics_and_adds_proposed_ones() {
        use crate::topics::Facet;
        let db = Db::open_in_memory().unwrap();
        let before = db.topics().unwrap().len();
        let id = digest_with_topics(
            &db,
            serde_json::json!(["燃料", "データセンター需要"]),
            serde_json::json!([{"name": "データセンター需要", "facet": "分野"}]),
        )
        .unwrap();
        assert_eq!(linked_topics(&db, id), ["燃料", "データセンター需要"]);
        let topics = db.topics().unwrap();
        assert_eq!(topics.len(), before + 1);
        assert_eq!(
            topics.last().unwrap(),
            &crate::topics::Topic {
                name: "データセンター需要".into(),
                facet: Facet::Field
            }
        );
        // 提案された語は追加の時刻を持つ（初期語彙は持たない）
        assert_eq!(
            db.query_strings("SELECT name FROM topics WHERE added_at = '2026-09-27T00:00:00.000Z'")
                .unwrap(),
            ["データセンター需要"]
        );
        // 同じ語を別の要約が提案しても、語彙は増えず同じ語に付く
        let again = digest_with_topics(
            &db,
            serde_json::json!(["データセンター需要"]),
            serde_json::json!([{"name": "データセンター需要", "facet": "炉型"}]),
        )
        .unwrap();
        assert_eq!(linked_topics(&db, again), ["データセンター需要"]);
        assert_eq!(db.topics().unwrap().len(), before + 1);
    }

    #[test]
    fn insert_digest_rejects_unknown_topics_without_writing() {
        let db = Db::open_in_memory().unwrap();
        let err = digest_with_topics(
            &db,
            serde_json::json!(["燃料", "新設炉"]),
            serde_json::json!([]),
        )
        .unwrap_err();
        assert!(
            matches!(&err, DbError::UnknownTopic(name) if name == "新設炉"),
            "{err}"
        );
        assert_eq!(db.query_i64("SELECT count(*) FROM artifacts").unwrap(), 0);
        assert_eq!(
            db.query_i64("SELECT count(*) FROM artifact_topics")
                .unwrap(),
            0
        );
    }
}
