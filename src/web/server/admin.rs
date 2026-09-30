//! 管理者だけの操作（計画 009）。訳語集は全員の和訳に効く共有のデータで、和訳は所有者の LLM の枠で作るので、
//! 編集は管理者だけにする。指摘の受付箱は管理者に宛てたもので、ほかの人の指摘の補足や返事を含む。

use super::*;

use axum::extract::Request;
use axum::middleware::Next;
use axum::routing::MethodRouter;

/// 管理者だけのルートに掛ける層。ログインの確認の層が入れた利用者が管理者でなければ 403。
async fn require_admin(req: Request, next: Next) -> Response {
    match req.extensions().get::<crate::db::Viewer>() {
        Some(viewer) if viewer.is_admin => next.run(req).await,
        _ => (StatusCode::FORBIDDEN, "admin only").into_response(),
    }
}

/// 管理者だけのルート。ルートの一覧でどれが管理者だけかが分かるよう、定義する所で包む。
pub(super) fn admin_only<S: Clone + Send + Sync + 'static>(
    route: MethodRouter<S>,
) -> MethodRouter<S> {
    route.route_layer(axum::middleware::from_fn(require_admin))
}

#[cfg(test)]
mod tests {
    use crate::db::{Db, NewReport, ReportKind};
    use crate::web::server::test_support::*;

    const MEMBER: &str = "member-session";

    fn with_member(db: &Db) -> i64 {
        let member = other_user(db, "member@example.com");
        insert_session(db, member, MEMBER);
        member
    }

    /// 管理者でなければ、訳語集の編集と受付箱（閲覧と処理）は 403 で、何も変わらない。訳語集を見ることはできる。
    #[tokio::test]
    async fn members_cannot_edit_the_glossary_or_open_the_inbox() {
        let db = Db::open_in_memory().unwrap();
        with_member(&db);
        let server = Server::start(db).await;
        let terms = server.count("SELECT count(*) FROM glossary_terms");
        let term = server.count("SELECT min(id) FROM glossary_terms");
        for (path, body) in [
            ("/glossary".to_string(), "target=訳&sources=word"),
            (format!("/glossary/{term}"), "target=訳&sources=word"),
            (format!("/glossary/{term}/delete"), ""),
            ("/reports/1".to_string(), "status=done"),
        ] {
            assert_eq!(server.post_as(MEMBER, &path, body).await, 403, "{path}");
        }
        assert_eq!(server.count("SELECT count(*) FROM glossary_terms"), terms);
        assert_eq!(server.get_as(MEMBER, "/reports").await.0, 403);
        let (status, html) = server.get_as(MEMBER, "/glossary").await;
        assert_eq!(status, 200);
        // 押すと必ず失敗するフォームは出さない
        assert!(!html.contains(r#"method="post""#), "{html}");
        // 管理者には出る
        let (_, html) = server.get("/glossary").await;
        assert!(
            html.contains(r#"<form method="post" action="/glossary">"#),
            "{html}"
        );
        assert_eq!(server.get_raw("/reports").await.status().as_u16(), 200);
    }

    /// 運用の警告（取得の失敗など。URL やバックエンドの診断を含む）は管理者の画面にだけ出す。
    #[tokio::test]
    async fn only_admins_see_operational_warnings() {
        let db = Db::open_in_memory().unwrap();
        let (id, _) = seed(&db, "https://e.com/a", "見出しA");
        with_member(&db);
        db.record_source_failure("wnn", "GET https://internal.example/feed: HTTP 503")
            .unwrap();
        let server = Server::start(db).await;
        let article = format!("/articles/{id}");
        for path in [
            "/",
            "/?min=0",
            article.as_str(),
            "/search?q=x",
            "/settings",
            "/glossary",
        ] {
            let (status, html) = server.get_as(MEMBER, path).await;
            assert_eq!(status, 200, "{path}");
            assert!(!html.contains("internal.example"), "{path}: {html}");
            let (_, html) = server.get(path).await;
            assert!(html.contains("internal.example"), "{path}: {html}");
        }
    }

    /// 一般の利用者には、自分の出した指摘だけを出す。設定画面の受付箱と受付中の件数は管理者にだけ出す。
    #[tokio::test]
    async fn members_see_only_their_own_reports() {
        let db = Db::open_in_memory().unwrap();
        let (id, _) = seed(&db, "https://e.com/a", "見出しA");
        let member = with_member(&db);
        let other = |body| NewReport::Other {
            kind: ReportKind::Digest,
            body,
        };
        let now = chrono::Utc::now();
        db.add_report(db.owner_id().unwrap(), id, &other("所有者の補足"), now)
            .unwrap();
        db.add_report(member, id, &other("自分の補足"), now)
            .unwrap();
        let server = Server::start(db).await;
        let article = format!("/articles/{id}");
        let (_, html) = server.get_as(MEMBER, &article).await;
        assert!(
            html.contains("自分の補足") && !html.contains("所有者の補足"),
            "{html}"
        );
        let (_, html) = server.get(&article).await;
        assert!(
            html.contains("自分の補足") && html.contains("所有者の補足"),
            "{html}"
        );

        let (_, html) = server.get_as(MEMBER, "/settings").await;
        assert!(
            !html.contains("/reports") && !html.contains("受付中"),
            "{html}"
        );
        let (_, html) = server.get("/settings").await;
        assert!(
            html.contains("/reports") && html.contains("受付中 2 件"),
            "{html}"
        );
    }
}
