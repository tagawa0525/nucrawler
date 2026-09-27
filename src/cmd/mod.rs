//! LLM を使うパイプラインを流すサブコマンド。バイナリだけが使う。

use nucrawler::pipeline::Cancel;
use nucrawler::pipeline::run::RunReport;

use crate::Error;

mod crawl;
mod eval;
mod redo;

pub(crate) use crawl::crawl;
pub(crate) use eval::eval;
pub(crate) use redo::redo;

/// 実行の結果をエラーにする。中断を最優先し、次に LLM の失敗、最後に取得に失敗したソースを報告する。
fn finish(report: RunReport) -> Result<(), Error> {
    if report.cancelled {
        return Err(Error::Interrupted);
    }
    if let Some(message) = report.llm_failure {
        return Err(Error::LlmFailed(message));
    }
    if report.failed_sources > 0 {
        return Err(Error::SourcesFailed(report.failed_sources));
    }
    Ok(())
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
