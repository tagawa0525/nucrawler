//! MCP の stdio サーバー。オーナーが Claude Code などから記事を検索・参照するための、
//! 読み取り専用のツールだけを持つ（LLM の呼び出しや DB への書き込みはしない）。
//! stdout は JSON-RPC に使うので、ログは stderr（tracing）にだけ出す。
//!
//! 閲覧判定と「記事ごとに見える最も詳しい版」は、Web UI と同じ `Db::list_articles` と
//! `Db::article_detail` に任せる。stdio で起動できるのはこのマシンの利用者だけなので、
//! オーナーとして判定する。

use std::sync::{Arc, Mutex, PoisonError};

use chrono::{DateTime, Duration, Utc};
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::model::{ContentBlock, Implementation, IntoContents, ServerCapabilities, ServerConfig};
use rmcp::schemars::{self, JsonSchema};
use rmcp::{ServerHandler, ServiceExt, tool, tool_handler, tool_router};
use serde::{Deserialize, Serialize};

use crate::config::WebConfig;
use crate::db::{ArtifactVersion, Db, DbError, ListItem, Rating, SearchOrder, SearchQuery};
use crate::web::html::SourceLabels;

#[derive(Debug, thiserror::Error)]
pub enum McpError {
    #[error("failed to start the mcp server")]
    Init(#[from] Box<rmcp::service::ServerInitializeError>),
    #[error("the mcp server task failed")]
    Join(#[from] tokio::task::JoinError),
}

/// ツールの失敗。呼び出し側（LLM）に見せるツールの結果のエラーとして返す。
#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error(transparent)]
    Db(#[from] DbError),
    #[error("a database task failed")]
    Join(#[from] tokio::task::JoinError),
    #[error("{0}")]
    InvalidParams(String),
    #[error("article {0} not found")]
    NotFound(i64),
}

impl IntoContents for ToolError {
    fn into_contents(self) -> Vec<ContentBlock> {
        let message = match &self {
            ToolError::Db(_) | ToolError::Join(_) => {
                // 詳細（SQL やスキーマ）はログにだけ残す
                tracing::error!("{}", crate::errors::error_chain(&self));
                "internal error".to_string()
            }
            ToolError::InvalidParams(_) | ToolError::NotFound(_) => self.to_string(),
        };
        vec![ContentBlock::text(message)]
    }
}

/// `search_articles` の引数。
#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct SearchParams {
    /// 原題・本文・要約・和訳に含む語（大文字と小文字を区別しない）。空白で区切るとすべてを含む記事に絞る
    pub keyword: Option<String>,
    /// この日（日本時間、YYYY-MM-DD）・月（YYYY-MM）・年（YYYY）以降に公開（無ければ取得）された記事。既定は Web UI の一覧と同じ期間（設定の web.list_days 日）
    pub since: Option<String>,
    /// この日（日本時間、YYYY-MM-DD）・月（YYYY-MM）・年（YYYY）までに公開（無ければ取得）された記事（その日・月・年を含む）
    pub until: Option<String>,
    /// ソースの ID（sources.toml の id）
    pub source: Option<String>,
    /// この点数以上の記事だけ。既定は Web UI の一覧と同じ（設定の web.min_score）。指定すると include_hidden でも未採点の記事は除く
    pub min_score: Option<u8>,
    /// Web UI の「すべて表示」と同じく、評価 1〜2・閾値未満・未採点・軽水炉に関係しない記事も含める
    #[serde(default)]
    pub include_hidden: bool,
    /// 最大件数。既定は Web UI の一覧と同じ（設定の web.list_limit）
    pub limit: Option<usize>,
}

/// `get_article` の引数。
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ArticleParams {
    /// 記事の ID（search_articles の結果の id）
    pub id: i64,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct SearchResult {
    /// 点数の高い順（未採点は後ろ）、同点なら新しい順
    pub articles: Vec<ArticleSummary>,
}

/// 記事の 1 行。要約は利用者が閲覧できる最新の版のもの。
#[derive(Debug, Serialize, JsonSchema)]
pub struct ArticleSummary {
    pub id: i64,
    pub source_id: String,
    /// ソースの表示名
    pub source: String,
    /// 元記事の URL
    pub url: String,
    /// 原題
    pub title: String,
    pub title_ja: Option<String>,
    pub summary_ja: Option<String>,
    /// 公開日時（無ければ取得日時）。UTC の RFC 3339
    pub date: String,
    /// 推薦点（0〜100）：関心プロファイルでの LLM の点数に、利用者の評価から学んだ補正を足した点数
    pub score: Option<u8>,
    /// 補正の前の LLM の点数（0〜100）
    pub llm_score: Option<u8>,
    /// 点数の理由
    pub reason: Option<String>,
    /// 点数が当たった関心分野（関心プロファイルの語）
    pub matched: Vec<String>,
    /// 点数が当たった推薦しない話題（関心プロファイルの語）
    pub excluded: Vec<String>,
    /// 軽水炉に関係する
    pub lwr_relevant: Option<bool>,
    /// 利用者の評価（1〜5。5 必読、4 読んでよかった、3 どちらでもない、2 不要、1 二度と出さないでほしい）。評価なしは null
    pub rating: Option<u8>,
    pub has_translation: bool,
    /// 原文を読むのに必要で、利用者が持っていない会員資格の名前
    pub locked_by: Vec<String>,
}

/// `get_article` の結果。
#[derive(Debug, Serialize, JsonSchema)]
pub struct ArticleOutput {
    #[serde(flatten)]
    pub article: ArticleSummary,
    /// 閲覧できる最新の要約（title_ja, summary_ja, points_ja, implications_ja, topics, lwr_relevant）
    pub digest: Option<DigestOutput>,
    /// 閲覧できる最新の全文和訳
    pub translation: Option<TranslationOutput>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct DigestOutput {
    pub model: String,
    pub created_at: String,
    pub payload: serde_json::Value,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct TranslationOutput {
    pub model: String,
    pub created_at: String,
    pub body_ja: String,
}

/// MCP のサーバー。`Db` は同期 API なので、`spawn_blocking` の中で 1 つずつ使う。
#[derive(Clone)]
pub struct Server {
    db: Arc<Mutex<Db>>,
    web: Arc<WebConfig>,
    labels: Arc<SourceLabels>,
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl Server {
    pub fn new(db: Db, web: WebConfig, labels: SourceLabels) -> Self {
        Self {
            db: Arc::new(Mutex::new(db)),
            web: Arc::new(web),
            labels: Arc::new(labels),
            tool_router: Self::tool_router(),
        }
    }

    #[tool(
        description = "原子力ニュースの記事を検索する。既定では Web UI の一覧と同じく、直近の期間の、閾値以上に採点された軽水炉関係の記事（評価 1〜2 を付けた記事を除く）を点数の高い順に返す。"
    )]
    pub async fn search_articles(
        &self,
        Parameters(params): Parameters<SearchParams>,
    ) -> Result<Json<SearchResult>, ToolError> {
        let web = self.web.clone();
        let labels = self.labels.clone();
        self.with_db(move |db| search(db, &web, &labels, params, Utc::now()))
            .await
            .map(Json)
    }

    #[tool(
        description = "記事 1 件の詳細を返す：元記事の URL、最新の要約（要点・示唆を含む）、全文和訳があればその本文。"
    )]
    pub async fn get_article(
        &self,
        Parameters(params): Parameters<ArticleParams>,
    ) -> Result<Json<ArticleOutput>, ToolError> {
        let labels = self.labels.clone();
        self.with_db(move |db| article(db, &labels, params.id))
            .await
            .map(Json)
    }
}

impl Server {
    /// DB の処理をブロッキング用のスレッドで行う。
    async fn with_db<T: Send + 'static>(
        &self,
        f: impl FnOnce(&Db) -> Result<T, ToolError> + Send + 'static,
    ) -> Result<T, ToolError> {
        let db = self.db.clone();
        tokio::task::spawn_blocking(move || {
            let db = db.lock().unwrap_or_else(PoisonError::into_inner);
            f(&db)
        })
        .await?
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for Server {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("nucrawler", env!("CARGO_PKG_VERSION")))
            .with_instructions(
                "nucrawler が巡回・要約・採点した原子力（軽水炉）ニュースを読み取り専用で参照する。\
                 search_articles で探し、get_article で要約や和訳を読む。",
            )
    }
}

/// stdin/stdout で、クライアントが接続を閉じるまで応答する。
pub async fn run(server: Server) -> Result<(), McpError> {
    let service = server
        .serve(rmcp::transport::stdio())
        .await
        .map_err(Box::new)?;
    tracing::info!("serving mcp on stdio");
    service.waiting().await?;
    Ok(())
}

/// 利用者（stdio なのでオーナー）と、現在のプロファイルのハッシュ。
fn viewer(db: &Db) -> Result<(i64, Option<String>), DbError> {
    let user = db.owner_id()?;
    let hash = db.profile_hash(user)?;
    Ok((user, hash))
}

fn search(
    db: &Db,
    web: &WebConfig,
    labels: &SourceLabels,
    params: SearchParams,
    now: DateTime<Utc>,
) -> Result<SearchResult, ToolError> {
    let invalid = |e: crate::search::SearchError| ToolError::InvalidParams(e.to_string());
    let since = match params.since.as_deref() {
        Some(date) => crate::search::since(date).map_err(invalid)?,
        None => now - Duration::days(web.list_days.into()),
    };
    let until = params
        .until
        .as_deref()
        .map(crate::search::until)
        .transpose()
        .map_err(invalid)?;
    if params.min_score.is_some_and(|min| min > 100) {
        return Err(ToolError::InvalidParams("min_score must be 0..=100".into()));
    }
    let (user, hash) = viewer(db)?;
    let items = db.search_articles(&SearchQuery {
        user_id: user,
        profile_hash: hash.as_deref(),
        terms: params
            .keyword
            .as_deref()
            .map(crate::search::parse_terms)
            .unwrap_or_default(),
        since: Some(since),
        until,
        sources: params.source.into_iter().collect(),
        min_score: params.min_score,
        // 既定は Web UI の一覧と同じく隠す（最低点の既定も一覧と同じ）
        hide: !params.include_hidden,
        hide_below: params
            .min_score
            .or_else(|| web.default_min(hash.as_deref())),
        order: SearchOrder::Score,
        limit: params.limit.unwrap_or(web.list_limit),
        ..SearchQuery::default()
    })?;
    let articles = items.into_iter().map(|i| summary(i, labels)).collect();
    Ok(SearchResult { articles })
}

fn article(db: &Db, labels: &SourceLabels, id: i64) -> Result<ArticleOutput, ToolError> {
    let (user, hash) = viewer(db)?;
    let detail = db
        .article_detail(user, hash.as_deref(), id)?
        .ok_or(ToolError::NotFound(id))?;
    Ok(ArticleOutput {
        digest: detail.digests.first().map(|v| DigestOutput {
            model: v.model.clone(),
            created_at: v.created_at.clone(),
            payload: v.payload.clone(),
        }),
        translation: detail.translations.first().map(translation),
        article: summary(detail.item, labels),
    })
}

fn translation(v: &ArtifactVersion) -> TranslationOutput {
    TranslationOutput {
        model: v.model.clone(),
        created_at: v.created_at.clone(),
        body_ja: v.payload["body_ja"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
    }
}

fn summary(item: ListItem, labels: &SourceLabels) -> ArticleSummary {
    ArticleSummary {
        source: labels
            .get(&item.source_id)
            .cloned()
            .unwrap_or_else(|| item.source_id.clone()),
        id: item.article_id,
        source_id: item.source_id,
        url: item.url,
        title: item.title,
        title_ja: item.title_ja,
        summary_ja: item.summary_ja,
        date: item.at,
        score: item.score,
        llm_score: item.llm_score,
        reason: item.reason,
        matched: item.matched,
        excluded: item.excluded,
        lwr_relevant: item.lwr_relevant,
        rating: item.rating.map(Rating::get),
        has_translation: item.has_translation,
        locked_by: item.locked_by,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Lang;
    use crate::db::{ArtifactKind, ContentKind, ContentOrigin, NewArticle, NewArtifact, ScoreKey};
    use crate::db::{ListQuery, Rating};
    use chrono::Duration;

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-09-27T03:00:00Z")
            .unwrap()
            .to_utc()
    }

    /// オーナーのプロファイルを保存し、そのハッシュを返す。
    fn with_profile(db: &Db) -> String {
        let profile = crate::profile::parse(include_str!("../examples/profile.toml")).unwrap();
        db.save_profile(db.owner_id().unwrap(), &profile, now())
            .unwrap();
        crate::profile::hash(&profile)
    }

    #[derive(Clone, Copy)]
    struct Seed<'a> {
        source_id: &'a str,
        url: &'a str,
        published: &'a str,
        title_ja: &'a str,
        summary_ja: &'a str,
        lwr_relevant: bool,
        score: Option<u8>,
    }

    impl Default for Seed<'_> {
        fn default() -> Self {
            Self {
                source_id: "wnn",
                url: "https://e.com/a",
                published: "2026-09-26T00:00:00Z",
                title_ja: "題",
                summary_ja: "要約",
                lwr_relevant: true,
                score: Some(80),
            }
        }
    }

    fn digest_payload(title_ja: &str, summary_ja: &str, lwr_relevant: bool) -> serde_json::Value {
        serde_json::json!({
            "title_ja": title_ja, "summary_ja": summary_ja, "points_ja": ["点"],
            "implications_ja": "示唆", "lwr_relevant": lwr_relevant, "topics": ["規制・審査"],
        })
    }

    /// 記事と公開の本文、その digest（と採点）を登録し、記事の ID を返す。
    fn seed(db: &Db, hash: &str, s: Seed) -> i64 {
        let id = db
            .insert_article(&NewArticle {
                source_id: s.source_id,
                url: s.url,
                title: "Original Title",
                lang: Lang::En,
                published_at: Some(s.published),
            })
            .unwrap()
            .unwrap();
        let body = db
            .insert_content(id, ContentKind::Body, ContentOrigin::Page, "body")
            .unwrap();
        let digest = db
            .insert_artifact(
                &NewArtifact {
                    article_id: id,
                    kind: ArtifactKind::Digest,
                    backend: "claude-cli",
                    model: "sonnet",
                    prompt_version: 1,
                    payload: &digest_payload(s.title_ja, s.summary_ja, s.lwr_relevant),
                    inputs: &[body],
                    glossary_at: None,
                },
                now(),
            )
            .unwrap();
        if let Some(score) = s.score {
            db.insert_score(
                ScoreKey {
                    user_id: db.owner_id().unwrap(),
                    profile_hash: hash,
                    backend: "claude-cli",
                    model: "sonnet",
                    prompt_version: 1,
                },
                digest,
                score,
                Some("理由"),
                now(),
            )
            .unwrap();
        }
        id
    }

    fn ids(result: &SearchResult) -> Vec<i64> {
        result.articles.iter().map(|a| a.id).collect()
    }

    fn run_search(db: &Db, params: SearchParams) -> Result<SearchResult, ToolError> {
        search(
            db,
            &WebConfig::default(),
            &SourceLabels::new(),
            params,
            now(),
        )
    }

    /// 既定では Web UI の一覧と同じく、評価 1〜2・閾値未満・未採点・非軽水炉を除く。
    #[test]
    fn search_hides_what_the_web_list_hides_by_default() {
        let db = Db::open_in_memory().unwrap();
        let hash = with_profile(&db);
        let shown = seed(&db, &hash, Seed::default());
        let low = seed(
            &db,
            &hash,
            Seed {
                url: "https://e.com/low",
                score: Some(10),
                ..Seed::default()
            },
        );
        let unscored = seed(
            &db,
            &hash,
            Seed {
                url: "https://e.com/unscored",
                score: None,
                ..Seed::default()
            },
        );
        let other = seed(
            &db,
            &hash,
            Seed {
                url: "https://e.com/other",
                lwr_relevant: false,
                ..Seed::default()
            },
        );
        let down = seed(
            &db,
            &hash,
            Seed {
                url: "https://e.com/down",
                score: Some(90),
                ..Seed::default()
            },
        );
        db.rate(db.owner_id().unwrap(), down, Rating::new(2), now())
            .unwrap();

        let result = run_search(&db, SearchParams::default()).unwrap();
        assert_eq!(ids(&result), [shown]);

        let all = run_search(
            &db,
            SearchParams {
                include_hidden: true,
                ..SearchParams::default()
            },
        )
        .unwrap();
        // 点数の高い順、同点なら新しい順（ここでは ID の大きい順）
        assert_eq!(ids(&all), [down, other, shown, low, unscored]);
        let a = &all.articles[0];
        assert_eq!(a.rating, Some(2));
        assert_eq!(a.url, "https://e.com/down");
        assert_eq!(a.title_ja.as_deref(), Some("題"));
        assert_eq!(a.reason.as_deref(), Some("理由"));
    }

    /// プロファイルが無い利用者は採点が無いので、既定では Web UI の一覧と同じく最低点を掛けない。評価 1〜2 と
    /// 軽水炉に関係しない記事は隠す。
    #[test]
    fn search_without_a_profile_shows_unscored_articles_by_default() {
        let db = Db::open_in_memory().unwrap();
        let unscored = Seed {
            score: None,
            ..Seed::default()
        };
        let shown = seed(&db, "", unscored);
        seed(
            &db,
            "",
            Seed {
                url: "https://e.com/other",
                lwr_relevant: false,
                ..unscored
            },
        );
        let down = seed(
            &db,
            "",
            Seed {
                url: "https://e.com/down",
                ..unscored
            },
        );
        db.rate(db.owner_id().unwrap(), down, Rating::new(2), now())
            .unwrap();
        let result = run_search(&db, SearchParams::default()).unwrap();
        assert_eq!(ids(&result), [shown]);
    }

    /// 最低点は Web UI と同じく 0〜100。
    #[test]
    fn search_rejects_a_minimum_above_100() {
        let db = Db::open_in_memory().unwrap();
        let err = run_search(
            &db,
            SearchParams {
                min_score: Some(101),
                ..SearchParams::default()
            },
        )
        .unwrap_err();
        assert!(matches!(err, ToolError::InvalidParams(_)), "{err:?}");
    }

    #[test]
    fn search_filters_by_keyword_source_period_score_and_limit() {
        let db = Db::open_in_memory().unwrap();
        let hash = with_profile(&db);
        let a = seed(
            &db,
            &hash,
            Seed {
                url: "https://e.com/1",
                summary_ja: "柏崎刈羽の再稼働",
                score: Some(90),
                ..Seed::default()
            },
        );
        let b = seed(
            &db,
            &hash,
            Seed {
                source_id: "nra",
                url: "https://e.com/2",
                published: "2026-09-21T00:00:00Z",
                score: Some(70),
                ..Seed::default()
            },
        );
        let c = seed(
            &db,
            &hash,
            Seed {
                url: "https://e.com/3",
                published: "2026-09-10T00:00:00Z",
                score: Some(60),
                ..Seed::default()
            },
        );
        let search = |params| ids(&run_search(&db, params).unwrap());

        // 既定の期間（web.list_days = 7 日）の外の記事は出ない
        assert_eq!(search(SearchParams::default()), [a, b]);
        assert_eq!(
            search(SearchParams {
                keyword: Some("柏崎".into()),
                ..SearchParams::default()
            }),
            [a]
        );
        // 原題も対象で、大文字と小文字を区別しない
        assert_eq!(
            search(SearchParams {
                keyword: Some("original".into()),
                ..SearchParams::default()
            }),
            [a, b]
        );
        assert_eq!(
            search(SearchParams {
                source: Some("nra".into()),
                ..SearchParams::default()
            }),
            [b]
        );
        // 日付は日本時間で、until はその日を含む
        assert_eq!(
            search(SearchParams {
                since: Some("2026-09-01".into()),
                until: Some("2026-09-21".into()),
                ..SearchParams::default()
            }),
            [b, c]
        );
        assert_eq!(
            search(SearchParams {
                min_score: Some(80),
                ..SearchParams::default()
            }),
            [a]
        );
        assert_eq!(
            search(SearchParams {
                limit: Some(1),
                ..SearchParams::default()
            }),
            [a]
        );
    }

    /// キーワードは本文も対象にし、空白で区切った語をすべて含む記事に絞る。月だけの期間も使える。
    #[test]
    fn search_keyword_covers_body_and_all_terms() {
        let db = Db::open_in_memory().unwrap();
        let hash = with_profile(&db);
        let a = seed(&db, &hash, Seed::default());
        db.insert_content(
            a,
            ContentKind::Body,
            ContentOrigin::Page,
            "蒸気発生器の伝熱管を交換した",
        )
        .unwrap();
        seed(
            &db,
            &hash,
            Seed {
                url: "https://e.com/b",
                ..Seed::default()
            },
        );
        let search = |keyword: &str| {
            ids(&run_search(
                &db,
                SearchParams {
                    keyword: Some(keyword.into()),
                    since: Some("2026-09".into()),
                    ..SearchParams::default()
                },
            )
            .unwrap())
        };
        assert_eq!(search("伝熱管"), [a]);
        assert_eq!(search("伝熱管　交換"), [a]);
        assert!(search("伝熱管 燃料棒").is_empty());
    }

    #[test]
    fn search_rejects_malformed_dates() {
        let db = Db::open_in_memory().unwrap();
        let err = run_search(
            &db,
            SearchParams {
                since: Some("2026/09/01".into()),
                ..SearchParams::default()
            },
        )
        .unwrap_err();
        assert!(matches!(err, ToolError::InvalidParams(_)), "{err:?}");
    }

    fn add_translation(db: &Db, article_id: i64, body_ja: &str) {
        let full = db
            .insert_content(
                article_id,
                ContentKind::Fulltext,
                ContentOrigin::Page,
                "full",
            )
            .unwrap();
        db.insert_translation(
            &NewArtifact {
                article_id,
                kind: ArtifactKind::Translation,
                backend: "claude-cli",
                model: "opus",
                prompt_version: 1,
                payload: &serde_json::json!({"body_ja": body_ja}),
                inputs: &[full],
                glossary_at: None,
            },
            now(),
        )
        .unwrap();
    }

    #[test]
    fn article_returns_url_latest_digest_and_translation() {
        let db = Db::open_in_memory().unwrap();
        let hash = with_profile(&db);
        let id = seed(&db, &hash, Seed::default());
        add_translation(&db, id, "和訳の本文");
        let mut labels = SourceLabels::new();
        labels.insert("wnn".into(), "World Nuclear News".into());

        let out = article(&db, &labels, id).unwrap();
        assert_eq!(out.article.url, "https://e.com/a");
        assert_eq!(out.article.source, "World Nuclear News");
        assert_eq!(out.article.score, Some(80));
        let digest = out.digest.unwrap();
        assert_eq!(digest.payload["implications_ja"], "示唆");
        assert_eq!(out.translation.unwrap().body_ja, "和訳の本文");

        // 詳細を読んでも既読などの行動は記録しない（読み取り専用）
        let listed = run_search(&db, SearchParams::default()).unwrap();
        assert_eq!(ids(&listed), [id]);
        assert!(
            db.list_articles(ListQuery {
                user_id: db.owner_id().unwrap(),
                profile_hash: Some(&hash),
                min_score: Some(0),
                since: now() - Duration::days(7),
                show_all: true,
                read: None,
                bookmarked: None,
                limit: 10,
            })
            .unwrap()[0]
                .read_at
                .is_none()
        );
    }

    #[test]
    fn article_not_found_is_a_tool_error() {
        let db = Db::open_in_memory().unwrap();
        let err = article(&db, &SourceLabels::new(), 42).unwrap_err();
        assert!(matches!(err, ToolError::NotFound(42)), "{err:?}");
    }

    /// 会員限定の本文から作った版は、会員資格の無いオーナーには見えず、公開の版を返す。
    #[test]
    fn article_shows_the_most_detailed_version_the_owner_can_view() {
        let db = Db::open_in_memory().unwrap();
        let hash = with_profile(&db);
        let id = seed(&db, &hash, Seed::default());
        let membership: i64 = db
            .conn()
            .query_row("SELECT id FROM memberships WHERE code = 'aesj'", [], |r| {
                r.get(0)
            })
            .unwrap();
        db.conn()
            .execute(
                "INSERT INTO contents (article_id, kind, access_membership_id, text, origin, fetched_at)
                 VALUES (?1, 'fulltext', ?2, 'member text', 'login', '2026-09-27T00:00:00Z')",
                rusqlite::params![id, membership],
            )
            .unwrap();
        let member_content = db.conn().last_insert_rowid();
        db.insert_artifact(
            &NewArtifact {
                article_id: id,
                kind: ArtifactKind::Digest,
                backend: "claude-cli",
                model: "sonnet",
                prompt_version: 1,
                payload: &digest_payload("会員版", "会員の要約", true),
                inputs: &[member_content],
                glossary_at: None,
            },
            now() + Duration::hours(1),
        )
        .unwrap();

        let out = article(&db, &SourceLabels::new(), id).unwrap();
        assert_eq!(out.digest.unwrap().payload["title_ja"], "題");
        let listed = run_search(&db, SearchParams::default()).unwrap();
        assert_eq!(listed.articles[0].title_ja.as_deref(), Some("題"));

        db.conn()
            .execute(
                "INSERT INTO user_memberships VALUES (?1, ?2)",
                [db.owner_id().unwrap(), membership],
            )
            .unwrap();
        let out = article(&db, &SourceLabels::new(), id).unwrap();
        assert_eq!(out.digest.unwrap().payload["title_ja"], "会員版");
    }

    /// 書き込みや LLM を使うツールは持たない。
    #[test]
    fn exposes_only_read_only_tools() {
        let mut names: Vec<String> = Server::tool_router()
            .list_all()
            .into_iter()
            .map(|t| t.name.to_string())
            .collect();
        names.sort();
        assert_eq!(names, ["get_article", "search_articles"]);
    }

    /// ツールのハンドラは、結果を構造化された JSON で返し、失敗はツールのエラーにする。
    #[tokio::test]
    async fn handlers_return_structured_json() {
        let db = Db::open_in_memory().unwrap();
        let hash = with_profile(&db);
        let id = seed(
            &db,
            &hash,
            Seed {
                published: "2099-01-01T00:00:00Z",
                ..Seed::default()
            },
        );
        let server = Server::new(db, WebConfig::default(), SourceLabels::new());
        let Json(found) = server
            .search_articles(Parameters(SearchParams::default()))
            .await
            .unwrap();
        assert_eq!(ids(&found), [id]);
        let Json(out) = server
            .get_article(Parameters(ArticleParams { id }))
            .await
            .unwrap();
        let value = serde_json::to_value(&out).unwrap();
        assert_eq!(value["id"], id);
        assert_eq!(value["digest"]["payload"]["summary_ja"], "要約");
        assert!(value["translation"].is_null());
        let missing = server
            .get_article(Parameters(ArticleParams { id: id + 1 }))
            .await;
        assert!(matches!(missing, Err(ToolError::NotFound(_))));
    }
}
