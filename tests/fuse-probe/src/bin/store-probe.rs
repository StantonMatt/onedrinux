//! Offline store assertions used across the power-loss VM's two boots. The migration
//! pause is in the real migration transaction, behind a feature absent in shipped code.
use std::path::Path;

use konedrive_tree::{find_stored_version, StoredVersion, TreeStore};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    let path = Path::new(&args[2]);
    match args[1].as_str() {
        "seed31" => {
            let store = TreeStore::open(path)?;
            store.set_pin("MIGRATION-PIN", true)?;
            store.bench_sql("DROP TRIGGER media_facts_item_gone; DROP TRIGGER media_facts_binding_gone; DROP TABLE media_facts; ALTER TABLE items DROP COLUMN onedrive_folder_ctag; UPDATE meta SET value='31' WHERE key='schema_version';")?;
            store.sync_write_ahead_log()?;
        }
        "open" => {
            TreeStore::open(path)?;
        }
        "verify31" => {
            assert_eq!(
                find_stored_version(path)?,
                StoredVersion::Version("31".into())
            );
            let conn = rusqlite::Connection::open(path)?;
            let columns: i64 = conn.query_row(
                "SELECT count(*) FROM pragma_table_info('items') WHERE name='onedrive_folder_ctag'",
                [],
                |row| row.get(0),
            )?;
            assert_eq!(
                columns, 0,
                "uncommitted ALTER must roll back with the version"
            );
        }
        "verify32" => {
            let store = TreeStore::open(path)?;
            assert_eq!(store.pins()?, ["MIGRATION-PIN"]);
            assert_eq!(
                find_stored_version(path)?,
                StoredVersion::Version(konedrive_tree::SCHEMA_VERSION.into())
            );
        }
        other => panic!("unknown command {other}"),
    }
    Ok(())
}
