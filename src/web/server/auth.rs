//! ログインとセッション（計画 009）。ログインしているかはルーター全体に掛ける層で確かめ、
//! ハンドラは層が入れた `Viewer` を受け取る（ハンドラごとに確かめると、付け忘れた画面が認証なしで開く）。

use super::*;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;
    use crate::web::server::test_support::*;

    const PASSWORD: &str = "correct horse battery";

    /// 所有者にパスワードを設定し、ログイン ID を返す。
    fn with_password(db: &Db) -> &'static str {
        db.reset_password("owner", &crate::auth::hash_password(PASSWORD).unwrap())
            .unwrap();
        "owner"
    }

    fn cookie_of(res: &reqwest::Response) -> String {
        res.headers()[reqwest::header::SET_COOKIE]
            .to_str()
            .unwrap()
            .to_string()
    }

    async fn login(server: &Server, body: String) -> reqwest::Response {
        server
            .client
            .post(format!("{}/login", server.base))
            .header("content-type", "application/x-www-form-urlencoded")
            .body(body)
            .send()
            .await
            .unwrap()
    }

    fn form(pairs: &[(&str, &str)]) -> String {
        url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs(pairs)
            .finish()
    }

    /// ログインしていなければ、画面はログイン画面へ（元の画面を `next` に持って）、JSON と書き込みは 401。
    /// ログイン画面とフィード（トークンで読む）は層を通らない。
    #[tokio::test]
    async fn requests_without_a_session_are_sent_to_login() {
        let db = Db::open_in_memory().unwrap();
        let (id, _) = seed(&db, "https://e.com/a", "見出しA");
        let server = Server::anonymous(db).await;
        for (path, next) in [
            ("/", "%2F"),
            ("/?min=0&read=any", "%2F%3Fmin%3D0%26read%3Dany"),
            ("/settings", "%2Fsettings"),
        ] {
            let res = server.get_raw(path).await;
            assert_eq!(res.status().as_u16(), 303, "{path}");
            assert_eq!(
                res.headers()["location"],
                format!("/login?next={next}"),
                "{path}"
            );
        }
        assert_eq!(server.get_raw("/api/articles").await.status().as_u16(), 401);
        let res = server
            .post(&format!("/articles/{id}/rating"), "value=4")
            .await;
        assert_eq!(res.status().as_u16(), 401);
        assert_eq!(server.count("SELECT count(*) FROM ratings"), 0);
        let (status, html) = server.get("/login?next=%2Fsettings").await;
        assert_eq!(status, 200);
        assert!(html.contains(r#"name="next" value="/settings""#), "{html}");
        assert_eq!(server.get_raw("/feed.xml").await.status().as_u16(), 401);
        // 期限切れや知らないセッションも、無いのと同じ
        let res = server
            .client
            .get(format!("{}/", server.base))
            .header(reqwest::header::COOKIE, format!("{SESSION_COOKIE}=unknown"))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status().as_u16(), 303);
    }

    /// ログインするとセッションの Cookie が付き、元の画面（自分のホストの絶対 URL）へ戻る。
    #[tokio::test]
    async fn logging_in_sets_the_session_cookie_and_returns() {
        let db = Db::open_in_memory().unwrap();
        let login_id = with_password(&db);
        let server = Server::anonymous(db).await;
        let res = login(
            &server,
            form(&[
                ("login", login_id),
                ("password", PASSWORD),
                ("next", "/?min=0"),
            ]),
        )
        .await;
        assert_eq!(res.status().as_u16(), 303);
        assert_eq!(res.headers()["location"], format!("{}/?min=0", server.base));
        let cookie = cookie_of(&res);
        let token = cookie
            .strip_prefix(&format!("{SESSION_COOKIE}="))
            .and_then(|c| c.split(';').next())
            .unwrap()
            .to_string();
        assert_eq!(token.len(), 64, "{cookie}");
        for attr in ["HttpOnly", "SameSite=Lax", "Path=/", "Max-Age=2592000"] {
            assert!(cookie.contains(attr), "{attr}: {cookie}");
        }
        // HTTP で待ち受けるので、Secure を付けるとブラウザが送らない
        assert!(!cookie.contains("Secure"), "{cookie}");
        let res = server
            .client
            .get(format!("{}/", server.base))
            .header(reqwest::header::COOKIE, format!("{SESSION_COOKIE}={token}"))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status().as_u16(), 200);
    }

    /// 失敗の応答は、ID が無い・パスワードが違う・パスワードが無い・長すぎる、のどれでも同じ。
    #[tokio::test]
    async fn every_login_failure_looks_the_same() {
        let db = Db::open_in_memory().unwrap();
        let login_id = with_password(&db);
        other_user(&db, "nopassword@example.com");
        let server = Server::anonymous(db).await;
        let long = "a".repeat(1025);
        let mut responses = Vec::new();
        for (id, password) in [
            (login_id, "wrong password!"),
            ("nobody@example.com", PASSWORD),
            ("nopassword@example.com", PASSWORD),
            (login_id, long.as_str()),
        ] {
            let res = login(&server, form(&[("login", id), ("password", password)])).await;
            assert!(res.headers().get(reqwest::header::SET_COOKIE).is_none());
            responses.push((res.status().as_u16(), res.text().await.unwrap()));
        }
        assert_eq!(responses[0].0, 401);
        assert!(
            responses.iter().all(|r| *r == responses[0]),
            "{responses:?}"
        );
        assert_eq!(server.count("SELECT count(*) FROM sessions"), 0);
    }

    /// 戻り先がほかのホストを指すなら、自分のホストの / に戻す（オープンリダイレクトにしない）。
    #[tokio::test]
    async fn logins_never_return_to_another_host() {
        let db = Db::open_in_memory().unwrap();
        let login_id = with_password(&db);
        let server = Server::anonymous(db).await;
        for next in [
            "//evil.example",
            "http://evil.example/",
            "/\\evil.example",
            "/\t/evil.example",
        ] {
            let res = login(
                &server,
                form(&[("login", login_id), ("password", PASSWORD), ("next", next)]),
            )
            .await;
            assert_eq!(res.status().as_u16(), 303, "{next:?}");
            assert_eq!(
                res.headers()["location"],
                format!("{}/", server.base),
                "{next:?}"
            );
        }
    }

    /// ログアウトはセッションを消し、同じ Path で期限切れの Cookie を返す。
    #[tokio::test]
    async fn logging_out_ends_the_session() {
        let server = Server::start(Db::open_in_memory().unwrap()).await;
        let res = server.post("/logout", "").await;
        assert_eq!(res.status().as_u16(), 303);
        assert_eq!(res.headers()["location"], "/login");
        let cookie = cookie_of(&res);
        assert!(
            cookie.contains("Max-Age=0") && cookie.contains("Path=/"),
            "{cookie}"
        );
        assert_eq!(server.get_raw("/").await.status().as_u16(), 303);
    }

    /// 他サイトの Origin からの POST は、ログインもログアウトも 403 で、セッションは作られず消えない。
    #[tokio::test]
    async fn cross_site_logins_and_logouts_are_refused() {
        let db = Db::open_in_memory().unwrap();
        let login_id = with_password(&db);
        let server = Server::start(db).await;
        let res = server
            .client
            .post(format!("{}/login", server.base))
            .header("content-type", "application/x-www-form-urlencoded")
            .header("origin", "https://evil.example")
            .body(form(&[("login", login_id), ("password", PASSWORD)]))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status().as_u16(), 403);
        let res = server
            .form("/logout", "")
            .header("origin", "https://evil.example")
            .send()
            .await
            .unwrap();
        assert_eq!(res.status().as_u16(), 403);
        assert_eq!(server.count("SELECT count(*) FROM sessions"), 1);
    }

    /// 同じ IP から 20 回を超えて失敗すると、ほかの ID でも、正しいパスワードでも、しばらくログインできない
    /// （ほかの ID での成功を挟んでも数え直さない）。
    #[tokio::test]
    async fn one_ip_failing_many_times_is_held_off() {
        let db = Db::open_in_memory().unwrap();
        let login_id = with_password(&db);
        let server = Server::anonymous(db).await;
        for n in 0..21 {
            let id = format!("nobody{n}@example.com");
            let res = login(&server, form(&[("login", &id), ("password", PASSWORD)])).await;
            assert_eq!(res.status().as_u16(), 401);
            if n == 10 {
                let ok = login(
                    &server,
                    form(&[("login", login_id), ("password", PASSWORD)]),
                )
                .await;
                assert_eq!(ok.status().as_u16(), 303, "a success does not reset the IP");
            }
        }
        let res = login(
            &server,
            form(&[("login", login_id), ("password", PASSWORD)]),
        )
        .await;
        assert_eq!(res.status().as_u16(), 401);
    }

    /// 2 人の利用者の評価は、互いの記録にならない。
    #[tokio::test]
    async fn each_user_rates_for_themselves() {
        let db = Db::open_in_memory().unwrap();
        let (id, _) = seed(&db, "https://e.com/a", "見出しA");
        let other = other_user(&db, "b@example.com");
        insert_session(&db, other, "b-session");
        let server = Server::start(db).await;
        let res = server
            .client
            .post(format!("{}/articles/{id}/rating", server.base))
            .header("content-type", "application/x-www-form-urlencoded")
            .header(
                reqwest::header::COOKIE,
                format!("{SESSION_COOKIE}=b-session"),
            )
            .body("value=5")
            .send()
            .await
            .unwrap();
        assert_eq!(res.status().as_u16(), 303);
        assert_eq!(
            server.count(&format!(
                "SELECT count(*) FROM ratings WHERE user_id = {other}"
            )),
            1
        );
        assert_eq!(
            server.count("SELECT count(*) FROM ratings WHERE user_id = (SELECT id FROM users WHERE is_owner = 1)"),
            0
        );
    }

    /// フィードはトークンの利用者のものを出し、self のリンクはトークン付きの購読用の URL。作り直すと古いトークンは 401。
    #[tokio::test]
    async fn feeds_are_read_with_the_users_token() {
        let db = Db::open_in_memory().unwrap();
        seed_recommended_and_hidden(&db);
        let server = Server::start(db).await;
        let first = server.feed_path();
        let (status, xml) = server.get(&first).await;
        assert_eq!(status, 200);
        let token = first.strip_prefix("/feed.xml?token=").unwrap();
        assert!(
            xml.contains(&format!(
                "<link rel=\"self\" href=\"{}/feed.xml?token={token}\"/>",
                server.base
            )),
            "{xml}"
        );
        // 設定画面に購読用の URL が出る
        let (_, html) = server.get("/settings").await;
        assert!(html.contains(&format!("/feed.xml?token={token}")), "{html}");
        let res = server.post("/settings/feed-token", "").await;
        assert_eq!(res.status().as_u16(), 303);
        assert_eq!(server.get_raw(&first).await.status().as_u16(), 401);
        assert_eq!(
            server.get_raw("/feed.xml?token=").await.status().as_u16(),
            401
        );
    }

    /// IP ごとの失敗は、ポートを除いたアドレスで数え、15 分途切れたら数え直す。記録の数には上限がある。
    #[test]
    fn ip_throttle_counts_by_address() {
        use std::net::{IpAddr, Ipv4Addr};
        use std::time::{Duration, Instant};
        let t0 = Instant::now();
        let a = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));
        let b = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 2));
        let throttle = IpThrottle::default();
        for n in 0..20 {
            throttle.record_failure(a, t0 + Duration::from_secs(n));
        }
        assert!(!throttle.blocked(a, t0 + Duration::from_secs(20)));
        throttle.record_failure(a, t0 + Duration::from_secs(20));
        assert!(throttle.blocked(a, t0 + Duration::from_secs(21)));
        assert!(!throttle.blocked(b, t0 + Duration::from_secs(21)));
        // 待ち時間中の試行は数えないので、延びない
        throttle.record_failure(a, t0 + Duration::from_secs(600));
        let after = t0 + Duration::from_secs(20) + Duration::from_secs(15 * 60);
        assert!(!throttle.blocked(a, after));
        // 15 分途切れたら数え直す
        throttle.record_failure(a, after);
        assert!(!throttle.blocked(a, after));

        let many = IpThrottle::default();
        for n in 0..(IP_ENTRIES_MAX as u32 + 100) {
            many.record_failure(IpAddr::V4(Ipv4Addr::from(n)), t0);
        }
        assert_eq!(many.len(), IP_ENTRIES_MAX);
    }

    /// パスワードの計算は同時に 2 つまでで、空きが無ければ待たずに断る。要求が捨てられても、
    /// 計算（と、その後の記録）は最後まで走り、終わるまで枠を返さない。
    #[tokio::test]
    async fn hashing_is_bounded_and_survives_dropped_requests() {
        let hasher = PasswordHasher::default();
        let (release, gate) = std::sync::mpsc::channel::<()>();
        let gate = std::sync::Arc::new(std::sync::Mutex::new(gate));
        let done = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut dropped = Vec::new();
        for _ in 0..2 {
            let (gate, done) = (gate.clone(), done.clone());
            let task = hasher.run(move || {
                gate.lock().unwrap().recv().unwrap();
                done.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            });
            // 走り出させてから要求を捨てる
            let mut task = Box::pin(task);
            assert!(futures_poll_once(&mut task).await);
            dropped.push(task);
        }
        drop(dropped);
        assert!(matches!(hasher.run(|| ()).await, Err(Busy)));
        release.send(()).unwrap();
        release.send(()).unwrap();
        for _ in 0..100 {
            if done.load(std::sync::atomic::Ordering::SeqCst) == 2 && hasher.available() == 2 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert_eq!(done.load(std::sync::atomic::Ordering::SeqCst), 2);
        assert!(hasher.run(|| ()).await.is_ok());
    }

    /// future を 1 回だけ進める（まだ終わっていなければ true）。
    async fn futures_poll_once<F: std::future::Future + Unpin>(f: &mut F) -> bool {
        std::future::poll_fn(|cx| {
            std::task::Poll::Ready(std::pin::Pin::new(&mut *f).poll(cx).is_pending())
        })
        .await
    }
}
