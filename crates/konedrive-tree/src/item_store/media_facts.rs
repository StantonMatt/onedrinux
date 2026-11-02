//! Version-bound media facts from accepted listings, including unchanged versions.
//!
//! **State owned:** one compact allowlisted JSON value per file, bound to its drive,
//! item id, eTag and six content columns. Replaced or cleared by every accepted listing;
//! removed with the item or its binding, or transferred with an unchanged binding.
//! Reading is one primary-key lookup with an
//! indexed join to items: stale content is never answered, including after a step.
//!
//! **Answers for:** the mount's user.konedrive.media xattr, without network or file I/O.
//!
//! **Tested without the rest:**
// test: media_facts_replace_clear_delete_and_reject_stale_content
// test: unchanged_full_listing_backfills_media_without_changing_rows_or_pins
// test: media_facts_follow_the_conflict_binding_to_its_cloud_only_successor
// test: media_facts_follow_merge_and_take_over_without_refreshing_stale_fences
// test: media_facts_follow_both_bindings_in_an_atomic_save
// test: media_facts_binding_transfer_rolls_back_with_its_rows
use rusqlite::{params_from_iter, types::Value, Connection, OptionalExtension};

use super::item_row::{content_values, CONTENT_COLUMNS};
use super::ItemReader;
use crate::{ContentVersion, LocalItemId, MediaFacts, OneDriveItemReference, TreeError};

/// Shared by fresh stores and the additive 32 to 33 migration.
pub(crate) fn schema() -> String {
    "CREATE TABLE media_facts (
         local_item_id INTEGER PRIMARY KEY, drive_id TEXT NOT NULL, item_id TEXT NOT NULL,
         etag TEXT, size INTEGER NOT NULL, mtime INTEGER, mtime_nsec INTEGER,
         hash TEXT, ctag TEXT, change_count INTEGER, json TEXT NOT NULL);
     CREATE TRIGGER media_facts_item_gone AFTER DELETE ON items
         BEGIN DELETE FROM media_facts WHERE local_item_id = OLD.local_item_id; END;
     CREATE TRIGGER media_facts_binding_gone AFTER UPDATE ON items
         WHEN NEW.onedrive_item_id IS NOT OLD.onedrive_item_id
           OR NEW.onedrive_drive_id IS NOT OLD.onedrive_drive_id
           OR NEW.onedrive_parent_id IS NULL
         BEGIN DELETE FROM media_facts WHERE local_item_id = OLD.local_item_id; END;".into()
}

/// Called only after a listing passed the outdated/refused checks, in its transaction.
pub(super) fn write(conn: &Connection, item: LocalItemId, reference: &OneDriveItemReference, etag: Option<&str>, content: Option<&ContentVersion>, facts: Option<&MediaFacts>) -> Result<(), TreeError> {
    let json = facts.filter(|_| content.is_some()).map(|facts| {
        // Every allowed field has a JSON representation (Number excludes nonfinite floats).
        serde_json::to_string(facts).expect("media facts serialize")
    });
    let Some(json) = json else {
        conn.prepare_cached("DELETE FROM media_facts WHERE local_item_id = ?1")?.execute([item.as_i64()])?;
        return Ok(());
    };
    let mut values = vec![Value::Integer(item.as_i64()), Value::Text(reference.drive_id.as_str().into()), Value::Text(reference.item_id.as_str().to_owned()), etag.map(|etag| Value::Text(etag.into())).unwrap_or(Value::Null)];
    values.extend(content_values(content));
    values.push(Value::Text(json));
    insert(conn, values)
}

fn insert(conn: &Connection, values: Vec<Value>) -> Result<(), TreeError> {
    conn.prepare_cached("INSERT OR REPLACE INTO media_facts VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)")?.execute(params_from_iter(values))?;
    Ok(())
}

/// A binding transfer saves the original facts before the release/delete triggers run,
/// then restores them on the successor in the SAME caller-owned transaction. Retaining
/// every original fence, even a stale one, never blesses facts for different content.
pub(super) struct BindingFacts(Vec<Value>);

impl BindingFacts {
    pub(super) fn read(conn: &Connection, item: LocalItemId) -> Result<Option<Self>, TreeError> {
        Ok(conn.prepare_cached("SELECT * FROM media_facts WHERE local_item_id = ?1")?
            .query_row([item.as_i64()], |row| (0..11).map(|column| row.get(column)).collect::<rusqlite::Result<Vec<Value>>>().map(Self)).optional()?)
    }

    pub(super) fn write_to(mut self, conn: &Connection, item: LocalItemId) -> Result<(), TreeError> {
        self.0[0] = Value::Integer(item.as_i64());
        insert(conn, self.0)
    }
}

impl ItemReader<'_> {
    /// Compact v1 JSON while the binding, eTag and complete OneDrive content still match
    /// the listing that delivered it. Presence on disk is the mount's separate decision.
    pub fn find_media_facts(&self, item: LocalItemId) -> Result<Option<String>, TreeError> {
        let same_content = CONTENT_COLUMNS.map(|column| format!("i.onedrive_{column} IS f.{column}")).join(" AND ");
        let sql = format!("SELECT f.json FROM media_facts f JOIN items i USING (local_item_id)
            WHERE f.local_item_id = ?1 AND i.kind = 'file' AND i.onedrive_parent_id IS NOT NULL
              AND i.onedrive_drive_id = f.drive_id AND i.onedrive_item_id = f.item_id
              AND i.onedrive_etag IS f.etag AND {same_content}");
        Ok(self.conn().prepare_cached(&sql)?.query_row([item.as_i64()], |row| row.get(0)).optional()?)
    }
}

#[cfg(test)]
mod tests;
