use std::collections::HashSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use serde::Deserialize;

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("failed to read {path}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to parse {path}")]
    Parse {
        path: PathBuf,
        source: toml::de::Error,
    },
    #[error("{path}: {reason}")]
    Invalid { path: PathBuf, reason: String },
    #[error("{path}: duplicate source id: {id}")]
    DuplicateSourceId { path: PathBuf, id: String },
    #[error("cannot determine config directory: neither XDG_CONFIG_HOME nor HOME is set")]
    NoConfigDir,
    #[error("cannot determine data directory: neither XDG_DATA_HOME nor HOME is set")]
    NoDataDir,
}

#[derive(Debug, Default, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub http: HttpConfig,
    pub pipeline: PipelineConfig,
    pub llm: LlmConfig,
    pub quota: crate::quota::QuotaConfig,
    /// Copilot の AI Credits の月の予算（copilot-cli で呼ぶときに使う）
    pub copilot_quota: Option<crate::quota::CreditsConfig>,
    pub web: WebConfig,
    pub recommend: RecommendConfig,
    /// 記事と好みの embedding を作る API。無ければ embedding を使わない
    pub embedding: Option<EmbeddingConfig>,
}

/// OpenAI 互換の embeddings API（`POST .../v1/embeddings`）の設定。r995 ではローカルのサーバー、
/// 社内では Azure OpenAI を呼ぶ。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmbeddingConfig {
    /// embeddings API の URL（例 `http://127.0.0.1:8080/v1/embeddings`）
    pub url: String,
    /// 送る `model`（Azure ではデプロイ名）
    pub model: String,
    /// 認証の形。`none` 以外では、鍵を `api_key_env` の環境変数から読む（設定ファイルには書かない）
    #[serde(default)]
    pub auth: EmbeddingAuth,
    pub api_key_env: Option<String>,
    /// 返すベクトルの次元（`dimensions` で切り詰められるモデルだけ）。無ければモデルの既定
    pub dimensions: Option<u32>,
    /// 好み（クエリ）の文の前に付ける接頭辞（ruri なら「検索クエリ: 」）
    #[serde(default)]
    pub query_prefix: String,
    /// 記事（文書）の文の前に付ける接頭辞（ruri なら「検索文書: 」）
    #[serde(default)]
    pub document_prefix: String,
    /// 1 回の呼び出しで送る文の数。モデルの指紋の試験文（`embedding::FINGERPRINT_TEXTS` 件）を含めて数える
    #[serde(default = "default_embedding_batch_size")]
    pub batch_size: usize,
    /// 1 回の呼び出しのタイムアウト（秒）
    #[serde(default = "default_embedding_timeout_secs")]
    pub timeout_secs: u64,
}

/// 1 回に送る文の数の上限。
pub const MAX_EMBEDDING_BATCH_SIZE: usize = 2048;
/// 1 回の呼び出しのタイムアウトの上限（秒）。
pub const MAX_EMBEDDING_TIMEOUT_SECS: u64 = 3600;

fn default_embedding_batch_size() -> usize {
    32
}

fn default_embedding_timeout_secs() -> u64 {
    30
}

/// embeddings API の認証の形。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EmbeddingAuth {
    /// 認証しない（ローカルのサーバー）
    #[default]
    None,
    /// `Authorization: Bearer <鍵>`（OpenAI）
    Bearer,
    /// `api-key: <鍵>`（Azure OpenAI）
    ApiKey,
}

impl EmbeddingConfig {
    /// 呼び出しが成り立たない値を拒否する。
    pub fn validate(&self) -> Result<(), String> {
        if !url::Url::parse(&self.url).is_ok_and(|u| matches!(u.scheme(), "http" | "https")) {
            return Err(format!(
                "embedding.url is not an http(s) url: {:?}",
                self.url
            ));
        }
        if self.model.trim().is_empty() {
            return Err("embedding.model must not be empty".into());
        }
        if self.auth != EmbeddingAuth::None
            && self
                .api_key_env
                .as_deref()
                .is_none_or(|name| name.trim().is_empty())
        {
            return Err("embedding.auth needs embedding.api_key_env".into());
        }
        // 鍵を平文で送らない
        if self.auth != EmbeddingAuth::None
            && !url::Url::parse(&self.url).is_ok_and(|u| u.scheme() == "https")
        {
            return Err(format!(
                "embedding.url must be https when embedding.auth sends a key, got {:?}",
                self.url
            ));
        }
        if self.dimensions == Some(0) {
            return Err("embedding.dimensions must be at least 1".into());
        }
        // 予約の期限は「呼び出しの回数 × タイムアウト」なので、どちらにも上限を置く
        if self.batch_size > MAX_EMBEDDING_BATCH_SIZE {
            return Err(format!(
                "embedding.batch_size must be at most {MAX_EMBEDDING_BATCH_SIZE}, got {}",
                self.batch_size
            ));
        }
        if self.timeout_secs > MAX_EMBEDDING_TIMEOUT_SECS {
            return Err(format!(
                "embedding.timeout_secs must be at most {MAX_EMBEDDING_TIMEOUT_SECS}, got {}",
                self.timeout_secs
            ));
        }
        if self.batch_size <= crate::embedding::FINGERPRINT_TEXTS {
            return Err(format!(
                "embedding.batch_size must be more than {} (the fingerprint texts sent with every call), got {}",
                crate::embedding::FINGERPRINT_TEXTS,
                self.batch_size
            ));
        }
        if self.timeout_secs == 0 {
            return Err("embedding.timeout_secs must be at least 1".into());
        }
        Ok(())
    }
}

/// 推薦点（関心プロファイルとの近さの点数に、評価から学んだ補正を足した点数）の設定。
#[derive(Debug, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RecommendConfig {
    /// 補正の正則化の強さ。大きいほど補正の前の点数に近いまま。ある特徴の補正が半分の強さに達するのは、その特徴の
    /// 評価がおよそ「強さ ÷ p(1−p)」件（p は LLM 点を確率とみなした値。50 点付近で強さの 4 倍）そろったとき。
    /// 1 件の評価は記事の特徴（ソース・トピック・関心分野など）のすべてに効くので、既定の 20 では、評価 1 件で
    /// 特徴を共有する記事が動くのは数点に収まる
    pub prior_strength: f64,
}

impl RecommendConfig {
    /// 学習できない強さ（0 以下・∞・NaN）を拒否する。
    pub fn validate(&self) -> Result<(), String> {
        if !(self.prior_strength.is_finite() && self.prior_strength > 0.0) {
            return Err(format!(
                "recommend.prior_strength must be positive and finite, got {}",
                self.prior_strength
            ));
        }
        Ok(())
    }
}

impl Default for RecommendConfig {
    fn default() -> Self {
        Self {
            prior_strength: 20.0,
        }
    }
}

/// LLM を呼ぶバックエンド。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LlmBackend {
    /// Claude Code の headless モード（`claude -p`）。サブスクリプションの枠で動かす（`[quota]`）
    #[default]
    ClaudeCli,
    /// GitHub Copilot CLI（`copilot`）。AI Credits の月の予算で動かす（`[copilot_quota]`）
    CopilotCli,
}

/// LLM を使う工程。バックエンドとモデルを選ぶ単位。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LlmTask {
    Digest,
    /// 採点（`profile suggest`・`eval --profile` も採点のモデルを使う）
    Score,
    Translate,
    Title,
    /// 同じ報道・関連の判定
    Story,
    Tidy,
}

impl LlmTask {
    pub const ALL: [LlmTask; 6] = [
        LlmTask::Digest,
        LlmTask::Score,
        LlmTask::Translate,
        LlmTask::Title,
        LlmTask::Story,
        LlmTask::Tidy,
    ];
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LlmConfig {
    /// LLM を呼ぶバックエンドの既定。工程ごとに `*_backend` で変えられる。各工程のモデル名は、
    /// その工程のバックエンドの名前にする
    pub backend: LlmBackend,
    /// 要約・採点・和訳・見出しの和訳・同じ報道の判定・語彙の整理のバックエンド（省けば `backend`）
    pub digest_backend: Option<LlmBackend>,
    pub score_backend: Option<LlmBackend>,
    pub translate_backend: Option<LlmBackend>,
    pub title_backend: Option<LlmBackend>,
    pub story_backend: Option<LlmBackend>,
    pub tidy_backend: Option<LlmBackend>,
    /// `claude` の実行ファイル（PATH から探す）。省けば `claude`
    pub command: Option<String>,
    /// `copilot` の実行ファイル（PATH から探す）。省けば `copilot`
    pub copilot_command: Option<String>,
    /// 1 回の呼び出しのタイムアウト
    pub timeout_secs: u64,
    /// 要約に使うモデル
    pub digest_model: String,
    /// 1 回の呼び出しで要約する記事数
    pub digest_batch_size: usize,
    /// プロンプトに入れる本文の部分ごとの最大文字数
    pub max_input_chars: usize,
    /// 採点に使うモデル
    pub score_model: String,
    /// 1 回の呼び出しで採点する記事数
    pub score_batch_size: usize,
    /// 要約が使い切らずに採点のために残す呼び出し回数（要約待ちが多くても推薦が止まらないように）
    pub score_reserved_calls: u32,
    /// 全文和訳に使うモデル
    pub translate_model: String,
    /// この点数以上の英語記事は、依頼が無くても先回りで和訳する
    pub translate_min_score: u8,
    /// 和訳に入れる本文の最大文字数（記事全体）
    pub translate_max_input_chars: usize,
    /// 語彙の整理（表記揺れの統合）に使うモデル
    pub tidy_model: String,
    /// 語彙の整理の間隔（日）。crawl のたびに、前回の整理からこの日数がたっていれば整理する
    pub tidy_interval_days: u32,
    /// 本文が無く要約できない英語記事の見出しの和訳に使うモデル
    pub title_model: String,
    /// 1 回の呼び出しで和訳する見出しの数
    pub title_batch_size: usize,
    /// 同じ報道・関連の判定に使うモデル
    pub story_model: String,
    /// 1 回の呼び出しで判定する記事数（候補の無い記事は LLM を呼ばずに済ませる）
    pub story_batch_size: usize,
    /// 同時に動かすバックエンド（claude・copilot）の数の上限（プロセスをまたいで数える）
    pub concurrency: usize,
}

impl LlmConfig {
    /// バックエンドの実行ファイル。
    pub fn command_for(&self, backend: LlmBackend) -> &str {
        match backend {
            LlmBackend::ClaudeCli => self.command.as_deref().unwrap_or("claude"),
            LlmBackend::CopilotCli => self.copilot_command.as_deref().unwrap_or("copilot"),
        }
    }

    /// 工程のバックエンド（工程ごとの指定が無ければ `backend`）。
    pub fn backend_for(&self, task: LlmTask) -> LlmBackend {
        match task {
            LlmTask::Digest => self.digest_backend,
            LlmTask::Score => self.score_backend,
            LlmTask::Translate => self.translate_backend,
            LlmTask::Title => self.title_backend,
            LlmTask::Story => self.story_backend,
            LlmTask::Tidy => self.tidy_backend,
        }
        .unwrap_or(self.backend)
    }

    /// どれかの工程が `backend` を使うか。
    pub fn uses(&self, backend: LlmBackend) -> bool {
        LlmTask::ALL.iter().any(|&t| self.backend_for(t) == backend)
    }

    /// 0 だと処理が黙って何もしなくなる値を拒否する。
    pub fn validate(&self) -> Result<(), String> {
        for (name, is_zero) in [
            ("digest_batch_size", self.digest_batch_size == 0),
            ("max_input_chars", self.max_input_chars == 0),
            ("score_batch_size", self.score_batch_size == 0),
            (
                "translate_max_input_chars",
                self.translate_max_input_chars == 0,
            ),
            ("timeout_secs", self.timeout_secs == 0),
            ("tidy_interval_days", self.tidy_interval_days == 0),
            ("title_batch_size", self.title_batch_size == 0),
            ("story_batch_size", self.story_batch_size == 0),
            ("concurrency", self.concurrency == 0),
        ] {
            if is_zero {
                return Err(format!("llm.{name} must be at least 1"));
            }
        }
        if !(1..=100).contains(&self.translate_min_score) {
            return Err(format!(
                "llm.translate_min_score must be 1..=100, got {}",
                self.translate_min_score
            ));
        }
        Ok(())
    }
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            backend: LlmBackend::ClaudeCli,
            digest_backend: None,
            score_backend: None,
            translate_backend: None,
            title_backend: None,
            story_backend: None,
            tidy_backend: None,
            command: None,
            copilot_command: None,
            timeout_secs: 300,
            digest_model: "sonnet".into(),
            digest_batch_size: 5,
            max_input_chars: 6000,
            score_model: "sonnet".into(),
            score_batch_size: 20,
            score_reserved_calls: 1,
            translate_model: "sonnet".into(),
            translate_min_score: 80,
            translate_max_input_chars: 20000,
            tidy_model: "sonnet".into(),
            tidy_interval_days: 7,
            title_model: "sonnet".into(),
            title_batch_size: 30,
            story_model: "sonnet".into(),
            story_batch_size: 10,
            concurrency: 2,
        }
    }
}

#[derive(Debug, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PipelineConfig {
    /// これより古い記事は、本文の抽出や LLM の処理の対象にしない（取り込み済みの過去記事を
    /// 一度に処理しないため）。公開日時が無い記事は取得日時で判断する。
    pub backlog_days: u32,
    /// 1 回の実行で本文を抽出する記事数の上限
    pub extract_max_per_run: usize,
}

impl Default for PipelineConfig {
    fn default() -> Self {
        Self {
            backlog_days: 14,
            extract_max_per_run: 100,
        }
    }
}

#[derive(Debug, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WebConfig {
    /// 待ち受けるアドレス。スマホから Tailscale 経由で見るなら tailnet の IP にする
    pub bind: std::net::SocketAddr,
    /// 一覧に既定で出す最低点（これ未満は「すべて表示」でだけ出す）
    pub min_score: u8,
    /// 一覧に出す記事の期間（日）
    pub list_days: u32,
    /// 一覧に出す最大件数
    pub list_limit: usize,
    /// 一覧を見てからこの分数以内の閲覧は、同じ訪問として「前回から」の区切りを保つ
    pub visit_gap_minutes: u32,
    /// 1 日に確認枠へ出す、閾値未満の記事の数（無作為に選ぶ）。0 なら出さない
    pub explore_per_day: u32,
}

impl WebConfig {
    /// 利用者の一覧の既定の最低点。今のプロファイルの点数があれば（`Db::has_scores`）`min_score`、無ければ
    /// 最低点なし（プロファイルが無いか、変えた直後や `embed rebuild` の後で、掛けると一覧が空になる）。
    /// Web・フィード・JSON・MCP で同じ規則を使う。
    pub fn default_min(&self, scored: bool) -> Option<u8> {
        scored.then_some(self.min_score)
    }

    /// 一覧が常に空になる値を拒否する。
    pub fn validate(&self) -> Result<(), String> {
        if self.min_score > 100 {
            return Err(format!(
                "web.min_score must be 0..=100, got {}",
                self.min_score
            ));
        }
        if self.list_days == 0 {
            return Err("web.list_days must be at least 1".into());
        }
        if self.list_limit == 0 {
            return Err("web.list_limit must be at least 1".into());
        }
        Ok(())
    }
}

impl Default for WebConfig {
    fn default() -> Self {
        Self {
            bind: std::net::SocketAddr::from(([127, 0, 0, 1], 8080)),
            min_score: 50,
            list_days: 7,
            list_limit: 200,
            visit_gap_minutes: 30,
            explore_per_day: 2,
        }
    }
}

#[derive(Debug, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HttpConfig {
    pub user_agent: String,
    pub per_host_delay_secs: u64,
    pub timeout_secs: u64,
    /// 応答本文の上限。これを超える応答は読み込まずにエラーにする。
    pub max_body_bytes: u64,
}

impl Default for HttpConfig {
    fn default() -> Self {
        Self {
            user_agent: concat!(
                "nucrawler/",
                env!("CARGO_PKG_VERSION"),
                " (+https://github.com/tagawa0525/nucrawler)"
            )
            .to_string(),
            per_host_delay_secs: 5,
            timeout_secs: 30,
            max_body_bytes: 20 * 1024 * 1024,
        }
    }
}

#[derive(Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Sources {
    #[serde(rename = "source", default)]
    pub sources: Vec<Source>,
}

/// ソースの ID から画面に出す名前へ（`Source::display_name`）。
pub type SourceLabels = std::collections::BTreeMap<String, String>;

impl Sources {
    /// 各ソースの表示名。
    pub fn labels(&self) -> SourceLabels {
        self.sources
            .iter()
            .map(|s| (s.id.clone(), s.display_name().to_string()))
            .collect()
    }
}

#[derive(Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    pub id: String,
    pub name: String,
    /// 画面に出す短い名前（例：九電）。無ければ `name`
    #[serde(default)]
    pub label: Option<String>,
    pub kind: SourceKind,
    pub url: String,
    pub lang: Lang,
    pub category: Category,
    #[serde(default = "enabled_by_default")]
    pub enabled: bool,
    #[serde(default)]
    pub filter: Filter,
    /// 記事ページの本文を示す CSS セレクタ。無ければ readability で推定する。
    #[serde(default)]
    pub body_selector: Option<String>,
    /// `html_list` の一覧ページの読み方
    #[serde(default)]
    pub list: Option<HtmlList>,
}

impl Source {
    /// 画面に出す名前。
    pub fn display_name(&self) -> &str {
        self.label.as_deref().unwrap_or(&self.name)
    }
}

fn enabled_by_default() -> bool {
    true
}

#[derive(Debug, PartialEq, Eq, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    /// RSS / Atom / RDF
    Feed,
    /// 電事連のニュース一覧 JSON
    FepcJson,
    /// RSS の無いサイトのニュース一覧ページ（読み方は `[source.list]`）
    HtmlList,
}

/// `html_list` の一覧ページの読み方。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HtmlList {
    /// 記事へのリンク（a 要素）の CSS セレクタ
    pub link: String,
    /// リンク先のファイル名に含まれる日付の形式。無ければ取得日時で扱う
    #[serde(default)]
    pub date_in_url: Option<UrlDate>,
    /// 見出しから除く要素の CSS セレクタ（会社名のラベルなど）
    #[serde(default)]
    pub title_skip: Option<String>,
    /// 一覧を読む前に、このセレクタに一致する最初のリンクをたどる
    /// （年度ごとに URL が変わる一覧を、入口のページから探すときに使う）
    #[serde(default)]
    pub follow: Option<String>,
    /// 日付の要素の CSS セレクタ。リンクを含む項目（ほかのリンクを含まない最も大きいまとまり）の
    /// 中で最初に一致する要素の文字列から、年・月・日の順の数字を読む（例 2026年9月18日、2026/09/07）。
    /// 読めなければ `date_in_url`、それも無ければ取得日時で扱う
    #[serde(default)]
    pub date: Option<String>,
    /// 一覧に続けて同じ読み方で読むほかのページの URL（`follow` はたどらない）。一覧にも載る記事は
    /// 一覧の側の 1 件だけにする。月ごとに切り替わる一覧の取りこぼしを、月をまたいで最新の数件を
    /// 載せるページ（トップの新着など）で補うときに使う
    #[serde(default)]
    pub also: Vec<String>,
}

/// URL のファイル名に含まれる日付の形式（最初に現れる、その桁数の数字の並び）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UrlDate {
    /// 例 20260925_1j.pdf
    Yyyymmdd,
    /// 例 260925j0101.pdf（2000 年代とみなす）
    Yymmdd,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Lang {
    En,
    Ja,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    Regulator,
    Industry,
    Research,
    Utility,
    Vendor,
    Paper,
}

/// 原子力関連に絞り込む条件。`keywords` か `url_contains` のいずれかに一致したものを取り込む
/// （両方空なら全件）。ただし、タイトルが `title_excludes` のいずれかを含むものは除く。
#[derive(Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Filter {
    pub keywords: Vec<String>,
    pub url_contains: Vec<String>,
    pub title_excludes: Vec<String>,
}

pub fn parse_config(text: &str, path: &Path) -> Result<Config, ConfigError> {
    let config: Config = parse_toml(text, path)?;
    config
        .quota
        .validate()
        .and_then(|()| config.llm.validate())
        .and_then(|()| config.web.validate())
        .and_then(|()| config.recommend.validate())
        .and_then(|()| {
            config
                .embedding
                .as_ref()
                .map_or(Ok(()), EmbeddingConfig::validate)
        })
        .and_then(|()| {
            config
                .copilot_quota
                .as_ref()
                .map_or(Ok(()), crate::quota::CreditsConfig::validate)
        })
        .and_then(|()| {
            // 使用率を返さない Copilot は、月の予算が無いと止める基準が無い
            if config.llm.uses(LlmBackend::CopilotCli) && config.copilot_quota.is_none() {
                Err(
                    "a stage runs on copilot-cli (llm.backend or llm.*_backend) and needs \
                     [copilot_quota] (monthly_credits of the Copilot plan)"
                        .into(),
                )
            } else {
                Ok(())
            }
        })
        .map_err(|reason| ConfigError::Invalid {
            path: path.to_path_buf(),
            reason,
        })?;
    Ok(config)
}

pub fn parse_sources(text: &str, path: &Path) -> Result<Sources, ConfigError> {
    let sources: Sources = parse_toml(text, path)?;
    let mut seen = HashSet::new();
    for s in &sources.sources {
        if !seen.insert(s.id.as_str()) {
            return Err(ConfigError::DuplicateSourceId {
                path: path.to_path_buf(),
                id: s.id.clone(),
            });
        }
        validate_list(s).map_err(|reason| ConfigError::Invalid {
            path: path.to_path_buf(),
            reason: format!("source {}: {reason}", s.id),
        })?;
    }
    Ok(sources)
}

/// `html_list` には読み方があり、セレクタが解釈できること。ほかの種類には書かないこと。
fn validate_list(s: &Source) -> Result<(), String> {
    let check = |name: &str, selector: &str| {
        scraper::Selector::parse(selector)
            .map(drop)
            .map_err(|e| format!("invalid list.{name} {selector:?}: {e}"))
    };
    match (s.kind, &s.list) {
        (SourceKind::HtmlList, None) => Err("html_list needs [source.list]".into()),
        (SourceKind::HtmlList, Some(list)) => {
            check("link", &list.link)?;
            if let Some(skip) = &list.title_skip {
                check("title_skip", skip)?;
            }
            if let Some(follow) = &list.follow {
                check("follow", follow)?;
            }
            if let Some(date) = &list.date {
                check("date", date)?;
            }
            for url in &list.also {
                match url::Url::parse(url) {
                    Ok(u) if matches!(u.scheme(), "http" | "https") => {}
                    Ok(_) => return Err(format!("invalid list.also {url:?}: not http(s)")),
                    Err(e) => return Err(format!("invalid list.also {url:?}: {e}")),
                }
            }
            Ok(())
        }
        (_, Some(_)) => Err("[source.list] is only for html_list".into()),
        (_, None) => Ok(()),
    }
}

fn parse_toml<T: serde::de::DeserializeOwned>(text: &str, path: &Path) -> Result<T, ConfigError> {
    toml::from_str(text).map_err(|source| ConfigError::Parse {
        path: path.to_path_buf(),
        source,
    })
}

/// `dir` の config.toml（無ければ既定値）と sources.toml（必須）を読む。
pub fn load(dir: &Path) -> Result<(Config, Sources), ConfigError> {
    let config_path = dir.join("config.toml");
    let config = match std::fs::read_to_string(&config_path) {
        Ok(text) => parse_config(&text, &config_path)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Config::default(),
        Err(source) => {
            return Err(ConfigError::Read {
                path: config_path,
                source,
            });
        }
    };
    let sources_path = dir.join("sources.toml");
    let text = std::fs::read_to_string(&sources_path).map_err(|source| ConfigError::Read {
        path: sources_path.clone(),
        source,
    })?;
    let sources = parse_sources(&text, &sources_path)?;
    Ok((config, sources))
}

/// `$XDG_CONFIG_HOME/nucrawler`、無ければ `$HOME/.config/nucrawler`。
pub fn default_dir(env: impl Fn(&str) -> Option<OsString>) -> Result<PathBuf, ConfigError> {
    let non_empty = |k| env(k).filter(|v: &OsString| !v.is_empty());
    let base = match (non_empty("XDG_CONFIG_HOME"), non_empty("HOME")) {
        (Some(xdg), _) => PathBuf::from(xdg),
        (None, Some(home)) => PathBuf::from(home).join(".config"),
        (None, None) => return Err(ConfigError::NoConfigDir),
    };
    Ok(base.join("nucrawler"))
}

/// `$XDG_DATA_HOME/nucrawler`、無ければ `$HOME/.local/share/nucrawler`（DB やロックファイルを置く）。
pub fn default_data_dir(env: impl Fn(&str) -> Option<OsString>) -> Result<PathBuf, ConfigError> {
    let non_empty = |k| env(k).filter(|v: &OsString| !v.is_empty());
    let base = match (non_empty("XDG_DATA_HOME"), non_empty("HOME")) {
        (Some(xdg), _) => PathBuf::from(xdg),
        (None, Some(home)) => PathBuf::from(home).join(".local/share"),
        (None, None) => return Err(ConfigError::NoDataDir),
    };
    Ok(base.join("nucrawler"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::temp_dir;

    fn p() -> &'static Path {
        Path::new("test.toml")
    }

    #[test]
    fn empty_config_uses_defaults() {
        let c = parse_config("", p()).unwrap();
        assert_eq!(c, Config::default());
        assert!(c.http.user_agent.starts_with("nucrawler/"));
        assert!(c.http.per_host_delay_secs > 0);
        assert!(c.http.timeout_secs > 0);
        assert!(c.http.max_body_bytes > 0);
    }

    #[test]
    fn pipeline_defaults_and_overrides() {
        let d = PipelineConfig::default();
        assert_eq!((d.backlog_days, d.extract_max_per_run), (14, 100));
        let c = parse_config("[pipeline]\nbacklog_days = 3\n", p()).unwrap();
        assert_eq!(c.pipeline.backlog_days, 3);
        assert_eq!(c.pipeline.extract_max_per_run, 100);
    }

    #[test]
    fn rejects_invalid_quota_values() {
        for (toml, needle) in [
            ("[quota]\nweekly_max = 2.0\n", "weekly_max"),
            (
                "[quota]\ndefault_max_five_hour = -0.1\n",
                "default_max_five_hour",
            ),
            (
                "[quota]\ndefault_max_five_hour = nan\n",
                "default_max_five_hour",
            ),
            ("[quota]\npace_ahead_days = -1.0\n", "pace_ahead_days"),
            (
                "[quota]\ntimezone_offset_hours = 24\n",
                "timezone_offset_hours",
            ),
            (
                "[quota]\nslots = [{ start = 10, end = 10, max_five_hour = 0.5 }]\n",
                "slot",
            ),
            (
                "[quota]\nslots = [{ start = 20, end = 25, max_five_hour = 0.5 }]\n",
                "slot",
            ),
            (
                "[quota]\nslots = [{ start = 1, end = 2, max_five_hour = 1.5 }]\n",
                "slot",
            ),
        ] {
            let err = parse_config(toml, p()).unwrap_err();
            assert!(
                matches!(&err, ConfigError::Invalid { reason, .. } if reason.contains(needle)),
                "{toml}: {err}"
            );
        }
    }

    #[test]
    fn selects_the_copilot_backend() {
        let c = parse_config(
            "[llm]\nbackend = \"copilot-cli\"\n[copilot_quota]\nmonthly_credits = 1500\n",
            p(),
        )
        .unwrap();
        assert_eq!(c.llm.backend, LlmBackend::CopilotCli);
        assert_eq!(c.llm.command_for(LlmBackend::CopilotCli), "copilot");
        // command は claude の、copilot_command は copilot の実行ファイル（両方を同時に使うため）
        let c = parse_config(
            "[llm]\nbackend = \"copilot-cli\"\ncommand = \"/opt/claude\"\n\
             [copilot_quota]\nmonthly_credits = 1500\n",
            p(),
        )
        .unwrap();
        assert_eq!(c.llm.command_for(LlmBackend::CopilotCli), "copilot");
        assert_eq!(c.llm.command_for(LlmBackend::ClaudeCli), "/opt/claude");
        let c = parse_config("[llm]\ncopilot_command = \"/opt/copilot\"\n", p()).unwrap();
        assert_eq!(c.llm.command_for(LlmBackend::CopilotCli), "/opt/copilot");
        assert_eq!(c.llm.command_for(LlmBackend::ClaudeCli), "claude");
        // 月の予算が無いまま Copilot で動かさない
        let err = parse_config("[llm]\nbackend = \"copilot-cli\"\n", p()).unwrap_err();
        assert!(
            matches!(&err, ConfigError::Invalid { reason, .. } if reason.contains("copilot_quota")),
            "{err}"
        );
        assert!(parse_config("[llm]\nbackend = \"gemini-cli\"\n", p()).is_err());
    }

    #[test]
    fn selects_backends_per_task() {
        let c = parse_config(
            "[llm]\nbackend = \"copilot-cli\"\nscore_backend = \"claude-cli\"\n\
             [copilot_quota]\nmonthly_credits = 1500\n",
            p(),
        )
        .unwrap();
        assert_eq!(c.llm.backend_for(LlmTask::Score), LlmBackend::ClaudeCli);
        for task in [
            LlmTask::Digest,
            LlmTask::Translate,
            LlmTask::Title,
            LlmTask::Story,
            LlmTask::Tidy,
        ] {
            assert_eq!(c.llm.backend_for(task), LlmBackend::CopilotCli, "{task:?}");
        }
        // どれかの工程が Copilot を使うなら、月の予算が要る
        let err = parse_config("[llm]\ntranslate_backend = \"copilot-cli\"\n", p()).unwrap_err();
        assert!(
            matches!(&err, ConfigError::Invalid { reason, .. } if reason.contains("copilot_quota")),
            "{err}"
        );
        // どの工程も Claude なら要らない
        let c = parse_config(
            "[llm]\nbackend = \"copilot-cli\"\ndigest_backend = \"claude-cli\"\n\
             score_backend = \"claude-cli\"\ntranslate_backend = \"claude-cli\"\n\
             title_backend = \"claude-cli\"\nstory_backend = \"claude-cli\"\n\
             tidy_backend = \"claude-cli\"\n",
            p(),
        );
        assert!(c.is_ok(), "{c:?}");
    }

    #[test]
    fn reads_the_copilot_quota() {
        assert_eq!(parse_config("", p()).unwrap().copilot_quota, None);
        let c = parse_config("[copilot_quota]\nmonthly_credits = 1500\n", p()).unwrap();
        assert_eq!(
            c.copilot_quota,
            Some(crate::quota::CreditsConfig {
                monthly_credits: 1500.0,
                pace: 0.8,
            })
        );
        // プランで大きく違うので、月のクレジットは省けない
        assert!(parse_config("[copilot_quota]\npace = 0.5\n", p()).is_err());
        for (toml, needle) in [
            ("[copilot_quota]\nmonthly_credits = 0\n", "monthly_credits"),
            (
                "[copilot_quota]\nmonthly_credits = nan\n",
                "monthly_credits",
            ),
            (
                "[copilot_quota]\nmonthly_credits = 1500\npace = 0\n",
                "pace",
            ),
            (
                "[copilot_quota]\nmonthly_credits = 1500\npace = 1.5\n",
                "pace",
            ),
        ] {
            let err = parse_config(toml, p()).unwrap_err();
            assert!(
                matches!(&err, ConfigError::Invalid { reason, .. } if reason.contains(needle)),
                "{toml}: {err}"
            );
        }
    }

    #[test]
    fn rejects_non_positive_llm_settings() {
        for (toml, needle) in [
            ("[llm]\ndigest_batch_size = 0\n", "digest_batch_size"),
            ("[llm]\nmax_input_chars = 0\n", "max_input_chars"),
            ("[llm]\ntimeout_secs = 0\n", "timeout_secs"),
            ("[llm]\nscore_batch_size = 0\n", "score_batch_size"),
            (
                "[llm]\ntranslate_max_input_chars = 0\n",
                "translate_max_input_chars",
            ),
            ("[llm]\ntranslate_min_score = 101\n", "translate_min_score"),
            ("[llm]\ntranslate_min_score = 0\n", "translate_min_score"),
            ("[llm]\ntidy_interval_days = 0\n", "tidy_interval_days"),
            ("[llm]\ntitle_batch_size = 0\n", "title_batch_size"),
            ("[llm]\nstory_batch_size = 0\n", "story_batch_size"),
            ("[llm]\nconcurrency = 0\n", "concurrency"),
        ] {
            let err = parse_config(toml, p()).unwrap_err();
            assert!(
                matches!(&err, ConfigError::Invalid { reason, .. } if reason.contains(needle)),
                "{toml}: {err}"
            );
        }
    }

    #[test]
    fn llm_defaults() {
        let d = LlmConfig::default();
        assert_eq!(d.backend, LlmBackend::ClaudeCli);
        assert_eq!(d.command_for(LlmBackend::ClaudeCli), "claude");
        assert_eq!(d.command_for(LlmBackend::CopilotCli), "copilot");
        assert_eq!(d.backend_for(LlmTask::Score), LlmBackend::ClaudeCli);
        assert_eq!(d.timeout_secs, 300);
        assert_eq!(d.digest_model, "sonnet");
        assert_eq!(d.digest_batch_size, 5);
        assert_eq!(d.max_input_chars, 6000);
        assert_eq!(d.score_model, "sonnet");
        assert_eq!(d.score_batch_size, 20);
        assert_eq!(d.score_reserved_calls, 1);
        assert_eq!(d.translate_model, "sonnet");
        assert_eq!(d.translate_min_score, 80);
        assert_eq!(d.translate_max_input_chars, 20000);
        assert_eq!(d.tidy_model, "sonnet");
        assert_eq!(d.tidy_interval_days, 7);
        assert_eq!((d.title_model.as_str(), d.title_batch_size), ("sonnet", 30));
        assert_eq!((d.story_model.as_str(), d.story_batch_size), ("sonnet", 10));
        assert_eq!(d.concurrency, 2);
    }

    #[test]
    fn parses_body_selector() {
        let s = parse_sources(
            r#"
            [[source]]
            id = "a"
            name = "A"
            kind = "feed"
            url = "https://example.com/rss"
            lang = "ja"
            category = "utility"
            body_selector = "div#contents"
            "#,
            p(),
        )
        .unwrap();
        assert_eq!(s.sources[0].body_selector.as_deref(), Some("div#contents"));
    }

    #[test]
    fn partial_config_keeps_other_defaults() {
        let c = parse_config("[http]\nuser_agent = \"x\"\n", p()).unwrap();
        assert_eq!(c.http.user_agent, "x");
        assert_eq!(
            c.http.per_host_delay_secs,
            HttpConfig::default().per_host_delay_secs
        );
    }

    #[test]
    fn unknown_config_field_is_error() {
        let err = parse_config("[http]\nuser_agnet = \"x\"\n", p()).unwrap_err();
        assert!(matches!(err, ConfigError::Parse { .. }), "{err}");
    }

    #[test]
    fn parses_source_with_defaults_and_filter() {
        let s = parse_sources(
            r#"
            [[source]]
            id = "a"
            name = "A"
            kind = "feed"
            url = "https://example.com/rss"
            lang = "ja"
            category = "utility"
            filter = { keywords = ["原子力"] }

            [[source]]
            id = "b"
            name = "B"
            kind = "fepc_json"
            url = "https://example.com/index.json"
            lang = "en"
            category = "industry"
            enabled = false
            "#,
            p(),
        )
        .unwrap();
        assert_eq!(
            s.sources[0],
            Source {
                id: "a".into(),
                name: "A".into(),
                label: None,
                kind: SourceKind::Feed,
                url: "https://example.com/rss".into(),
                lang: Lang::Ja,
                category: Category::Utility,
                enabled: true,
                filter: Filter {
                    keywords: vec!["原子力".into()],
                    url_contains: vec![],
                    title_excludes: vec![],
                },
                body_selector: None,
                list: None,
            }
        );
        assert_eq!(s.sources[1].kind, SourceKind::FepcJson);
        assert!(!s.sources[1].enabled);
        assert_eq!(s.sources[1].filter, Filter::default());
    }

    #[test]
    fn duplicate_source_id_is_error() {
        let one = r#"
            [[source]]
            id = "a"
            name = "A"
            kind = "feed"
            url = "https://example.com/rss"
            lang = "en"
            category = "industry"
        "#;
        let err = parse_sources(&format!("{one}\n{one}"), p()).unwrap_err();
        assert!(
            matches!(&err, ConfigError::DuplicateSourceId { id, .. } if id == "a"),
            "{err}"
        );
    }

    #[test]
    fn unknown_kind_is_error() {
        let err = parse_sources(
            r#"
            [[source]]
            id = "a"
            name = "A"
            kind = "gopher"
            url = "gopher://example.com"
            lang = "en"
            category = "industry"
            "#,
            p(),
        )
        .unwrap_err();
        assert!(matches!(err, ConfigError::Parse { .. }), "{err}");
    }

    #[test]
    fn web_defaults_to_localhost() {
        let c = parse_config("", p()).unwrap();
        assert_eq!(c.web.bind, "127.0.0.1:8080".parse().unwrap());
        let c = parse_config("[web]\nbind = \"100.64.0.1:8080\"\nmin_score = 70\n", p()).unwrap();
        assert_eq!(c.web.bind, "100.64.0.1:8080".parse().unwrap());
        assert_eq!(c.web.min_score, 70);
    }

    #[test]
    fn rejects_web_values_that_hide_everything() {
        for (text, reason) in [
            ("[web]\nmin_score = 101\n", "web.min_score"),
            ("[web]\nlist_days = 0\n", "web.list_days"),
            ("[web]\nlist_limit = 0\n", "web.list_limit"),
        ] {
            let err = parse_config(text, p()).unwrap_err();
            assert!(
                matches!(&err, ConfigError::Invalid { reason: r, .. } if r.contains(reason)),
                "{text}: {err}"
            );
        }
    }

    /// 推薦点の補正の強さ。既定は 1.0 で、正の有限値だけを受け付ける（∞ や 0 では学習できない）。
    #[test]
    fn reads_the_recommend_prior_strength() {
        assert_eq!(
            parse_config("", p()).unwrap().recommend.prior_strength,
            20.0
        );
        let c = parse_config("[recommend]\nprior_strength = 2.5\n", p()).unwrap();
        assert_eq!(c.recommend.prior_strength, 2.5);
        for text in [
            "[recommend]\nprior_strength = 0.0\n",
            "[recommend]\nprior_strength = -1.0\n",
            "[recommend]\nprior_strength = inf\n",
            "[recommend]\nprior_strength = nan\n",
        ] {
            let err = parse_config(text, p()).unwrap_err();
            assert!(
                matches!(&err, ConfigError::Invalid { reason, .. } if reason.contains("recommend.prior_strength")),
                "{text}: {err}"
            );
        }
    }

    /// embedding は設定が無ければ使わない。あれば既定（認証なし・32 件・30 秒）を補って読む。
    #[test]
    fn reads_the_embedding_settings() {
        assert_eq!(parse_config("", p()).unwrap().embedding, None);
        let c = parse_config(
            "[embedding]\nurl = \"http://127.0.0.1:8080/v1/embeddings\"\nmodel = \"ruri\"\n\
             query_prefix = \"検索クエリ: \"\ndocument_prefix = \"検索文書: \"\n",
            p(),
        )
        .unwrap();
        let e = c.embedding.unwrap();
        assert_eq!(e.auth, EmbeddingAuth::None);
        assert_eq!((e.batch_size, e.timeout_secs), (32, 30));
        assert_eq!(e.dimensions, None);
        assert_eq!(e.query_prefix, "検索クエリ: ");
        let azure = parse_config(
            "[embedding]\nurl = \"https://x.openai.azure.com/openai/v1/embeddings\"\n\
             model = \"emb\"\nauth = \"api-key\"\napi_key_env = \"AZURE_KEY\"\ndimensions = 1024\n",
            p(),
        )
        .unwrap()
        .embedding
        .unwrap();
        assert_eq!(azure.auth, EmbeddingAuth::ApiKey);
        assert_eq!(azure.dimensions, Some(1024));
    }

    /// 呼び出しが成り立たない値は、設定の誤りにする。1 回に送る件数は、指紋の試験文（2 件）より多くないと
    /// 記事を 1 件も載せられない。
    #[test]
    fn rejects_unusable_embedding_settings() {
        let base = "[embedding]\nurl = \"http://127.0.0.1:8080/v1/embeddings\"\nmodel = \"m\"\n";
        assert!(parse_config(&format!("{base}batch_size = 3\n"), p()).is_ok());
        for (extra, word) in [
            ("batch_size = 2\n", "batch_size"),
            ("timeout_secs = 0\n", "timeout_secs"),
            ("dimensions = 0\n", "dimensions"),
            ("auth = \"bearer\"\n", "api_key_env"),
            ("auth = \"bearer\"\napi_key_env = \"\"\n", "api_key_env"),
            // 鍵を平文で送らない
            ("auth = \"api-key\"\napi_key_env = \"K\"\n", "https"),
            // 予約の期限（呼び出しの回数 × タイムアウト）を表せない値
            ("batch_size = 2049\n", "batch_size"),
            ("timeout_secs = 3601\n", "timeout_secs"),
        ] {
            let text = format!("{base}{extra}");
            let err = parse_config(&text, p()).unwrap_err();
            assert!(
                matches!(&err, ConfigError::Invalid { reason, .. } if reason.contains(word)),
                "{text}: {err}"
            );
        }
        for (url, model) in [
            ("", "m"),
            ("not a url", "m"),
            ("ftp://127.0.0.1/v1", "m"),
            ("http://127.0.0.1/v1", ""),
        ] {
            let text = format!("[embedding]\nurl = \"{url}\"\nmodel = \"{model}\"\n");
            assert!(
                matches!(parse_config(&text, p()), Err(ConfigError::Invalid { .. })),
                "{text}"
            );
        }
    }

    #[test]
    fn source_label_falls_back_to_name() {
        let s = parse_sources(
            r#"
            [[source]]
            id = "kyuden"
            name = "九州電力"
            label = "九電"
            kind = "feed"
            url = "https://example.com/rss"
            lang = "ja"
            category = "utility"

            [[source]]
            id = "wnn"
            name = "World Nuclear News"
            kind = "feed"
            url = "https://example.com/wnn"
            lang = "en"
            category = "industry"
            "#,
            p(),
        )
        .unwrap();
        assert_eq!(s.sources[0].display_name(), "九電");
        assert_eq!(s.sources[1].display_name(), "World Nuclear News");
    }

    fn source_toml(kind: &str, extra: &str) -> String {
        format!(
            "[[source]]\nid = \"a\"\nname = \"A\"\nkind = \"{kind}\"\n\
             url = \"https://e.example/\"\nlang = \"ja\"\ncategory = \"utility\"\n{extra}"
        )
    }

    #[test]
    fn html_list_needs_valid_list_settings() {
        let ok = parse_sources(
            &source_toml(
                "html_list",
                "list = { link = \"dd > a\", date_in_url = \"yymmdd\", title_skip = \".x\", follow = \"h3 a\", date = \"dt\", also = [\"https://e.example/top\"] }\n",
            ),
            p(),
        )
        .unwrap();
        let list = ok.sources[0].list.as_ref().unwrap();
        assert_eq!(list.date_in_url, Some(UrlDate::Yymmdd));
        assert_eq!(list.date.as_deref(), Some("dt"));
        assert_eq!(list.also, ["https://e.example/top"]);
        for (text, reason) in [
            (source_toml("html_list", ""), "[source.list]"),
            (
                source_toml("feed", "list = { link = \"a\" }\n"),
                "only for html_list",
            ),
            (
                source_toml("html_list", "list = { link = \"dd >\" }\n"),
                "list.link",
            ),
            (
                source_toml("html_list", "list = { link = \"a\", follow = \"[\" }\n"),
                "list.follow",
            ),
            (
                source_toml("html_list", "list = { link = \"a\", date = \"[\" }\n"),
                "list.date",
            ),
            (
                source_toml("html_list", "list = { link = \"a\", also = [\"/top\"] }\n"),
                "list.also",
            ),
            (
                source_toml(
                    "html_list",
                    "list = { link = \"a\", also = [\"ftp://e.example/\"] }\n",
                ),
                "list.also",
            ),
        ] {
            let err = parse_sources(&text, p()).unwrap_err();
            assert!(
                matches!(&err, ConfigError::Invalid { reason: r, .. } if r.contains(reason)),
                "{text}: {err}"
            );
        }
    }

    #[test]
    fn examples_are_valid() {
        parse_config(include_str!("../examples/config.toml"), p()).unwrap();
        let s = parse_sources(include_str!("../examples/sources.toml"), p()).unwrap();
        assert!(!s.sources.is_empty());
    }

    #[test]
    fn load_without_config_toml_uses_defaults() {
        let dir = temp_dir("load-defaults");
        std::fs::write(dir.join("sources.toml"), "").unwrap();
        let (c, s) = load(&dir).unwrap();
        assert_eq!(c, Config::default());
        assert!(s.sources.is_empty());
    }

    #[test]
    fn load_without_sources_toml_is_error() {
        let dir = temp_dir("load-no-sources");
        let err = load(&dir).unwrap_err();
        assert!(
            matches!(&err, ConfigError::Read { path, .. } if path.ends_with("sources.toml")),
            "{err}"
        );
    }

    #[test]
    fn default_dir_prefers_xdg() {
        let dir = default_dir(|k| match k {
            "XDG_CONFIG_HOME" => Some("/xdg".into()),
            "HOME" => Some("/home/u".into()),
            _ => None,
        })
        .unwrap();
        assert_eq!(dir, PathBuf::from("/xdg/nucrawler"));
    }

    #[test]
    fn default_dir_falls_back_to_home() {
        let dir = default_dir(|k| (k == "HOME").then(|| "/home/u".into())).unwrap();
        assert_eq!(dir, PathBuf::from("/home/u/.config/nucrawler"));
    }

    #[test]
    fn default_data_dir_prefers_xdg_then_home() {
        let xdg = default_data_dir(|k| match k {
            "XDG_DATA_HOME" => Some("/xdg".into()),
            "HOME" => Some("/home/u".into()),
            _ => None,
        })
        .unwrap();
        assert_eq!(xdg, PathBuf::from("/xdg/nucrawler"));
        let home = default_data_dir(|k| (k == "HOME").then(|| "/home/u".into())).unwrap();
        assert_eq!(home, PathBuf::from("/home/u/.local/share/nucrawler"));
        let err = default_data_dir(|_| None).unwrap_err();
        assert!(matches!(err, ConfigError::NoDataDir), "{err}");
    }

    #[test]
    fn default_dir_without_env_is_error() {
        let err = default_dir(|_| None).unwrap_err();
        assert!(matches!(err, ConfigError::NoConfigDir), "{err}");
    }
}
