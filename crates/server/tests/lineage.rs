//! lineup 0.5 opens only databases of its own schema lineage. Older databases (built here from the
//! frozen 0.3 and 0.4 migrations) are refused without being touched.

use std::path::Path;

use lineup_server::{Error, Store};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

const LEGACY: &[&str] = &[
    "0001_init.sql",
    "0002_epg.sql",
    "0003_hooks.sql",
    "0004_health.sql",
    "0005_generic_hooks.sql",
];

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

fn legacy_dir() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/migrations-0.3")
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

fn assert_refused(r: Result<Store, Error>, what: &str) {
    match r {
        Err(Error::UnsupportedDatabase(m)) => assert!(m.contains("0.4 or older"), "{what}: {m}"),
        Err(e) => panic!("{what}: wrong error: {e}"),
        Ok(_) => panic!("{what}: opened"),
    }
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

#[tokio::test]
async fn legacy_databases_are_refused_untouched() {
    for n in [1, LEGACY.len()] {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("lineup.db");
        migrate(&db, dir.path(), &legacy_dir(), &LEGACY[..n]).await;
        let before = std::fs::read(&db).unwrap();
        assert_refused(Store::open(&db).await, &format!("0.3 schema up to {}", LEGACY[n - 1]));
        assert_eq!(
            std::fs::read(&db).unwrap(),
            before,
            "the old database is left as it was"
        );
        assert_eq!(ledger(&db).await.len(), n);
    }
}

/// lineup 0.4 databases (with integrations and typed sources) are refused untouched.
#[tokio::test]
async fn databases_of_0_4_are_refused_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("lineup.db");
    let from = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/migrations-0.4");
    migrate(&db, dir.path(), &from, &["0001_init.sql"]).await;
    let before = std::fs::read(&db).unwrap();
    assert_refused(Store::open(&db).await, "0.4 schema");
    assert_eq!(
        std::fs::read(&db).unwrap(),
        before,
        "the old database is left as it was"
    );
}

#[tokio::test]
async fn untracked_tables_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("lineup.db");
    let p = pool(&db).await;
    sqlx::query("CREATE TABLE channels (id INTEGER PRIMARY KEY)")
        .execute(&p)
        .await
        .unwrap();
    p.close().await;
    match Store::open(&db).await {
        Err(Error::UnsupportedDatabase(m)) => assert!(m.contains("not a lineup 0.5 database"), "{m}"),
        Err(e) => panic!("wrong error: {e}"),
        Ok(_) => panic!("opened"),
    }
}

/// A database from a later 0.5.x with an extra migration is of this lineage: it is not called an
/// older database, but the migrator reports that it is newer than this binary.
#[tokio::test]
async fn newer_database_of_this_lineage() {
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
