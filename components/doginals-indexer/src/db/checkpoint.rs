use std::path::PathBuf;

use config::Config;
use redb::{Database, ReadableTable, TableDefinition};

const CHECKPOINT_TABLE: TableDefinition<&str, u64> = TableDefinition::new("checkpoint");
const INDEXER_KEY: &str = "doginals:last_height";

fn checkpoint_path(config: &Config) -> PathBuf {
    config
        .expected_cache_path()
        .join("doginals-checkpoint.redb")
}

pub fn write_checkpoint(config: &Config, height: u64) -> Result<(), String> {
    std::fs::create_dir_all(config.expected_cache_path())
        .map_err(|e| format!("checkpoint create_dir_all: {e}"))?;
    let db =
        Database::create(checkpoint_path(config)).map_err(|e| format!("checkpoint open: {e}"))?;
    let write_txn = db
        .begin_write()
        .map_err(|e| format!("checkpoint begin_write: {e}"))?;
    {
        let mut table = write_txn
            .open_table(CHECKPOINT_TABLE)
            .map_err(|e| format!("checkpoint open_table: {e}"))?;
        table
            .insert(INDEXER_KEY, height)
            .map_err(|e| format!("checkpoint insert: {e}"))?;
    }
    write_txn
        .commit()
        .map_err(|e| format!("checkpoint commit: {e}"))?;
    Ok(())
}

pub fn read_checkpoint(config: &Config) -> Result<Option<u64>, String> {
    let path = checkpoint_path(config);
    if !path.exists() {
        return Ok(None);
    }
    let db = Database::create(path).map_err(|e| format!("checkpoint open: {e}"))?;
    let read_txn = db
        .begin_read()
        .map_err(|e| format!("checkpoint begin_read: {e}"))?;
    let table = match read_txn.open_table(CHECKPOINT_TABLE) {
        Ok(table) => table,
        Err(_) => return Ok(None),
    };
    Ok(table
        .get(INDEXER_KEY)
        .map_err(|e| format!("checkpoint get: {e}"))?
        .map(|v| v.value()))
}
