//! The SQLite plumbing both halves of [`super`] share — decision 100.
//!
//! Opening a connection, the two pragmas that make one writer and many readers
//! safe, and a migration runner. None of it is interesting; all of it is the
//! kind of thing that is written twice and then diverges, which is the whole
//! argument decision 100 made for putting `memory/` and `history/` under one
//! module.
//!
//! # What lives here and what does not
//!
//! The **schema** does not. Decision 93's rule is unchanged and it is about
//! *files*: the pages, `aria.db` and the index are three files, never one.
//! `memory/` and `history/` share this plumbing, **not a database** — not the
//! same connection, not the same schema, not the same lifetime.
//!
//! # Why `user_version` and not a migrations table
//!
//! SQLite carries a four-byte integer in its own header for exactly this. A
//! table would need a migration to create, which is a chicken-and-egg problem
//! solved by a pragma that is already there. The cost is that migrations are an
//! ordered list and can never be reordered or removed — which is true of
//! migrations anyway, and is better stated than discovered.

use std::path::Path;
use std::time::Duration;

use rusqlite::Connection;

/// How long a statement waits for the writer before giving up.
///
/// Generous because the competing writer is us: a memory write and a turn
/// landing in the history are milliseconds apart at worst, and a busy error
/// surfaced to a voice turn would be a sentence lost to a lock.
pub const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// Open a database file, creating it and its parent if need be.
///
/// WAL and `busy_timeout` are decision 91's single-writer queue in the form
/// SQLite already provides; `foreign_keys` is off by default in SQLite and has
/// to be asked for on **every** connection, which is the kind of fact that is
/// known once and forgotten everywhere else.
pub fn open(path: &Path) -> rusqlite::Result<Connection> {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let db = Connection::open(path)?;
    tune(&db)?;
    Ok(db)
}

/// Open a database that exists only for this process.
///
/// What the tests use. WAL is meaningless in memory and SQLite says so by
/// refusing it quietly, so only the rest is applied.
pub fn open_in_memory() -> rusqlite::Result<Connection> {
    let db = Connection::open_in_memory()?;
    db.pragma_update(None, "foreign_keys", "ON")?;
    Ok(db)
}

fn tune(db: &Connection) -> rusqlite::Result<()> {
    db.pragma_update(None, "journal_mode", "WAL")?;
    db.busy_timeout(BUSY_TIMEOUT)?;
    db.pragma_update(None, "foreign_keys", "ON")?;
    Ok(())
}

/// Bring a connection up to the last step of `steps`.
///
/// Each entry is one version. A database at version `n` runs every step from
/// `n` onwards, each in its own transaction, so a failure halfway leaves the
/// version it had rather than half of the one it wanted.
///
/// Returns the version it ended on.
pub fn migrate(db: &mut Connection, steps: &[&str]) -> rusqlite::Result<usize> {
    let current: usize = db.pragma_query_value(None, "user_version", |row| row.get(0))?;
    // A file written by a newer build. Refusing is the only honest answer: the
    // schema it holds is not one this binary knows how to read, and running
    // ahead of it would corrupt an audit nothing may lose.
    if current > steps.len() {
        return Err(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_ERROR),
            Some(format!(
                "la base est en version {current} et cette version d'ARIA n'en connaît que \
                 {} : elle a été écrite par une version plus récente",
                steps.len()
            )),
        ));
    }
    for (index, step) in steps.iter().enumerate().skip(current) {
        let tx = db.transaction()?;
        tx.execute_batch(step)?;
        // Not a bound parameter: SQLite does not accept one in a pragma.
        tx.pragma_update(None, "user_version", index + 1)?;
        tx.commit()?;
    }
    Ok(steps.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    const STEPS: [&str; 2] = [
        "CREATE TABLE a(x INTEGER);",
        "CREATE TABLE b(y INTEGER);",
    ];

    fn version(db: &Connection) -> usize {
        db.pragma_query_value(None, "user_version", |row| row.get(0))
            .expect("user_version")
    }

    #[test]
    fn a_fresh_database_runs_every_step() {
        let mut db = open_in_memory().expect("open");
        assert_eq!(migrate(&mut db, &STEPS).expect("migrate"), 2);
        assert_eq!(version(&db), 2);
        db.execute_batch("INSERT INTO a VALUES (1); INSERT INTO b VALUES (2);")
            .expect("both tables exist");
    }

    #[test]
    fn migrating_twice_does_nothing_the_second_time() {
        let mut db = open_in_memory().expect("open");
        migrate(&mut db, &STEPS).expect("first");
        // The steps are not idempotent — `CREATE TABLE` without `IF NOT EXISTS`
        // — which is deliberate here: it is what makes this test prove that the
        // version, and not luck, is what stops them running again.
        migrate(&mut db, &STEPS).expect("second");
        assert_eq!(version(&db), 2);
    }

    #[test]
    fn a_later_step_runs_on_an_older_database() {
        let mut db = open_in_memory().expect("open");
        migrate(&mut db, &STEPS[..1]).expect("first");
        assert_eq!(version(&db), 1);
        migrate(&mut db, &STEPS).expect("second");
        assert_eq!(version(&db), 2);
        db.execute_batch("INSERT INTO b VALUES (2);").expect("b arrived");
    }

    #[test]
    fn a_database_from_a_newer_build_is_refused_rather_than_run_on() {
        let mut db = open_in_memory().expect("open");
        migrate(&mut db, &STEPS).expect("migrate");
        let refused = migrate(&mut db, &STEPS[..1]);
        assert!(refused.is_err(), "une base en avance doit être refusée, pas rattrapée");
        assert_eq!(version(&db), 2, "et laissée telle quelle");
    }

    #[test]
    fn a_failing_step_leaves_the_version_it_had() {
        let mut db = open_in_memory().expect("open");
        migrate(&mut db, &STEPS).expect("migrate");
        let broken = [STEPS[0], STEPS[1], "CREATE TABLE a(x INTEGER);"];
        assert!(migrate(&mut db, &broken).is_err(), "la table a existe déjà");
        assert_eq!(version(&db), 2, "la version reste celle qui a réussi");
    }

    #[test]
    fn foreign_keys_are_on_which_sqlite_does_not_do_by_itself() {
        let db = open_in_memory().expect("open");
        let on: i64 = db
            .pragma_query_value(None, "foreign_keys", |row| row.get(0))
            .expect("foreign_keys");
        assert_eq!(on, 1);
    }
}

// ----------------------------------------------------------- stamps in SQL

/// How a [`Stamp`] is written in a column.
///
/// RFC 3339 **with its offset**, because the stamp is local time and a database
/// outlives a daylight-saving change. Stored as text and not as an integer so
/// that a row stays legible to anyone who opens the file with `sqlite3`, which
/// is the only tool an audit is ever read with in anger. It also sorts
/// correctly as text for a single offset, which is the ordinary case.
pub fn stamp_to_sql(at: crate::agent::clock::Stamp) -> String {
    at.to_rfc3339()
}

/// Read one back.
///
/// A row that cannot be parsed yields `None` rather than failing the query: a
/// corrupt timestamp must not make a whole conversation unreadable, and the
/// caller decides what an entry with no time is worth.
pub fn stamp_from_sql(text: &str) -> Option<crate::agent::clock::Stamp> {
    chrono::DateTime::parse_from_rfc3339(text)
        .ok()
        .map(|at| at.with_timezone(&chrono::Local))
}

#[cfg(test)]
mod stamp_tests {
    use super::*;

    #[test]
    fn a_stamp_survives_the_round_trip_to_the_second() {
        let at = crate::agent::clock::now();
        let back = stamp_from_sql(&stamp_to_sql(at)).expect("parse");
        assert_eq!(back.timestamp(), at.timestamp());
    }

    #[test]
    fn a_corrupt_stamp_is_none_and_not_a_panic() {
        assert!(stamp_from_sql("hier soir").is_none());
        assert!(stamp_from_sql("").is_none());
    }
}
