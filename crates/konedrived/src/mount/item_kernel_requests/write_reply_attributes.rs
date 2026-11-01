//! Attributes assembled through the writer for replies to write operations.

use super::*;

/// The attributes a write operation's reply carries, made on the write-operation thread
/// once the operation wrote its row: of the item mkdir, mknod and create made, and of the
/// node a truncation or a time was set on.
///
/// **State owned:** none. It reads the row and the path through the writer
/// (`Store::call_blocking`), as every job of that thread reads, and hands both to
/// `ItemLookup`, whose code makes the attributes for the reading requests too: one code
/// for what getattr answers and for what a reply carries.
pub(super) struct WriteReplyAttributes {
    pub(super) store: Store,
    pub(super) lookup: Arc<ItemLookup>,
}

impl WriteReplyAttributes {
    /// The path of the local version of `node`, read through the writer.
    fn find_path(&self, node: LocalItemId) -> Result<Option<PathBuf>, TreeError> {
        self.store.call_blocking(move |store| store.item_store().reader().find_path(node))
    }

    /// The attributes of `item`, a row the folder shows; `ESTALE` for one it does not.
    pub(super) fn describe(&self, item: Item) -> Result<ShownEntry, Errno> {
        self.lookup.describe_through(item, &|node| self.find_path(node))
    }

    /// The attributes of `node` after a truncation or a time set on it, as getattr
    /// answers them (`ItemLookup::describe_node`): of its row when the folder shows it;
    /// of the file `fh` holds when the row is gone or shows nothing and the handle is
    /// that node's (`docs/design/mount.md` §4.1); `ESTALE` otherwise.
    pub(super) fn find_attributes(&self, node: LocalItemId, fh: Option<Fh>) -> Result<FileAttr, Errno> {
        let row = self.store.call_blocking(move |store| store.item_store().reader().find_item(node)).map_err(|error| {
            tracing::warn!("the row of item {node} could not be read for the attributes of a reply: {error}");
            Errno::EIO
        })?;
        self.lookup.describe_node(to_node_row(row), node, fh, &|node| self.find_path(node))
    }
}
