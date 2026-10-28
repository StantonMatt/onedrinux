//! A transaction-safe, connection-local change feed for derived mount state. TEMP
//! triggers cover every writer (including listing sweeps and cascading deletes), and
//! roll back with its transaction. Nothing is persisted and no schema upgrade is needed.
use std::collections::BTreeSet;

use crate::{ItemReader, LocalItemId, TreeError, TreeStore};

#[derive(Default)]
pub struct ItemChanges {
    pub items: BTreeSet<LocalItemId>,
    /// Pin changes can affect descendants, including exceptions removed by a folder pin.
    pub pin_roots: BTreeSet<LocalItemId>,
}

pub(crate) type ItemObserver = Box<dyn Fn(&mut TreeStore) -> Result<(), TreeError> + Send>;

pub(crate) fn install(
    store: &mut TreeStore,
    observer: impl Fn(&ItemReader<'_>, ItemChanges) -> Result<(), TreeError> + Send + 'static,
) -> Result<ItemObserver, TreeError> {
    store.conn.execute_batch("CREATE TEMP TABLE IF NOT EXISTS mount_item_changes (id INTEGER PRIMARY KEY, pin INTEGER NOT NULL DEFAULT 0);")?;
    // Pins are keyed by remote id, whereas the feed is keyed by local id. Keep a
    // temporary indexed mapping so setting one pin never scans a 100k-row store.
    store.conn.execute_batch(
        "CREATE TEMP TABLE IF NOT EXISTS mount_item_bindings (id INTEGER PRIMARY KEY, binding TEXT NOT NULL);
        CREATE INDEX IF NOT EXISTS mount_binding_id ON mount_item_bindings(binding);
        INSERT OR REPLACE INTO mount_item_bindings SELECT local_item_id, onedrive_item_id FROM items WHERE onedrive_item_id IS NOT NULL;
        CREATE TEMP TRIGGER IF NOT EXISTS mount_binding_insert AFTER INSERT ON main.items WHEN NEW.onedrive_item_id IS NOT NULL BEGIN
            INSERT OR REPLACE INTO mount_item_bindings VALUES(NEW.local_item_id, NEW.onedrive_item_id); END;
        CREATE TEMP TRIGGER IF NOT EXISTS mount_binding_update AFTER UPDATE OF onedrive_item_id ON main.items BEGIN
            DELETE FROM mount_item_bindings WHERE id=NEW.local_item_id;
            INSERT INTO mount_item_bindings SELECT NEW.local_item_id, NEW.onedrive_item_id WHERE NEW.onedrive_item_id IS NOT NULL; END;
        CREATE TEMP TRIGGER IF NOT EXISTS mount_binding_delete AFTER DELETE ON main.items BEGIN
            DELETE FROM mount_item_bindings WHERE id=OLD.local_item_id; END;",
    )?;
    for table in ["items", "step_memory"] {
        for (event, row) in [("INSERT", "NEW"), ("UPDATE", "NEW"), ("DELETE", "OLD")] {
            store.conn.execute_batch(&format!(
                "CREATE TEMP TRIGGER IF NOT EXISTS mount_{table}_{event} AFTER {event} ON main.{table} BEGIN
                INSERT OR IGNORE INTO mount_item_changes(id) VALUES ({row}.local_item_id); END;"
            ))?;
        }
    }
    for (event, row) in [("INSERT", "NEW"), ("UPDATE", "NEW"), ("DELETE", "OLD")] {
        store.conn.execute_batch(&format!(
            "CREATE TEMP TRIGGER IF NOT EXISTS mount_pins_{event} AFTER {event} ON main.pins BEGIN
            INSERT INTO mount_item_changes(id, pin) SELECT id, 1 FROM mount_item_bindings WHERE binding = {row}.id
            ON CONFLICT(id) DO UPDATE SET pin = 1; END;"
        ))?;
    }
    // Registration and the initial snapshot are one writer job: no lost changes.
    store.conn.execute_batch("INSERT OR IGNORE INTO mount_item_changes(id) SELECT local_item_id FROM items;")?;
    let flush: ItemObserver = Box::new(move |store| {
        let changes = {
            let mut changes = ItemChanges::default();
            let mut query = store.conn.prepare_cached("SELECT id, pin FROM mount_item_changes")?;
            for row in query.query_map([], |row| Ok((LocalItemId::new(row.get(0)?), row.get::<_, bool>(1)?)))? {
                let (item, pin) = row?;
                changes.items.insert(item);
                if pin {
                    changes.pin_roots.insert(item);
                }
            }
            changes
        };
        if changes.items.is_empty() {
            return Ok(());
        }
        store.read_items(|reader| observer(reader, changes))?;
        store.conn.execute_batch("DELETE FROM mount_item_changes;")?;
        Ok(())
    });
    flush(store)?;
    Ok(flush)
}

#[cfg(test)]
mod tests;
