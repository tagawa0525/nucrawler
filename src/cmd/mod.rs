//! LLM を使うパイプラインを流すサブコマンド。バイナリだけが使う。

use nucrawler::pipeline::{Cancel, Halt};

mod crawl;
mod redo;

pub(crate) use crawl::crawl;
pub(crate) use redo::redo;

/// 止めた理由をログに出し、同じ実行で LLM をもう使わないほうがよいなら true を返す。
/// 認証切れなど利用者が対処すべき失敗は `llm_failure` に残し、最後にエラーとして報告する。
fn report_halt(halt: Option<Halt>, llm_failure: &mut Option<String>) -> bool {
    match halt {
        Some(Halt::LlmFailed(message)) => {
            *llm_failure = Some(message);
            true
        }
        Some(Halt::UsageLimit { resets_at }) => {
            tracing::warn!(?resets_at, "stopped at the subscription usage limit");
            true
        }
        Some(Halt::Quota(stop)) => {
            tracing::info!("llm work deferred: {stop}");
            false
        }
        None => false,
    }
}

/// 1 回目の SIGINT/SIGTERM では処理中の 1 件を終えてから止め、2 回目で即座に終了する。
fn spawn_signal_handler(cancel: Cancel) {
    tokio::spawn(async move {
        let mut term =
            match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                Ok(term) => term,
                Err(e) => {
                    tracing::error!("cannot listen for SIGTERM: {e}");
                    return;
                }
            };
        loop {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = term.recv() => {}
            }
            if cancel.is_requested() {
                tracing::warn!("second signal; exiting immediately");
                std::process::exit(130);
            }
            tracing::warn!("stopping after the current item (signal again to exit immediately)");
            cancel.request();
        }
    });
}
