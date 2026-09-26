//! パイプラインの各ステージ。各ステージは未処理の作業を選んで 1 件ずつ処理し、
//! 結果をすぐ DB に書く。途中で止まっても、次回は残りから再開する。

pub mod extract;
pub mod fetch;
pub mod lock;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// 中断の要求。ステージは作業の切れ目ごとに確認し、要求があれば処理中の 1 件を終えて止まる。
#[derive(Clone, Default)]
pub struct Cancel(Arc<AtomicBool>);

impl Cancel {
    pub fn request(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    pub fn is_requested(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// パイプラインのステージ（実行順）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Fetch,
    Extract,
}

impl Stage {
    pub const ALL: &[Stage] = &[Stage::Fetch, Stage::Extract];

    pub fn name(self) -> &'static str {
        match self {
            Stage::Fetch => "fetch",
            Stage::Extract => "extract",
        }
    }

    pub fn from_name(name: &str) -> Option<Stage> {
        Stage::ALL.iter().copied().find(|s| s.name() == name)
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
}
