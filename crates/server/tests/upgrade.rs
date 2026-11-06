//! Upgrading a 0.2 database (migrations 0001-0004) with Jellyfin hooks and follow-up deliveries.

use std::path::Path;

use api_types::{DeliveryTrigger, HookConfig, HookPatch, SECRET_MASK};
use lineup_server::Store;
use lineup_server::hooks::MISSING_TASK_ID;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

/// A database at schema 0004, built from the repository's own migration files.
async fn v02_database(db: &Path, scratch: &Path) -> sqlx::SqlitePool {
    let old = scratch.join("migrations");
    std::fs::create_dir_all(&old).unwrap();
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations");
    for f in ["0001_init.sql", "0002_epg.sql", "0003_hooks.sql", "0004_health.sql"] {
        std::fs::copy(src.join(f), old.join(f)).unwrap();
    }
    let opts = SqliteConnectOptions::new()
        .filename(db)
        .create_if_missing(true)
        .foreign_keys(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(opts)
        .await
        .unwrap();
    sqlx::migrate::Migrator::new(old.as_path())
        .await
        .unwrap()
        .run(&pool)
        .await
        .unwrap();
    pool
}

async fn insert_hook(pool: &sqlx::SqlitePool, id: i64, kind: &str, config: &str, events: &str, enabled: bool) {
    sqlx::query(
        "INSERT INTO hooks(id, name, kind, config, events, debounce_secs, enabled, created_at, updated_at)
         VALUES(?, ?, ?, ?, ?, 30, ?, '2026-11-01T00:00:00Z', '2026-11-01T00:00:00Z')",
    )
    .bind(id)
    .bind(format!("hook{id}"))
    .bind(kind)
    .bind(config)
    .bind(events)
    .bind(enabled)
    .execute(pool)
    .await
    .unwrap();
}

async fn insert_delivery(pool: &sqlx::SqlitePool, id: i64, hook: i64, trigger: &str) {
    sqlx::query(
        "INSERT INTO hook_deliveries(id, hook_id, events, trigger, attempt, ok, http_code, error, at)
         VALUES(?, ?, '[]', ?, 1, 1, 204, NULL, '2026-11-01T00:00:00Z')",
    )
    .bind(id)
    .bind(hook)
    .bind(trigger)
    .execute(pool)
    .await
    .unwrap();
}

fn url_of(c: &HookConfig) -> &str {
    let HookConfig::Webhook { url, .. } = c;
    url
}

#[tokio::test]
async fn jellyfin_hooks_become_webhooks() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("lineup.db");
    let pool = v02_database(&db, dir.path()).await;
    let two = r#"["playlist.changed","epg.updated"]"#;
    // 1: task id, enabled. 2: task id, disabled. 3: no task id (the owner's case). 4: empty task id.
    // 5: an ordinary webhook. Base URLs with a sub-path and a trailing slash.
    insert_hook(
        &pool,
        1,
        "jellyfin",
        r#"{"kind":"jellyfin","base_url":"http://jf:8096","api_key":"k1","task_id":"abc","followup_minutes":65}"#,
        two,
        true,
    )
    .await;
    insert_hook(
        &pool,
        2,
        "jellyfin",
        r#"{"kind":"jellyfin","base_url":"http://jf/media/","api_key":"k2","task_id":" def ","followup_minutes":0}"#,
        two,
        false,
    )
    .await;
    insert_hook(
        &pool,
        3,
        "jellyfin",
        r#"{"kind":"jellyfin","base_url":"http://jf:8096","api_key":"k\"3","followup_minutes":65}"#,
        two,
        true,
    )
    .await;
    insert_hook(
        &pool,
        4,
        "jellyfin",
        r#"{"kind":"jellyfin","base_url":"http://jf:8096","api_key":"k4","task_id":"","followup_minutes":65}"#,
        two,
        true,
    )
    .await;
    let webhook = r#"{"kind":"webhook","method":"PUT","url":"http://hook/x/","headers":{"X-T":"t"}}"#;
    insert_hook(&pool, 5, "webhook", webhook, r#"["channel.unhealthy"]"#, true).await;
    insert_delivery(&pool, 10, 1, "events").await;
    insert_delivery(&pool, 11, 1, "followup").await;
    insert_delivery(&pool, 12, 1, "test").await;
    insert_delivery(&pool, 13, 5, "events").await;
    pool.close().await;

    // Upgrade, then reopen: the second open must be a no-op.
    drop(Store::open(&db).await.unwrap());
    let store = Store::open(&db).await.unwrap();

    let hooks = store.hooks_with_secrets().await.unwrap();
    assert_eq!(hooks.len(), 5);
    let h = |id: i64| hooks.iter().find(|h| h.id == id).unwrap();
    let auth = |id: i64| {
        let HookConfig::Webhook { headers, method, .. } = &h(id).config;
        assert_eq!(method, "POST");
        headers["Authorization"].clone()
    };

    assert_eq!(url_of(&h(1).config), "http://jf:8096/ScheduledTasks/Running/abc");
    assert_eq!(auth(1), r#"MediaBrowser Token="k1""#);
    assert!(h(1).enabled);
    assert_eq!(h(1).events, ["playlist.changed", "epg.updated"], "subscriptions kept");
    assert_eq!(h(1).name, "hook1");
    assert_eq!(h(1).debounce_secs, 30);

    assert_eq!(url_of(&h(2).config), "http://jf/media/ScheduledTasks/Running/def");
    assert!(!h(2).enabled, "a disabled hook stays disabled");

    for id in [3, 4] {
        assert_eq!(
            url_of(&h(id).config),
            format!("http://jf:8096/ScheduledTasks/Running/{MISSING_TASK_ID}")
        );
        assert!(!h(id).enabled, "no task id: disabled until the URL is fixed");
    }
    assert_eq!(auth(3), r#"MediaBrowser Token="k"3""#);

    assert_eq!(
        h(5).config,
        serde_json::from_str::<HookConfig>(webhook).unwrap(),
        "webhooks untouched"
    );
    assert!(h(5).enabled);

    // Delivery history without follow-ups, ids kept, still tied to its hook.
    let log = store.hook_deliveries(1, 10).await.unwrap();
    let mut seen: Vec<_> = log.iter().map(|d| (d.id, d.trigger)).collect();
    seen.sort_by_key(|(id, _)| *id);
    assert_eq!(seen, [(10, DeliveryTrigger::Events), (12, DeliveryTrigger::Test)]);
    assert_eq!(store.hook_deliveries(5, 10).await.unwrap().len(), 1);

    // The placeholder hook cannot be enabled until its URL is fixed...
    let enable = HookPatch {
        enabled: Some(true),
        ..Default::default()
    };
    assert!(store.patch_hook(3, enable.clone()).await.is_err());

    // ...and fixing it through the API (masked header sent back) keeps the token.
    let shown = store.get_hook(3).await.unwrap();
    let HookConfig::Webhook { method, headers, .. } = shown.config;
    assert_eq!(headers["Authorization"], SECRET_MASK);
    let fixed = HookConfig::Webhook {
        method,
        url: "http://jf:8096/ScheduledTasks/Running/real".into(),
        headers,
    };
    store
        .patch_hook(
            3,
            HookPatch {
                config: Some(fixed),
                enabled: Some(true),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(auth_of(&store, 3).await, r#"MediaBrowser Token="k"3""#);
    assert!(store.get_hook(3).await.unwrap().enabled);

    // Deleting a hook still removes its deliveries (the rebuilt table kept the cascade).
    store.delete_hook(1).await.unwrap();
    assert!(store.hook_deliveries(1, 10).await.unwrap_or_default().is_empty());
}

async fn auth_of(store: &Store, id: i64) -> String {
    let HookConfig::Webhook { headers, .. } = store.hook_with_secrets(id).await.unwrap().config;
    headers["Authorization"].clone()
}
