//! トピックの語彙・訳語集・プロファイル。

use super::*;

/// 語彙の語と、その使われ方（語彙の整理に使う）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TopicUsage {
    pub name: String,
    pub facet: crate::topics::Facet,
    /// 要約が提案して語彙に加えた時刻。初期語彙と `topics import` で入れた語は None
    pub added_at: Option<String>,
    /// この語が付いている要約の版の数
    pub uses: i64,
}

/// 語彙の統合：`from` の語を `into` にまとめ、`from` は以後 `into` の別名として扱う。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TopicMerge {
    pub from: String,
    pub into: String,
}

impl Db {
    /// ユーザーのプロファイルを保存する（既にあれば置き換える）。
    pub fn save_profile(
        &self,
        user_id: i64,
        profile: &crate::profile::Profile,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), DbError> {
        self.conn.execute(
            "INSERT INTO profiles (user_id, interests, excludes, hash, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT (user_id) DO UPDATE SET
               interests = excluded.interests,
               excludes = excluded.excludes,
               hash = excluded.hash,
               updated_at = excluded.updated_at",
            rusqlite::params![
                user_id,
                serde_json::to_string(&profile.interests)?,
                serde_json::to_string(&profile.exclude)?,
                crate::profile::hash(profile),
                timestamp(now),
            ],
        )?;
        Ok(())
    }

    /// トピックの語彙（登録順）。
    pub fn topics(&self) -> Result<Vec<crate::topics::Topic>, DbError> {
        let mut stmt = self
            .conn
            .prepare("SELECT name, facet FROM topics ORDER BY id")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        rows.map(|row| {
            let (name, facet) = row?;
            let facet = crate::topics::Facet::parse(&facet)
                .ok_or_else(|| DbError::UnexpectedValue(format!("topic facet {facet:?}")))?;
            Ok(crate::topics::Topic { name, facet })
        })
        .collect()
    }

    /// 訳語集（登録順）。原語も登録順に並べる。
    pub fn glossary_entries(&self) -> Result<Vec<crate::glossary::Entry>, DbError> {
        let mut stmt = self.conn.prepare(
            "SELECT t.id, t.target, t.abbr, t.note, t.changed_at, s.source, s.added_at
             FROM glossary_terms AS t JOIN glossary_sources AS s ON s.term_id = t.id
             ORDER BY t.id, s.rowid",
        )?;
        let mut rows = stmt.query([])?;
        let mut entries: Vec<crate::glossary::Entry> = Vec::new();
        while let Some(r) = rows.next()? {
            let id: i64 = r.get(0)?;
            let source: String = r.get(5)?;
            let added_at: Option<String> = r.get(6)?;
            match entries.last_mut() {
                Some(entry) if entry.id == id => {
                    entry.term.sources.push(source);
                    entry.sources_added_at.push(added_at);
                }
                _ => entries.push(crate::glossary::Entry {
                    id,
                    term: crate::glossary::Term {
                        sources: vec![source],
                        target: r.get(1)?,
                        abbr: r.get(2)?,
                        note: r.get(3)?,
                    },
                    term_changed_at: r.get(4)?,
                    sources_added_at: vec![added_at],
                }),
            }
        }
        Ok(entries)
    }

    /// 訳語を加えて id を返す。訳語・略語・原語がほかの訳語のものと重なれば、何も変えずに失敗する。
    pub fn add_glossary_term(
        &self,
        term: &crate::glossary::Term,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<i64, DbError> {
        let tx = self.conn.unchecked_transaction()?;
        check_glossary_conflicts(&tx, None, term)?;
        tx.execute(
            "INSERT INTO glossary_terms (target, abbr, note, changed_at) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![term.target, term.abbr, term.note, timestamp(now)],
        )?;
        let id = tx.last_insert_rowid();
        replace_glossary_sources(&tx, id, &term.sources, now)?;
        tx.commit()?;
        Ok(id)
    }

    /// 訳語を置き換える。無ければ false。訳語・略語・原語がほかの訳語のものと重なれば、
    /// 何も変えずに失敗する。残した原語は加えた時刻を保ち、訳・略語・メモは変わったときだけ時刻を進める。
    pub fn update_glossary_term(
        &self,
        id: i64,
        term: &crate::glossary::Term,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<bool, DbError> {
        use rusqlite::OptionalExtension;
        let tx = self.conn.unchecked_transaction()?;
        let exists = tx
            .query_row("SELECT 1 FROM glossary_terms WHERE id = ?1", [id], |_| {
                Ok(())
            })
            .optional()?
            .is_some();
        if !exists {
            return Ok(false);
        }
        check_glossary_conflicts(&tx, Some(id), term)?;
        tx.execute(
            "UPDATE glossary_terms SET target = ?2, abbr = ?3, note = ?4, changed_at = ?5
             WHERE id = ?1
               AND (target IS NOT ?2 OR abbr IS NOT ?3 OR note IS NOT ?4)",
            rusqlite::params![id, term.target, term.abbr, term.note, timestamp(now)],
        )?;
        replace_glossary_sources(&tx, id, &term.sources, now)?;
        tx.commit()?;
        Ok(true)
    }

    /// 訳語と原語を消す。無ければ false。
    pub fn delete_glossary_term(&self, id: i64) -> Result<bool, DbError> {
        Ok(self
            .conn
            .execute("DELETE FROM glossary_terms WHERE id = ?1", [id])?
            > 0)
    }

    /// 書き出す語彙（登録順）。LLM が足した語は追加した時刻を持つ。
    pub fn vocabulary(&self) -> Result<Vec<crate::topics::Entry>, DbError> {
        Ok(self
            .topic_usage()?
            .into_iter()
            .map(|u| crate::topics::Entry {
                name: u.name,
                facet: u.facet,
                added_at: u.added_at,
            })
            .collect())
    }

    /// 語彙を `topics` に置き換える。名前で突き合わせ、無い語は追加、軸が変わった語は更新し、
    /// 並びに無い語は削除する。要約に付いている語を消そうとしたら何も変えずに失敗する。
    pub fn replace_topics(&self, topics: &[crate::topics::Entry]) -> Result<(), DbError> {
        let names = serde_json::to_string(&topics.iter().map(|t| &t.name).collect::<Vec<_>>())?;
        let tx = self.conn.unchecked_transaction()?;
        let in_use: Vec<String> = {
            let mut stmt = tx.prepare(
                "SELECT DISTINCT t.name FROM topics AS t
                 JOIN artifact_topics AS at ON at.topic_id = t.id
                 WHERE t.name NOT IN (SELECT value FROM json_each(?1))
                 ORDER BY t.name",
            )?;
            stmt.query_map([&names], |r| r.get(0))?
                .collect::<Result<_, _>>()?
        };
        if !in_use.is_empty() {
            return Err(DbError::TopicsInUse(in_use));
        }
        tx.execute(
            "DELETE FROM topics WHERE name NOT IN (SELECT value FROM json_each(?1))",
            [&names],
        )?;
        for t in topics {
            // LLM が足した語かどうか（added_at）も語彙ファイルに従う
            tx.execute(
                "INSERT INTO topics (name, facet, added_at) VALUES (?1, ?2, ?3)
                 ON CONFLICT (name) DO UPDATE SET facet = excluded.facet, added_at = excluded.added_at",
                rusqlite::params![t.name, t.facet.as_str(), t.added_at],
            )?;
        }
        // 語として取り込んだ名前は、別名ではなくその語を指すようにする
        tx.execute(
            "DELETE FROM topic_aliases WHERE alias IN (SELECT value FROM json_each(?1))",
            [&names],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// 語彙の語と使われ方（登録順）。
    pub fn topic_usage(&self) -> Result<Vec<TopicUsage>, DbError> {
        let mut stmt = self.conn.prepare(
            "SELECT t.name, t.facet, t.added_at,
                    (SELECT count(*) FROM artifact_topics AS at WHERE at.topic_id = t.id)
             FROM topics AS t ORDER BY t.id",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get(2)?,
                r.get(3)?,
            ))
        })?;
        rows.map(|row| {
            let (name, facet, added_at, uses) = row?;
            let facet = crate::topics::Facet::parse(&facet)
                .ok_or_else(|| DbError::UnexpectedValue(format!("topic facet {facet:?}")))?;
            Ok(TopicUsage {
                name,
                facet,
                added_at,
                uses,
            })
        })
        .collect()
    }

    /// 語を統合する。要約への付与を統合先に付け替え、統合元の名前を別名として記録し、統合元を消す。
    /// 統合元を指していた別名も統合先に付け替える。どれか 1 つでも失敗したら何も変えない。
    /// `backend` と `model` は統合を決めた LLM（別名の記録に残す）。
    pub fn merge_topics(
        &self,
        merges: &[TopicMerge],
        backend: &str,
        model: &str,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), DbError> {
        use rusqlite::OptionalExtension;
        let tx = self.conn.unchecked_transaction()?;
        let topic_id = |name: &str| -> Result<i64, DbError> {
            tx.query_row("SELECT id FROM topics WHERE name = ?1", [name], |r| {
                r.get(0)
            })
            .optional()?
            .ok_or_else(|| DbError::UnknownTopic(name.to_string()))
        };
        for m in merges {
            if m.from == m.into {
                return Err(DbError::SelfMerge(m.from.clone()));
            }
            let from = topic_id(&m.from)?;
            let into = topic_id(&m.into)?;
            // 両方が付いている要約は、統合先の付与を残す
            tx.execute(
                "UPDATE OR IGNORE artifact_topics SET topic_id = ?2 WHERE topic_id = ?1",
                [from, into],
            )?;
            tx.execute("DELETE FROM artifact_topics WHERE topic_id = ?1", [from])?;
            tx.execute(
                "UPDATE topic_aliases SET topic_id = ?2 WHERE topic_id = ?1",
                [from, into],
            )?;
            tx.execute(
                "INSERT INTO topic_aliases (alias, topic_id, merged_at, backend, model)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![m.from, into, timestamp(now), backend, model],
            )?;
            tx.execute("DELETE FROM topics WHERE id = ?1", [from])?;
        }
        tx.commit()?;
        Ok(())
    }

    /// ユーザーのプロファイルとそのハッシュ。
    pub fn load_profile(
        &self,
        user_id: i64,
    ) -> Result<Option<(crate::profile::Profile, String)>, DbError> {
        use rusqlite::OptionalExtension;
        let row: Option<(String, String, String)> = self
            .conn
            .query_row(
                "SELECT interests, excludes, hash FROM profiles WHERE user_id = ?1",
                [user_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        row.map(|(interests, excludes, hash)| {
            let profile = crate::profile::Profile {
                interests: serde_json::from_str(&interests)?,
                exclude: serde_json::from_str(&excludes)?,
            };
            Ok((profile, hash))
        })
        .transpose()
    }

    /// ユーザーの今のプロファイルのハッシュ。プロファイルが無ければ `None`。
    pub fn profile_hash(&self, user_id: i64) -> Result<Option<String>, DbError> {
        todo!("{user_id}")
    }
}

/// 訳語・略語・原語が、`id` 以外の訳語のものと重なっていないか確かめる。
/// 原語は大文字小文字を問わず比べる（`glossary_sources.source` の照合順序）。
fn check_glossary_conflicts(
    conn: &Connection,
    id: Option<i64>,
    term: &crate::glossary::Term,
) -> Result<(), DbError> {
    use rusqlite::OptionalExtension;
    let owner = |sql: &str, value: &str| -> Result<Option<String>, DbError> {
        Ok(conn
            .query_row(sql, rusqlite::params![value, id], |r| r.get(0))
            .optional()?)
    };
    if owner(
        "SELECT target FROM glossary_terms WHERE target = ?1 AND id IS NOT ?2",
        &term.target,
    )?
    .is_some()
    {
        return Err(DbError::GlossaryConflict(format!(
            "訳語「{}」は登録済み",
            term.target
        )));
    }
    if let Some(abbr) = &term.abbr
        && let Some(target) = owner(
            "SELECT target FROM glossary_terms WHERE abbr = ?1 AND id IS NOT ?2",
            abbr,
        )?
    {
        return Err(DbError::GlossaryConflict(format!(
            "略語「{abbr}」は「{target}」で使っている"
        )));
    }
    for source in &term.sources {
        if let Some(target) = owner(
            "SELECT t.target FROM glossary_sources AS s
             JOIN glossary_terms AS t ON t.id = s.term_id
             WHERE s.source = ?1 AND s.term_id IS NOT ?2",
            source,
        )? {
            return Err(DbError::GlossaryConflict(format!(
                "原語「{source}」は「{target}」に登録済み"
            )));
        }
    }
    Ok(())
}

/// 訳語 `id` の原語を `sources` にする。表記の同じ原語は加えた時刻を保ち、
/// 無くした原語は消し、新しい原語（大文字小文字だけ変えたものを含む）は `now` に加えた扱いにする。
fn replace_glossary_sources(
    conn: &Connection,
    id: i64,
    sources: &[String],
    now: chrono::DateTime<chrono::Utc>,
) -> Result<(), DbError> {
    let sources = serde_json::to_string(sources)?;
    conn.execute(
        "DELETE FROM glossary_sources
         WHERE term_id = ?1
           AND source COLLATE BINARY NOT IN (SELECT value FROM json_each(?2))",
        rusqlite::params![id, sources],
    )?;
    conn.execute(
        "INSERT INTO glossary_sources (source, term_id, added_at)
         SELECT value, ?1, ?3 FROM json_each(?2) WHERE true
         ON CONFLICT (source) DO NOTHING",
        rusqlite::params![id, sources, timestamp(now)],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_support::linked_topics;
    use crate::db::test_support::*;

    #[test]
    fn saves_and_replaces_profiles() {
        let db = Db::open_in_memory().unwrap();
        let owner = db.owner_id().unwrap();
        assert_eq!(db.load_profile(owner).unwrap(), None);
        let mut p = crate::profile::parse(include_str!("../../examples/profile.toml")).unwrap();
        db.save_profile(owner, &p, t("2026-09-27T00:00:00Z"))
            .unwrap();
        let (loaded, hash) = db.load_profile(owner).unwrap().unwrap();
        assert_eq!(loaded, p);
        assert_eq!(hash, crate::profile::hash(&p));

        p.exclude.push("医療".into());
        db.save_profile(owner, &p, t("2026-09-28T00:00:00Z"))
            .unwrap();
        let (loaded, new_hash) = db.load_profile(owner).unwrap().unwrap();
        assert_eq!(loaded.exclude.last().map(String::as_str), Some("医療"));
        assert_ne!(new_hash, hash);
        assert_eq!(db.query_i64("SELECT count(*) FROM profiles").unwrap(), 1);
    }

    #[test]
    fn reads_only_the_profile_hash() {
        let db = Db::open_in_memory().unwrap();
        let owner = db.owner_id().unwrap();
        assert_eq!(db.profile_hash(owner).unwrap(), None);
        let p = crate::profile::parse(include_str!("../../examples/profile.toml")).unwrap();
        db.save_profile(owner, &p, t("2026-09-27T00:00:00Z"))
            .unwrap();
        let (_, hash) = db.load_profile(owner).unwrap().unwrap();
        assert_eq!(db.profile_hash(owner).unwrap(), Some(hash));
    }

    fn vocab(names: &[(&str, crate::topics::Facet)]) -> Vec<crate::topics::Entry> {
        names
            .iter()
            .map(|&(name, facet)| crate::topics::Entry {
                name: name.into(),
                facet,
                added_at: None,
            })
            .collect()
    }

    fn glossary_entry(db: &Db, id: i64) -> crate::glossary::Entry {
        db.glossary_entries()
            .unwrap()
            .into_iter()
            .find(|e| e.id == id)
            .unwrap()
    }

    /// 初期値の語は変更した時刻を持たない。加えた語は加えた時刻を持つ。
    #[test]
    fn glossary_terms_are_added_with_their_time() {
        let db = Db::open_in_memory().unwrap();
        assert!(
            db.glossary_entries()
                .unwrap()
                .iter()
                .all(|e| e.changed_at().is_none())
        );
        let term = glossary_term(
            &["emergency diesel generator", "EDG"],
            "非常用ディーゼル発電機",
            Some("EDG"),
        );
        let id = db
            .add_glossary_term(&term, t("2026-09-27T00:00:00Z"))
            .unwrap();
        let entry = glossary_entry(&db, id);
        assert_eq!(entry.term, term);
        assert_eq!(
            entry.changed_at(),
            Some(timestamp(t("2026-09-27T00:00:00Z")).as_str())
        );
        assert!(
            db.glossary_entries()
                .unwrap()
                .iter()
                .any(|e| e.term == term)
        );
    }

    /// 置き換えでは、残した原語はそのまま、足した原語は加え、無くした原語は消す。
    /// 変更の時刻は、訳語か原語が変わったときだけ進む。
    #[test]
    fn glossary_term_update_replaces_sources_and_tracks_changes() {
        let db = Db::open_in_memory().unwrap();
        let id = db
            .add_glossary_term(
                &glossary_term(&["reactor coolant pump", "RCP"], "一次冷却材ポンプ", None),
                t("2026-09-27T00:00:00Z"),
            )
            .unwrap();
        let later = t("2026-09-27T00:00:00Z") + chrono::Duration::hours(1);
        let updated = glossary_term(
            &["reactor coolant pump", "primary coolant pump"],
            "一次冷却材ポンプ",
            None,
        );
        assert!(db.update_glossary_term(id, &updated, later).unwrap());
        let entry = glossary_entry(&db, id);
        assert_eq!(entry.term, updated);
        assert_eq!(entry.changed_at(), Some(timestamp(later).as_str()));
        // 同じ内容で保存しても変更にならない
        let even_later = later + chrono::Duration::hours(1);
        assert!(db.update_glossary_term(id, &updated, even_later).unwrap());
        assert_eq!(
            glossary_entry(&db, id).changed_at(),
            Some(timestamp(later).as_str())
        );
        assert!(!db.update_glossary_term(9999, &updated, later).unwrap());
    }

    /// ほかの訳語の原語・訳語・略語と重なれば、その旨を返して何も変えない。
    #[test]
    fn glossary_rejects_conflicts_without_writing() {
        let db = Db::open_in_memory().unwrap();
        let before = db.glossary_entries().unwrap();
        for term in [
            glossary_term(&["new term", "atf"], "新しい語", None),
            glossary_term(&["new term"], "事故耐性燃料", None),
            glossary_term(&["new term"], "新しい語", Some("ATF")),
        ] {
            let err = db
                .add_glossary_term(&term, t("2026-09-27T00:00:00Z"))
                .unwrap_err();
            assert!(matches!(err, DbError::GlossaryConflict(_)), "{err:?}");
        }
        let err = db
            .add_glossary_term(
                &glossary_term(&["new term", "ATF"], "新しい語", None),
                t("2026-09-27T00:00:00Z"),
            )
            .unwrap_err();
        assert!(err.to_string().contains("事故耐性燃料"), "{err}");
        // 自分の原語はそのまま保存できる
        let atf = before
            .iter()
            .find(|e| e.term.target == "事故耐性燃料")
            .unwrap();
        assert!(
            db.update_glossary_term(atf.id, &atf.term, t("2026-09-27T00:00:00Z"))
                .unwrap()
        );
        assert_eq!(db.glossary_entries().unwrap(), before);
    }

    #[test]
    fn glossary_terms_can_be_deleted() {
        let db = Db::open_in_memory().unwrap();
        let id = db
            .add_glossary_term(
                &glossary_term(&["scrams"], "スクラム回数", None),
                t("2026-09-27T00:00:00Z"),
            )
            .unwrap();
        assert!(db.delete_glossary_term(id).unwrap());
        assert!(!db.delete_glossary_term(id).unwrap());
        assert_eq!(
            db.query_i64(&format!(
                "SELECT count(*) FROM glossary_sources WHERE term_id = {id}"
            ))
            .unwrap(),
            0
        );
    }

    /// 同じ原語（大文字小文字の違いを含む）を別の訳語に結び付けられない。
    #[test]
    fn glossary_rejects_a_source_of_two_terms() {
        let db = Db::open_in_memory().unwrap();
        let err = db
            .conn()
            .execute_batch(
                "INSERT INTO glossary_terms (target) VALUES ('運転許可更新');
                 INSERT INTO glossary_sources (term_id, source)
                 SELECT id, 'License Renewal' FROM glossary_terms WHERE target = '運転許可更新';",
            )
            .unwrap_err();
        assert!(err.to_string().contains("UNIQUE"), "{err}");
    }

    fn propose(db: &Db, name: &str) -> i64 {
        digest_with_topics(
            db,
            serde_json::json!([name]),
            serde_json::json!([{"name": name, "facet": "分野"}]),
        )
        .unwrap()
    }

    fn aliases(db: &Db) -> Vec<String> {
        db.query_strings(
            "SELECT a.alias || '>' || t.name || '|' || a.backend || '|' || a.model || '|' || a.merged_at
             FROM topic_aliases AS a JOIN topics AS t ON t.id = a.topic_id ORDER BY a.alias",
        )
        .unwrap()
    }

    #[test]
    fn merge_topics_moves_links_and_records_aliases() {
        let db = Db::open_in_memory().unwrap();
        let curated = digest_with_topics(
            &db,
            serde_json::json!(["新設・建設"]),
            serde_json::json!([]),
        )
        .unwrap();
        let proposed = propose(&db, "新設炉");
        let both = digest_with_topics(
            &db,
            serde_json::json!(["新設・建設", "新設炉"]),
            serde_json::json!([]),
        )
        .unwrap();
        db.merge_topics(
            &[merge("新設炉", "新設・建設")],
            "claude-cli",
            "sonnet",
            t("2026-10-04T00:00:00Z"),
        )
        .unwrap();
        for id in [curated, proposed, both] {
            assert_eq!(linked_topics(&db, id), ["新設・建設"], "digest {id}");
        }
        assert!(db.topics().unwrap().iter().all(|t| t.name != "新設炉"));
        assert_eq!(
            aliases(&db),
            ["新設炉>新設・建設|claude-cli|sonnet|2026-10-04T00:00:00.000Z"]
        );
    }

    /// 統合した語を後でさらに統合しても、古い別名は最終的な統合先を指す。
    #[test]
    fn merge_topics_repoints_aliases_of_the_merged_topic() {
        let db = Db::open_in_memory().unwrap();
        propose(&db, "新設炉");
        propose(&db, "新規建設");
        let at = t("2026-10-04T00:00:00Z");
        db.merge_topics(&[merge("新設炉", "新規建設")], "b", "m", at)
            .unwrap();
        db.merge_topics(&[merge("新規建設", "新設・建設")], "b", "m", at)
            .unwrap();
        let targets: Vec<String> = aliases(&db)
            .iter()
            .map(|a| a.split('|').next().unwrap().to_string())
            .collect();
        assert_eq!(targets, ["新規建設>新設・建設", "新設炉>新設・建設"]);
    }

    /// 統合した語を LLM がまた付けたり提案したりしても、統合先に付き、語彙に戻らない。
    #[test]
    fn saved_digests_resolve_aliases() {
        let db = Db::open_in_memory().unwrap();
        propose(&db, "新設炉");
        db.merge_topics(
            &[merge("新設炉", "新設・建設")],
            "b",
            "m",
            t("2026-10-04T00:00:00Z"),
        )
        .unwrap();
        let again = propose(&db, "新設炉");
        assert_eq!(linked_topics(&db, again), ["新設・建設"]);
        assert!(db.topics().unwrap().iter().all(|t| t.name != "新設炉"));
    }

    /// 統合した語を付けた要約も、詳細・API・MCP・採点では統合先の語で見せる
    /// （payload は LLM の出力の記録として書き換えず、読み出しを付与にそろえる）。
    #[test]
    fn digest_topics_are_read_from_links_after_merges() {
        let db = Db::open_in_memory().unwrap();
        let owner = db.owner_id().unwrap();
        let a = db
            .insert_article(&article("https://e.com/merged"))
            .unwrap()
            .unwrap();
        let c = db
            .insert_content(a, ContentKind::Body, ContentOrigin::Page, "body")
            .unwrap();
        let payload = serde_json::json!({
            "title_ja": "題", "summary_ja": "要約", "points_ja": ["点"], "implications_ja": "",
            "lwr_relevant": true, "topics": ["燃料", "新設炉"],
            "new_topics": [{"name": "新設炉", "facet": "分野"}],
        });
        db.insert_artifact(
            &NewArtifact {
                article_id: a,
                kind: ArtifactKind::Digest,
                backend: "claude-cli",
                model: "sonnet",
                prompt_version: 2,
                payload: &payload,
                inputs: &[c],
                glossary_at: None,
            },
            t("2026-09-27T01:00:00Z"),
        )
        .unwrap();
        db.merge_topics(
            &[merge("新設炉", "新設・建設")],
            "b",
            "m",
            t("2026-10-04T00:00:00Z"),
        )
        .unwrap();

        let detail = db.article_detail(owner, None, a).unwrap().unwrap();
        assert_eq!(
            detail.digests[0].payload["topics"],
            serde_json::json!(["燃料", "新設・建設"]),
            "in vocabulary order"
        );
        let inputs = db
            .pending_score(
                score_key(&db),
                t("2026-09-10T00:00:00Z"),
                t("2026-09-28T00:00:00Z"),
                10,
            )
            .unwrap();
        assert_eq!(inputs[0].topics, ["燃料", "新設・建設"]);
    }

    #[test]
    fn merge_topics_changes_nothing_on_failure() {
        let db = Db::open_in_memory().unwrap();
        let id = propose(&db, "新設炉");
        let at = t("2026-10-04T00:00:00Z");
        let err = db
            .merge_topics(
                &[merge("新設炉", "新設・建設"), merge("無い語", "燃料")],
                "b",
                "m",
                at,
            )
            .unwrap_err();
        assert!(
            matches!(&err, DbError::UnknownTopic(name) if name == "無い語"),
            "{err}"
        );
        let err = db
            .merge_topics(&[merge("新設炉", "新設炉")], "b", "m", at)
            .unwrap_err();
        assert!(
            matches!(&err, DbError::SelfMerge(name) if name == "新設炉"),
            "{err}"
        );
        assert_eq!(linked_topics(&db, id), ["新設炉"]);
        assert!(aliases(&db).is_empty());
    }

    /// 手で取り込んだ語彙に別名と同じ名前があれば、その名前は語として復活し、別名ではなくなる。
    #[test]
    fn importing_an_alias_name_makes_it_a_topic_again() {
        use crate::topics::{Entry, Facet};
        let db = Db::open_in_memory().unwrap();
        propose(&db, "新設炉");
        db.merge_topics(
            &[merge("新設炉", "新設・建設")],
            "b",
            "m",
            t("2026-10-04T00:00:00Z"),
        )
        .unwrap();
        let mut topics = db.vocabulary().unwrap();
        topics.push(Entry {
            name: "新設炉".into(),
            facet: Facet::Reactor,
            added_at: None,
        });
        db.replace_topics(&topics).unwrap();
        assert!(aliases(&db).is_empty());
        let id =
            digest_with_topics(&db, serde_json::json!(["新設炉"]), serde_json::json!([])).unwrap();
        assert_eq!(linked_topics(&db, id), ["新設炉"]);
    }

    #[test]
    fn topic_usage_counts_digests_and_marks_proposals() {
        use crate::topics::Facet;
        let db = Db::open_in_memory().unwrap();
        digest_with_topics(&db, serde_json::json!(["燃料"]), serde_json::json!([])).unwrap();
        propose(&db, "データセンター需要");
        digest_with_topics(
            &db,
            serde_json::json!(["燃料", "データセンター需要"]),
            serde_json::json!([]),
        )
        .unwrap();
        let usage = db.topic_usage().unwrap();
        assert_eq!(usage.len(), db.topics().unwrap().len());
        let fuel = usage.iter().find(|u| u.name == "燃料").unwrap();
        assert_eq!(
            (fuel.facet, fuel.added_at.as_deref(), fuel.uses),
            (Facet::Field, None, 2)
        );
        let dc = usage.last().unwrap();
        assert_eq!(dc.name, "データセンター需要");
        assert_eq!(dc.added_at.as_deref(), Some("2026-09-27T00:00:00.000Z"));
        assert_eq!(dc.uses, 2);
        assert_eq!(usage.iter().find(|u| u.name == "PWR").unwrap().uses, 0);
    }

    #[test]
    fn replace_topics_adds_updates_and_removes_by_name() {
        use crate::topics::Facet;
        let db = Db::open_in_memory().unwrap();
        let first = vocab(&[("燃料", Facet::Field), ("PWR", Facet::Reactor)]);
        db.replace_topics(&first).unwrap();
        assert_eq!(db.vocabulary().unwrap(), first);
        let fuel_id: i64 = db
            .conn()
            .query_row("SELECT id FROM topics WHERE name = '燃料'", [], |r| {
                r.get(0)
            })
            .unwrap();

        let second = vocab(&[("燃料", Facet::Reactor), ("米国", Facet::Region)]);
        db.replace_topics(&second).unwrap();
        assert_eq!(db.vocabulary().unwrap(), second);
        let kept_id: i64 = db
            .conn()
            .query_row("SELECT id FROM topics WHERE name = '燃料'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(kept_id, fuel_id, "an updated topic keeps its id");
    }

    /// LLM が足した語かどうかは語彙ファイルの added_at で決まる。書き出した語彙を取り込み直しても変わらず、
    /// added_at を消して取り込めば人が決めた語になる（整理で統合されない）。
    #[test]
    fn import_decides_whether_topics_are_proposed() {
        let db = Db::open_in_memory().unwrap();
        propose(&db, "新設炉");
        let exported = db.vocabulary().unwrap();
        let proposed = exported.iter().find(|e| e.name == "新設炉").unwrap();
        assert_eq!(
            proposed.added_at.as_deref(),
            Some("2026-09-27T00:00:00.000Z")
        );
        assert!(
            exported
                .iter()
                .filter(|e| e.name != "新設炉")
                .all(|e| e.added_at.is_none())
        );

        db.replace_topics(&exported).unwrap();
        assert_eq!(
            db.vocabulary().unwrap(),
            exported,
            "round trip keeps origins"
        );

        let curated: Vec<_> = exported
            .iter()
            .cloned()
            .map(|e| crate::topics::Entry {
                added_at: None,
                ..e
            })
            .collect();
        db.replace_topics(&curated).unwrap();
        let usage = db.topic_usage().unwrap();
        assert!(usage.iter().all(|u| u.added_at.is_none()));
    }

    #[test]
    fn replace_topics_refuses_to_remove_topics_in_use() {
        use crate::topics::Facet;
        let db = Db::open_in_memory().unwrap();
        let before = vocab(&[("燃料", Facet::Field), ("PWR", Facet::Reactor)]);
        db.replace_topics(&before).unwrap();
        let a = db
            .insert_article(&article("https://e.com/a"))
            .unwrap()
            .unwrap();
        let digest = insert_artifact(&db, a, "public");
        db.conn()
            .execute(
                "INSERT INTO artifact_topics (artifact_id, topic_id)
                 SELECT ?1, id FROM topics WHERE name = 'PWR'",
                [digest],
            )
            .unwrap();
        let err = db
            .replace_topics(&vocab(&[("燃料", Facet::Region)]))
            .unwrap_err();
        assert!(
            matches!(&err, DbError::TopicsInUse(names) if names == &["PWR"]),
            "{err}"
        );
        assert_eq!(
            db.vocabulary().unwrap(),
            before,
            "nothing changes on failure"
        );

        // 要約の版が消えれば付与も消え、語を削除できる
        db.conn()
            .execute("DELETE FROM artifacts WHERE id = ?1", [digest])
            .unwrap();
        db.replace_topics(&vocab(&[("燃料", Facet::Field)]))
            .unwrap();
    }
}
