use std::fs;
use std::path::Path;
use std::time::Duration;

use rusqlite::{types::ValueRef, Connection, Params, Transaction, TransactionBehavior};

use crate::Result;

pub(crate) struct SqliteConnection {
    connection: Connection,
}

#[derive(Default)]
pub(crate) struct QueryResult {
    pub(crate) rows: Vec<Vec<Option<String>>>,
}

impl SqliteConnection {
    pub(crate) fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let connection = Connection::open(path)?;
        connection.busy_timeout(Duration::from_secs(5))?;
        Ok(Self { connection })
    }

    pub(crate) fn exec(&self, sql: &str) -> Result<()> {
        self.connection.execute_batch(sql)?;
        Ok(())
    }

    pub(crate) fn execute(&self, sql: &str, params: impl Params) -> Result<()> {
        self.connection.prepare_cached(sql)?.execute(params)?;
        Ok(())
    }

    pub(crate) fn transaction(&self, write: impl FnOnce(&Self) -> Result<()>) -> Result<()> {
        let transaction =
            Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        write(self)?;
        transaction.commit()?;
        Ok(())
    }

    pub(crate) fn query(&self, sql: &str, params: impl Params) -> Result<QueryResult> {
        let mut statement = self.connection.prepare_cached(sql)?;
        let column_count = statement.column_count();
        let mut rows = statement.query(params)?;
        let mut result = QueryResult::default();
        while let Some(row) = rows.next()? {
            let mut values = Vec::with_capacity(column_count);
            for index in 0..column_count {
                values.push(match row.get_ref(index)? {
                    ValueRef::Null => None,
                    ValueRef::Integer(value) => Some(value.to_string()),
                    // Keep SQLite's own numeric text format used by the old CLI.
                    ValueRef::Real(value) => Some(self.connection.query_row(
                        "SELECT CAST(?1 AS TEXT)",
                        [value],
                        |row| row.get(0),
                    )?),
                    ValueRef::Text(value) | ValueRef::Blob(value) => {
                        Some(String::from_utf8_lossy(value).into_owned())
                    }
                });
            }
            result.rows.push(values);
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::params;

    #[test]
    fn bindings_preserve_quotes_separators_nulls_and_numbers() {
        let db = SqliteConnection::open(Path::new(":memory:")).unwrap();
        db.exec("CREATE TABLE sample (text TEXT, optional TEXT, confidence REAL);")
            .unwrap();
        let text =
            "O'Brien'); DROP TABLE sample;-- \"合成\"\n\0\u{1e}\u{1f}__HARNESSKIT_SQLITE_NULL__";
        db.execute(
            "INSERT INTO sample VALUES (?1, ?2, ?3);",
            params![text, None::<String>, 1.0],
        )
        .unwrap();
        let result = db
            .query("SELECT * FROM sample WHERE text = ?1;", [text])
            .unwrap();
        assert_eq!(
            result.rows,
            vec![vec![Some(text.to_string()), None, Some("1.0".to_string())]]
        );
        db.execute(
            "INSERT INTO sample VALUES (?1, ?2, ?3);",
            params!["", "__HARNESSKIT_SQLITE_NULL__", 0.95],
        )
        .unwrap();
        assert_eq!(
            db.query("SELECT * FROM sample WHERE text = '';", [])
                .unwrap()
                .rows,
            vec![vec![
                Some(String::new()),
                Some("__HARNESSKIT_SQLITE_NULL__".to_string()),
                Some("0.95".to_string())
            ]]
        );
    }

    #[test]
    fn failed_transaction_rolls_back_and_connection_can_be_reused() {
        let db = SqliteConnection::open(Path::new(":memory:")).unwrap();
        db.exec("CREATE TABLE sample (id INTEGER PRIMARY KEY);")
            .unwrap();
        let failed = db.transaction(|db| {
            db.execute("INSERT INTO sample VALUES (?1);", [1])?;
            db.execute("INSERT INTO sample VALUES (?1);", [1])
        });
        assert!(failed.is_err());
        assert!(db
            .query("SELECT * FROM sample;", [])
            .unwrap()
            .rows
            .is_empty());
        db.transaction(|db| db.execute("INSERT INTO sample VALUES (?1);", [2]))
            .unwrap();
        assert_eq!(
            db.query("SELECT * FROM sample;", []).unwrap().rows,
            vec![vec![Some("2".to_string())]]
        );
    }
}
