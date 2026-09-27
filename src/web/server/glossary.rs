//! 訳語集の画面と編集。

use super::*;

pub(super) async fn glossary(State(state): State<AppState>) -> Result<Html<String>, AppError> {
    let labels = state.labels.clone();
    let page = with_db(&state, move |db| {
        let entries = db.glossary_entries()?;
        let warnings = warnings(db)?;
        let page = Page {
            warnings: &warnings,
            labels: &labels,
        };
        Ok(html::glossary_page(&entries, &page))
    })
    .await?;
    Ok(Html(page))
}

#[derive(serde::Deserialize)]
pub(super) struct GlossaryForm {
    // 欄が無いときも空と同じく検証で 400 にする
    #[serde(default)]
    target: String,
    #[serde(default)]
    abbr: String,
    #[serde(default)]
    note: String,
    /// 1 行に 1 つ
    #[serde(default)]
    sources: String,
}

impl GlossaryForm {
    /// 空白を除き、空の行と大文字小文字だけ違う重複の原語を落とす。訳語と原語は必須。
    fn into_term(self) -> Result<crate::glossary::Term, AppError> {
        let filled = |s: &str| Some(s.trim()).filter(|s| !s.is_empty()).map(str::to_string);
        let target =
            filled(&self.target).ok_or(AppError::BadRequest("target must not be empty"))?;
        let mut sources: Vec<String> = Vec::new();
        for source in self.sources.lines().filter_map(filled) {
            if !sources.iter().any(|s| s.eq_ignore_ascii_case(&source)) {
                sources.push(source);
            }
        }
        if sources.is_empty() {
            return Err(AppError::BadRequest("sources must not be empty"));
        }
        Ok(crate::glossary::Term {
            sources,
            target,
            abbr: filled(&self.abbr),
            note: filled(&self.note),
        })
    }
}

/// 訳語集の重なりは、どの訳語と重なったかを利用者に返す。
fn glossary_error(e: DbError) -> AppError {
    match e {
        DbError::GlossaryConflict(message) => AppError::Conflict(message),
        e => e.into(),
    }
}

pub(super) async fn add_glossary_term(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<GlossaryForm>,
) -> Result<Redirect, AppError> {
    check_same_origin(&headers)?;
    let term = form.into_term()?;
    let id = with_db(&state, move |db| {
        db.add_glossary_term(&term, Utc::now())
            .map_err(glossary_error)
    })
    .await?;
    Ok(Redirect::to(&format!("/glossary#term-{id}")))
}

pub(super) async fn update_glossary_term(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    headers: HeaderMap,
    Form(form): Form<GlossaryForm>,
) -> Result<Redirect, AppError> {
    check_same_origin(&headers)?;
    let term = form.into_term()?;
    let found = with_db(&state, move |db| {
        db.update_glossary_term(id, &term, Utc::now())
            .map_err(glossary_error)
    })
    .await?;
    if !found {
        return Err(AppError::NotFound);
    }
    Ok(Redirect::to(&format!("/glossary#term-{id}")))
}

pub(super) async fn delete_glossary_term(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    headers: HeaderMap,
) -> Result<Redirect, AppError> {
    check_same_origin(&headers)?;
    let found = with_db(&state, move |db| Ok(db.delete_glossary_term(id)?)).await?;
    if !found {
        return Err(AppError::NotFound);
    }
    Ok(Redirect::to("/glossary"))
}

#[cfg(test)]
mod tests {
    use crate::db::Db;
    use crate::web::server::test_support::*;

    /// 原語は 1 行に 1 つ。空の行と、大文字小文字だけ違う重複は除く。
    #[tokio::test]
    async fn glossary_terms_are_added_updated_and_deleted() {
        let server = Server::start(Db::open_in_memory().unwrap()).await;
        let sources = "SELECT group_concat(s.source, '|') FROM glossary_sources AS s
                       JOIN glossary_terms AS t ON t.id = s.term_id WHERE t.abbr = 'EDG'";
        let res = server
            .post(
                "/glossary",
                "target=%E9%9D%9E%E5%B8%B8%E7%94%A8DG&abbr=+EDG+&note=&sources=emergency+diesel+generator%0D%0AEDG%0D%0A+%0D%0Aedg",
            )
            .await;
        assert_eq!(res.status().as_u16(), 303);
        let id = server.count("SELECT id FROM glossary_terms WHERE abbr = 'EDG'");
        assert_eq!(
            res.headers()["location"].to_str().unwrap(),
            format!("/glossary#term-{id}")
        );
        assert_eq!(server.strings(sources), ["emergency diesel generator|EDG"]);
        assert_eq!(
            server.count("SELECT count(*) FROM glossary_terms WHERE abbr = 'EDG' AND note IS NULL"),
            1
        );

        let path = format!("/glossary/{id}");
        let res = server
            .post(
                &path,
                "target=%E9%9D%9E%E5%B8%B8%E7%94%A8DG&abbr=EDG&note=&sources=EDG",
            )
            .await;
        assert_eq!(res.status().as_u16(), 303);
        assert_eq!(server.strings(sources), ["EDG"]);

        let delete = format!("/glossary/{id}/delete");
        let res = server.post(&delete, "").await;
        assert_eq!(res.status().as_u16(), 303);
        assert_eq!(res.headers()["location"].to_str().unwrap(), "/glossary");
        assert_eq!(
            server.count("SELECT count(*) FROM glossary_terms WHERE abbr = 'EDG'"),
            0
        );
        assert_eq!(server.post(&delete, "").await.status().as_u16(), 404);
        assert_eq!(
            server
                .post(&path, "target=x&sources=x")
                .await
                .status()
                .as_u16(),
            404
        );
    }

    #[tokio::test]
    async fn glossary_rejects_invalid_or_conflicting_terms() {
        let server = Server::start(Db::open_in_memory().unwrap()).await;
        let before = server.count("SELECT count(*) FROM glossary_sources");
        // 訳語と原語は必須
        assert_eq!(
            server
                .post("/glossary", "target=+&sources=x")
                .await
                .status()
                .as_u16(),
            400
        );
        assert_eq!(
            server
                .post("/glossary", "target=x&sources=%0D%0A+")
                .await
                .status()
                .as_u16(),
            400
        );
        assert_eq!(
            server.post("/glossary", "target=x").await.status().as_u16(),
            400
        );
        // ほかの訳語の原語は使えず、どの訳語のものかを返す
        let res = server.post("/glossary", "target=x&sources=atf").await;
        assert_eq!(res.status().as_u16(), 409);
        assert!(res.text().await.unwrap().contains("事故耐性燃料"));
        let res = server
            .form("/glossary", "target=x&sources=x")
            .header("origin", "https://evil.example")
            .send()
            .await
            .unwrap();
        assert_eq!(res.status().as_u16(), 403);
        assert_eq!(
            server.count("SELECT count(*) FROM glossary_sources"),
            before
        );
    }
}
