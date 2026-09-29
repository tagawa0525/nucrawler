//! 画面（一覧・検索・記事の詳細・設定）とフィード。

use super::*;

/// Web の一覧の条件（設定の期間・件数と、表示する最低点）。最低点が 0 なら、評価 1〜2・未採点・
/// 軽水炉と無関係の記事も出す（すべて）。既読は一覧の既定と同じく未読だけ（フィード・JSON の一覧もこれを使う）。
fn list_query<'a>(
    web: &WebConfig,
    user: i64,
    profile_hash: Option<&'a str>,
    now: chrono::DateTime<Utc>,
    min_score: u8,
) -> ListQuery<'a> {
    ListQuery {
        user_id: user,
        profile_hash,
        min_score,
        since: now - Duration::days(web.list_days.into()),
        show_all: min_score == 0,
        read: Some(false),
        bookmarked: None,
        limit: web.list_limit,
    }
}

/// Web の一覧に出す記事。
pub(super) fn list_items(
    db: &Db,
    web: &WebConfig,
    user: i64,
    profile_hash: Option<&str>,
    now: chrono::DateTime<Utc>,
    min_score: u8,
) -> Result<Vec<crate::db::ListItem>, DbError> {
    db.list_articles(list_query(web, user, profile_hash, now, min_score))
}

/// 警告は直近 24 時間のものだけ出す。
pub(super) fn warnings(db: &Db) -> Result<Vec<crate::db::Warning>, DbError> {
    let now = Utc::now();
    db.warnings(now - Duration::hours(24), now)
}

#[derive(serde::Deserialize)]
pub(super) struct ListParams {
    /// 表示する最低点（0〜100）。無ければ設定の `web.min_score`
    min: Option<String>,
    /// Web の一覧だけが使う（`1` なら既読も出し、`0` なら隠す。無ければ一覧は隠し、絞り込みは出す）
    read: Option<String>,
    /// Web の一覧だけが使う（この評価（1〜5）以上に絞る。空なら絞らない）
    rating: Option<String>,
    /// Web の一覧だけが使う（ブックマークに絞る）
    bookmarked: Option<String>,
}

impl ListParams {
    pub(super) fn min(&self, web: &WebConfig) -> Result<u8, AppError> {
        self.min_or(web.min_score)
    }

    /// 表示する最低点。無ければ `default`。
    fn min_or(&self, default: u8) -> Result<u8, AppError> {
        match self.min.as_deref() {
            None => Ok(default),
            Some(v) => v
                .parse()
                .ok()
                .filter(|min| *min <= 100)
                .ok_or(AppError::BadRequest("min must be 0..=100")),
        }
    }

    fn rating(&self) -> Result<Option<u8>, AppError> {
        match self.rating.as_deref().filter(|v| !v.is_empty()) {
            None => Ok(None),
            Some(v) => v
                .parse()
                .ok()
                // 0 は評価の無い記事だけ
                .filter(|r| (0..=5).contains(r))
                .map(Some)
                .ok_or(AppError::BadRequest("rating must be 0..=5")),
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
    Query(params): Query<ListParams>,
    RawQuery(raw): RawQuery,
) -> Result<Response, AppError> {
    let rating = params.rating()?;
    let bookmarked = bookmark_mark(params.bookmarked.as_deref())?;
    let filtering = rating.is_some() || bookmarked == Some(true);
    // JavaScript が無いときの評価の「★」（絞らない）は、絞り込みの条件（最低点・既読）も一緒に送る。一覧へ戻るので、
    // それらは使わずに一覧の既定にする（JavaScript があれば、選択肢の正規の URL へ移るので送られない）
    let leaving = params.rating.as_deref() == Some("") && !filtering;
    let carried = |value: Option<&str>| value.filter(|_| !leaving).map(str::to_string);
    let params = ListParams {
        min: carried(params.min.as_deref()),
        read: carried(params.read.as_deref()),
        ..params
    };
    // 最低点の既定は、一覧では設定の最低点、絞り込みでは 0（点数で絞らない）
    let min = params.min_or(if filtering { 0 } else { state.web.min_score })?;
    // 既読の既定は、一覧では未読だけ、絞り込み（評価した記事を探す）では絞らない
    let read =
        read_mark(params.read.as_deref())?.unwrap_or(if filtering { None } else { Some(false) });
    let view = html::ListView {
        min,
        default_min: state.web.min_score,
        read,
        rating,
        bookmarked,
    };
    // 正規の形でなければ（既定と同じ値・空の値が残っているなど）、正規の URL へ移す。JavaScript が無いときの
    // 選択のフォームは、評価の「★」（絞らない）で `rating=` や、絞り込みを外したときの `read=0` を残す
    let canonical = view.url();
    let requested = match raw.as_deref() {
        None | Some("") => "/".to_string(),
        Some(q) => format!("/?{q}"),
    };
    if requested != canonical {
        return Ok(Redirect::to(&canonical).into_response());
    }
    let web = state.web.clone();
    let labels = state.labels.clone();
    let default_min = state.web.min_score;
    let page = with_db(&state, move |db| {
        let now = Utc::now();
        let (user, hash) = viewer(db)?;
        if view.filtered() {
            return filtered(db, &web, &labels, user, hash.as_deref(), view);
        }
        let boundary =
            db.begin_visit(user, now, Duration::minutes(web.visit_gap_minutes.into()))?;
        // 既読・ブックマークでは件数の上限より前に絞る（上位が既読で埋まっても、下の未読が出るように）
        let items = db.list_articles(ListQuery {
            read: view.read,
            bookmarked: view.bookmarked,
            ..list_query(&web, user, hash.as_deref(), now, min)
        })?;
        let (new, earlier) = html::split_sections(items, boundary.as_deref());
        // 「すべて」では閾値未満も並んでいるので、確認枠は出さない
        let explore = if view.shows_all() {
            Vec::new()
        } else {
            // 見逃し率を偏りなく測るため、選ぶ基準は画面で選んだ最低点ではなく設定の最低点
            let today = now.with_timezone(&crate::jst::offset()).format("%Y-%m-%d");
            let picks = db.explore(
                list_query(&web, user, hash.as_deref(), now, web.min_score),
                web.explore_per_day as usize,
                &today.to_string(),
            )?;
            // 最低点を下げて一覧に既に出ている記事は重ねない
            let listed: std::collections::HashSet<i64> =
                new.iter().chain(&earlier).map(|i| i.article_id).collect();
            let picks = picks
                .into_iter()
                .filter(|i| !listed.contains(&i.article_id))
                .collect();
            // 一覧と同じく、既読・ブックマークで絞る
            html::filter_read(picks, view.read)
                .into_iter()
                .filter(|i| view.bookmarked.is_none_or(|b| i.bookmarked == b))
                .collect()
        };
        let warnings = warnings(db)?;
        let page = Page {
            warnings: &warnings,
            labels: &labels,
            default_min,
        };
        Ok(html::list_page_with_explore(
            &new, &earlier, &explore, view, &page,
        ))
    })
    .await?;
    Ok(Html(page).into_response())
}

/// 評価・ブックマークで絞った記事を、検索と同じく全期間から新しい順に出す（既読の表示は 👁 のとおり）。
/// 検索と同じく閲覧ではないので、訪問は始めない。
fn filtered(
    db: &Db,
    web: &WebConfig,
    labels: &html::SourceLabels,
    user: i64,
    hash: Option<&str>,
    view: html::ListView,
) -> Result<String, AppError> {
    let params = Params {
        min_rating: view
            .rating
            .filter(|r| *r > 0)
            .map(|r| r.to_string())
            .unwrap_or_default(),
        unrated: view.rating == Some(0),
        bookmarked: view.bookmarked,
        read: view.read,
        min_score: if view.min > 0 {
            view.min.to_string()
        } else {
            String::new()
        },
        ..Params::default()
    };
    let query = params
        .to_query(user, hash, web.list_limit)
        .map_err(|_| AppError::BadRequest("rating must be 1..=5"))?;
    let items = db.search_articles(&query)?;
    let warnings = warnings(db)?;
    let page = Page {
        warnings: &warnings,
        labels,
        default_min: web.min_score,
    };
    Ok(html::filtered_page(&items, view, &page))
}

/// Web の既定の一覧と同じ記事の Atom フィード。閲覧ではないので、訪問も開いたことも記録しない。
pub(super) async fn feed(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    // 記事のリンクは絶対 URL にする。http で待ち受けているので `http://` + Host
    let host = headers
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .map_or_else(|| state.web.bind.to_string(), str::to_string);
    let base = format!("http://{host}");
    let web = state.web.clone();
    let labels = state.labels.clone();
    let xml = with_db(&state, move |db| {
        let now = Utc::now();
        let (user, hash) = viewer(db)?;
        let items = list_items(db, &web, user, hash.as_deref(), now, web.min_score)?;
        Ok(feed::atom(
            &items,
            &base,
            &labels,
            &crate::db::timestamp(now),
        ))
    })
    .await?;
    Ok((
        [(header::CONTENT_TYPE, "application/atom+xml; charset=utf-8")],
        xml,
    )
        .into_response())
}

/// 検索画面。一覧で隠す記事も語や条件で探せる。閲覧ではないので、訪問も開いたことも記録しない。
/// 条件の誤りは、条件を残したフォームとともに 400 で返す。
pub(super) async fn search(
    State(state): State<AppState>,
    RawQuery(raw): RawQuery,
) -> Result<Response, AppError> {
    let params = Params::from_query(raw.as_deref().unwrap_or(""));
    let web = state.web.clone();
    let labels = state.labels.clone();
    let default_min = state.web.min_score;
    let (status, page) = with_db(&state, move |db| {
        let (user, hash) = viewer(db)?;
        let vocabulary = db.topic_usage()?;
        let warnings = warnings(db)?;
        let page = Page {
            warnings: &warnings,
            labels: &labels,
            default_min,
        };
        // 条件が無くても（並びだけでも）値の誤りは 400 で返してから、フォームだけの画面にする
        let html = match params.to_query(user, hash.as_deref(), web.list_limit) {
            Ok(_) if params.is_empty() => {
                html::search_page(&params, None, &vocabulary, None, &page)
            }
            Ok(q) => {
                let items = db.search_articles(&q)?;
                html::search_page(&params, Some(&items), &vocabulary, None, &page)
            }
            Err(e) => {
                let html =
                    html::search_page(&params, None, &vocabulary, Some(&e.to_string()), &page);
                return Ok((StatusCode::BAD_REQUEST, html));
            }
        };
        Ok((StatusCode::OK, html))
    })
    .await?;
    Ok((status, Html(page)).into_response())
}

#[derive(serde::Deserialize)]
pub(super) struct DetailParams {
    view: Option<String>,
    digest: Option<i64>,
    translation: Option<i64>,
    reported: Option<String>,
    /// 書き込みの後に戻った（`back_to_detail`）。開いたとは数えない
    back: Option<String>,
}

pub(super) async fn detail(
    State(state): State<AppState>,
    method: Method,
    Path(id): Path<i64>,
    Query(params): Query<DetailParams>,
) -> Result<Html<String>, AppError> {
    let view = DetailView {
        digest: params.digest,
        show_translation: params.view.as_deref() == Some("translation"),
        translation: params.translation,
        reported: params.reported.is_some(),
    };
    // 書き込みの後に戻った詳細と、HEAD（リンクの確かめなど。axum は GET の受付に回す）は開いたと数えない
    let returned = params.back.is_some() || method == Method::HEAD;
    let labels = state.labels.clone();
    let default_min = state.web.min_score;
    let page = with_db(&state, move |db| {
        let now = Utc::now();
        let (user, hash) = viewer(db)?;
        let mut detail = db
            .article_detail(user, hash.as_deref(), id)?
            .ok_or(AppError::NotFound)?;
        let reports = db.reports(
            user,
            &ReportFilter {
                article_id: Some(id),
                ..ReportFilter::default()
            },
        )?;
        // 開いたことだけを記録し、版の切り替えと書き込みの後の戻りは数えない
        // （開いた回数を、読んだ回数として数えられるように）
        let opened = if returned {
            None
        } else if view.show_translation {
            (view.translation.is_none() && !detail.translations.is_empty())
                .then_some(OpenKind::Translation)
        } else {
            view.digest.is_none().then_some(OpenKind::Detail)
        };
        if let Some(kind) = opened {
            db.record_open(user, id, kind, now)?;
            // 開いたので既読になった（読み出したのは記録の前なので、表示に合わせる）
            detail
                .item
                .read_at
                .get_or_insert_with(|| crate::db::timestamp(now));
        }
        let warnings = warnings(db)?;
        let page = Page {
            warnings: &warnings,
            labels: &labels,
            default_min,
        };
        let comments = db.comments(user, id)?;
        let notes = html::Notes {
            reports: &reports,
            comments: &comments,
        };
        Ok(html::detail_page(&detail, &notes, view, &page))
    })
    .await?;
    Ok(Html(page))
}

/// 原文へ移る。開いたことを記録してから、元の記事の URL へリダイレクトする。
pub(super) async fn source(
    State(state): State<AppState>,
    method: Method,
    Path(id): Path<i64>,
) -> Result<Response, AppError> {
    let url = with_db(&state, move |db| {
        let (user, _) = viewer(db)?;
        // 移るだけなので、詳細の中身（要約・和訳・本文）は読まない
        let url = db.article_url(id)?.ok_or(AppError::NotFound)?;
        // HEAD（リンクの確かめなど。axum は GET の受付に回す）は開いたと数えない
        if method != Method::HEAD {
            db.record_open(user, id, OpenKind::Source, Utc::now())?;
        }
        Ok(url)
    })
    .await?;
    Ok(Redirect::to(&url).into_response())
}

pub(super) async fn settings(State(state): State<AppState>) -> Result<Html<String>, AppError> {
    let labels = state.labels.clone();
    let default_min = state.web.min_score;
    let page = with_db(&state, move |db| {
        let terms = db.glossary_entries()?.len();
        let pending = db
            .report_counts()?
            .into_iter()
            .find_map(|(status, n)| (status == ReportStatus::Pending).then_some(n))
            .unwrap_or(0);
        let warnings = warnings(db)?;
        let page = Page {
            warnings: &warnings,
            labels: &labels,
            default_min,
        };
        Ok(html::settings_page(terms, pending, &page))
    })
    .await?;
    Ok(Html(page))
}

#[cfg(test)]
mod tests {
    use super::*;
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
        for hidden in ["評価 2", "無関係", "未採点"] {
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
        seed(&db, "https://e.com/unscored", "未採点");
        let server = Server::start(db).await;
        let (status, html) = server.get("/?min=30").await;
        assert_eq!(status, 200);
        assert!(html.contains("四十点"), "{html}");
        assert!(!html.contains("未採点"), "{html}");
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
        for title in ["四十点", "二十点", "未採点"] {
            assert!(html.contains(title), "{title}: {html}");
        }
        for bad in ["x", "101", "-1"] {
            let (status, _) = server.get(&format!("/?min={bad}")).await;
            assert_eq!(status, 400, "{bad}");
        }
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

    /// フィードは既定の一覧と同じ記事を Atom で出し、閲覧としては記録しない。
    #[tokio::test]
    async fn feed_lists_recommended_articles_as_atom() {
        let db = Db::open_in_memory().unwrap();
        let good = seed_recommended_and_hidden(&db);
        let server = Server::start(db).await;
        let (status, content_type, xml) = server.get_with_type("/feed.xml").await;
        assert_eq!(status, 200);
        assert!(
            content_type.starts_with("application/atom+xml"),
            "{content_type}"
        );
        assert!(
            xml.starts_with("<?xml version=\"1.0\" encoding=\"utf-8\"?>"),
            "{xml}"
        );
        assert!(
            xml.contains("<feed xmlns=\"http://www.w3.org/2005/Atom\">"),
            "{xml}"
        );
        assert_eq!(xml.matches("<entry>").count(), 1, "{xml}");
        for hidden in ["低い点", "評価 2", "無関係", "未採点"] {
            assert!(!xml.contains(hidden), "{hidden}: {xml}");
        }
        // 和訳タイトル・要約・元記事と詳細ページへのリンク・日付
        assert!(xml.contains("<title>A&amp;B &lt;C&gt;</title>"), "{xml}");
        assert!(xml.contains("<summary>要約</summary>"), "{xml}");
        assert!(
            xml.contains(&format!(
                "<link rel=\"alternate\" href=\"{}/articles/{good}\"/>",
                server.base
            )),
            "{xml}"
        );
        assert!(
            xml.contains("<link rel=\"related\" href=\"https://e.com/good?a=1&amp;b=2\"/>"),
            "{xml}"
        );
        assert!(xml.contains("<updated>20"), "{xml}");
        // XML 1.0 に書けない制御文字は、実体参照にもできないので落とす
        assert!(!xml.contains('\u{1}') && !xml.contains("&#1;"), "{xml}");
        server.assert_no_views();
    }

    /// フィードとエントリの ID は、アクセスしたアドレスによらず同じ（リーダーが既読を見失わない）。
    /// リンクはアクセスしたアドレスから作る。
    #[tokio::test]
    async fn feed_ids_do_not_depend_on_the_host() {
        let db = Db::open_in_memory().unwrap();
        let good = seed_recommended_and_hidden(&db);
        let server = Server::start(db).await;
        let hosts = ["100.64.0.1:8080", "nucrawler.tailnet.ts.net"];
        let mut feeds = Vec::new();
        for host in hosts {
            let res = server
                .client
                .get(format!("{}/feed.xml", server.base))
                .header(header::HOST, host)
                .send()
                .await
                .unwrap();
            assert_eq!(res.status().as_u16(), 200);
            let xml = res.text().await.unwrap();
            assert!(
                xml.contains(&format!(
                    "<link rel=\"alternate\" href=\"http://{host}/articles/{good}\"/>"
                )),
                "{xml}"
            );
            feeds.push(xml);
        }
        let ids: Vec<Vec<&str>> = feeds
            .iter()
            .map(|xml| {
                xml.split("<id>")
                    .skip(1)
                    .map(|s| s.split_once("</id>").unwrap().0)
                    .collect()
            })
            .collect();
        // フィードとエントリ 1 件
        assert_eq!(ids[0].len(), 2, "{}", feeds[0]);
        assert_eq!(ids[0], ids[1]);
        for id in &ids[0] {
            for host in hosts {
                assert!(!id.contains(host), "{id}");
            }
        }
    }

    /// 検索画面は一覧で隠す記事も語で引ける。条件が無ければフォームだけで、閲覧としては記録しない。
    #[tokio::test]
    async fn search_page_finds_articles_by_terms() {
        let db = Db::open_in_memory().unwrap();
        let (hit, _) = seed_with(&db, "https://e.com/hit", "炉心溶融の解析", false);
        seed(&db, "https://e.com/other", "燃料の話");
        let server = Server::start(db).await;
        let (status, html) = server.get("/search").await;
        assert_eq!(status, 200);
        assert!(html.contains(r#"action="/search""#), "{html}");
        assert!(!html.contains("燃料の話"), "{html}");

        let (status, html) = server.get("/search?q=%E7%82%89%E5%BF%83").await;
        assert_eq!(status, 200);
        assert!(html.contains(&format!("/articles/{hit}")), "{html}");
        assert!(!html.contains("燃料の話"), "{html}");
        server.assert_no_views();
    }

    #[tokio::test]
    async fn search_rejects_invalid_conditions() {
        let server = Server::start(Db::open_in_memory().unwrap()).await;
        let (status, html) = server.get("/search?since=2026%2F09").await;
        assert_eq!(status, 400);
        // 並びは条件に数えないが、誤りは誤りとして返す
        let (status, _) = server.get("/search?sort=old").await;
        assert_eq!(status, 400);
        let (status, _) = server.get("/search?sort=score").await;
        assert_eq!(status, 200);
        assert!(
            html.contains("since must be YYYY, YYYY-MM or YYYY-MM-DD"),
            "{html}"
        );
        assert!(
            html.contains(r#"action="/search""#),
            "the form stays usable: {html}"
        );
        let (status, body) = server.get("/api/search?lang=fr").await;
        assert_eq!(status, 400);
        assert!(body.contains("lang must be en or ja"), "{body}");
    }

    #[tokio::test]
    async fn list_shows_articles_and_starts_a_visit() {
        let db = Db::open_in_memory().unwrap();
        seed(&db, "https://e.com/a", "見出しA");
        let server = Server::start(db).await;
        let (status, html) = server.get("/?min=0").await;
        assert_eq!(status, 200);
        assert!(html.contains("見出しA"), "{html}");
        assert_eq!(
            server.count("SELECT count(*) FROM users WHERE last_seen_at IS NOT NULL"),
            1
        );
        // 未採点の記事は既定の一覧には出ない
        let (_, html) = server.get("/").await;
        assert!(!html.contains("見出しA"), "{html}");
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

    /// フィードと JSON の一覧は既定の一覧と同じ記事なので、既読の記事は出さない。
    #[tokio::test]
    async fn feed_and_api_leave_out_read_articles() {
        let db = Db::open_in_memory().unwrap();
        let (id, digest) = seed(&db, "https://e.com/a", "見出しA");
        score(&db, digest, 80);
        let server = Server::start(db).await;
        for path in ["/feed.xml", "/api/articles"] {
            let (_, body) = server.get(path).await;
            assert!(body.contains("見出しA"), "{path}: {body}");
        }
        server.post(&format!("/articles/{id}/read"), "on=1").await;
        for path in ["/feed.xml", "/api/articles"] {
            let (_, body) = server.get(path).await;
            assert!(!body.contains("見出しA"), "{path}: {body}");
        }
    }

    /// 一覧の URL は正規の形に揃える（既定と同じ値・空の値を落とす）。選択のフォームは値を選べないので、
    /// 👍 を「👍」に戻すと `rating=` や、絞り込みの `read=0` が残る。正規の形なら移らない。
    #[tokio::test]
    async fn list_redirects_to_the_canonical_url() {
        let server = Server::start(Db::open_in_memory().unwrap()).await;
        for (from, to) in [
            ("/?rating=", "/"),
            ("/?rating=&read=0", "/"),
            ("/?rating=&read=0&bookmarked=1", "/?read=0&bookmarked=1"),
            ("/?min=50", "/"),
            ("/?read=1&min=30", "/?min=30&read=1"),
            // 絞り込みの既読の既定は絞らない（any）
            ("/?rating=4&read=any", "/?rating=4"),
            // 絞り込みの最低点の既定は 0（00）
            ("/?rating=4&min=0", "/?rating=4"),
            ("/?rating=4&min=60", "/?min=60&rating=4"),
            // JavaScript が無いときの評価の「★」（絞らない）は、絞り込みの条件（最低点・既読）を一緒に送るが、
            // 一覧へ戻るので一覧の既定にする
            ("/?rating=&min=60", "/"),
            ("/?rating=&min=0&read=0", "/"),
            // ブックマークで絞り込んだままなら、絞り込みの条件を引き継ぐ
            ("/?rating=&min=60&bookmarked=1", "/?min=60&bookmarked=1"),
        ] {
            let res = server.get_raw(from).await;
            assert_eq!(res.status().as_u16(), 303, "{from}");
            assert_eq!(res.headers()["location"], to, "{from}");
        }
        for canonical in [
            "/",
            "/?min=0",
            "/?min=30&read=1",
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
        assert_eq!(server.get_raw("/?rating=9").await.status().as_u16(), 400);
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

    /// 原文へは、開いたことを記録してから元の記事へ移す。無い記事は 404。
    #[tokio::test]
    async fn source_link_records_the_open_and_redirects() {
        let db = Db::open_in_memory().unwrap();
        let (id, _) = seed(&db, "https://e.com/a?x=1&y=2", "見出しA");
        let server = Server::start(db).await;
        let res = server.get_raw(&format!("/articles/{id}/source")).await;
        assert_eq!(res.status().as_u16(), 303);
        assert_eq!(res.headers()["location"], "https://e.com/a?x=1&y=2");
        assert_eq!(
            server.count(&format!(
                "SELECT count(*) FROM events WHERE article_id = {id} AND kind = 'open_source'"
            )),
            1
        );
        // 原文を開いても既読は変えない（既読は詳細の既読だけ）
        assert_eq!(server.count("SELECT count(*) FROM reads"), 0);
        assert_eq!(server.get("/articles/999/source").await.0, 404);
    }

    /// HEAD（リンクの確かめなど）は開いたわけではないので、詳細も原文も開いた記録・既読を残さない。
    #[tokio::test]
    async fn head_requests_are_not_opens() {
        let db = Db::open_in_memory().unwrap();
        let (id, _) = seed(&db, "https://e.com/a", "見出しA");
        let server = Server::start(db).await;
        assert_eq!(server.head(&format!("/articles/{id}")).await, 200);
        assert_eq!(server.head(&format!("/articles/{id}/source")).await, 303);
        assert_eq!(server.count("SELECT count(*) FROM events"), 0);
        assert_eq!(server.count("SELECT count(*) FROM reads"), 0);
    }

    #[tokio::test]
    async fn detail_records_opens_once_per_view() {
        let db = Db::open_in_memory().unwrap();
        let (id, digest) = seed(&db, "https://e.com/a", "見出しA");
        add_translation(&db, id);
        let server = Server::start(db).await;
        let events = |kind: &str| {
            server.count(&format!(
                "SELECT count(*) FROM events WHERE article_id = {id} AND kind = '{kind}'"
            ))
        };

        let (status, html) = server.get(&format!("/articles/{id}")).await;
        assert_eq!(status, 200);
        assert!(html.contains("見出しA"), "{html}");
        assert_eq!(events("open_detail"), 1);
        // 版の切り替えは新たに開いたことにしない
        server.get(&format!("/articles/{id}?digest={digest}")).await;
        assert_eq!(events("open_detail"), 1);

        let (status, html) = server
            .get(&format!("/articles/{id}?view=translation"))
            .await;
        assert_eq!(status, 200);
        assert!(html.contains("和訳の本文"), "{html}");
        assert_eq!(events("open_translation"), 1);
        assert_eq!(events("open_detail"), 1);
        // 開いた記事は既読
        assert_eq!(server.count("SELECT count(*) FROM reads"), 1);
    }

    /// 書き込み（評価・印・和訳の依頼・指摘・コメント）の後に戻った詳細は、開いたとは数えない。
    /// 数えると、外した既読が付き直り、開いた記録も戻るたびに増える。
    #[tokio::test]
    async fn returning_after_a_write_is_not_an_open() {
        let db = Db::open_in_memory().unwrap();
        let (id, _) = seed(&db, "https://e.com/a", "見出しA");
        add_translation(&db, id);
        let server = Server::start(db).await;
        let opens = || server.count("SELECT count(*) FROM events");
        server.get(&format!("/articles/{id}")).await;
        assert_eq!(opens(), 1);
        let writes = [
            ("read", "on=0"),
            ("bookmark", "on=1"),
            ("rating", "value=3"),
            ("comments", "body=x"),
            ("report", "kind=other&note=x"),
        ];
        for (path, body) in writes {
            let res = server.post(&format!("/articles/{id}/{path}"), body).await;
            assert_eq!(res.status().as_u16(), 303, "{path}");
            let location = res.headers()["location"].to_str().unwrap().to_string();
            assert!(location.contains("back=1"), "{path}: {location}");
            let (status, _) = server.get(&location).await;
            assert_eq!(status, 200, "{path}");
        }
        server.post(&format!("/articles/{id}/read"), "on=0").await;
        server
            .get(&format!("/articles/{id}?view=translation&back=1"))
            .await;
        assert_eq!(opens(), 1);
        assert_eq!(server.count("SELECT count(*) FROM reads"), 0);
    }

    /// 開いた詳細は、開いたときに付いた既読をそのまま示す。一覧と同じく、印はその場で付け外しする。
    #[tokio::test]
    async fn detail_shows_the_read_mark_it_just_set() {
        let db = Db::open_in_memory().unwrap();
        let (id, _) = seed(&db, "https://e.com/a", "見出しA");
        let server = Server::start(db).await;
        let (_, html) = server.get(&format!("/articles/{id}")).await;
        assert!(
            html.contains(r#"<button name="on" value="0" aria-pressed="true" aria-label="既読" title="既読" class="on">👁</button>"#),
            "{html}"
        );
        assert!(html.contains("requestSubmit"), "{html}");
    }

    #[tokio::test]
    async fn translation_view_without_translation_is_not_an_open() {
        let db = Db::open_in_memory().unwrap();
        let (id, _) = seed(&db, "https://e.com/a", "見出しA");
        let server = Server::start(db).await;
        let (status, _) = server
            .get(&format!("/articles/{id}?view=translation"))
            .await;
        assert_eq!(status, 200);
        assert_eq!(
            server.count("SELECT count(*) FROM events WHERE kind = 'open_translation'"),
            0
        );
    }

    #[tokio::test]
    async fn unknown_article_is_not_found() {
        let server = Server::start(Db::open_in_memory().unwrap()).await;
        assert_eq!(server.get("/articles/999").await.0, 404);
        let res = server.post("/articles/999/rating", "value=4").await;
        assert_eq!(res.status().as_u16(), 404);
        let res = server.post("/articles/999/translation-request", "").await;
        assert_eq!(res.status().as_u16(), 404);
        assert_eq!(server.count("SELECT count(*) FROM events"), 0);
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
        // どちらも未採点なので、既定の一覧には出ない
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

    #[tokio::test]
    async fn settings_lead_to_the_glossary() {
        let server = Server::start(Db::open_in_memory().unwrap()).await;
        let (status, html) = server.get("/settings").await;
        assert_eq!(status, 200);
        assert!(html.contains(r#"href="/glossary""#), "{html}");
        let (status, html) = server.get("/glossary").await;
        assert_eq!(status, 200);
        assert!(html.contains("事故耐性燃料"), "{html}");
    }
}
