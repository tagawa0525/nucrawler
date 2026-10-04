//! 一覧の画面（既定の一覧・確認枠・評価やブックマークでの絞り込み）。

use super::*;

/// Web の一覧の条件（設定の期間・件数と、表示する最低点）。最低点が無ければ推薦点で絞らず、0 なら、未採点・
/// 軽水炉と無関係の記事も出す（すべて）。既読・評価は一覧の既定と同じく未読だけ・★1〜2 を隠す（フィード・JSON の
/// 一覧もこれを使う）。
fn list_query<'a>(
    web: &WebConfig,
    user: i64,
    profile_hash: Option<&'a str>,
    now: chrono::DateTime<Utc>,
    min_score: Option<u8>,
) -> ListQuery<'a> {
    ListQuery {
        user_id: user,
        profile_hash,
        min_score,
        since: now - Duration::days(web.list_days.into()),
        show_all: min_score == Some(0),
        read: Some(false),
        bookmarked: None,
        rating: RatingFilter::HideLow,
        limit: web.list_limit,
    }
}

/// Web の一覧に出す記事（既読・評価は一覧の既定、評価は `rating` があればその条件）。
pub(super) fn list_items(
    db: &Db,
    web: &WebConfig,
    user: i64,
    profile_hash: Option<&str>,
    now: chrono::DateTime<Utc>,
    min_score: Option<u8>,
    rating: Option<RatingFilter>,
) -> Result<Vec<crate::db::ListItem>, DbError> {
    let query = list_query(web, user, profile_hash, now, min_score);
    db.list_articles(ListQuery {
        rating: rating.unwrap_or(query.rating),
        ..query
    })
}

#[derive(serde::Deserialize)]
pub(super) struct ListParams {
    /// 表示する最低点（0〜100）。無ければ既定（`default_min`）
    min: Option<String>,
    /// Web の一覧だけが使う（`1` なら既読も出し、`0` なら隠す。無ければ一覧は隠し、絞り込みは出す）
    read: Option<String>,
    /// 評価で絞る（`rating` を見る）
    rating: Option<String>,
    /// Web の一覧だけが使う（ブックマークに絞る）
    bookmarked: Option<String>,
    /// Web の一覧だけが使う（JavaScript が無いときの評価の選択を、一覧（`list`）と絞り込み（`filtered`）の
    /// どちらから送ったか）
    from: Option<String>,
}

impl ListParams {
    /// 表示する最低点。無ければ（JavaScript が無いときの選択で送られる空の値も）`default`。
    pub(super) fn min_or(&self, default: Option<u8>) -> Result<Option<u8>, AppError> {
        match self.min.as_deref() {
            None | Some("") => Ok(default),
            Some(v) => v
                .parse()
                .ok()
                .filter(|min| *min <= 100)
                .map(Some)
                .ok_or(AppError::BadRequest("min must be 0..=100")),
        }
    }

    /// 評価の条件。`any` は絞らない、`hide-low` は ★1〜2 を隠す、1〜5 は ★N 以上、0 は評価の無い記事だけ。
    /// 無ければ（空の値も）`None`（既定）。
    pub(super) fn rating(&self) -> Result<Option<RatingFilter>, AppError> {
        let Some(value) = self.rating.as_deref().filter(|v| !v.is_empty()) else {
            return Ok(None);
        };
        let rating = match value {
            "any" => RatingFilter::Any,
            "hide-low" => RatingFilter::HideLow,
            "0" => RatingFilter::Unrated,
            v => v
                .parse()
                .ok()
                .and_then(crate::db::Rating::new)
                .map(RatingFilter::AtLeast)
                .ok_or(AppError::BadRequest(
                    "rating must be any, hide-low or 0..=5",
                ))?,
        };
        Ok(Some(rating))
    }

    /// JavaScript が無いときの評価の選択を、絞り込み（`Some(true)`）と一覧（`Some(false)`）のどちらから送ったか。
    fn sent_from_filtered(&self) -> Result<Option<bool>, AppError> {
        match self.from.as_deref() {
            None => Ok(None),
            Some("filtered") => Ok(Some(true)),
            Some("list") => Ok(Some(false)),
            Some(_) => Err(AppError::BadRequest("from must be list or filtered")),
        }
    }
}

/// 一覧の既読で絞る値。`1` は既読だけ、`0` は未読だけ、`any` は絞らない（`Some(None)`）。無ければ `None`（既定）。
fn read_mark(value: Option<&str>) -> Result<Option<Option<bool>>, AppError> {
    match value {
        None => Ok(None),
        Some("1") => Ok(Some(Some(true))),
        Some("0") => Ok(Some(Some(false))),
        Some("any") => Ok(Some(None)),
        Some(_) => Err(AppError::BadRequest("read must be 1, 0 or any")),
    }
}

/// 一覧のブックマークで絞る値。`1` はブックマーク中だけ、`0` はしていない記事だけ。無ければ絞らない。
fn bookmark_mark(value: Option<&str>) -> Result<Option<bool>, AppError> {
    match value {
        None => Ok(None),
        Some("1") => Ok(Some(true)),
        Some("0") => Ok(Some(false)),
        Some(_) => Err(AppError::BadRequest("bookmarked must be 1 or 0")),
    }
}

pub(super) async fn list(
    State(state): State<AppState>,
    Extension(me): Extension<crate::db::Viewer>,
    Query(params): Query<ListParams>,
    RawQuery(raw): RawQuery,
) -> Result<Response, AppError> {
    // 既定の規則（一覧か絞り込みか、それぞれの最低点と既読の既定）は `ListView` にだけ置き、正規の URL と
    // そろえる。絞り込みの条件を先に入れ、最低点と既読は既定を受け取ってから決める
    // 評価の既定も一覧か絞り込みか（ブックマーク中だけか）で決まるので、指定が無ければ既定を後で決める
    let rating = params.rating()?;
    let mut view = html::ListView {
        min: None,
        default_min: None,
        read: None,
        rating: rating.unwrap_or(RatingFilter::Any),
        bookmarked: bookmark_mark(params.bookmarked.as_deref())?,
    };
    view.rating = rating.unwrap_or(view.rating_default());
    // JavaScript が無いときの評価の選択は、送った画面の条件（最低点・既読）も一緒に送る。一覧と絞り込みを行き来した
    // なら、それらは使わずに行き先の既定にする（JavaScript があれば、選択肢の正規の URL へ移るので送られない）
    let crossing = params
        .sent_from_filtered()?
        .is_some_and(|from| from != view.filtered());
    let carried = |value: Option<&str>| value.filter(|_| !crossing).map(str::to_string);
    let params = ListParams {
        min: carried(params.min.as_deref()),
        read: carried(params.read.as_deref()),
        ..params
    };
    // 正規の URL がプロファイルの有無で変わる（一覧の既定の最低点）ので、利用者を先に引く
    let (user, hash) = with_db(&state, move |db| Ok(viewer(db, me)?)).await?;
    view.default_min = state.web.default_min(hash.as_deref());
    view.min = params.min_or(view.min_default())?;
    view.read = read_mark(params.read.as_deref())?.unwrap_or(view.read_default());
    // 正規の形でなければ（既定と同じ値・空の値・送った画面（`from`）が残っているなど）、正規の URL へ移す
    let canonical = view.url();
    let requested = match raw.as_deref() {
        None | Some("") => "/".to_string(),
        Some(q) => format!("/?{q}"),
    };
    if requested != canonical {
        return Ok(Redirect::to(&canonical).into_response());
    }
    let page = with_db_and_config(&state, move |db, web, labels| {
        let now = Utc::now();
        if view.filtered() {
            return filtered(db, web, labels, me, hash.as_deref(), view);
        }
        let boundary =
            db.begin_visit(user, now, Duration::minutes(web.visit_gap_minutes.into()))?;
        // 既読・ブックマークでは件数の上限より前に絞る（上位が既読で埋まっても、下の未読が出るように）
        let query = ListQuery {
            read: view.read,
            bookmarked: view.bookmarked,
            rating: view.rating,
            ..list_query(web, user, hash.as_deref(), now, view.min)
        };
        let items = db.list_articles(query)?;
        let hidden = hidden_counts(db, query)?;
        let (new, earlier) = html::split_sections(items, boundary.as_deref());
        // 「すべて」では閾値未満も並んでいるので、確認枠は出さない。既定の最低点が無ければ（プロファイルが無い）、
        // 閾値未満という区別も無いので出さない
        let explore = if let Some(floor) = view.default_min.filter(|_| !view.shows_all()) {
            // 見逃し率を偏りなく測るため、選ぶ基準は画面で選んだ最低点ではなく既定の最低点
            let today = now.with_timezone(&crate::jst::offset()).format("%Y-%m-%d");
            let picks = db.explore(
                list_query(web, user, hash.as_deref(), now, Some(floor)),
                web.explore_per_day as usize,
                &today.to_string(),
            )?;
            // 最低点を下げて一覧に既に出ている記事は、同じ報道のグループごと重ねない
            let listed: std::collections::HashSet<i64> =
                new.iter().chain(&earlier).map(|i| i.story_id).collect();
            let picks = picks
                .into_iter()
                .filter(|i| !listed.contains(&i.story_id))
                .collect();
            // 一覧と同じく、既読・ブックマークで絞る
            html::filter_read(picks, view.read)
                .into_iter()
                .filter(|i| view.bookmarked.is_none_or(|b| i.bookmarked == b))
                .collect()
        } else {
            Vec::new()
        };
        let parts = PageParts::new(db, me, hash.as_deref(), web)?;
        let page = parts.page(labels);
        Ok(html::list_page_with_explore(
            &new, &earlier, &explore, view, hidden, &page,
        ))
    })
    .await?;
    Ok(Html(page).into_response())
}

/// 一覧の条件をそれぞれ 1 つだけ外したときに加わる記事の数（その条件で隠れている記事の数）。外したときに出る記事の
/// うち、今は出ていないものを数える（同じ報道のカードの代表が入れ替わるだけでも、外すと出る記事は数える）。件数の
/// 上限は掛けずに、記事を組み立てずに数える。
fn hidden_counts(db: &Db, query: ListQuery) -> Result<html::HiddenCounts, DbError> {
    let shown: std::collections::HashSet<i64> = db.list_article_ids(query)?.into_iter().collect();
    // 外す条件が効いていなければ数えない
    let added = |effective: bool, lifted: ListQuery| -> Result<usize, DbError> {
        if !effective {
            return Ok(0);
        }
        let ids = db.list_article_ids(lifted)?;
        Ok(ids.iter().filter(|id| !shown.contains(id)).count())
    };
    Ok(html::HiddenCounts {
        min: added(
            !query.show_all && query.min_score.is_some(),
            ListQuery {
                min_score: Some(0),
                show_all: true,
                ..query
            },
        )?,
        read: added(
            query.read.is_some(),
            ListQuery {
                read: None,
                ..query
            },
        )?,
        rating: added(
            query.rating != RatingFilter::Any,
            ListQuery {
                rating: RatingFilter::Any,
                ..query
            },
        )?,
        bookmarked: added(
            query.bookmarked.is_some(),
            ListQuery {
                bookmarked: None,
                ..query
            },
        )?,
    })
}

/// 評価・ブックマークで絞った記事を、検索と同じく全期間から新しい順に出す（既読の表示は 👁 のとおり）。
/// 検索と同じく閲覧ではないので、訪問は始めない。
fn filtered(
    db: &Db,
    web: &WebConfig,
    labels: &crate::config::SourceLabels,
    me: crate::db::Viewer,
    hash: Option<&str>,
    view: html::ListView,
) -> Result<String, AppError> {
    let user = me.user_id;
    let params = Params {
        bookmarked: view.bookmarked,
        read: view.read,
        min_score: view
            .min
            .filter(|m| *m > 0)
            .map(|m| m.to_string())
            .unwrap_or_default(),
        ..Params::default()
    };
    let query = SearchQuery {
        rating: view.rating,
        ..params.to_query(user, hash, web.list_limit)?
    };
    let items = db.search_articles(&query)?;
    let parts = PageParts::new(db, me, hash, web)?;
    let page = parts.page(labels);
    Ok(html::filtered_page(&items, view, &page))
}

#[cfg(test)]
mod tests {
    use crate::db::Db;
    use crate::web::server::test_support::*;

    /// 既定の一覧には、閾値未満の記事を確認枠として出す。「すべて表示」では全部出ているので出さない。
    #[tokio::test]
    async fn list_shows_below_threshold_articles_in_the_explore_section() {
        let db = Db::open_in_memory().unwrap();
        seed_recommended_and_hidden(&db);
        let server = Server::start(db).await;
        let (status, html) = server.get("/").await;
        assert_eq!(status, 200);
        let section = html
            .split("<h2>確認枠</h2>")
            .nth(1)
            .expect("explore section");
        assert!(section.contains("低い点"), "{html}");
        // 👎・無関係・未採点は候補にしない
        for hidden in ["評価 2", "無関係", "採点前の記事"] {
            assert!(!section.contains(hidden), "{hidden}: {html}");
        }
        let (_, all) = server.get("/?min=0").await;
        assert!(!all.contains("確認枠"), "{all}");
    }

    /// 表示する最低点は `min` で選べる。0 は未採点なども含めてすべて。
    /// 確認枠は設定の最低点で選び、一覧に既に出ている記事は重ねて出さない。
    #[tokio::test]
    async fn list_takes_the_minimum_score() {
        let db = Db::open_in_memory().unwrap();
        let (_, digest) = seed(&db, "https://e.com/forty", "四十点");
        score(&db, digest, 40);
        let (_, digest) = seed(&db, "https://e.com/twenty", "二十点");
        score(&db, digest, 20);
        seed(&db, "https://e.com/unscored", "採点前の記事");
        let server = Server::start(db).await;
        let (status, html) = server.get("/?min=30").await;
        assert_eq!(status, 200);
        assert!(html.contains("四十点"), "{html}");
        assert!(!html.contains("採点前の記事"), "{html}");
        // 二十点は一覧に無く、確認枠（設定の 50 点未満）にだけ出うる。四十点は確認枠に重ねない
        let explore = html.split("<h2>確認枠</h2>").nth(1).unwrap_or("");
        assert!(!explore.contains("四十点"), "{html}");
        assert!(
            !html
                .split("<h2>確認枠</h2>")
                .next()
                .unwrap()
                .contains("二十点"),
            "{html}"
        );
        assert_eq!(html.matches("四十点").count(), 1, "{html}");
        assert!(
            html.contains(r#"<option value="30" data-href="/?min=30" selected>30</option>"#),
            "{html}"
        );
        let (_, html) = server.get("/?min=0").await;
        for title in ["四十点", "二十点", "採点前の記事"] {
            assert!(html.contains(title), "{title}: {html}");
        }
        for bad in ["x", "101", "-1"] {
            let (status, _) = server.get(&format!("/?min={bad}")).await;
            assert_eq!(status, 400, "{bad}");
        }
    }

    fn group(db: &Db, ids: &[i64]) {
        let story = *ids.iter().min().unwrap();
        for id in ids {
            db.conn()
                .execute(
                    "UPDATE article_stories SET story_id = ?2 WHERE article_id = ?1",
                    [*id, story],
                )
                .unwrap();
        }
    }

    /// 一覧に出たグループの記事は、確認枠に重ねない。
    #[tokio::test]
    async fn explore_skips_stories_already_listed() {
        let db = Db::open_in_memory().unwrap();
        let (forty, digest) = seed(&db, "https://e.com/forty", "四十点");
        score(&db, digest, 40);
        let (twenty, digest) = seed(&db, "https://e.com/twenty", "二十点");
        score(&db, digest, 20);
        group(&db, &[forty, twenty]);
        let server = Server::start(db).await;
        let (_, html) = server.get("/?min=30").await;
        assert!(html.contains("四十点"), "{html}");
        assert!(!html.contains("二十点"), "{html}");
    }

    /// 確認枠に選んだ後で、同じグループのほかの記事を読んだら、既読の記事と同じく出さない。
    #[tokio::test]
    async fn explore_hides_picks_whose_story_was_read() {
        let db = Db::open_in_memory().unwrap();
        let (low, digest) = seed(&db, "https://e.com/low", "低い点");
        score(&db, digest, 10);
        let (high, digest) = seed(&db, "https://e.com/high", "高い点");
        score(&db, digest, 90);
        let server = Server::start(db).await;
        let (_, html) = server.get("/").await;
        assert!(html.contains("低い点"), "{html}");
        group(&server.state.db.lock().unwrap(), &[low, high]);
        server.post(&format!("/articles/{high}/read"), "on=1").await;
        let (_, html) = server.get("/").await;
        assert!(!html.contains("低い点"), "{html}");
    }

    /// 確認枠の記事も一覧と同じく、既読にしたものは出さない（`read=1` なら出す）。
    #[tokio::test]
    async fn explore_hides_read_picks() {
        let db = Db::open_in_memory().unwrap();
        let (low, digest) = seed(&db, "https://e.com/low", "低い点");
        score(&db, digest, 10);
        let server = Server::start(db).await;
        let in_explore = |html: &str| {
            html.split("<h2>確認枠</h2>")
                .nth(1)
                .is_some_and(|s| s.contains("低い点"))
        };
        let (_, html) = server.get("/").await;
        assert!(in_explore(&html), "{html}");
        server.post(&format!("/articles/{low}/read"), "on=1").await;
        let (_, html) = server.get("/").await;
        assert!(!html.contains("低い点"), "{html}");
        let (_, html) = server.get("/?read=1").await;
        assert!(in_explore(&html), "{html}");
    }

    #[tokio::test]
    async fn list_shows_articles_and_starts_a_visit() {
        let db = Db::open_in_memory().unwrap();
        give_profile(&db);
        seed(&db, "https://e.com/a", "見出しA");
        let server = Server::start(db).await;
        let (status, html) = server.get("/?min=0").await;
        assert_eq!(status, 200);
        assert!(html.contains("見出しA"), "{html}");
        assert_eq!(
            server.count("SELECT count(*) FROM users WHERE last_seen_at IS NOT NULL"),
            1
        );
        // プロファイルがあれば、未採点の記事は既定の一覧には出ない
        let (_, html) = server.get("/").await;
        assert!(!html.contains("見出しA"), "{html}");
    }

    /// プロファイルが無ければ採点が無いので、既定の一覧は推薦点で絞らず、未採点の記事を新しい順に出す
    /// （評価 1〜2 と軽水炉と無関係の記事は隠す）。確認枠は出さない。フィードと JSON の一覧も同じ既定。
    /// `?min=0`（すべて）と `?min=N`（N 点以上）は既定と別の表示で、URL もそのまま残る。
    #[tokio::test]
    async fn list_without_a_profile_has_no_score_floor() {
        let db = Db::open_in_memory().unwrap();
        seed(&db, "https://e.com/a", "採点前の記事");
        seed_with(&db, "https://e.com/unrelated", "無関係", false);
        let server = Server::start(db).await;

        let (status, html) = server.get("/").await;
        assert_eq!(status, 200);
        assert!(
            html.contains("採点前の記事") && !html.contains("無関係"),
            "{html}"
        );
        assert!(!html.contains("確認枠"), "{html}");
        // 最低点の選択は「最低点なし」を選んでいる
        assert!(
            html.contains(r#"<option value="" data-href="/" selected>--</option>"#),
            "{html}"
        );

        for canonical in ["/?min=0", "/?min=50"] {
            let res = server.get_raw(canonical).await;
            assert_eq!(res.status().as_u16(), 200, "{canonical}");
        }
        let (_, html) = server.get("/?min=0").await;
        assert!(
            html.contains("採点前の記事") && html.contains("無関係"),
            "{html}"
        );
        let (_, html) = server.get("/?min=50").await;
        assert!(!html.contains("採点前の記事"), "{html}");
        // JavaScript が無いときの選択で送られる空の値は、既定（最低点なし）
        let res = server.get_raw("/?min=").await;
        assert_eq!(res.status().as_u16(), 303);
        assert_eq!(res.headers()["location"], "/");

        let (_, xml) = server.get(&server.feed_path()).await;
        assert!(
            xml.contains("採点前の記事") && !xml.contains("無関係"),
            "{xml}"
        );
        let (_, json) = server.get_json("/api/articles").await;
        assert_eq!(json["articles"].as_array().unwrap().len(), 1, "{json}");
    }

    /// 既読にした記事は、同じ訪問のうちでも「前回から」の欄でも、次に一覧を出したときには出さない。
    #[tokio::test]
    async fn list_hides_articles_read_in_this_visit() {
        let db = Db::open_in_memory().unwrap();
        let (id, digest) = seed(&db, "https://e.com/a", "見出しA");
        score(&db, digest, 80);
        let server = Server::start(db).await;
        let (_, html) = server.get("/").await;
        assert!(html.contains("見出しA"), "{html}");
        server.post(&format!("/articles/{id}/read"), "on=1").await;
        let (_, html) = server.get("/").await;
        assert!(!html.contains("見出しA"), "{html}");
        let (_, html) = server.get("/?read=1").await;
        assert!(html.contains("見出しA"), "{html}");
    }

    /// 一覧の URL は正規の形に揃える（既定と同じ値・空の値を落とす）。正規の形なら移らない。
    /// JavaScript が無いときの評価の選択は、今の条件とどちらの画面から送ったか（`from`）を送るので、一覧と絞り込みを
    /// 行き来したときは最低点と 👁 を行き先の既定にする。
    #[tokio::test]
    async fn list_redirects_to_the_canonical_url() {
        // 既定の最低点（設定の値）は、プロファイルがあるときのもの
        let db = Db::open_in_memory().unwrap();
        give_profile(&db);
        let server = Server::start(db).await;
        for (from, to) in [
            ("/?rating=", "/"),
            ("/?rating=hide-low", "/"),
            ("/?rating=&read=0", "/"),
            ("/?rating=&read=0&bookmarked=1", "/?read=0&bookmarked=1"),
            ("/?min=50", "/"),
            ("/?read=1&min=30", "/?min=30&read=1"),
            // 絞り込みの既読の既定は絞らない（any）
            ("/?rating=4&read=any", "/?rating=4"),
            // 絞り込みの最低点の既定は 0（00）
            ("/?rating=4&min=0", "/?rating=4"),
            ("/?rating=4&min=60", "/?min=60&rating=4"),
            // 絞り込みから一覧へ戻るときは、絞り込みの最低点・既読を使わない
            ("/?rating=hide-low&min=60&from=filtered", "/"),
            ("/?rating=any&min=0&read=0&from=filtered", "/?rating=any"),
            // ブックマークで絞り込んだままなら、絞り込みの条件を引き継ぐ
            (
                "/?rating=hide-low&min=60&bookmarked=1&from=filtered",
                "/?min=60&rating=hide-low&bookmarked=1",
            ),
            // 一覧から絞り込みへ移るときは、一覧の最低点・既読を使わない
            ("/?rating=4&min=60&read=any&from=list", "/?rating=4"),
            // 一覧の中で評価を変えたなら、一覧の条件を引き継ぐ
            ("/?rating=any&min=60&from=list", "/?min=60&rating=any"),
        ] {
            let res = server.get_raw(from).await;
            assert_eq!(res.status().as_u16(), 303, "{from}");
            assert_eq!(res.headers()["location"], to, "{from}");
        }
        for canonical in [
            "/",
            "/?min=0",
            "/?min=30&read=1",
            "/?rating=any",
            "/?rating=hide-low&bookmarked=1",
            "/?rating=4&read=0&bookmarked=1",
            "/?rating=0",
        ] {
            assert_eq!(
                server.get_raw(canonical).await.status().as_u16(),
                200,
                "{canonical}"
            );
        }
        // 誤った値は移さずに 400 のまま
        for bad in ["/?rating=9", "/?rating=x&from=list", "/?from=x"] {
            assert_eq!(server.get_raw(bad).await.status().as_u16(), 400, "{bad}");
        }
    }

    /// 一覧の既定は ★1〜2 を付けた記事を隠す（バーの ★ の選択に見える条件）。評価で絞らない（`rating=any`）と出る。
    /// 最低点の「すべて」（00）は点数の条件だけを外し、★1〜2 は隠したまま。
    #[tokio::test]
    async fn list_hides_low_ratings_by_the_rating_choice() {
        let db = Db::open_in_memory().unwrap();
        let (down, digest) = seed(&db, "https://e.com/down", "星二つの記事");
        score(&db, digest, 80);
        let (_, digest) = seed(&db, "https://e.com/kept", "評価前の記事");
        score(&db, digest, 80);
        let server = Server::start(db).await;
        server
            .post(&format!("/articles/{down}/rating"), "value=2")
            .await;
        for hidden in ["/", "/?min=0"] {
            let (_, html) = server.get(hidden).await;
            assert!(
                !html.contains("星二つの記事") && html.contains("評価前の記事"),
                "{hidden}: {html}"
            );
        }
        let (_, html) = server.get("/?rating=any").await;
        assert!(
            html.contains("星二つの記事") && html.contains("評価前の記事"),
            "{html}"
        );
    }

    /// 一覧は、条件で隠れている記事の数を、条件ごとに数えて出す（条件を 1 つ外したときに加わる記事の数）。
    #[tokio::test]
    async fn list_counts_what_each_condition_hides() {
        let db = Db::open_in_memory().unwrap();
        let (_, digest) = seed(&db, "https://e.com/shown", "出る記事");
        score(&db, digest, 90);
        let (_, digest) = seed(&db, "https://e.com/low", "低い点の記事");
        score(&db, digest, 20);
        let (read, digest) = seed(&db, "https://e.com/read", "読んだ記事");
        score(&db, digest, 90);
        let (down, digest) = seed(&db, "https://e.com/down", "星二つの記事");
        score(&db, digest, 90);
        let server = Server::start(db).await;
        server.post(&format!("/articles/{read}/read"), "on=1").await;
        server
            .post(&format!("/articles/{down}/rating"), "value=2")
            .await;
        let (_, html) = server.get("/").await;
        assert!(
            html.contains(
                r#"<p class="meta">条件で隠れている記事：<a href="/?min=0">点数 50 未満・未採点 1 件</a>・<a href="/?read=any">既読 1 件</a>・<a href="/?rating=any">★1〜2 1 件</a></p>"#
            ),
            "{html}"
        );
    }

    /// 条件を外したときに出る記事のうち、今は出ていない記事を数える。同じ報道のカードの代表が入れ替わるだけでも、
    /// 外すと出る記事は隠れている記事として数える（件数の差では 0 になる）。
    #[tokio::test]
    async fn list_counts_a_story_card_replaced_by_a_hidden_article() {
        let db = Db::open_in_memory().unwrap();
        let (kept, digest) = seed(&db, "https://e.com/kept", "取っておく記事");
        score(&db, digest, 90);
        let (other, digest) = seed(&db, "https://e.com/other", "同じ話の記事");
        score(&db, digest, 70);
        // 同じ報道のグループにする
        db.conn()
            .execute(
                "UPDATE article_stories SET story_id = ?1 WHERE article_id = ?2",
                [kept, other],
            )
            .unwrap();
        let server = Server::start(db).await;
        server
            .post(&format!("/articles/{kept}/bookmark"), "on=1")
            .await;
        let (_, html) = server.get("/?bookmarked=0").await;
        assert!(
            html.contains("同じ話の記事") && !html.contains("取っておく記事"),
            "{html}"
        );
        assert!(
            html.contains(r#"<a href="/">ブックマーク中 1 件</a>"#),
            "{html}"
        );
    }

    /// 一覧の 👁 と 🔖 は、印のある記事だけ（1）・無い記事だけ（0）・絞らない（any）で絞る。
    #[tokio::test]
    async fn list_filters_by_marks_both_ways() {
        let db = Db::open_in_memory().unwrap();
        let (read, digest) = seed(&db, "https://e.com/read", "読んだ記事");
        score(&db, digest, 80);
        let (kept, digest) = seed(&db, "https://e.com/kept", "取っておく記事");
        score(&db, digest, 80);
        let server = Server::start(db).await;
        server.post(&format!("/articles/{read}/read"), "on=1").await;
        server
            .post(&format!("/articles/{kept}/bookmark"), "on=1")
            .await;
        let (_, html) = server.get("/?read=1").await;
        assert!(
            html.contains("読んだ記事") && !html.contains("取っておく記事"),
            "{html}"
        );
        let (_, html) = server.get("/?read=any").await;
        assert!(
            html.contains("読んだ記事") && html.contains("取っておく記事"),
            "{html}"
        );
        let (_, html) = server.get("/?read=any&bookmarked=0").await;
        assert!(
            html.contains("読んだ記事") && !html.contains("取っておく記事"),
            "{html}"
        );
        // 一覧の既定は未読だけ（read=0 は既定なので付けない）
        let res = server.get_raw("/?read=0").await;
        assert_eq!(res.headers()["location"], "/");
    }

    /// `read=1`（既読だけ）を受け取り、切り替えのリンクに反映する（押すと一覧の既定の未読だけへ）。
    #[tokio::test]
    async fn list_reads_the_read_toggle() {
        let server = Server::start(Db::open_in_memory().unwrap()).await;
        let (status, html) = server.get("/?min=0&read=1").await;
        assert_eq!(status, 200);
        assert!(html.contains("既読：既読だけ（押すと未読だけ）"), "{html}");
        assert!(html.contains(r#"href="/?min=0""#), "{html}");
        assert!(
            html.contains(r#"<input type="hidden" name="read" value="1">"#),
            "{html}"
        );
    }

    /// ブックマークは印なので、付けても一覧に残る。
    #[tokio::test]
    async fn list_keeps_bookmarked_articles() {
        let db = Db::open_in_memory().unwrap();
        let (id, _) = seed(&db, "https://e.com/a", "見出しA");
        let server = Server::start(db).await;
        server
            .post(&format!("/articles/{id}/bookmark"), "on=1")
            .await;
        let (_, html) = server.get("/?min=0").await;
        assert!(html.contains("見出しA"), "{html}");
        let (_, html) = server.get("/?bookmarked=1").await;
        assert!(html.contains("見出しA"), "{html}");
    }

    /// 絞り込みの「☆」（`rating=0`）は評価の無い記事だけ、最低点（`min`）は絞り込みの中でも効く（既定は絞らない）。
    #[tokio::test]
    async fn filtered_list_takes_unrated_and_the_minimum_score() {
        let db = Db::open_in_memory().unwrap();
        let (low, digest) = seed(&db, "https://e.com/low", "低い点");
        score(&db, digest, 20);
        let (high, digest) = seed(&db, "https://e.com/high", "高い点");
        score(&db, digest, 80);
        seed(&db, "https://e.com/none", "評価なし");
        let server = Server::start(db).await;
        for id in [low, high] {
            server
                .post(&format!("/articles/{id}/rating"), "value=4")
                .await;
        }
        let (_, html) = server.get("/?rating=4").await;
        assert!(html.contains("低い点") && html.contains("高い点"), "{html}");
        let (_, html) = server.get("/?min=60&rating=4").await;
        assert!(
            !html.contains("低い点") && html.contains("高い点"),
            "{html}"
        );
        let (_, html) = server.get("/?rating=0").await;
        assert!(
            html.contains("評価なし") && !html.contains("低い点") && !html.contains("高い点"),
            "{html}"
        );
    }

    /// 👍（`rating=N`）と 🔖（`bookmarked=1`）の絞り込みは、一覧の期間・最低点・既読によらず全期間から探す。
    /// 検索と同じく閲覧ではないので、訪問は始めない。
    #[tokio::test]
    async fn list_filters_by_rating_and_bookmark() {
        let db = Db::open_in_memory().unwrap();
        // プロファイルはあるがどちらも未採点なので、既定の一覧には出ない
        give_profile(&db);
        let (four, _) = seed(&db, "https://e.com/four", "星四つ");
        let (two, _) = seed(&db, "https://e.com/two", "星二つ");
        let server = Server::start(db).await;
        server
            .post(&format!("/articles/{four}/rating"), "value=4")
            .await;
        server
            .post(&format!("/articles/{two}/rating"), "value=2")
            .await;
        server
            .post(&format!("/articles/{two}/bookmark"), "on=1")
            .await;
        let (status, html) = server.get("/?rating=4").await;
        assert_eq!(status, 200);
        assert!(
            html.contains("星四つ") && !html.contains("星二つ"),
            "{html}"
        );
        assert!(!html.contains(r#"action="/search""#), "{html}");
        let (_, html) = server.get("/?rating=2").await;
        assert!(html.contains("星四つ") && html.contains("星二つ"), "{html}");
        let (_, html) = server.get("/?bookmarked=1").await;
        assert!(
            !html.contains("星四つ") && html.contains("星二つ"),
            "{html}"
        );
        // 絞り込みの既定は既読も出す。`read=0` なら既読を隠す
        server.post(&format!("/articles/{four}/read"), "on=1").await;
        let (_, html) = server.get("/?rating=4").await;
        assert!(html.contains("星四つ"), "{html}");
        let (_, html) = server.get("/?rating=4&read=0").await;
        assert!(
            !html.contains("星四つ") && html.contains("該当する記事はありません"),
            "{html}"
        );
        let (_, html) = server.get("/?rating=4&bookmarked=1").await;
        assert!(html.contains("該当する記事はありません"), "{html}");
        assert_eq!(
            server.count("SELECT count(*) FROM users WHERE last_seen_at IS NOT NULL"),
            0
        );
        // 評価の「★」（絞らない）（空）を選ぶと一覧に戻る
        let res = server.get_raw("/?rating=").await;
        assert_eq!(res.status().as_u16(), 303);
        assert_eq!(res.headers()["location"], "/");
        // 0 は評価の無い記事だけなので誤りではない
        for bad in ["-1", "6", "x"] {
            let (status, _) = server.get(&format!("/?rating={bad}")).await;
            assert_eq!(status, 400, "{bad}");
        }
        // any（絞らない）は既読だけの値。ブックマークは 1・0 だけ
        for bad in ["bookmarked=any", "bookmarked=x", "read=x"] {
            let (status, _) = server.get(&format!("/?{bad}")).await;
            assert_eq!(status, 400, "{bad}");
        }
    }
}
