use crate::{cli::GuardCmd, daemon::Daemon};
use konedrive_dbus::download_guard::DownloadGuardProxy;
use zbus::proxy::CacheProperties;

pub(crate) async fn guard(daemon: &Daemon, command: GuardCmd) -> anyhow::Result<()> {
    let guard = DownloadGuardProxy::builder(&daemon.connection).cache_properties(CacheProperties::No).build().await?;
    match command {
        GuardCmd::Status => {
            println!("Download guard: {}", if guard.enabled().await? { "on" } else { "off" });
            println!(
                "Limits: {} files, {} bytes in {} seconds",
                guard.max_files().await?,
                guard.max_bytes().await?,
                guard.window_seconds().await?
            );
        }
        GuardCmd::Apps => {
            for app in guard.list_apps().await? {
                println!(
                    "{}\t{}\t{}\t{}\t{} files\t{} bytes\t{}",
                    app.identity,
                    app.display_name,
                    app.policy,
                    if app.blocked { "blocked" } else { "seen" },
                    app.files,
                    app.bytes,
                    app.reason
                );
            }
        }
        GuardCmd::Allow { identity, always } => guard.allow_app(&identity, always).await?,
        GuardCmd::Block { identity } => guard.block_app(&identity).await?,
        GuardCmd::Free { identity } => {
            let (files, bytes) = guard.free_up_app_downloads(&identity).await?;
            println!("Freed {files} files, {bytes} bytes");
        }
        GuardCmd::Reset { identity } => guard.reset_app(&identity).await?,
        GuardCmd::Enable { choice } => guard.set_enabled(choice == "on").await?,
        GuardCmd::Thresholds { files, bytes, seconds } => guard.set_thresholds(files, bytes, seconds).await?,
    }
    Ok(())
}
