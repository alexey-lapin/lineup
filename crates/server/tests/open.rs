//! Opening the database file: a new one is created and migrated; a newer one is refused.

use std::path::Path;

use lineup_server::{Error, Store};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

async fn pool(db: &Path) -> sqlx::SqlitePool {
    let opts = SqliteConnectOptions::new().filename(db).create_if_missing(true);
    SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(opts)
        .await
        .unwrap()
}

/// Apply `files` from `from` to the database at `db`.
async fn migrate(db: &Path, scratch: &Path, from: &Path, files: &[&str]) {
    let dir = scratch.join(format!("m{}", files.len()));
    std::fs::create_dir_all(&dir).unwrap();
    for f in files {
        std::fs::copy(from.join(f), dir.join(f)).unwrap();
    }
    let pool = pool(db).await;
    sqlx::migrate::Migrator::new(dir.as_path())
        .await
        .unwrap()
        .run(&pool)
        .await
        .unwrap();
    pool.close().await;
}

async fn ledger(db: &Path) -> Vec<i64> {
    let pool = pool(db).await;
    let v = sqlx::query_scalar("SELECT version FROM _sqlx_migrations ORDER BY version")
        .fetch_all(&pool)
        .await
        .unwrap();
    pool.close().await;
    v
}

#[tokio::test]
async fn fresh_and_reopened() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("lineup.db");
    drop(Store::open(&db).await.unwrap());
    let s = Store::open(&db).await.unwrap();
    assert_eq!(s.list_routes().await.unwrap().len(), 1, "the seeded Direct route");
    assert_eq!(ledger(&db).await, [1]);
}

/// A database with a migration this binary does not have is refused by the migrator.
#[tokio::test]
async fn newer_database() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("lineup.db");
    let src = dir.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    let ours = Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations/0001_init.sql");
    std::fs::copy(ours, src.join("0001_init.sql")).unwrap();
    std::fs::write(src.join("0002_later.sql"), "CREATE TABLE later (id INTEGER);").unwrap();
    migrate(&db, dir.path(), &src, &["0001_init.sql", "0002_later.sql"]).await;
    match Store::open(&db).await {
        Err(Error::Migrate(_)) => {}
        Err(e) => panic!("wrong error: {e}"),
        Ok(_) => panic!("opened a database newer than this binary"),
    }
}
