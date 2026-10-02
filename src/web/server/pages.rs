//! 画面（一覧・検索・記事の詳細・設定）とフィード。

use super::*;

/// Web の一覧の条件（設定の期間・件数と、表示する最低点）。最低点が無ければ推薦点で絞らず、0 なら、評価 1〜2・未採点・
/// 軽水炉と無関係の記事も出す（すべて）。既読は一覧の既定と同じく未読だけ（フィード・JSON の一覧もこれを使う）。
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
    min_score: Option<u8>,
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
    /// 表示する最低点（0〜100）。無ければ既定（`default_min`）
    min: Option<String>,
    /// Web の一覧だけが使う（`1` なら既読も出し、`0` なら隠す。無ければ一覧は隠し、絞り込みは出す）
    read: Option<String>,
    /// Web の一覧だけが使う（この評価（1〜5）以上に絞る。空なら絞らない）
    rating: Option<String>,
    /// Web の一覧だけが使う（ブックマークに絞る）
    bookmarked: Option<String>,
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
    Extension(me): Extension<crate::db::Viewer>,
    Query(params): Query<ListParams>,
    RawQuery(raw): RawQuery,
) -> Result<Response, AppError> {
    // 既定の規則（一覧か絞り込みか、それぞれの最低点と既読の既定）は `ListView` にだけ置き、正規の URL と
    // そろえる。絞り込みの条件を先に入れ、最低点と既読は既定を受け取ってから決める
    let mut view = html::ListView {
        min: None,
        default_min: None,
        read: None,
        rating: params.rating()?,
        bookmarked: bookmark_mark(params.bookmarked.as_deref())?,
    };
    // JavaScript が無いときの評価の「★」（絞らない）は、絞り込みの条件（最低点・既読）も一緒に送る。一覧へ戻るので、
    // それらは使わずに一覧の既定にする（JavaScript があれば、選択肢の正規の URL へ移るので送られない）
    let leaving = params.rating.as_deref() == Some("") && !view.filtered();
    let carried = |value: Option<&str>| value.filter(|_| !leaving).map(str::to_string);
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
    let page = with_db_and_config(&state, move |db, web, labels| {
        let now = Utc::now();
        if view.filtered() {
            return filtered(db, web, labels, me, hash.as_deref(), view);
        }
        let boundary =
            db.begin_visit(user, now, Duration::minutes(web.visit_gap_minutes.into()))?;
        // 既読・ブックマークでは件数の上限より前に絞る（上位が既読で埋まっても、下の未読が出るように）
        let items = db.list_articles(ListQuery {
            read: view.read,
            bookmarked: view.bookmarked,
            ..list_query(web, user, hash.as_deref(), now, view.min)
        })?;
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
    me: crate::db::Viewer,
    hash: Option<&str>,
    view: html::ListView,
) -> Result<String, AppError> {
    let user = me.user_id;
    let params = Params {
        min_rating: view
            .rating
            .filter(|r| *r > 0)
            .map(|r| r.to_string())
            .unwrap_or_default(),
        unrated: view.rating == Some(0),
        bookmarked: view.bookmarked,
        read: view.read,
        min_score: view
            .min
            .filter(|m| *m > 0)
            .map(|m| m.to_string())
            .unwrap_or_default(),
        ..Params::default()
    };
    let query = params
        .to_query(user, hash, web.list_limit)
        .map_err(|_| AppError::BadRequest("rating must be 1..=5"))?;
    let items = db.search_articles(&query)?;
    let parts = PageParts::new(db, me, hash, web)?;
    let page = parts.page(labels);
    Ok(html::filtered_page(&items, view, &page))
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
    // 記事のリンクは絶対 URL にする。http で待ち受けているので `http://` + Host
    let base = format!("http://{}", request_host(&headers, &state.web));
    let xml = with_db_and_config(&state, move |db, web, labels| {
        let now = Utc::now();
        let Some(me) = feed_viewer(db, params.token.as_deref())? else {
            return Ok(None);
        };
        let (user, hash) = viewer(db, me)?;
        let min = web.default_min(hash.as_deref());
        let items = list_items(db, web, user, hash.as_deref(), now, min)?;
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
    let base = format!("http://{}", request_host(headers, &state.web));
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
        seed(&db, "https://e.com/a", "未採点");
        seed_with(&db, "https://e.com/unrelated", "無関係", false);
        let server = Server::start(db).await;

        let (status, html) = server.get("/").await;
        assert_eq!(status, 200);
        assert!(
            html.contains("未採点") && !html.contains("無関係"),
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
        assert!(html.contains("未採点") && html.contains("無関係"), "{html}");
        let (_, html) = server.get("/?min=50").await;
        assert!(!html.contains("未採点"), "{html}");
        // JavaScript が無いときの選択で送られる空の値は、既定（最低点なし）
        let res = server.get_raw("/?min=").await;
        assert_eq!(res.status().as_u16(), 303);
        assert_eq!(res.headers()["location"], "/");

        let (_, xml) = server.get(&server.feed_path()).await;
        assert!(xml.contains("未採点") && !xml.contains("無関係"), "{xml}");
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
