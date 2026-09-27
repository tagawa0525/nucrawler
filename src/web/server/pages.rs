//! 画面（一覧・検索・記事の詳細・設定）とフィード。

use super::*;

/// Web の一覧に出す記事（設定の期間・件数・最低点）。
pub(super) fn list_items(
    db: &Db,
    web: &WebConfig,
    user: i64,
    profile_hash: Option<&str>,
    now: chrono::DateTime<Utc>,
    show_all: bool,
) -> Result<Vec<crate::db::ListItem>, DbError> {
    db.list_articles(ListQuery {
        user_id: user,
        profile_hash,
        min_score: web.min_score,
        since: now - Duration::days(web.list_days.into()),
        show_all,
        limit: web.list_limit,
    })
}

/// 警告は直近 24 時間のものだけ出す。
pub(super) fn warnings(db: &Db) -> Result<Vec<crate::db::Warning>, DbError> {
    let now = Utc::now();
    db.warnings(now - Duration::hours(24), now)
}

#[derive(serde::Deserialize)]
pub(super) struct ListParams {
    pub(super) all: Option<String>,
    /// Web の一覧だけが使う（過去の欄に既読も出す）
    read: Option<String>,
}

pub(super) async fn list(
    State(state): State<AppState>,
    Query(params): Query<ListParams>,
) -> Result<Html<String>, AppError> {
    let show_all = params.all.as_deref() == Some("1");
    let show_read = params.read.as_deref() == Some("1");
    let web = state.web.clone();
    let labels = state.labels.clone();
    let page = with_db(&state, move |db| {
        let now = Utc::now();
        let (user, hash) = viewer(db)?;
        let boundary =
            db.begin_visit(user, now, Duration::minutes(web.visit_gap_minutes.into()))?;
        let items = list_items(db, &web, user, hash.as_deref(), now, show_all)?;
        let (new, earlier) = html::split_sections(items, boundary.as_deref(), show_read);
        let warnings = warnings(db)?;
        let page = Page {
            warnings: &warnings,
            labels: &labels,
        };
        let view = html::ListView {
            all: show_all,
            read: show_read,
        };
        Ok(html::list_page(&new, &earlier, view, &page))
    })
    .await?;
    Ok(Html(page))
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
        let items = list_items(db, &web, user, hash.as_deref(), now, false)?;
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
    let (status, page) = with_db(&state, move |db| {
        let (user, hash) = viewer(db)?;
        let vocabulary = db.topic_usage()?;
        let warnings = warnings(db)?;
        let page = Page {
            warnings: &warnings,
            labels: &labels,
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
}

pub(super) async fn detail(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Query(params): Query<DetailParams>,
) -> Result<Html<String>, AppError> {
    let view = DetailView {
        digest: params.digest,
        show_translation: params.view.as_deref() == Some("translation"),
        translation: params.translation,
        reported: params.reported.is_some(),
    };
    let labels = state.labels.clone();
    let page = with_db(&state, move |db| {
        let (user, hash) = viewer(db)?;
        let detail = db
            .article_detail(user, hash.as_deref(), id)?
            .ok_or(AppError::NotFound)?;
        let reports = db.reports(
            user,
            &ReportFilter {
                article_id: Some(id),
                ..ReportFilter::default()
            },
        )?;
        // 開いたことだけを記録し、版の切り替えは数えない（同じ記事の反応が重なると
        // 採点に渡す直近の反応が偏る）
        let opened = if view.show_translation {
            (view.translation.is_none() && !detail.translations.is_empty())
                .then_some(SignalKind::OpenTranslation)
        } else {
            view.digest.is_none().then_some(SignalKind::OpenDetail)
        };
        if let Some(kind) = opened {
            db.record_event(user, id, kind, Utc::now())?;
        }
        let warnings = warnings(db)?;
        let page = Page {
            warnings: &warnings,
            labels: &labels,
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

pub(super) async fn settings(State(state): State<AppState>) -> Result<Html<String>, AppError> {
    let labels = state.labels.clone();
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
        for hidden in ["👎した", "無関係", "未採点"] {
            assert!(!section.contains(hidden), "{hidden}: {html}");
        }
        let (_, all) = server.get("/?all=1").await;
        assert!(!all.contains("確認枠"), "{all}");
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
        for hidden in ["低い点", "👎した", "無関係", "未採点"] {
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
        let (status, html) = server.get("/?all=1").await;
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

    /// `read=1` を受け取り、切り替えのリンクに反映する。
    #[tokio::test]
    async fn list_reads_the_read_toggle() {
        let server = Server::start(Db::open_in_memory().unwrap()).await;
        let (status, html) = server.get("/?all=1&read=1").await;
        assert_eq!(status, 200);
        assert!(html.contains("過去の既読も表示：ON"), "{html}");
        assert!(html.contains(r#"href="/?all=1""#), "{html}");
        assert!(html.contains(r#"href="/?read=1""#), "{html}");
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
        let res = server.post("/articles/999/feedback", "kind=up").await;
        assert_eq!(res.status().as_u16(), 404);
        let res = server.post("/articles/999/translation-request", "").await;
        assert_eq!(res.status().as_u16(), 404);
        assert_eq!(server.count("SELECT count(*) FROM events"), 0);
    }

    /// 一覧でブックマークした記事は、振り分け済みとして一覧から外れる。
    #[tokio::test]
    async fn list_leaves_out_bookmarked_articles() {
        let db = Db::open_in_memory().unwrap();
        let (id, _) = seed(&db, "https://e.com/a", "見出しA");
        let server = Server::start(db).await;
        server
            .post(&format!("/articles/{id}/feedback"), "kind=bookmark")
            .await;
        let (_, html) = server.get("/?all=1").await;
        assert!(!html.contains("見出しA"), "{html}");
        let (_, html) = server.get("/search?bookmarked=1").await;
        assert!(html.contains("見出しA"), "{html}");
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
