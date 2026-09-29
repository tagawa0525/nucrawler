//! 作業者を同じタスクの中で同時に回す。LLM のステージ（呼び出しの応答待ちの間に次を呼ぶ）と、
//! 取得・抽出（ホストごとの間隔待ちの間にほかのホストへアクセスする）で使う。

use std::future::Future;
use std::pin::Pin;
use std::task::Poll;
/// `n` 個の作業者を同じタスクの中で同時に回し、すべて終わったら結果を並べて返す。どれかが失敗
/// したら、その失敗を返す（ほかの作業者は捨てる。予約は drop で外れる）。作業者は `make` に
/// 番号を渡して作る。
pub async fn run_workers<T, E, F, Fut>(n: usize, mut make: F) -> Result<Vec<T>, E>
where
    F: FnMut(usize) -> Fut,
    Fut: Future<Output = Result<T, E>>,
{
    let mut workers: Vec<Pin<Box<Fut>>> = (0..n.max(1)).map(|i| Box::pin(make(i))).collect();
    let mut results: Vec<Option<T>> = workers.iter().map(|_| None).collect();
    std::future::poll_fn(|cx| {
        let mut pending = false;
        for (worker, result) in workers.iter_mut().zip(results.iter_mut()) {
            if result.is_some() {
                continue;
            }
            match worker.as_mut().poll(cx) {
                Poll::Ready(Ok(value)) => *result = Some(value),
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                Poll::Pending => pending = true,
            }
        }
        if pending {
            Poll::Pending
        } else {
            Poll::Ready(Ok(()))
        }
    })
    .await?;
    Ok(results
        .into_iter()
        .map(|r| r.expect("every worker finished"))
        .collect())
}
