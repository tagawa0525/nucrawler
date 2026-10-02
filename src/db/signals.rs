//! 利用者の反応：評価（1〜5）と、既読・ブックマークの印、開いた記録。
//! 評価のラベル（`eval`・確認枠・`profile suggest`）は評価だけから決め、印と開いた記録は使わない。

use super::*;

/// 評価の値（1〜5）：記事を自分に推薦すべきだったか。5 必読、4 読んでよかった、3 どちらでもない、
/// 2 不要、1 二度と出さないでほしい。評価なしは値を持たないこと（`Option::None`）で表し、3 とは区別する。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
#[serde(transparent)]
pub struct Rating(u8);

impl Rating {
    pub const MIN: u8 = 1;
    pub const MAX: u8 = 5;

    /// 1〜5 でなければ None。
    pub fn new(value: u8) -> Option<Self> {
        (Self::MIN..=Self::MAX)
            .contains(&value)
            .then_some(Self(value))
    }

    pub fn get(self) -> u8 {
        self.0
    }

    /// 関心（4〜5）
    pub fn is_positive(self) -> bool {
        self.0 >= 4
    }

    /// 不要（1〜2）
    pub fn is_negative(self) -> bool {
        self.0 <= 2
    }

    /// 段階の意味（画面と LLM への説明に使う）。
    pub fn meaning(self) -> &'static str {
        match self.0 {
            5 => "必読",
            4 => "読んでよかった",
            3 => "どちらでもない",
            2 => "不要",
            _ => "二度と出さないでほしい",
        }
    }

    /// 1 から 5 までの評価。
    pub fn all() -> impl DoubleEndedIterator<Item = Self> {
        (Self::MIN..=Self::MAX).map(Self)
    }
}

/// `ratings.value`（CHECK で 1〜5）を読む。範囲外なら読み出しの誤りにする。
impl rusqlite::types::FromSql for Rating {
    fn column_result(value: rusqlite::types::ValueRef<'_>) -> rusqlite::types::FromSqlResult<Self> {
        let n = i64::column_result(value)?;
        u8::try_from(n)
            .ok()
            .and_then(Self::new)
            .ok_or(rusqlite::types::FromSqlError::OutOfRange(n))
    }
}

impl rusqlite::ToSql for Rating {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        self.0.to_sql()
    }
}

/// 記事 1 件の印（一覧に戻ったときの読み直し用）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Marks {
    pub article_id: i64,
    pub rating: Option<Rating>,
    pub bookmarked: bool,
    pub read: bool,
}

/// 開いたものの種類（`events.kind`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenKind {
    /// 詳細を開いた
    Detail,
    /// 全文和訳を開いた
    Translation,
    /// 詳細の「原文」のリンクから元の記事を開いた
    Source,
}

impl OpenKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Detail => "open_detail",
            Self::Translation => "open_translation",
            Self::Source => "open_source",
        }
    }

    /// 開いたら既読にするか。既読は詳細（要約）の既読だけで、原文は詳細からしか開けないので数えない
    fn marks_read(self) -> bool {
        // 種類を足したときに既読を付けるかを決め忘れないよう、すべての種類を書く
        match self {
            Self::Detail | Self::Translation => true,
            Self::Source => false,
        }
    }
}

/// 既読にする（既に既読なら、最初に既読になった時刻のまま）。
fn mark_read(
    conn: &Connection,
    user_id: i64,
    article_id: i64,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<(), DbError> {
    conn.execute(
        "INSERT OR IGNORE INTO reads (user_id, article_id, read_at) VALUES (?1, ?2, ?3)",
        rusqlite::params![user_id, article_id, timestamp(now)],
    )?;
    Ok(())
}

impl Db {
    /// 指定した記事の印を、指定した順に返す。無い記事は返さない。
    pub fn marks(&self, user_id: i64, article_ids: &[i64]) -> Result<Vec<Marks>, DbError> {
        let ids = serde_json::to_string(article_ids)?;
        let mut stmt = self.conn.prepare(
            "SELECT a.id,
                    (SELECT value FROM ratings WHERE user_id = :user AND article_id = a.id),
                    EXISTS (SELECT 1 FROM bookmarks WHERE user_id = :user AND article_id = a.id),
                    EXISTS (SELECT 1 FROM reads WHERE user_id = :user AND article_id = a.id)
             FROM json_each(:ids) AS j
             JOIN articles AS a ON a.id = j.value
             ORDER BY j.key",
        )?;
        let rows = stmt.query_map(
            rusqlite::named_params! {":user": user_id, ":ids": ids},
            |r| {
                Ok(Marks {
                    article_id: r.get(0)?,
                    rating: r.get(1)?,
                    bookmarked: r.get(2)?,
                    read: r.get(3)?,
                })
            },
        )?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// 開いたことを記録する。詳細・全文和訳を開いたら既読にする（原文は既読を変えない）。
    pub fn record_open(
        &self,
        user_id: i64,
        article_id: i64,
        kind: OpenKind,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), DbError> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO events (user_id, article_id, kind, created_at) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![user_id, article_id, kind.as_str(), timestamp(now)],
        )?;
        if kind.marks_read() {
            mark_read(&tx, user_id, article_id, now)?;
        }
        tx.commit()?;
        Ok(())
    }

    /// 既読の印を付け外しする。
    pub fn set_read(
        &self,
        user_id: i64,
        article_id: i64,
        read: bool,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), DbError> {
        if read {
            mark_read(&self.conn, user_id, article_id, now)
        } else {
            self.conn.execute(
                "DELETE FROM reads WHERE user_id = ?1 AND article_id = ?2",
                rusqlite::params![user_id, article_id],
            )?;
            Ok(())
        }
    }

    /// ブックマーク（後で読む）の印を付け外しする。既に付いていれば、付けた時刻のまま。
    pub fn set_bookmark(
        &self,
        user_id: i64,
        article_id: i64,
        bookmarked: bool,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), DbError> {
        if bookmarked {
            self.conn.execute(
                "INSERT OR IGNORE INTO bookmarks (user_id, article_id, bookmarked_at)
                 VALUES (?1, ?2, ?3)",
                rusqlite::params![user_id, article_id, timestamp(now)],
            )?;
        } else {
            self.conn.execute(
                "DELETE FROM bookmarks WHERE user_id = ?1 AND article_id = ?2",
                rusqlite::params![user_id, article_id],
            )?;
        }
        Ok(())
    }

    /// 評価を付ける（付け直すと置き換わる）。`None` なら評価なしに戻す。
    /// 評価は「推薦すべきだったか」のラベルで、読んだかどうか（既読）とは別の印なので、既読は変えない。
    pub fn rate(
        &self,
        user_id: i64,
        article_id: i64,
        rating: Option<Rating>,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), DbError> {
        let tx = self.conn.unchecked_transaction()?;
        match rating {
            Some(rating) => {
                tx.execute(
                    "INSERT INTO ratings (user_id, article_id, value, rated_at)
                     VALUES (?1, ?2, ?3, ?4)
                     ON CONFLICT (user_id, article_id)
                     DO UPDATE SET value = excluded.value, rated_at = excluded.rated_at",
                    rusqlite::params![user_id, article_id, rating, timestamp(now)],
                )?;
            }
            None => {
                tx.execute(
                    "DELETE FROM ratings WHERE user_id = ?1 AND article_id = ?2",
                    rusqlite::params![user_id, article_id],
                )?;
            }
        }
        tx.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_support::*;

    fn rating(db: &Db, article_id: i64) -> Option<Rating> {
        db.search_articles(&search_query(db))
            .unwrap()
            .into_iter()
            .find(|i| i.article_id == article_id)
            .unwrap()
            .rating
    }

    #[test]
    fn ratings_are_one_to_five() {
        assert_eq!(Rating::new(0), None);
        assert_eq!(Rating::new(1).map(Rating::get), Some(1));
        assert_eq!(Rating::new(5).map(Rating::get), Some(5));
        assert_eq!(Rating::new(6), None);
    }

    /// 記事ごとに評価は 1 つで、付け直すと置き換わり、None で評価なしに戻る。
    #[test]
    fn rating_is_replaced_and_cleared() {
        let db = Db::open_in_memory().unwrap();
        let owner = db.owner_id().unwrap();
        let a = scored_article(
            &db,
            "https://e.com/a",
            Lang::En,
            "2026-09-26T00:00:00.000Z",
            90,
        );
        let four = Rating::new(4).unwrap();
        let two = Rating::new(2).unwrap();
        assert_eq!(rating(&db, a), None);
        db.rate(owner, a, Some(four), t("2026-09-27T00:00:00Z"))
            .unwrap();
        assert_eq!(rating(&db, a), Some(four));
        db.rate(owner, a, Some(two), t("2026-09-27T01:00:00Z"))
            .unwrap();
        assert_eq!(rating(&db, a), Some(two));
        assert_eq!(
            db.query_strings("SELECT rated_at FROM ratings").unwrap(),
            ["2026-09-27T01:00:00.000Z"]
        );
        db.rate(owner, a, None, t("2026-09-27T02:00:00Z")).unwrap();
        assert_eq!(rating(&db, a), None);
        assert_eq!(db.query_i64("SELECT count(*) FROM ratings").unwrap(), 0);
    }

    /// 評価 1〜2（不要）の記事は一覧の既定の評価の条件（★1〜2 を隠す）で隠れ、3 以上と未評価は残る。
    /// 最低点の「すべて」（00）は点数の条件だけを外すので、★1〜2 は隠れたまま。
    #[test]
    fn low_ratings_are_hidden_by_default() {
        let db = Db::open_in_memory().unwrap();
        let owner = db.owner_id().unwrap();
        let article =
            |url, score| scored_article(&db, url, Lang::En, "2026-09-26T00:00:00.000Z", score);
        let two = article("https://e.com/two", 90);
        let three = article("https://e.com/three", 80);
        let unrated = article("https://e.com/unrated", 70);
        for (id, value) in [(two, 2), (three, 3)] {
            db.rate(owner, id, Rating::new(value), t("2026-09-27T00:00:00Z"))
                .unwrap();
        }
        assert_eq!(list_ids(&db, false), [three, unrated]);
        assert_eq!(list_ids(&db, true), [three, unrated]);
        let hidden = |rating| {
            found(
                &db,
                SearchQuery {
                    hide: true,
                    hide_below: Some(60),
                    rating,
                    ..search_query(&db)
                },
            )
        };
        assert_eq!(hidden(RatingFilter::HideLow), [unrated, three]);
        assert_eq!(hidden(RatingFilter::Any), [unrated, three, two]);
    }

    fn item(db: &Db, article_id: i64) -> ListItem {
        db.search_articles(&search_query(db))
            .unwrap()
            .into_iter()
            .find(|i| i.article_id == article_id)
            .unwrap()
    }

    /// 開くと既読になり、既読の時刻は最初に既読になったときのまま。開いた記録は開くたびに残す。
    #[test]
    fn opening_marks_read_once() {
        let db = Db::open_in_memory().unwrap();
        let owner = db.owner_id().unwrap();
        let a = scored_article(
            &db,
            "https://e.com/a",
            Lang::En,
            "2026-09-26T00:00:00.000Z",
            90,
        );
        assert_eq!(item(&db, a).read_at, None);
        // 原文を開いても既読にはしない（既読は詳細の既読だけ）
        db.record_open(owner, a, OpenKind::Source, t("2026-09-26T12:00:00Z"))
            .unwrap();
        assert_eq!(item(&db, a).read_at, None);
        db.record_open(owner, a, OpenKind::Detail, t("2026-09-27T00:00:00Z"))
            .unwrap();
        db.record_open(owner, a, OpenKind::Translation, t("2026-09-27T01:00:00Z"))
            .unwrap();
        db.record_open(owner, a, OpenKind::Source, t("2026-09-27T02:00:00Z"))
            .unwrap();
        assert_eq!(
            item(&db, a).read_at.as_deref(),
            Some("2026-09-27T00:00:00.000Z")
        );
        assert_eq!(
            db.query_strings("SELECT kind FROM events ORDER BY id")
                .unwrap(),
            [
                "open_source",
                "open_detail",
                "open_translation",
                "open_source"
            ]
        );
    }

    /// 既読は開かなくても付け外しでき、付け直すとその時刻になる。
    #[test]
    fn read_mark_is_toggled() {
        let db = Db::open_in_memory().unwrap();
        let owner = db.owner_id().unwrap();
        let a = scored_article(
            &db,
            "https://e.com/a",
            Lang::En,
            "2026-09-26T00:00:00.000Z",
            90,
        );
        db.set_read(owner, a, true, t("2026-09-27T00:00:00Z"))
            .unwrap();
        db.set_read(owner, a, true, t("2026-09-27T01:00:00Z"))
            .unwrap();
        assert_eq!(
            item(&db, a).read_at.as_deref(),
            Some("2026-09-27T00:00:00.000Z")
        );
        db.set_read(owner, a, false, t("2026-09-27T02:00:00Z"))
            .unwrap();
        assert_eq!(item(&db, a).read_at, None);
        db.set_read(owner, a, true, t("2026-09-27T03:00:00Z"))
            .unwrap();
        assert_eq!(
            item(&db, a).read_at.as_deref(),
            Some("2026-09-27T03:00:00.000Z")
        );
        // 開いた記録は残さない（開いたわけではない）
        assert_eq!(db.query_i64("SELECT count(*) FROM events").unwrap(), 0);
    }

    /// 既読は「この話を読んだ」という印なので、同じ報道のグループ単位で見る。どれかを読めばグループの記事は
    /// どれも既読に見え（印の読み直しも同じ）、既読を外すとグループの記事すべての既読を外す（記事 1 件だけ外しても、
    /// 話は既読のままで一覧に戻らないため）。
    #[test]
    fn read_mark_covers_the_story() {
        let db = Db::open_in_memory().unwrap();
        let owner = db.owner_id().unwrap();
        let article = |url| scored_article(&db, url, Lang::En, "2026-09-26T00:00:00.000Z", 90);
        let a = article("https://e.com/a");
        let b = article("https://e.com/b");
        let other = article("https://e.com/other");
        group(&db, &[a, b]);
        db.set_read(owner, a, true, t("2026-09-27T00:00:00Z"))
            .unwrap();
        let read = |db: &Db| -> Vec<bool> {
            db.marks(owner, &[a, b, other])
                .unwrap()
                .into_iter()
                .map(|m| m.read)
                .collect()
        };
        assert_eq!(read(&db), [true, true, false]);
        assert!(item(&db, b).is_read());
        db.set_read(owner, other, true, t("2026-09-27T00:00:00Z"))
            .unwrap();
        // グループのほかの記事のカードで外しても、話の既読がすべて外れる。ほかの話は変えない
        db.set_read(owner, b, false, t("2026-09-27T01:00:00Z"))
            .unwrap();
        assert_eq!(read(&db), [false, false, true]);
        assert!(!item(&db, a).is_read());
    }

    /// 評価と既読は別の印で、評価しても既読にはならない。評価なしに戻しても既読は変わらない。
    #[test]
    fn rating_leaves_read_alone() {
        let db = Db::open_in_memory().unwrap();
        let owner = db.owner_id().unwrap();
        let a = scored_article(
            &db,
            "https://e.com/a",
            Lang::En,
            "2026-09-26T00:00:00.000Z",
            90,
        );
        db.rate(owner, a, Rating::new(4), t("2026-09-27T00:00:00Z"))
            .unwrap();
        assert_eq!(item(&db, a).read_at, None);
        db.set_read(owner, a, true, t("2026-09-27T01:00:00Z"))
            .unwrap();
        db.rate(owner, a, None, t("2026-09-27T02:00:00Z")).unwrap();
        assert_eq!(
            item(&db, a).read_at.as_deref(),
            Some("2026-09-27T01:00:00.000Z")
        );
    }

    /// ブックマークは付け外しでき、付けても一覧に残り、評価のラベルにはならない。
    #[test]
    fn bookmark_is_a_mark_kept_in_the_list() {
        let db = Db::open_in_memory().unwrap();
        let owner = db.owner_id().unwrap();
        let a = scored_article(
            &db,
            "https://e.com/a",
            Lang::En,
            "2026-09-26T00:00:00.000Z",
            90,
        );
        db.set_bookmark(owner, a, true, t("2026-09-27T00:00:00Z"))
            .unwrap();
        db.set_bookmark(owner, a, true, t("2026-09-27T01:00:00Z"))
            .unwrap();
        assert!(item(&db, a).bookmarked);
        assert_eq!(
            db.query_strings("SELECT bookmarked_at FROM bookmarks")
                .unwrap(),
            ["2026-09-27T00:00:00.000Z"]
        );
        assert_eq!(list_ids(&db, false), [a]);
        assert_eq!(
            found(
                &db,
                SearchQuery {
                    bookmarked: Some(true),
                    ..search_query(&db)
                }
            ),
            [a]
        );
        // ブックマークしていない記事だけ
        assert!(
            found(
                &db,
                SearchQuery {
                    bookmarked: Some(false),
                    ..search_query(&db)
                }
            )
            .is_empty()
        );
        assert!(db.eval_labels(owner).unwrap().is_empty());
        db.set_bookmark(owner, a, false, t("2026-09-27T02:00:00Z"))
            .unwrap();
        assert!(!item(&db, a).bookmarked);
        // ブックマークは既読にしない（後で読むため）
        assert_eq!(item(&db, a).read_at, None);
    }

    /// 既読は一覧の既定では隠さない（前の訪問より前に既読になった記事を隠すのは画面の区切り）。
    /// 未読の検索は既読を除く。
    #[test]
    fn read_articles_stay_in_the_list_and_leave_unread_search() {
        let db = Db::open_in_memory().unwrap();
        let owner = db.owner_id().unwrap();
        let a = scored_article(
            &db,
            "https://e.com/a",
            Lang::En,
            "2026-09-26T00:00:00.000Z",
            90,
        );
        let b = scored_article(
            &db,
            "https://e.com/b",
            Lang::En,
            "2026-09-26T00:00:00.000Z",
            80,
        );
        db.set_read(owner, a, true, t("2026-09-27T00:00:00Z"))
            .unwrap();
        assert_eq!(list_ids(&db, false), [a, b]);
        assert_eq!(
            found(
                &db,
                SearchQuery {
                    read: Some(false),
                    ..search_query(&db)
                }
            ),
            [b]
        );
    }

    /// 印の読み直し：指定した記事ごとに、評価・ブックマーク・既読を返す（無い記事は返さない）。
    #[test]
    fn marks_of_the_given_articles() {
        let db = Db::open_in_memory().unwrap();
        let owner = db.owner_id().unwrap();
        let article = |url| scored_article(&db, url, Lang::En, "2026-09-26T00:00:00.000Z", 80);
        let a = article("https://e.com/a");
        let b = article("https://e.com/b");
        let c = article("https://e.com/c");
        db.rate(owner, a, Rating::new(4), t("2026-09-27T00:00:00Z"))
            .unwrap();
        db.set_read(owner, a, true, t("2026-09-27T00:00:00Z"))
            .unwrap();
        db.set_bookmark(owner, b, true, t("2026-09-27T00:00:00Z"))
            .unwrap();
        let marks = db.marks(owner, &[a, b, 999]).unwrap();
        assert_eq!(
            marks,
            [
                Marks {
                    article_id: a,
                    rating: Rating::new(4),
                    bookmarked: false,
                    read: true,
                },
                Marks {
                    article_id: b,
                    rating: None,
                    bookmarked: true,
                    read: false,
                },
            ]
        );
        assert!(
            db.marks(owner, &[c]).unwrap()[0]
                == Marks {
                    article_id: c,
                    rating: None,
                    bookmarked: false,
                    read: false,
                }
        );
        assert!(db.marks(owner, &[]).unwrap().is_empty());
    }

    /// 検索は、指定した評価以上の記事に絞れる（評価なしは除く）。
    #[test]
    fn search_filters_by_minimum_rating() {
        let db = Db::open_in_memory().unwrap();
        let owner = db.owner_id().unwrap();
        let article = |url, published| scored_article(&db, url, Lang::En, published, 80);
        let five = article("https://e.com/five", "2026-09-26T00:00:00.000Z");
        let four = article("https://e.com/four", "2026-09-25T00:00:00.000Z");
        let three = article("https://e.com/three", "2026-09-24T00:00:00.000Z");
        let _unrated = article("https://e.com/unrated", "2026-09-23T00:00:00.000Z");
        for (id, value) in [(five, 5), (four, 4), (three, 3)] {
            db.rate(owner, id, Rating::new(value), t("2026-09-27T00:00:00Z"))
                .unwrap();
        }
        assert_eq!(
            found(
                &db,
                SearchQuery {
                    rating: RatingFilter::AtLeast(Rating::new(4).unwrap()),
                    ..search_query(&db)
                }
            ),
            [five, four]
        );
    }
}
