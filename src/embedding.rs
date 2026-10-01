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
        todo!()
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
    todo!()
}

/// 接頭辞を付けた入力の文。
pub fn input(cfg: &EmbeddingConfig, role: Role, text: &str) -> String {
    todo!()
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
        todo!()
    }

    /// `role` の経路で返った試験文のベクトルが、この指紋と同じ空間のものか。
    pub fn matches(&self, role: Role, vectors: &[Vec<f32>]) -> bool {
        todo!()
    }
}

/// ベクトルを保存する形（f32 のリトルエンディアン）。
pub fn encode(vector: &[f32]) -> Vec<u8> {
    todo!()
}

/// `encode` の逆。長さが 4 の倍数でなければ `None`。
pub fn decode(bytes: &[u8]) -> Option<Vec<f32>> {
    todo!()
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
        todo!()
    }
}

impl Embedder for Client {
    async fn embed(&self, inputs: &[String]) -> Result<Vec<Vec<f32>>, EmbedError> {
        todo!()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{Route, Server};

    fn cfg(url: &str) -> EmbeddingConfig {
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

    fn no_env(_: &str) -> Option<String> {
        None
    }

    /// `data` を OpenAI の形の応答にする（`index` は与えた順）。
    fn response(data: &[(usize, Vec<f32>)]) -> Route {
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
    fn echo_server() -> Server {
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
