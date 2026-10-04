//! 興味プロファイルの画面：今のプロファイルと版の履歴、前の版に戻す（計画 016）。

use super::*;

pub(super) async fn profile_page(
    State(state): State<AppState>,
    Extension(me): Extension<crate::db::Viewer>,
) -> Result<Html<String>, AppError> {
    let page = with_db_and_config(&state, move |db, web, labels| {
        let (user, hash) = viewer(db, me)?;
        let versions = db.profile_versions(user)?;
        let parts = PageParts::new(db, me, hash.as_deref(), web)?;
        let page = parts.page(labels);
        Ok(html::profile_page(&versions, &page))
    })
    .await?;
    Ok(Html(page))
}

pub(super) async fn revert_profile_version(
    State(state): State<AppState>,
    Extension(me): Extension<crate::db::Viewer>,
    Path(id): Path<i64>,
) -> Result<Redirect, AppError> {
    with_db(&state, move |db| {
        match db.revert_profile(me.user_id, id, Utc::now()) {
            Ok(_) => Ok(()),
            // ほかの利用者の版も、無い版と同じに扱う
            Err(DbError::UnknownProfileVersion(_)) => Err(AppError::NotFound),
            Err(e) => Err(e.into()),
        }
    })
    .await?;
    Ok(Redirect::to("/settings/profile"))
}

#[cfg(test)]
mod tests {
    use crate::db::{Db, ProfileOrigin};
    use crate::profile::{Interest, Profile};
    use crate::web::server::test_support::*;

    fn profile(topic: &str, weight: f64) -> Profile {
        Profile {
            interests: vec![Interest {
                topic: topic.into(),
                weight,
                note: Some(format!("{topic}の補足")),
            }],
            exclude: vec!["核融合".into()],
        }
    }

    fn two_versions(db: &Db) -> (i64, i64) {
        let owner = db.owner_id().unwrap();
        db.save_profile(owner, &profile("燃料", 1.0), chrono::Utc::now())
            .unwrap();
        db.save_profile_version(
            owner,
            &profile("燃料", 0.5),
            ProfileOrigin::Auto,
            &[],
            chrono::Utc::now(),
        )
        .unwrap();
        let versions = db.profile_versions(owner).unwrap();
        (versions[1].id, versions[0].id)
    }

    /// 今のプロファイルと履歴を出す。履歴は版ごとに出どころと 1 つ前からの変更を添え、
    /// 今でない版にだけ「この版に戻す」を出す。設定画面から入れる。
    #[tokio::test]
    async fn shows_the_profile_and_its_versions() {
        let db = Db::open_in_memory().unwrap();
        let (first, current) = two_versions(&db);
        let server = Server::start(db).await;
        let (_, settings) = server.get("/settings").await;
        assert!(
            settings.contains("href=\"/settings/profile\""),
            "{settings}"
        );
        let (status, html) = server.get("/settings/profile").await;
        assert_eq!(status, 200);
        let now = html
            .split("<h2>今のプロファイル</h2>")
            .nth(1)
            .expect("current");
        assert!(now.contains("燃料"), "{html}");
        assert!(now.contains("燃料の補足"), "{html}");
        assert!(now.contains("核融合"), "{html}");
        let history = html.split("<h2>履歴</h2>").nth(1).expect("history");
        assert!(history.contains("自動で適用"), "{html}");
        assert!(history.contains("取り込み"), "{html}");
        assert!(history.contains("燃料の重み 1 → 0.5"), "{html}");
        assert!(
            history.contains(&format!(
                "action=\"/settings/profile/versions/{first}/revert\""
            )),
            "{html}"
        );
        assert!(
            !history.contains(&format!("/settings/profile/versions/{current}/revert")),
            "{html}"
        );
    }

    /// 前の版に戻すと、その中身が今のプロファイルになり、画面に戻る。
    #[tokio::test]
    async fn reverts_to_a_version() {
        let db = Db::open_in_memory().unwrap();
        let (first, _) = two_versions(&db);
        let server = Server::start(db).await;
        let res = server
            .form(&format!("/settings/profile/versions/{first}/revert"), "")
            .send()
            .await
            .unwrap();
        assert_eq!(res.status().as_u16(), 303);
        assert_eq!(res.headers()["location"], "/settings/profile");
        assert_eq!(
            server.strings("SELECT origin FROM profile_versions ORDER BY id DESC LIMIT 1"),
            ["revert"]
        );
        assert_eq!(
            server.count("SELECT count(*) FROM profiles WHERE interests LIKE '%1.0%'"),
            1
        );
    }

    /// ほかの利用者の版や、無い版には戻せない。プロファイルの無い利用者には、無いと出す。
    #[tokio::test]
    async fn keeps_versions_to_their_user() {
        let db = Db::open_in_memory().unwrap();
        let (first, _) = two_versions(&db);
        let other = other_user(&db, "o@example.com");
        insert_session(&db, other, "other-session");
        let server = Server::start(db).await;
        let path = format!("/settings/profile/versions/{first}/revert");
        assert_eq!(server.post_as("other-session", &path, "").await, 404);
        assert_eq!(
            server
                .post("/settings/profile/versions/999/revert", "")
                .await
                .status()
                .as_u16(),
            404
        );
        let (status, html) = server.get_as("other-session", "/settings/profile").await;
        assert_eq!(status, 200);
        assert!(html.contains("プロファイルはまだありません"), "{html}");
        assert!(!html.contains("燃料"), "{html}");
    }
}
