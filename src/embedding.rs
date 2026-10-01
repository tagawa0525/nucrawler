//! 文章の embedding（意味のベクトル）を、OpenAI 互換の embeddings API（`POST .../v1/embeddings`）で作る。
//! r995 ではローカルのサーバー（text-embeddings-inference）、社内では Azure OpenAI を同じクライアントで呼ぶ。
//!
//! ベクトルの空間はモデルごとに違い、違う空間のベクトルは比べられない。設定の名前が同じままサーバーの中身の
//! モデルが替わることもあるので、決まった試験文のベクトル（指紋）を保存しておき、呼び出しのたびに試験文も一緒に
//! 送って、返った指紋が保存したものと一致したときだけ結果を使う（`pipeline::embed`）。

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::config::{EmbeddingAuth, EmbeddingConfig};

/// 1 回の呼び出しに一緒に入れる、指紋の試験文の数。
pub const FINGERPRINT_TEXTS: usize = 2;

/// 指紋の試験文。日本語と英語を 1 つずつ（モデルの違いが出やすいように、話題も変える）。
const FINGERPRINT: [&str; FINGERPRINT_TEXTS] = [
    "原子炉の安全規制に関する最新の動向",
    "The quick brown fox jumps over the lazy dog.",
];

/// 入力の組み立て方（記事の見出し・要約・要点のつなぎ方など）の版。変えたら上げる。版が違えば、
/// 保存したベクトルと比べられないので作り直す（`nucrawler embed rebuild`）。
pub const INPUT_VERSION: i64 = 1;

/// 指紋が一致したとみなすコサイン類似度の下限。同じモデルでも、まとめて送る件数などで値がわずかに揺れる。
const SAME_SPACE_MIN_COSINE: f32 = 0.999;

#[derive(Debug, thiserror::Error)]
pub enum EmbedError {
    #[error("environment variable {env} (embedding.api_key_env) is not set")]
    MissingKey { env: String },
    #[error("failed to build the embedding http client")]
    Build(#[source] reqwest::Error),
    #[error("embedding request failed")]
    Request(#[source] reqwest::Error),
    #[error("embedding api returned {status}: {body}")]
    Status { status: u16, body: String },
    #[error("invalid embedding response: {0}")]
    Invalid(String),
}

impl EmbedError {
    /// 送った文のせいの失敗か（長すぎるなど）。そうでなければ、サービスの側の失敗（止まっている・認証・
    /// 上限）なので、記事の失敗としては数えず、そのステージを止める。
    pub fn is_input_error(&self) -> bool {
        matches!(
            self,
            EmbedError::Status {
                status: 400 | 413 | 422,
                ..
            }
        )
    }
}

/// 文の列を embedding にする。返すベクトルは L2 正規化してあり、入力と同じ順・同じ数。
pub trait Embedder {
    fn embed(
        &self,
        inputs: &[String],
    ) -> impl Future<Output = Result<Vec<Vec<f32>>, EmbedError>> + Send;
}

/// 文の使われ方。モデルによっては、クエリと文書で接頭辞を変える。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// 好み（関心分野・推薦しない話題）
    Query,
    /// 記事
    Document,
}

/// 設定から作る空間の名前。設定が変われば名前も変わる（中身の違いは指紋で見分ける）。
pub fn space_name(cfg: &EmbeddingConfig) -> String {
    // 区切り文字を含む値でも取り違えないよう、JSON の配列にする
    serde_json::json!([
        cfg.url,
        cfg.model,
        cfg.dimensions,
        cfg.query_prefix,
        cfg.document_prefix
    ])
    .to_string()
}

/// 接頭辞を付けた入力の文。
pub fn input(cfg: &EmbeddingConfig, role: Role, text: &str) -> String {
    let prefix = match role {
        Role::Query => &cfg.query_prefix,
        Role::Document => &cfg.document_prefix,
    };
    format!("{prefix}{text}")
}

/// 指紋の試験文（接頭辞を付けたもの）。
pub fn fingerprint_inputs(cfg: &EmbeddingConfig, role: Role) -> Vec<String> {
    FINGERPRINT.iter().map(|t| input(cfg, role, t)).collect()
}

/// モデルの指紋：試験文をクエリと文書の両方で embedding にしたベクトル。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Fingerprint {
    pub query: Vec<Vec<f32>>,
    pub document: Vec<Vec<f32>>,
}

impl Fingerprint {
    /// 試験文を、クエリと文書でそれぞれ 1 回ずつ呼んで作る。
    pub async fn make(embedder: &impl Embedder, cfg: &EmbeddingConfig) -> Result<Self, EmbedError> {
        Ok(Self {
            query: embedder
                .embed(&fingerprint_inputs(cfg, Role::Query))
                .await?,
            document: embedder
                .embed(&fingerprint_inputs(cfg, Role::Document))
                .await?,
        })
    }

    /// `role` の経路で返った試験文のベクトルが、この指紋と同じ空間のものか。
    pub fn matches(&self, role: Role, vectors: &[Vec<f32>]) -> bool {
        let saved = match role {
            Role::Query => &self.query,
            Role::Document => &self.document,
        };
        saved.len() == vectors.len()
            && saved.iter().zip(vectors).all(|(a, b)| {
                // どちらも正規化してあるので、内積がコサイン類似度
                a.len() == b.len() && dot(a, b) >= SAME_SPACE_MIN_COSINE
            })
    }
}

/// ベクトルを保存する形（f32 のリトルエンディアン）。
pub fn encode(vector: &[f32]) -> Vec<u8> {
    vector.iter().flat_map(|x| x.to_le_bytes()).collect()
}

/// `encode` の逆。長さが 4 の倍数でなければ `None`。
pub fn decode(bytes: &[u8]) -> Option<Vec<f32>> {
    let (chunks, rest) = bytes.as_chunks::<4>();
    rest.is_empty()
        .then(|| chunks.iter().map(|c| f32::from_le_bytes(*c)).collect())
}

/// 内積。正規化したベクトルどうしならコサイン類似度。
pub fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// L2 正規化する。値に NaN・無限大があるか、0 ベクトルなら `None`。
fn normalize(mut v: Vec<f32>) -> Option<Vec<f32>> {
    if !v.iter().all(|x| x.is_finite()) {
        return None;
    }
    let norm = v.iter().map(|x| f64::from(*x).powi(2)).sum::<f64>().sqrt();
    if norm == 0.0 || !norm.is_finite() {
        return None;
    }
    v.iter_mut()
        .for_each(|x| *x = (f64::from(*x) / norm) as f32);
    Some(v)
}

/// 応答の大きさの上限：1 件あたり（3,072 次元を JSON の数で書いても収まる量）と、それ以外の部分。
const RESPONSE_BYTES_PER_INPUT: usize = 128 * 1024;
const RESPONSE_BYTES_BASE: usize = 64 * 1024;
/// 失敗の応答の本文は、表示に使う分だけを読む。
const ERROR_BODY_BYTES: usize = 4 * 1024;

/// 本文を `limit` バイトまで読む。超える分があれば、そこで読むのをやめて `Err` に読んだ分を入れて返す。
async fn read_limited(
    resp: &mut reqwest::Response,
    limit: usize,
) -> Result<Result<Vec<u8>, Vec<u8>>, EmbedError> {
    let mut body = Vec::new();
    while let Some(chunk) = resp.chunk().await.map_err(EmbedError::Request)? {
        if body.len() + chunk.len() > limit {
            body.extend_from_slice(&chunk[..limit - body.len()]);
            return Ok(Err(body));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(Ok(body))
}

#[derive(Deserialize)]
struct Response {
    data: Vec<Datum>,
}

#[derive(Deserialize)]
struct Datum {
    index: usize,
    embedding: Vec<f32>,
}

/// OpenAI 互換の embeddings API のクライアント。
pub struct Client {
    http: reqwest::Client,
    url: String,
    model: String,
    /// 認証のヘッダーの名前と値
    auth: Option<(&'static str, String)>,
    dimensions: Option<u32>,
}

impl Client {
    /// `env` で環境変数を読む（テストでは差し替える）。
    pub fn from_config(
        cfg: &EmbeddingConfig,
        env: impl Fn(&str) -> Option<String>,
    ) -> Result<Self, EmbedError> {
        let header = match cfg.auth {
            EmbeddingAuth::None => None,
            EmbeddingAuth::Bearer => Some("authorization"),
            EmbeddingAuth::ApiKey => Some("api-key"),
        };
        let auth = match (header, &cfg.api_key_env) {
            (Some(header), Some(name)) => {
                let key = env(name).ok_or_else(|| EmbedError::MissingKey { env: name.clone() })?;
                let value = match cfg.auth {
                    EmbeddingAuth::Bearer => format!("Bearer {key}"),
                    _ => key,
                };
                Some((header, value))
            }
            // 設定の検証で、認証には鍵の環境変数を求めている
            _ => None,
        };
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(cfg.timeout_secs))
            // embeddings API はリダイレクトしない。たどると鍵のヘッダー（api-key）をほかのサイトへ送ってしまう
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(EmbedError::Build)?;
        Ok(Self {
            http,
            url: cfg.url.clone(),
            model: cfg.model.clone(),
            auth,
            dimensions: cfg.dimensions,
        })
    }
}

impl Client {
    /// 応答を確かめ、入力の順に並べて正規化する。
    fn check(&self, sent: usize, resp: Response) -> Result<Vec<Vec<f32>>, EmbedError> {
        let invalid = |reason: String| Err(EmbedError::Invalid(reason));
        if resp.data.len() != sent {
            return invalid(format!(
                "sent {sent} inputs, got {} vectors",
                resp.data.len()
            ));
        }
        let mut slots: Vec<Option<Vec<f32>>> = vec![None; sent];
        for d in resp.data {
            match slots.get_mut(d.index) {
                Some(slot @ None) => *slot = Some(d.embedding),
                _ => return invalid(format!("unexpected or repeated index {}", d.index)),
            }
        }
        let vectors: Vec<Vec<f32>> = slots.into_iter().map(|v| v.expect("all filled")).collect();
        let dim = vectors.first().map_or(0, Vec::len);
        if vectors.iter().any(|v| v.len() != dim) {
            return invalid("vectors differ in dimension".into());
        }
        if let Some(want) = self.dimensions
            && usize::try_from(want).ok() != Some(dim)
        {
            return invalid(format!("asked for {want} dimensions, got {dim}"));
        }
        vectors
            .into_iter()
            .map(|v| {
                normalize(v).ok_or_else(|| EmbedError::Invalid("zero or non-finite vector".into()))
            })
            .collect()
    }
}

impl Embedder for Client {
    async fn embed(&self, inputs: &[String]) -> Result<Vec<Vec<f32>>, EmbedError> {
        let mut body = serde_json::json!({"model": self.model, "input": inputs});
        if let Some(d) = self.dimensions {
            body["dimensions"] = d.into();
        }
        let mut req = self
            .http
            .post(&self.url)
            .header("content-type", "application/json")
            .body(body.to_string());
        if let Some((name, value)) = &self.auth {
            req = req.header(*name, value);
        }
        let mut resp = req.send().await.map_err(EmbedError::Request)?;
        let status = resp.status();
        if !status.is_success() {
            // 表示に使う分だけを読む
            let body = match read_limited(&mut resp, ERROR_BODY_BYTES).await {
                Ok(Ok(body) | Err(body)) => body,
                Err(_) => Vec::new(),
            };
            return Err(EmbedError::Status {
                status: status.as_u16(),
                body: String::from_utf8_lossy(&body).chars().take(500).collect(),
            });
        }
        let limit = RESPONSE_BYTES_PER_INPUT * inputs.len() + RESPONSE_BYTES_BASE;
        let bytes = read_limited(&mut resp, limit)
            .await?
            .map_err(|_| EmbedError::Invalid(format!("response larger than {limit} bytes")))?;
        let resp: Response = serde_json::from_slice(&bytes)
            .map_err(|e| EmbedError::Invalid(format!("not an embeddings response: {e}")))?;
        self.check(inputs.len(), resp)
    }
}

/// テストで使う、偽の embedding と応答を返すサーバー。
#[cfg(test)]
pub(crate) mod fake {
    use std::collections::VecDeque;
    use std::sync::Mutex;

    use super::*;
    use crate::testutil::{Route, Server};

    /// テスト用の設定（ruri と同じ接頭辞）。
    pub(crate) fn cfg(url: &str) -> EmbeddingConfig {
        EmbeddingConfig {
            url: url.to_string(),
            model: "m".into(),
            auth: EmbeddingAuth::None,
            api_key_env: None,
            dimensions: None,
            query_prefix: "検索クエリ: ".into(),
            document_prefix: "検索文書: ".into(),
            batch_size: 32,
            timeout_secs: 30,
        }
    }

    pub(crate) fn no_env(_: &str) -> Option<String> {
        None
    }

    /// `data` を OpenAI の形の応答にする（`index` は与えた順）。
    pub(crate) fn response(data: &[(usize, Vec<f32>)]) -> Route {
        let data: Vec<_> = data
            .iter()
            .map(|(i, v)| serde_json::json!({"object": "embedding", "index": i, "embedding": v}))
            .collect();
        Route::ok(serde_json::json!({"object": "list", "data": data, "model": "m"}).to_string())
    }

    /// 文から決まる、正規化していないベクトル（文が違えばほぼ直交する）。
    pub(crate) fn fake_vector(text: &str) -> Vec<f32> {
        use std::hash::{Hash, Hasher};
        (0..16)
            .map(|i| {
                let mut h = std::collections::hash_map::DefaultHasher::new();
                (text, i).hash(&mut h);
                (h.finish() % 1000) as f32 - 499.5
            })
            .collect()
    }

    /// 入力の文ごとに `fake_vector` を返すサーバー。
    pub(crate) fn echo_server() -> Server {
        Server::start_with(|req| {
            let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
            let inputs = body["input"].as_array().unwrap();
            response(
                &inputs
                    .iter()
                    .enumerate()
                    .map(|(i, t)| (i, fake_vector(t.as_str().unwrap())))
                    .collect::<Vec<_>>(),
            )
        })
    }

    /// 偽の embedding。`model` を変えると、同じ文でも違うベクトルを返す（中身のモデルの入れ替え）。
    /// `bad` を含む文があれば、文のせいの失敗（413）を返す。`errors` に入れた失敗は、先頭から順に返す。
    /// `hang_from` 回目（0 から数える）以降の呼び出しは応答しない。`cancel_at` 回目の呼び出しでは中断を要求する。
    #[derive(Default)]
    pub(crate) struct FakeEmbedder {
        pub model: Mutex<String>,
        pub bad: Option<String>,
        pub errors: Mutex<VecDeque<EmbedError>>,
        pub calls: Mutex<Vec<Vec<String>>>,
        pub hang_from: Option<usize>,
        pub cancel_at: Option<(usize, crate::pipeline::Cancel)>,
        /// 文ごとに返すベクトルを決める（正規化して返す）。無い文は `vector` で作る
        pub fixed: Mutex<std::collections::HashMap<String, Vec<f32>>>,
    }

    impl FakeEmbedder {
        pub(crate) fn calls(&self) -> Vec<Vec<String>> {
            self.calls.lock().unwrap().clone()
        }

        /// `model` のモデルが `text` に返すベクトル。
        pub(crate) fn vector(model: &str, text: &str) -> Vec<f32> {
            normalize(fake_vector(&format!("{model}{text}"))).unwrap()
        }
    }

    impl Embedder for FakeEmbedder {
        async fn embed(&self, inputs: &[String]) -> Result<Vec<Vec<f32>>, EmbedError> {
            let n = {
                let mut calls = self.calls.lock().unwrap();
                calls.push(inputs.to_vec());
                calls.len() - 1
            };
            if let Some((at, cancel)) = &self.cancel_at
                && *at == n
            {
                cancel.request();
            }
            if self.hang_from.is_some_and(|from| n >= from) {
                std::future::pending::<()>().await;
            }
            if let Some(e) = self.errors.lock().unwrap().pop_front() {
                return Err(e);
            }
            if let Some(bad) = &self.bad
                && inputs.iter().any(|t| t.contains(bad.as_str()))
            {
                return Err(EmbedError::Status {
                    status: 413,
                    body: "too long".into(),
                });
            }
            let model = self.model.lock().unwrap().clone();
            let fixed = self.fixed.lock().unwrap();
            Ok(inputs
                .iter()
                .map(|t| match fixed.get(t) {
                    Some(v) => normalize(v.clone()).unwrap(),
                    None => Self::vector(&model, t),
                })
                .collect())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::*;
    use super::*;
    use crate::testutil::{Route, Server};

    fn assert_unit(v: &[f32]) {
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-5, "{v:?}");
    }

    /// `model`・`input`・`dimensions` を送り、返ったベクトルを正規化して入力の順に返す。
    #[tokio::test]
    async fn sends_inputs_and_normalizes_vectors() {
        let server = echo_server();
        let cfg = EmbeddingConfig {
            dimensions: Some(16),
            ..cfg(&server.url("/v1/embeddings"))
        };
        let client = Client::from_config(&cfg, no_env).unwrap();
        let out = client
            .embed(&["ab".to_string(), "abcd".to_string()])
            .await
            .unwrap();
        assert_eq!(out.len(), 2);
        out.iter().for_each(|v| assert_unit(v));
        let norm = |v: &[f32]| v.iter().map(|x| x * x).sum::<f32>().sqrt();
        let raw = fake_vector("abcd");
        assert!((out[1][0] - raw[0] / norm(&raw)).abs() < 1e-6, "{out:?}");
        let req = &server.requests()[0];
        assert_eq!(
            (req.method.as_str(), req.path.as_str()),
            ("POST", "/v1/embeddings")
        );
        let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
        assert_eq!(body["model"], "m");
        assert_eq!(body["input"], serde_json::json!(["ab", "abcd"]));
        assert_eq!(body["dimensions"], 16);
        assert!(!req.headers.contains_key("authorization"));
    }

    /// 応答の `data` が順不同でも、`index` に従って入力の順に並べる。
    #[tokio::test]
    async fn orders_vectors_by_index() {
        let server = Server::start_with(|_| response(&[(1, vec![0.0, 1.0]), (0, vec![1.0, 0.0])]));
        let client = Client::from_config(&cfg(&server.url("/e")), no_env).unwrap();
        let out = client
            .embed(&["a".to_string(), "b".to_string()])
            .await
            .unwrap();
        assert_eq!(out, [vec![1.0, 0.0], vec![0.0, 1.0]]);
    }

    /// OpenAI は `Authorization: Bearer`、Azure は `api-key` ヘッダーで鍵を送る。鍵は環境変数から読む。
    #[tokio::test]
    async fn sends_the_key_in_the_configured_header() {
        let server = echo_server();
        let env = |name: &str| (name == "KEY").then(|| "secret".to_string());
        for (auth, header, value) in [
            (EmbeddingAuth::Bearer, "authorization", "Bearer secret"),
            (EmbeddingAuth::ApiKey, "api-key", "secret"),
        ] {
            let cfg = EmbeddingConfig {
                auth,
                api_key_env: Some("KEY".into()),
                ..cfg(&server.url("/e"))
            };
            let client = Client::from_config(&cfg, env).unwrap();
            client.embed(&["a".to_string()]).await.unwrap();
            let req = server.requests().pop().unwrap();
            assert_eq!(req.headers.get(header).map(String::as_str), Some(value));
        }
        let missing = EmbeddingConfig {
            auth: EmbeddingAuth::Bearer,
            api_key_env: Some("NOPE".into()),
            ..cfg(&server.url("/e"))
        };
        assert!(matches!(
            Client::from_config(&missing, env),
            Err(EmbedError::MissingKey { env }) if env == "NOPE"
        ));
    }

    /// 件数が合わない・次元がそろわない・設定の次元と違う・0 ベクトルの応答は、使わずに誤りにする。
    #[tokio::test]
    async fn rejects_malformed_responses() {
        for (data, dimensions) in [
            (vec![(0, vec![1.0, 0.0])], None),
            (vec![(0, vec![1.0, 0.0]), (1, vec![1.0])], None),
            (vec![(0, vec![1.0, 0.0]), (1, vec![0.0, 1.0])], Some(3)),
            (vec![(0, vec![1.0, 0.0]), (1, vec![0.0, 0.0])], None),
        ] {
            let route = response(&data);
            let server = Server::start_with(move |_| route.clone());
            let cfg = EmbeddingConfig {
                dimensions,
                ..cfg(&server.url("/e"))
            };
            let client = Client::from_config(&cfg, no_env).unwrap();
            let got = client.embed(&["a".to_string(), "b".to_string()]).await;
            assert!(
                matches!(got, Err(EmbedError::Invalid(_))),
                "{data:?}: {got:?}"
            );
        }
    }

    /// 失敗の応答はステータスを返す。文のせいの失敗（400・413・422）と、サービスの側の失敗を分ける。
    #[tokio::test]
    async fn classifies_failed_responses() {
        for (status, input) in [
            (400, true),
            (413, true),
            (422, true),
            (401, false),
            (429, false),
            (503, false),
        ] {
            let server = Server::start_with(move |_| Route::status(status));
            let client = Client::from_config(&cfg(&server.url("/e")), no_env).unwrap();
            let err = client.embed(&["a".to_string()]).await.unwrap_err();
            assert!(
                matches!(err, EmbedError::Status { status: s, .. } if s == status),
                "{err:?}"
            );
            assert_eq!(err.is_input_error(), input, "{status}");
        }
    }

    /// リダイレクトはたどらない（鍵のヘッダーをほかのサイトへ送らない）。サービスの側の失敗にする。
    #[tokio::test]
    async fn does_not_follow_redirects() {
        let elsewhere = echo_server();
        let target = elsewhere.url("/e");
        let server = Server::start_with(move |_| Route::redirect(&target));
        let cfg = EmbeddingConfig {
            auth: EmbeddingAuth::ApiKey,
            api_key_env: Some("KEY".into()),
            ..cfg(&server.url("/e"))
        };
        let client = Client::from_config(&cfg, |_| Some("secret".into())).unwrap();
        let err = client.embed(&["a".to_string()]).await.unwrap_err();
        assert!(
            matches!(err, EmbedError::Status { status: 302, .. }),
            "{err:?}"
        );
        assert!(!err.is_input_error());
        assert!(elsewhere.requests().is_empty());
    }

    /// 応答は、送った文の数から決めた大きさまでしか読まない。超えれば誤りにする（壊れたサーバーで
    /// メモリを使い切らない）。失敗の応答の本文も、表示に使う分だけを読む。
    #[tokio::test]
    async fn limits_response_sizes() {
        let huge = vec![b' '; 4 * 1024 * 1024];
        let body = huge.clone();
        let server = Server::start_with(move |_| Route::ok(body.clone()));
        let client = Client::from_config(&cfg(&server.url("/e")), no_env).unwrap();
        let err = client.embed(&["a".to_string()]).await.unwrap_err();
        assert!(
            matches!(&err, EmbedError::Invalid(m) if m.contains("larger")),
            "{err:?}"
        );
        let failing = Server::start_with(move |_| Route {
            body: huge.clone(),
            ..Route::status(503)
        });
        let client = Client::from_config(&cfg(&failing.url("/e")), no_env).unwrap();
        let err = client.embed(&["a".to_string()]).await.unwrap_err();
        assert!(
            matches!(&err, EmbedError::Status { status: 503, body } if body.len() <= 500),
            "{err:?}"
        );
    }

    /// 応答しないサーバーへの呼び出しは、`timeout_secs` で失敗になる（サービスの側の失敗）。
    #[tokio::test]
    async fn times_out() {
        let server = Server::start_with(|_| Route {
            delay: Duration::from_secs(3),
            ..response(&[(0, vec![1.0])])
        });
        let cfg = EmbeddingConfig {
            timeout_secs: 1,
            ..cfg(&server.url("/e"))
        };
        let client = Client::from_config(&cfg, no_env).unwrap();
        let err = client.embed(&["a".to_string()]).await.unwrap_err();
        assert!(
            matches!(&err, EmbedError::Request(e) if e.is_timeout()),
            "{err:?}"
        );
        assert!(!err.is_input_error());
    }

    /// 接頭辞は役割ごとに付く。空間の名前は、URL・モデル・次元・接頭辞のどれが変わっても変わる。
    #[test]
    fn prefixes_inputs_and_names_the_space() {
        let base = cfg("http://h/e");
        assert_eq!(input(&base, Role::Query, "燃料"), "検索クエリ: 燃料");
        assert_eq!(input(&base, Role::Document, "燃料"), "検索文書: 燃料");
        let name = space_name(&base);
        for changed in [
            EmbeddingConfig {
                url: "http://h2/e".into(),
                ..base.clone()
            },
            EmbeddingConfig {
                model: "m2".into(),
                ..base.clone()
            },
            EmbeddingConfig {
                dimensions: Some(8),
                ..base.clone()
            },
            EmbeddingConfig {
                query_prefix: String::new(),
                ..base.clone()
            },
            EmbeddingConfig {
                document_prefix: String::new(),
                ..base.clone()
            },
        ] {
            assert_ne!(space_name(&changed), name, "{changed:?}");
        }
        // 呼び出しの量に関わる設定は、空間を変えない
        let same = EmbeddingConfig {
            batch_size: 3,
            timeout_secs: 5,
            ..base.clone()
        };
        assert_eq!(space_name(&same), name);
    }

    /// 指紋はクエリと文書の両方の経路で作り、どちらかの接頭辞だけを変えても一致しなくなる。
    #[tokio::test]
    async fn fingerprints_both_roles() {
        let server = echo_server();
        let base = cfg(&server.url("/e"));
        let client = Client::from_config(&base, no_env).unwrap();
        let fp = Fingerprint::make(&client, &base).await.unwrap();
        assert_eq!(
            (fp.query.len(), fp.document.len()),
            (FINGERPRINT_TEXTS, FINGERPRINT_TEXTS)
        );
        let again = Fingerprint::make(&client, &base).await.unwrap();
        assert!(fp.matches(Role::Query, &again.query));
        assert!(fp.matches(Role::Document, &again.document));
        for changed in [
            EmbeddingConfig {
                query_prefix: "q: ".into(),
                ..base.clone()
            },
            EmbeddingConfig {
                document_prefix: "d: ".into(),
                ..base.clone()
            },
        ] {
            let other = Fingerprint::make(&client, &changed).await.unwrap();
            assert!(
                !(fp.matches(Role::Query, &other.query)
                    && fp.matches(Role::Document, &other.document)),
                "{changed:?}"
            );
        }
        // 次元が違えば一致しない
        assert!(!fp.matches(Role::Query, &[vec![1.0], vec![1.0]]));
        // 件数が違えば一致しない
        assert!(!fp.matches(Role::Query, &fp.query[..1]));
    }

    #[test]
    fn encodes_vectors() {
        let v = vec![0.25f32, -1.5, 3.0];
        assert_eq!(decode(&encode(&v)), Some(v));
        assert_eq!(decode(&[0, 0, 0]), None);
    }
}
