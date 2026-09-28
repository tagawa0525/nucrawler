//! パイプラインの各ステージ。各ステージは未処理の作業を選んで 1 件ずつ処理し、
//! 結果をすぐ DB に書く。途中で止まっても、次回は残りから再開する。

pub mod digest;
pub mod extract;
pub mod fetch;
pub mod llm_call;
pub mod lock;
pub mod run;
pub mod score;
pub mod suggest;
pub mod tidy;
pub mod translate;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// 中断の要求。ステージは作業の切れ目ごとに確認し、要求があれば処理中の 1 件を終えて止まる。
/// LLM の呼び出しのように長く待つ処理は、`requested` と競わせて待たずに止める。
#[derive(Clone, Default)]
pub struct Cancel(Arc<CancelState>);

#[derive(Default)]
struct CancelState {
    requested: AtomicBool,
    notify: tokio::sync::Notify,
}

impl Cancel {
    pub fn request(&self) {
        self.0.requested.store(true, Ordering::SeqCst);
        self.0.notify.notify_waiters();
    }

    pub fn is_requested(&self) -> bool {
        self.0.requested.load(Ordering::SeqCst)
    }

    /// 中断が要求されるまで待つ。
    pub async fn requested(&self) {
        loop {
            let notified = self.0.notify.notified();
            tokio::pin!(notified);
            // 確認より先に待ち受けを登録し、その間の要求を取りこぼさない
            notified.as_mut().enable();
            if self.is_requested() {
                return;
            }
            notified.await;
        }
    }
}

/// ステージを途中で止めた理由。
#[derive(Debug, Clone, PartialEq)]
pub enum Halt {
    /// クォータの判定で止めた（正常。残りは次回）
    Quota(crate::quota::Stop),
    /// サブスクリプションの上限に達した（記事の失敗としては数えない）
    UsageLimit { resets_at: Option<i64> },
    /// 認証切れなど記事によらない失敗の可能性があるので、失敗を広げないよう止めた
    LlmFailed(String),
}

/// LLM ステージが処理する対象。
#[derive(Debug, Clone, PartialEq)]
pub enum Target {
    /// 通常の crawl：まだ成果物の無い記事。`requests_only` なら和訳は依頼だけ
    Pending { requests_only: bool },
    /// `nucrawler redo`：指定したモデル・プロンプト版の成果物がまだ無い記事を、条件で絞って作り直す
    Redo(RedoSpec),
}

#[derive(Debug, Clone, PartialEq)]
pub struct RedoSpec {
    pub filter: crate::db::RedoFilter,
    /// 点数の条件（min_score）に使う利用者とプロファイル
    pub user_id: i64,
    pub profile_hash: Option<String>,
    /// 成果物がまだ無い記事ではなく、このモデルの最新の版が訳語集の変更より前に作られた記事を作り直す
    pub glossary: bool,
}

/// パイプラインのステージ（実行順）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Fetch,
    Extract,
    Digest,
    Score,
    Translate,
    /// 語彙の整理。前回から `llm.tidy_interval_days` 日たったときだけ実行する
    Tidy,
}

impl Stage {
    pub const ALL: &[Stage] = &[
        Stage::Fetch,
        Stage::Extract,
        Stage::Digest,
        Stage::Score,
        Stage::Translate,
        Stage::Tidy,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Stage::Fetch => "fetch",
            Stage::Extract => "extract",
            Stage::Digest => "digest",
            Stage::Score => "score",
            Stage::Translate => "translate",
            Stage::Tidy => "tidy",
        }
    }

    pub fn from_name(name: &str) -> Option<Stage> {
        Stage::ALL.iter().copied().find(|s| s.name() == name)
    }
}

/// 要約が採点のために残す呼び出し回数。採点が計画に無いか、プロファイルが無くて採点が
/// 何もしないときは、残しても使われないので 0。
pub fn score_reserve(stages: &[Stage], cfg: &crate::config::LlmConfig, has_profile: bool) -> u32 {
    if has_profile && stages.contains(&Stage::Score) {
        cfg.score_reserved_calls
    } else {
        0
    }
}

/// `until` を指定すれば最初からそのステージまで、`only` を指定すればそのステージだけ。
/// どちらも無ければ全ステージ。
pub fn plan(until: Option<Stage>, only: Option<Stage>) -> Vec<Stage> {
    match (only, until) {
        (Some(s), _) => vec![s],
        (None, Some(last)) => {
            let end = Stage::ALL
                .iter()
                .position(|&s| s == last)
                .expect("every stage is in ALL");
            Stage::ALL[..=end].to_vec()
        }
        (None, None) => Stage::ALL.to_vec(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stage_names_round_trip() {
        for &s in Stage::ALL {
            assert_eq!(Stage::from_name(s.name()), Some(s));
        }
        assert_eq!(Stage::from_name("nope"), None);
    }

    #[test]
    fn reserves_calls_only_when_scoring_is_planned() {
        let cfg = crate::config::LlmConfig {
            score_reserved_calls: 2,
            ..crate::config::LlmConfig::default()
        };
        assert_eq!(score_reserve(&plan(None, None), &cfg, true), 2);
        assert_eq!(
            score_reserve(&plan(Some(Stage::Digest), None), &cfg, true),
            0
        );
        assert_eq!(
            score_reserve(&plan(None, Some(Stage::Digest)), &cfg, true),
            0
        );
        assert_eq!(
            score_reserve(&plan(None, Some(Stage::Score)), &cfg, true),
            2
        );
        // プロファイルが無ければ採点は何もしないので、残しても使われない
        assert_eq!(score_reserve(&plan(None, None), &cfg, false), 0);
    }

    #[test]
    fn plans_all_until_or_only() {
        assert_eq!(plan(None, None), Stage::ALL);
        assert_eq!(plan(Some(Stage::Fetch), None), [Stage::Fetch]);
        assert_eq!(
            plan(Some(Stage::Extract), None),
            [Stage::Fetch, Stage::Extract]
        );
        assert_eq!(plan(None, Some(Stage::Extract)), [Stage::Extract]);
        assert_eq!(plan(None, Some(Stage::Fetch)), [Stage::Fetch]);
    }

    /// 取得のステージと LLM のステージは、それぞれのロックを取って順に実行する。
    #[test]
    fn groups_stages_by_lock() {
        assert_eq!(
            lock_groups(&plan(None, None)),
            [
                (LockKind::Fetch, vec![Stage::Fetch, Stage::Extract]),
                (
                    LockKind::Llm,
                    vec![Stage::Digest, Stage::Score, Stage::Translate, Stage::Tidy]
                ),
            ]
        );
        assert_eq!(
            lock_groups(&[Stage::Extract]),
            [(LockKind::Fetch, vec![Stage::Extract])]
        );
        assert_eq!(
            lock_groups(&[Stage::Translate]),
            [(LockKind::Llm, vec![Stage::Translate])]
        );
        assert_eq!(lock_groups(&[]), []);
    }
}
