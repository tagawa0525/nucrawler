//! パイプラインの各ステージ。各ステージは未処理の作業を選んで 1 件ずつ処理し、
//! 結果をすぐ DB に書く。途中で止まっても、次回は残りから再開する。

pub mod fetch;

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
