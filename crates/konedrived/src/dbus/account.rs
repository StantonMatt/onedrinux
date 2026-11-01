use std::sync::Arc;

use tokio::task::JoinHandle;
use zbus::object_server::{InterfaceRef, SignalEmitter};
use zbus::zvariant::ObjectPath;
use zbus::{interface, Connection};

use crate::account::AccountService;
use crate::account::state::AccountSnapshot;
use crate::dbus::fault::Result;
#[cfg(feature = "dev-tools")]
use crate::dbus::token_export::TokenExport;

pub(crate) struct Account {
    service: Arc<AccountService>,
}

#[interface(name = "org.konedrive.Account")]
impl Account {
    async fn begin_sign_in(&self) -> Result<String> {
        Ok(self.service.begin_sign_in().await?)
    }

    async fn cancel_sign_in(&self) {
        self.service.cancel_sign_in().await;
    }

    async fn sign_out(&self) -> Result<()> {
        Ok(self.service.sign_out().await?)
    }

    async fn refresh_info(&self) {
        let service = Arc::clone(&self.service);
        tokio::spawn(async move { service.refresh_account_info().await });
    }

    /// The rules of a label (`konedrive_dbus::LABEL_RULE`); `InvalidArgs` otherwise.
    async fn set_label(&self, label: &str) -> Result<()> {
        Ok(self.service.set_label(label)?)
    }

    /// Switches the account's mode (`docs/design/accounts.md` §10); the URL of the sign-in the switch
    /// needs, empty when it needs none.
    #[zbus(out_args("sign_in_url"))]
    async fn set_mode(&self, mode: &str, force: bool) -> Result<String> {
        Ok(self.service.set_mode(mode, force).await?)
    }

    async fn set_write_scope(&self, paths: Vec<String>, #[zbus(signal_emitter)] emitter: SignalEmitter<'_>) -> Result<()> {
        self.service.set_write_scope(Some(paths))?;
        self.write_paths_changed(&emitter).await?;
        self.write_scope_enabled_changed(&emitter).await?;
        Ok(())
    }

    async fn clear_write_scope(&self, #[zbus(signal_emitter)] emitter: SignalEmitter<'_>) -> Result<()> {
        self.service.set_write_scope(None)?;
        self.write_paths_changed(&emitter).await?;
        self.write_scope_enabled_changed(&emitter).await?;
        Ok(())
    }

    /// Refusals since the previous report (not a running total). Folder is empty at
    /// the drive root; app is the download guard's stable identity, or empty if unknown.
    #[zbus(signal)]
    pub(crate) async fn write_refused(emitter: &SignalEmitter<'_>, path: &str, folder: &str, app: &str, count: u64) -> zbus::Result<()>;

    #[zbus(property)]
    async fn write_paths(&self) -> Vec<String> { self.service.write_paths().unwrap_or_default() }

    #[zbus(property)]
    async fn write_scope_enabled(&self) -> bool { self.service.write_paths().is_some() }

    #[zbus(property)]
    async fn id(&self) -> String {
        self.service.id().to_string()
    }

    #[zbus(property)]
    async fn label(&self) -> String {
        self.service.state().get().label
    }

    /// The mode the account runs in (`docs/design/accounts.md` §10), not only the one `config.toml` asks
    /// for: `LastError` says why the two differ.
    #[zbus(property)]
    async fn mode(&self) -> String {
        self.service.mode().as_str().to_owned()
    }

    /// Retained Microsoft write permission while signed in and running read-only.
    #[zbus(property)]
    async fn write_still_granted(&self) -> bool {
        self.service.state().get().write_still_granted()
    }

    #[zbus(property)]
    async fn state(&self) -> String {
        self.service.state().get().state.as_str().to_owned()
    }

    #[zbus(property)]
    async fn last_error(&self) -> String {
        self.service.state().get().published_error()
    }

    /// The user asked for read-only while changes were unsent: the account is read-write
    /// still, sends them, and switches by itself when none is left.
    #[zbus(property)]
    async fn read_only_wanted(&self) -> bool {
        self.service.state().get().read_only_wish.is_some()
    }

    /// While `ReadOnlyWanted`: how many changes are still to send before the switch. 0 otherwise.
    #[zbus(property)]
    async fn unsent_before_read_only(&self) -> u64 {
        self.service.state().get().read_only_wish.map_or(0, |wish| wish.unsent)
    }

    #[zbus(property)]
    async fn display_name(&self) -> String {
        self.service.state().get().display_name
    }

    #[zbus(property)]
    async fn email(&self) -> String {
        self.service.state().get().email
    }

    #[zbus(property)]
    async fn quota_used(&self) -> u64 {
        self.service.state().get().quota.used
    }

    #[zbus(property)]
    async fn quota_total(&self) -> u64 {
        self.service.state().get().quota.total
    }

    /// Graph's `quota.remaining` as last read, less what went up since (`crate::account::quota`).
    #[zbus(property)]
    async fn quota_remaining(&self) -> u64 {
        self.service.state().get().quota.remaining
    }

    /// Graph's `quota.state` as last read: `normal`, `nearing`, `critical`, `exceeded`.
    #[zbus(property)]
    async fn quota_state(&self) -> String {
        self.service.state().get().quota.state
    }
}

/// Serves one account's `Account` (and, in a development build, `TokenExport`) at `path`, and turns its state changes into
/// `PropertiesChanged`; the task that sends them, to stop when the account goes.
///
/// At startup this runs before the bus name is claimed (`crate::daemon::startup::serve`), and
/// after the session was restored from the wallet: a D-Bus-activated client's first call is
/// never answered from stale, pre-restore state.
pub(crate) async fn export(connection: &Connection, path: &ObjectPath<'_>, service: Arc<AccountService>) -> zbus::Result<JoinHandle<()>> {
    // Captured before the interface is on the bus (not inside the task): otherwise a state
    // change landing in between would be absorbed into this baseline instead of being
    // emitted as a PropertiesChanged signal.
    let reports = service.config().write_refusals(service.id());
    let mut changes = service.state().subscribe();
    let mut previous = changes.borrow_and_update().clone();
    let server = connection.object_server();
    server.at(path, Account { service: Arc::clone(&service) }).await?;
    #[cfg(feature = "dev-tools")]
    server.at(path, TokenExport { service: Arc::clone(&service) }).await?;
    let iface = server.interface::<_, Account>(path).await?;
    let mut refusals = reports.take_receiver().ok_or_else(|| zbus::Error::Failure("account refusal consumer already exported".into()))?;
    Ok(tokio::spawn(async move {
        let _reports = reports;
        let callers = crate::download_guard::Callers::default();
        loop {
            tokio::select! {
                changed = changes.changed() => {
                    if changed.is_err() { break; }
                    let current = changes.borrow_and_update().clone();
                    if let Err(e) = emit_changes(&iface, &previous, &current).await {
                        tracing::warn!("cannot emit PropertiesChanged: {e}");
                    }
                    previous = current;
                }
                Some((refusal, count)) = refusals.next() => {
                    let app = callers.identify(refusal.pid).map(|caller| caller.identity).unwrap_or_default();
                    if let Err(e) = Account::write_refused(iface.signal_emitter(), &refusal.path.to_string_lossy(), &refusal.folder.to_string_lossy(), &app, count).await {
                        tracing::warn!("cannot emit WriteRefused: {e}");
                    }
                }
            }
        }
    }))
}

/// Takes one account's `Account` and `TokenExport` off the bus (`Accounts.Remove`).
pub(crate) async fn unexport(connection: &Connection, path: &ObjectPath<'_>, partly: bool) -> zbus::Result<()> {
    use crate::dbus::export::{all_taken_off, take_off};
    all_taken_off([
        #[cfg(feature = "dev-tools")]
        take_off::<TokenExport>(connection, path, partly).await,
        take_off::<Account>(connection, path, partly).await,
    ])
}

async fn emit_changes(
    iface: &InterfaceRef<Account>,
    old: &AccountSnapshot,
    new: &AccountSnapshot,
) -> zbus::Result<()> {
    let emitter = iface.signal_emitter();
    let account = iface.get().await;
    if old.state != new.state {
        account.state_changed(emitter).await?;
    }
    if old.published_error() != new.published_error() {
        account.last_error_changed(emitter).await?;
    }
    if old.read_only_wish.is_some() != new.read_only_wish.is_some() {
        account.read_only_wanted_changed(emitter).await?;
    }
    if old.read_only_wish.map_or(0, |wish| wish.unsent) != new.read_only_wish.map_or(0, |wish| wish.unsent) {
        account.unsent_before_read_only_changed(emitter).await?;
    }
    if old.label != new.label {
        account.label_changed(emitter).await?;
    }
    if old.display_name != new.display_name {
        account.display_name_changed(emitter).await?;
    }
    if old.email != new.email {
        account.email_changed(emitter).await?;
    }
    if old.quota.used != new.quota.used {
        account.quota_used_changed(emitter).await?;
    }
    if old.quota.total != new.quota.total {
        account.quota_total_changed(emitter).await?;
    }
    if old.quota.remaining != new.quota.remaining {
        account.quota_remaining_changed(emitter).await?;
    }
    if old.quota.state != new.quota.state {
        account.quota_state_changed(emitter).await?;
    }
    if old.write_still_granted() != new.write_still_granted() {
        account.write_still_granted_changed(emitter).await?;
    }
    if old.mode != new.mode {
        account.mode_changed(emitter).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
