//! ソースを取得して解析し、記事の候補を集める（crawl の取得と `sources check` が使う）。

use std::collections::HashSet;

use url::Url;

use super::{Candidate, SourceError, html_list};
use crate::config::{HtmlList, Source, SourceKind};
use crate::http::{Fetcher, HttpError};
use crate::text;

/// 1 つのソースの取得失敗。呼び出し側はほかのソースを続ける。
#[derive(Debug, thiserror::Error)]
pub enum SourceFailure {
    #[error("invalid source url {url:?}")]
    InvalidUrl {
        url: String,
        source: url::ParseError,
    },
    #[error(transparent)]
    Http(#[from] HttpError),
    #[error(transparent)]
    Parse(#[from] SourceError),
}

#[derive(Debug)]
pub struct Stats {
    /// フィードに含まれていた件数
    pub total: usize,
    /// 絞り込み条件に一致したもの（フィードの順）
    pub matched: Vec<Candidate>,
}

/// 1 つのソースを取得・解析し、絞り込み条件に一致した候補を返す。
pub async fn fetch_source(fetcher: &Fetcher, s: &Source) -> Result<Stats, SourceFailure> {
    let url = Url::parse(&s.url).map_err(|source| SourceFailure::InvalidUrl {
        url: s.url.clone(),
        source,
    })?;
    let candidates = match (s.kind, &s.list) {
        (SourceKind::HtmlList, Some(list)) => fetch_html_list(fetcher, &url, list).await?,
        _ => {
            let fetched = fetcher.get(&url).await?;
            super::parse(s.kind, &fetched.body, &fetched.url)?
        }
    };
    let total = candidates.len();
    let matched = candidates
        .into_iter()
        .filter(|c| super::matches(&s.filter, c))
        .collect();
    Ok(Stats { total, matched })
}

/// 一覧ページは記事ページと同じく robots.txt に従って取得する。`also` のページは一覧に続けて
/// 同じ読み方で読み、同じ URL の記事は最初の 1 件だけにする。
async fn fetch_html_list(
    fetcher: &Fetcher,
    url: &Url,
    list: &HtmlList,
) -> Result<Vec<Candidate>, SourceFailure> {
    let mut page = fetcher.get_page(url).await?;
    if let Some(follow) = &list.follow {
        let html = text::decode_html(&page.body, page.content_type.as_deref());
        let next = html_list::follow(follow, &html, &page.url)?;
        page = fetcher.get_page(&next).await?;
    }
    let html = text::decode_html(&page.body, page.content_type.as_deref());
    let mut items = html_list::parse(list, &html, &page.url)?;
    for also in &list.also {
        let url = Url::parse(also).map_err(|source| SourceFailure::InvalidUrl {
            url: also.clone(),
            source,
        })?;
        let page = fetcher.get_page(&url).await?;
        let html = text::decode_html(&page.body, page.content_type.as_deref());
        items.extend(html_list::parse(list, &html, &page.url)?);
    }
    let mut seen = HashSet::new();
    items.retain(|c| seen.insert(c.url.clone()));
    Ok(items)
}
