//! 画面（検索・記事の詳細・設定）とフィード。一覧は `list`。

use super::*;

/// 警告は直近 24 時間のものだけ出す。
pub(super) fn warnings(db: &Db) -> Result<Vec<crate::db::Warning>, DbError> {
    let now = Utc::now();
    db.warnings(now - Duration::hours(24), now)
}

#[derive(serde::Deserialize)]
pub(super) struct FeedParams {
    token: Option<String>,
}

/// Web の既定の一覧と同じ記事の Atom フィード。閲覧ではないので、訪問も開いたことも記録しない。
/// フィードリーダーはログインできないので、利用者は URL のトークンで決める（無い・違えば 401）。
pub(super) async fn feed(
    State(state): State<AppState>,
    Query(params): Query<FeedParams>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    // 記事のリンクは絶対 URL にする
    let base = base_url(&headers, &state.web);
    let xml = with_db_and_config(&state, move |db, web, labels| {
        let now = Utc::now();
        let Some(me) = feed_viewer(db, params.token.as_deref())? else {
            return Ok(None);
        };
        let (user, hash) = viewer(db, me)?;
        let min = web.default_min(db.has_scores(user, hash.as_deref())?);
        let items = list_items(db, web, user, hash.as_deref(), now, min, None)?;
        // フィード自身の URL はトークン付き（購読し直すリーダーが読めるように）
        let token = params.token.unwrap_or_default();
        let self_href = format!("{base}/feed.xml?token={token}");
        Ok(Some(feed::atom(
            &items,
            &base,
            &self_href,
            user,
            labels,
            &crate::db::timestamp(now),
        )))
    })
    .await?;
    let Some(xml) = xml else {
        return Ok((StatusCode::UNAUTHORIZED, "feed token required").into_response());
    };
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
    Extension(me): Extension<crate::db::Viewer>,
    RawQuery(raw): RawQuery,
) -> Result<Response, AppError> {
    let params = Params::from_query(raw.as_deref().unwrap_or(""));
    let (status, page) = with_db_and_config(&state, move |db, web, labels| {
        let (user, hash) = viewer(db, me)?;
        let vocabulary = db.topic_usage()?;
        let parts = PageParts::new(db, me, hash.as_deref(), web)?;
        let page = parts.page(labels);
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
    Extension(me): Extension<crate::db::Viewer>,
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
    let page = with_db_and_config(&state, move |db, web, labels| {
        let now = Utc::now();
        let (user, hash) = viewer(db, me)?;
        let mut detail = db
            .article_detail(user, hash.as_deref(), id)?
            .ok_or(AppError::NotFound)?;
        let reports = db.reports(
            user,
            &ReportFilter {
                article_id: Some(id),
                // 指摘は管理者に宛てたものなので、一般の利用者には自分の指摘だけを出す
                reporter: (!me.is_admin).then_some(user),
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
        let parts = PageParts::new(db, me, hash.as_deref(), web)?;
        let page = parts.page(labels);
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
    Extension(me): Extension<crate::db::Viewer>,
    method: Method,
    Path(id): Path<i64>,
) -> Result<Response, AppError> {
    let url = with_db(&state, move |db| {
        let (user, _) = viewer(db, me)?;
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

#[derive(serde::Deserialize)]
pub(super) struct SettingsParams {
    /// パスワードの変更の結果（`changed`・`wrong`・`locked`）
    password: Option<String>,
}

pub(super) async fn settings(
    State(state): State<AppState>,
    Extension(me): Extension<crate::db::Viewer>,
    Query(params): Query<SettingsParams>,
    headers: HeaderMap,
) -> Result<Html<String>, AppError> {
    let notice = params
        .password
        .as_deref()
        .and_then(html::PasswordNotice::from_query);
    let page = settings_html(&state, me, &headers, notice).await?;
    Ok(Html(page))
}

/// 設定画面（パスワードの変更で条件を満たさないときも、理由を添えてこれを出す）。
pub(super) async fn settings_html(
    state: &AppState,
    me: crate::db::Viewer,
    headers: &HeaderMap,
    notice: Option<html::PasswordNotice>,
) -> Result<String, AppError> {
    let base = base_url(headers, &state.web);
    with_db_and_config(state, move |db, web, labels| {
        let (user, hash) = viewer(db, me)?;
        // 購読用のフィードの URL（コピーして使うので絶対 URL）
        let feed_url = db
            .feed_token(user)?
            .map(|token| format!("{base}/feed.xml?token={token}"));
        let terms = db.glossary_entries()?.len();
        // 受付箱は管理者だけ
        let pending = if me.is_admin {
            Some(
                db.report_counts()?
                    .into_iter()
                    .find_map(|(status, n)| (status == ReportStatus::Pending).then_some(n))
                    .unwrap_or(0),
            )
        } else {
            None
        };
        let parts = PageParts::new(db, me, hash.as_deref(), web)?;
        let page = parts.page(labels);
        Ok(html::settings_page(
            terms,
            pending,
            feed_url.as_deref(),
            notice,
            &page,
        ))
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;
    use crate::web::server::test_support::*;

    /// 詳細には、同じ報道のほかの記事と関連記事を出す。
    #[tokio::test]
    async fn detail_shows_the_story_and_related_articles() {
        let db = Db::open_in_memory().unwrap();
        let (a, _) = seed(&db, "https://e.com/a", "記事A");
        let (b, _) = seed(&db, "https://e.com/b", "記事B");
        let (c, _) = seed(&db, "https://e.com/c", "続報C");
        let judge = |id: i64, links: &[(i64, crate::db::StoryRelation)]| {
            let links: Vec<crate::db::StoryLink> = links
                .iter()
                .map(|&(other_id, relation)| crate::db::StoryLink {
                    other_id,
                    relation,
                    similarity: 0.5,
                })
                .collect();
            db.insert_story(
                &crate::db::NewArtifact {
                    article_id: id,
                    kind: crate::db::ArtifactKind::Story,
                    backend: "claude-cli",
                    model: "sonnet",
                    prompt_version: 1,
                    payload: &serde_json::json!({"candidates": [], "same": [], "related": []}),
                    inputs: &[],
                    glossary_at: None,
                },
                &links,
                chrono::Utc::now(),
            )
            .unwrap();
        };
        use crate::db::StoryRelation::{Related, Same};
        judge(a, &[(b, Same), (c, Related)]);
        judge(b, &[(a, Same)]);
        db.rebuild_stories().unwrap();
        let server = Server::start(db).await;
        let (_, html) = server.get(&format!("/articles/{a}")).await;
        let story = html.split("<h2>同じ報道</h2>").nth(1).expect("story");
        assert!(story.contains("記事B"), "{html}");
        let related = html.split("<h2>関連記事</h2>").nth(1).expect("related");
        assert!(related.contains("続報C"), "{html}");
    }

    /// フィードは既定の一覧と同じ記事を Atom で出し、閲覧としては記録しない。
    #[tokio::test]
    async fn feed_lists_recommended_articles_as_atom() {
        let db = Db::open_in_memory().unwrap();
        let good = seed_recommended_and_hidden(&db);
        let server = Server::start(db).await;
        let (status, content_type, xml) = server.get_with_type(&server.feed_path()).await;
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
        for hidden in ["低い点", "評価 2", "無関係", "採点前の記事"] {
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
        let feed = server.feed_path();
        let mut feeds = Vec::new();
        for host in hosts {
            let res = server
                .client
                .get(format!("{}{feed}", server.base))
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

    /// フィードと JSON の一覧は既定の一覧と同じ記事なので、既読の記事は出さない。
    #[tokio::test]
    async fn feed_and_api_leave_out_read_articles() {
        let db = Db::open_in_memory().unwrap();
        let (id, digest) = seed(&db, "https://e.com/a", "見出しA");
        score(&db, digest, 80);
        let server = Server::start(db).await;
        let feed = server.feed_path();
        for path in [feed.as_str(), "/api/articles"] {
            let (_, body) = server.get(path).await;
            assert!(body.contains("見出しA"), "{path}: {body}");
        }
        server.post(&format!("/articles/{id}/read"), "on=1").await;
        for path in [feed.as_str(), "/api/articles"] {
            let (_, body) = server.get(path).await;
            assert!(!body.contains("見出しA"), "{path}: {body}");
        }
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
