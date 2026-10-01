//! オフライン評価（`nucrawler eval`）：評価（1〜5）を正解ラベルにして、採点と突き合わせる材料。

use super::*;

/// 記事の正解ラベル：利用者の評価。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Label {
    pub article_id: i64,
    pub rating: Rating,
    /// 評価した時刻
    pub at: String,
}

/// 採点のキー（どのプロファイル・バックエンド・モデル・プロンプトの版で採点したか）。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct EvalKey {
    pub profile_hash: String,
    pub backend: String,
    pub model: String,
    pub prompt_version: i64,
}

/// ラベルの付いた記事の、あるキーでの点数。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LabeledScore {
    pub key: EvalKey,
    pub article_id: i64,
    pub score: u8,
    /// 採点した時刻
    pub scored_at: String,
    /// 推薦点の学習に使う特徴（記事のソース、採点した要約のトピック、その採点が当たった関心分野と推薦しない話題）
    pub features: Vec<crate::recommend::Feature>,
}

/// プロファイルの見直しの根拠：評価した記事の見出しとトピック。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Evidence {
    pub article_id: i64,
    pub rating: Rating,
    pub title_ja: String,
    pub topics: Vec<String>,
    /// 評価した時刻
    pub at: String,
}

/// 確認枠（閾値未満から無作為に選んだ記事）の評価の内訳。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ExploreStats {
    /// 確認枠に選んだ記事の数
    pub picked: usize,
    /// そのうち関心（評価 4〜5）の記事
    pub positive: usize,
    /// そのうち中立（評価 3）の記事
    pub neutral: usize,
    /// そのうち不要（評価 1〜2）の記事
    pub negative: usize,
}

impl Db {
    /// 利用者の評価から決めた正解ラベル（article_id 順）。
    pub fn eval_labels(&self, user_id: i64) -> Result<Vec<Label>, DbError> {
        let mut stmt = self.conn.prepare(
            "SELECT article_id, value, rated_at FROM ratings WHERE user_id = ?1 ORDER BY article_id",
        )?;
        let rows = stmt.query_map([user_id], |r| {
            Ok(Label {
                article_id: r.get(0)?,
                rating: r.get(1)?,
                at: r.get(2)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// 評価した記事に、利用者が閲覧できる最新の digest の見出しとトピックを付けて、評価の
    /// 新しい順に返す。digest の無い記事は含めない。
    pub fn label_evidence(&self, user_id: i64) -> Result<Vec<Evidence>, DbError> {
        let mut stmt = self.conn.prepare(&format!(
            "WITH digests AS (
               SELECT rt.article_id, rt.value, rt.rated_at,
                      {digest_id} AS digest_id
               FROM ratings AS rt
               WHERE rt.user_id = :user)
             SELECT d.article_id, d.value, d.rated_at, r.title_ja, {topics}
             FROM digests AS d
             JOIN artifacts AS r ON r.id = d.digest_id
             ORDER BY d.rated_at DESC, d.article_id DESC",
            digest_id = super::read::latest_digest("id", "rt.article_id", ":user"),
            topics = super::read::linked_topics("r"),
        ))?;
        let rows = stmt.query_map(rusqlite::named_params! {":user": user_id}, |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, Rating>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
            ))
        })?;
        rows.map(|row| {
            let (article_id, rating, at, title_ja, topics) = row?;
            Ok(Evidence {
                article_id,
                rating,
                title_ja,
                topics: serde_json::from_str(&topics)?,
                at,
            })
        })
        .collect()
    }

    /// 確認枠に選んだ記事と、その評価の内訳。
    pub fn explore_stats(&self, user_id: i64) -> Result<ExploreStats, DbError> {
        let picked: std::collections::HashSet<i64> = {
            let mut stmt = self
                .conn
                .prepare("SELECT article_id FROM explore_picks WHERE user_id = ?1")?;
            let rows = stmt.query_map([user_id], |r| r.get(0))?;
            rows.collect::<Result<_, _>>()?
        };
        let mut stats = ExploreStats {
            picked: picked.len(),
            ..ExploreStats::default()
        };
        for label in self.eval_labels(user_id)? {
            if !picked.contains(&label.article_id) {
                continue;
            }
            if label.rating.is_positive() {
                stats.positive += 1;
            } else if label.rating.is_negative() {
                stats.negative += 1;
            } else {
                stats.neutral += 1;
            }
        }
        Ok(stats)
    }

    /// ラベルの付いた記事の点数と特徴。キーごとに、そのキーで採点された最新の digest の点数を使う
    /// （キー・article_id 順）。
    pub fn eval_scores(&self, user_id: i64) -> Result<Vec<LabeledScore>, DbError> {
        let mut stmt = self.conn.prepare(&format!(
            "WITH labeled AS (SELECT article_id FROM ratings WHERE user_id = ?1),
             ranked AS (
               SELECT s.profile_hash, s.backend, s.model, s.prompt_version, r.article_id,
                      s.score, s.created_at, s.id AS score_id, r.id,
                      row_number() OVER (
                        PARTITION BY s.profile_hash, s.backend, s.model, s.prompt_version,
                                     r.article_id
                        ORDER BY r.created_at DESC, r.id DESC) AS rn
               FROM scores AS s
               JOIN artifacts AS r ON r.id = s.artifact_id AND r.kind = 'digest'
               JOIN labeled AS l ON l.article_id = r.article_id
               WHERE s.user_id = ?1)
             SELECT r.profile_hash, r.backend, r.model, r.prompt_version, r.article_id, r.score,
                    r.created_at, a.source_id, {topics},
                    {matched},
                    {excluded}
             FROM ranked AS r
             JOIN articles AS a ON a.id = r.article_id
             WHERE r.rn = 1
             ORDER BY r.profile_hash, r.backend, r.model, r.prompt_version, r.article_id",
            topics = super::read::linked_topics("r"),
            matched = super::read::matched_topics("r.score_id", "interest"),
            excluded = super::read::matched_topics("r.score_id", "exclude"),
        ))?;
        let rows = stmt.query_map([user_id], |r| {
            Ok((
                LabeledScore {
                    key: EvalKey {
                        profile_hash: r.get(0)?,
                        backend: r.get(1)?,
                        model: r.get(2)?,
                        prompt_version: r.get(3)?,
                    },
                    article_id: r.get(4)?,
                    score: r.get(5)?,
                    scored_at: r.get(6)?,
                    features: Vec::new(),
                },
                r.get::<_, String>(7)?,
                r.get::<_, String>(8)?,
                r.get::<_, String>(9)?,
                r.get::<_, String>(10)?,
            ))
        })?;
        rows.map(|row| {
            let (mut score, source, topics, matched, excluded) = row?;
            let [topics, matched, excluded]: [Vec<String>; 3] = [
                serde_json::from_str(&topics)?,
                serde_json::from_str(&matched)?,
                serde_json::from_str(&excluded)?,
            ];
            score.features = crate::recommend::features(&source, &topics, &matched, &excluded);
            Ok(score)
        })
        .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_support::*;

    /// ラベルは評価だけから決める。ブックマーク・見送り・開いた記録はラベルにしない。
    #[test]
    fn labels_come_from_ratings() {
        let db = Db::open_in_memory().unwrap();
        let owner = db.owner_id().unwrap();
        let a = page_article(&db, "https://e.com/a", "2026-09-26T00:00:00.000Z");
        let b = page_article(&db, "https://e.com/b", "2026-09-26T00:00:00.000Z");
        let c = page_article(&db, "https://e.com/c", "2026-09-26T00:00:00.000Z");
        let d = page_article(&db, "https://e.com/d", "2026-09-26T00:00:00.000Z");
        let rate = |article, value, at| db.rate(owner, article, Rating::new(value), t(at)).unwrap();
        // 付け直した評価 → 最後の評価
        rate(a, 2, "2026-09-27T00:00:00Z");
        rate(a, 5, "2026-09-27T01:00:00Z");
        // 開いただけ・ブックマーク・見送り → ラベルなし
        db.record_open(owner, b, OpenKind::Detail, t("2026-09-27T00:00:00Z"))
            .unwrap();
        db.set_bookmark(owner, b, true, t("2026-09-27T00:00:00Z"))
            .unwrap();
        db.set_read(owner, c, true, t("2026-09-27T00:00:00Z"))
            .unwrap();
        // 評価なしに戻した → ラベルなし
        rate(d, 4, "2026-09-27T00:00:00Z");
        db.rate(owner, d, None, t("2026-09-27T01:00:00Z")).unwrap();
        // 別の利用者の評価は使わない
        let other = db
            .conn()
            .query_row(
                "INSERT INTO users (login, display_name) VALUES ('other', 'other') RETURNING id",
                [],
                |r| r.get::<_, i64>(0),
            )
            .unwrap();
        db.rate(other, b, Rating::new(5), t("2026-09-27T00:00:00Z"))
            .unwrap();

        let labels = db.eval_labels(owner).unwrap();
        assert_eq!(
            labels,
            [Label {
                article_id: a,
                rating: Rating::new(5).unwrap(),
                at: "2026-09-27T01:00:00.000Z".into(),
            }]
        );
    }

    #[test]
    fn evidence_carries_the_latest_digest_newest_first() {
        let db = Db::open_in_memory().unwrap();
        let owner = db.owner_id().unwrap();
        let a = page_article(&db, "https://e.com/a", "2026-09-26T00:00:00.000Z");
        add_digest(&db, a, "haiku", "古い見出し", true, "2026-09-26T01:00:00Z");
        add_digest(
            &db,
            a,
            "sonnet",
            "新しい見出し",
            true,
            "2026-09-26T02:00:00Z",
        );
        let b = page_article(&db, "https://e.com/b", "2026-09-26T00:00:00.000Z");
        add_digest(&db, b, "sonnet", "不要な記事", true, "2026-09-26T01:00:00Z");
        // digest の無い記事は根拠にできない
        let bare = page_article(&db, "https://e.com/bare", "2026-09-26T00:00:00.000Z");
        let rate = |article, value, at| db.rate(owner, article, Rating::new(value), t(at)).unwrap();
        rate(a, 4, "2026-09-27T00:00:00Z");
        rate(b, 2, "2026-09-27T01:00:00Z");
        rate(bare, 5, "2026-09-27T02:00:00Z");

        let evidence = db.label_evidence(owner).unwrap();
        assert_eq!(
            evidence,
            [
                Evidence {
                    article_id: b,
                    rating: Rating::new(2).unwrap(),
                    title_ja: "不要な記事".into(),
                    topics: vec!["規制・審査".into()],
                    at: "2026-09-27T01:00:00.000Z".into(),
                },
                Evidence {
                    article_id: a,
                    rating: Rating::new(4).unwrap(),
                    title_ja: "新しい見出し".into(),
                    topics: vec!["規制・審査".into()],
                    at: "2026-09-27T00:00:00.000Z".into(),
                },
            ]
        );
    }

    /// 会員限定の本文から作った digest は、会員でない利用者の根拠にしない（見出しを LLM に渡すため）。
    #[test]
    fn evidence_skips_digests_the_user_cannot_view() {
        let db = Db::open_in_memory().unwrap();
        let owner = db.owner_id().unwrap();
        let aesj: i64 = db
            .conn()
            .query_row("SELECT id FROM memberships WHERE code = 'aesj'", [], |r| {
                r.get(0)
            })
            .unwrap();
        let gated_digest = |article_id: i64, title: &str, at: &str| {
            let gated = insert_content(&db, article_id, Some(aesj));
            let payload = serde_json::json!({
                "title_ja": title, "summary_ja": "s", "points_ja": ["p"],
                "implications_ja": "", "lwr_relevant": true, "topics": ["燃料"],
            });
            db.insert_artifact(
                &NewArtifact {
                    article_id,
                    kind: ArtifactKind::Digest,
                    backend: "claude-cli",
                    model: "sonnet",
                    prompt_version: 1,
                    payload: &payload,
                    inputs: &[gated],
                    glossary_at: None,
                },
                t(at),
            )
            .unwrap();
        };
        // 公開の digest より新しい会員限定の digest があっても、公開の方を使う
        let a = page_article(&db, "https://e.com/a", "2026-09-26T00:00:00.000Z");
        add_digest(
            &db,
            a,
            "sonnet",
            "公開の見出し",
            true,
            "2026-09-26T01:00:00Z",
        );
        gated_digest(a, "会員限定の見出し", "2026-09-26T02:00:00Z");
        // 会員限定の digest しか無い記事は根拠にしない
        let b = page_article(&db, "https://e.com/b", "2026-09-26T00:00:00.000Z");
        gated_digest(b, "会員限定だけ", "2026-09-26T01:00:00Z");
        for article in [a, b] {
            db.rate(owner, article, Rating::new(4), t("2026-09-27T00:00:00Z"))
                .unwrap();
        }
        let titles: Vec<String> = db
            .label_evidence(owner)
            .unwrap()
            .into_iter()
            .map(|e| e.title_ja)
            .collect();
        assert_eq!(titles, ["公開の見出し"]);
    }

    #[test]
    fn explore_stats_count_reactions_to_picks() {
        let db = Db::open_in_memory().unwrap();
        let owner = db.owner_id().unwrap();
        let ids: Vec<i64> = (0..4)
            .map(|i| {
                page_article(
                    &db,
                    &format!("https://e.com/{i}"),
                    "2026-09-26T00:00:00.000Z",
                )
            })
            .collect();
        for &id in &ids[..3] {
            db.conn()
                .execute(
                    "INSERT INTO explore_picks VALUES (?1, ?2, '2026-09-27')",
                    [owner, id],
                )
                .unwrap();
        }
        let rate = |article, value| {
            db.rate(
                owner,
                article,
                Rating::new(value),
                t("2026-09-27T01:00:00Z"),
            )
            .unwrap()
        };
        rate(ids[0], 5);
        rate(ids[1], 3);
        rate(ids[2], 1);
        // 確認枠に選んでいない記事の評価は数えない
        rate(ids[3], 4);
        assert_eq!(
            db.explore_stats(owner).unwrap(),
            ExploreStats {
                picked: 3,
                positive: 1,
                neutral: 1,
                negative: 1,
            }
        );
    }

    /// 推薦点の学習に使う特徴：記事のソース、採点した要約のトピック、その採点が当たった関心分野と推薦しない話題。
    #[test]
    fn scores_carry_the_features_of_the_scored_digest() {
        let db = Db::open_in_memory().unwrap();
        let owner = db.owner_id().unwrap();
        let key = score_key(&db);
        let a = page_article(&db, "https://e.com/a", "2026-09-26T00:00:00.000Z");
        let digest = add_digest(&db, a, "sonnet", "題", true, "2026-09-26T01:00:00Z");
        db.insert_score_with_matches(
            key,
            digest,
            70,
            None,
            ScoreMatches {
                interests: &["規制・審査".to_string()],
                excludes: &["核融合".to_string()],
            },
            t("2026-09-26T02:00:00Z"),
        )
        .unwrap();
        db.rate(owner, a, Rating::new(4), t("2026-09-27T00:00:00Z"))
            .unwrap();
        let scores = db.eval_scores(owner).unwrap();
        let feature = |kind, key: &str| crate::recommend::Feature {
            kind,
            key: key.into(),
        };
        use crate::recommend::FeatureKind::*;
        let mut features = scores[0].features.clone();
        features.sort();
        let source: String = db
            .conn()
            .query_row("SELECT source_id FROM articles WHERE id = ?1", [a], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(
            features,
            [
                feature(Topic, "規制・審査"),
                feature(Source, &source),
                feature(Interest, "規制・審査"),
                feature(Exclude, "核融合"),
            ]
        );
    }

    #[test]
    fn scores_use_the_latest_digest_scored_by_each_key() {
        let db = Db::open_in_memory().unwrap();
        let owner = db.owner_id().unwrap();
        let key = score_key(&db);
        let a = page_article(&db, "https://e.com/a", "2026-09-26T00:00:00.000Z");
        let old = add_digest(&db, a, "haiku", "古い版", true, "2026-09-26T01:00:00Z");
        let new = add_digest(&db, a, "sonnet", "新しい版", true, "2026-09-26T02:00:00Z");
        db.insert_score(key, old, 30, None, t("2026-09-26T03:00:00Z"))
            .unwrap();
        db.insert_score(key, new, 80, None, t("2026-09-26T04:00:00Z"))
            .unwrap();
        let v2 = ScoreKey {
            prompt_version: 2,
            ..key
        };
        db.insert_score(v2, old, 60, None, t("2026-09-26T05:00:00Z"))
            .unwrap();
        // ラベルの無い記事の点数は返さない
        let unlabeled = page_article(&db, "https://e.com/u", "2026-09-26T00:00:00.000Z");
        let u = add_digest(&db, unlabeled, "sonnet", "題", true, "2026-09-26T01:00:00Z");
        db.insert_score(key, u, 90, None, t("2026-09-26T03:00:00Z"))
            .unwrap();
        // digest 以外の成果物に付いた点数は使わない（より新しくても）
        let c = insert_content(&db, a, None);
        let translation = db
            .insert_artifact(
                &NewArtifact {
                    article_id: a,
                    kind: ArtifactKind::Translation,
                    backend: "claude-cli",
                    model: "sonnet",
                    prompt_version: 1,
                    payload: &serde_json::json!({ "body_ja": "和訳" }),
                    inputs: &[c],
                    glossary_at: None,
                },
                t("2026-09-26T06:00:00Z"),
            )
            .unwrap();
        db.insert_score(key, translation, 5, None, t("2026-09-26T07:00:00Z"))
            .unwrap();
        db.rate(owner, a, Rating::new(4), t("2026-09-27T00:00:00Z"))
            .unwrap();

        let scores = db.eval_scores(owner).unwrap();
        let summary: Vec<_> = scores
            .iter()
            .map(|s| {
                (
                    s.key.prompt_version,
                    s.article_id,
                    s.score,
                    s.scored_at.as_str(),
                )
            })
            .collect();
        assert_eq!(
            summary,
            [
                (1, a, 80, "2026-09-26T04:00:00.000Z"),
                (2, a, 60, "2026-09-26T05:00:00.000Z"),
            ]
        );
        assert_eq!(
            scores[0].key,
            EvalKey {
                profile_hash: "h1".into(),
                backend: "claude-cli".into(),
                model: "sonnet".into(),
                prompt_version: 1,
            }
        );
    }
}
