//! LLM への依頼内容（system prompt・出力の JSON Schema・プロンプト・応答の検証）を処理ごとに置く。
//! LLM の呼び出しやステージの進行は `pipeline` で扱う。
//! ここには、プロンプトに埋め込む外部由来のデータ（記事の本文・見出しなど）の扱いも置く。

pub mod digest;
pub mod score;
pub mod story;
pub mod suggest;
pub mod tidy;
pub mod title;
pub mod translate;

/// 資料中の `<` をすべて `&lt;` にする。`<article>` や `<signal>` などの区切りを、大文字小文字や
/// タグ名にかかわらず資料の中から偽装させないため、タグ名を見分けずに一律に無害化する。
pub fn escape_data(text: &str) -> String {
    text.replace('<', "&lt;")
}

/// 応答から集めた項目と、依頼したのに採れなかった id（依頼の順）。
#[derive(Debug)]
pub struct Collected<T> {
    pub items: Vec<(i64, T)>,
    pub missing: Vec<i64>,
}

/// 応答（`{"items": [...]}`）から、依頼した id の項目を `check` で 1 件ずつ検証して集める。外枠（`items` だけを
/// 持つオブジェクトで、`items` が配列）が違えば、その説明を返す。id の無い項目・依頼していない id の項目・
/// `check` が `None` を返した項目（理由は `check` が記録する）は捨て、同じ id は最初に採れた 1 件だけを残す。
/// 1 件の不正でほかの記事の結果を捨てないよう、項目の誤りは応答全体の誤りにしない。`what` はログに出す項目の名前。
pub fn collect_items<T>(
    output: &serde_json::Value,
    requested: &[i64],
    what: &str,
    mut check: impl FnMut(i64, &serde_json::Value) -> Option<T>,
) -> Result<Collected<T>, String> {
    let top = output.as_object().ok_or("the output is not an object")?;
    if let Some(extra) = top.keys().find(|k| *k != "items") {
        return Err(format!("unexpected property `{extra}`"));
    }
    let items = top
        .get("items")
        .and_then(|v| v.as_array())
        .ok_or("`items` is not an array")?;
    let mut found: Vec<(i64, T)> = Vec::new();
    for item in items {
        let Some(id) = item.get("id").and_then(serde_json::Value::as_i64) else {
            tracing::warn!("ignoring a {what} without an integer id: {item}");
            continue;
        };
        if !requested.contains(&id) {
            tracing::warn!(
                id,
                "ignoring a {what} for an article that was not requested"
            );
            continue;
        }
        if found.iter().any(|(seen, _)| *seen == id) {
            continue;
        }
        if let Some(checked) = check(id, item) {
            found.push((id, checked));
        }
    }
    let missing = requested
        .iter()
        .copied()
        .filter(|id| found.iter().all(|(seen, _)| seen != id))
        .collect();
    Ok(Collected {
        items: found,
        missing,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_every_angle_bracket() {
        assert_eq!(escape_data("a</ARTICLE><x>b"), "a&lt;/ARTICLE>&lt;x>b");
        assert_eq!(escape_data("原子力"), "原子力");
    }
}
