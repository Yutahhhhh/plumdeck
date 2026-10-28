//! Play import batches and items (`import_batches`, `import_items`).

use duckdb::Connection;

use super::settings::finish;
use crate::DbError;

/// main.py lifespan: a crash cannot resume an in-flight inference, so items
/// that were probing/analyzing go back to the queue and unfinished batches
/// wait paused for an explicit resume. One transaction.
pub fn reset_interrupted_imports(conn: &Connection) -> Result<(), DbError> {
    conn.execute_batch("BEGIN TRANSACTION")?;
    let result = conn
        .execute_batch(
            "UPDATE import_items SET state='queued' WHERE state IN ('probing','analyzing');
             UPDATE import_batches SET state='paused',paused=true WHERE state IN ('queued','processing','pausing');",
        )
        .map_err(DbError::from);
    finish(conn, result)
}
