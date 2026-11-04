//! `settings.providerOverrides` has to reach the outbound headers, not just the
//! settings blob: a save that only round-trips through the API but never lands
//! on a request is the failure this guards.
//!
//! The chain is three hops — settings → `get_provider_credentials` →
//! `Credentials.header_overrides` → `apply_header_overrides` — so the test walks
//! all of them against a real database.

use router_db::Db;
use router_sse::executors::executor::apply_header_overrides;
use router_sse::services::auth::get_provider_credentials;
use serde_json::json;

fn db_with_override(overrides: serde_json::Value) -> Db {
    let db = Db::open(":memory:", 1).unwrap();
    db.with_conn(router_db::migrations::ensure_schema).unwrap();
    db.write(move |tx| {
        router_db::repos::settings::update_settings(tx, json!({ "providerOverrides": overrides }))
            .map(|_| ())
    })
    .unwrap();
    db.write(|tx| {
        router_db::repos::connections::create_provider_connection(
            tx,
            &json!({
                "id": "conn-1",
                "provider": "codex",
                "authType": "oauth",
                "accessToken": "tok",
                "isActive": true,
            }),
        )
        .map(|_| ())
    })
    .unwrap();
    db
}

fn select(db: &Db) -> router_sse::credentials::Credentials {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        match get_provider_credentials(db, "codex", None, None, None).await {
            router_sse::services::auth::AccountSelection::Selected(account) => account.credentials,
            _ => panic!("expected a selected account"),
        }
    })
}

#[test]
fn a_stored_override_reaches_the_outbound_headers() {
    let db = db_with_override(json!({
        "codex": { "headers": { "version": "0.160.0", "X-Test": "abc" } }
    }));

    let credentials = select(&db);
    assert_eq!(
        credentials.header_overrides.get("version"),
        Some(&json!("0.160.0"))
    );

    // Start from what the executor would have built, then apply the override.
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert("version", "0.159.0".parse().unwrap());
    headers.insert("authorization", "Bearer tok".parse().unwrap());
    apply_header_overrides(&mut headers, &credentials.header_overrides).unwrap();

    assert_eq!(headers.get("version").unwrap(), "0.160.0");
    assert_eq!(headers.get("x-test").unwrap(), "abc");
    // Auth is never touched by the override pass.
    assert_eq!(headers.get("authorization").unwrap(), "Bearer tok");
}

#[test]
fn no_override_leaves_the_builtin_headers_alone() {
    let db = db_with_override(json!({}));
    let credentials = select(&db);
    assert!(credentials.header_overrides.is_empty());

    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert("version", "0.159.0".parse().unwrap());
    apply_header_overrides(&mut headers, &credentials.header_overrides).unwrap();
    assert_eq!(headers.get("version").unwrap(), "0.159.0");
}

#[test]
fn an_override_for_another_provider_is_not_applied() {
    let db = db_with_override(json!({
        "codebuddy-intl": { "headers": { "X-Product": "SaaS" } }
    }));
    let credentials = select(&db);
    assert!(
        credentials.header_overrides.is_empty(),
        "codex must not pick up the codebuddy override"
    );
}
