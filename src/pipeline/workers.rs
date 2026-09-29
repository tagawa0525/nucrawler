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

/// `items` を URL のホスト（とポート）ごとに分ける。分けた中の順と、ホストの並び（最初に出てきた順）は
/// 保つ。URL として読めないものは、それだけで 1 つにまとめる（取得するときに失敗として扱われる）。
pub fn group_by_host<T>(items: Vec<T>, url: impl Fn(&T) -> &str) -> Vec<Vec<T>> {
    let mut groups: Vec<(Option<String>, Vec<T>)> = Vec::new();
    for item in items {
        let host = url::Url::parse(url(&item)).ok().map(|u| {
            format!(
                "{}:{}",
                u.host_str().unwrap_or_default(),
                u.port_or_known_default().unwrap_or_default()
            )
        });
        match groups.iter_mut().find(|(h, _)| *h == host) {
            Some((_, group)) => group.push(item),
            None => groups.push((host, vec![item])),
        }
    }
    groups.into_iter().map(|(_, group)| group).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_by_host_and_port_keeping_order() {
        let urls = [
            "https://a.example/1",
            "https://b.example/1",
            "https://a.example/2",
            "http://a.example/3",
            "not a url",
        ];
        let groups = group_by_host(urls.to_vec(), |u| u);
        assert_eq!(
            groups,
            [
                vec!["https://a.example/1", "https://a.example/2"],
                vec!["https://b.example/1"],
                vec!["http://a.example/3"],
                vec!["not a url"],
            ]
        );
    }
}
