//! Account-scoped outgoing holds, with no implicit approval on enable/disable.
use crate::{cli::ChangesCmd, daemon::Daemon};
use konedrive_dbus::change_guard::ChangeGuardProxy;
use std::collections::BTreeMap;
use zbus::proxy::CacheProperties;

pub(crate) async fn changes(
    daemon: &Daemon,
    option: Option<&str>,
    command: ChangesCmd,
) -> anyhow::Result<()> {
    let connection = if matches!(&command, ChangesCmd::Restore { .. }) {
        daemon.unlimited_connection().await?
    } else {
        &daemon.connection
    };
    let guard = ChangeGuardProxy::builder(connection)
        .cache_properties(CacheProperties::No)
        .build()
        .await?;
    let account = if matches!(
        &command,
        ChangesCmd::Enable | ChangesCmd::Disable | ChangesCmd::Thresholds { .. }
    ) {
        String::new()
    } else {
        daemon.chosen(option).await?.account.id
    };
    let account = account.as_str();
    match command {
        ChangesCmd::Status => {
            let (enabled, limits) = guard.get_settings().await?;
            println!("Change guard: {}", if enabled { "on" } else { "off" });
            for (key, value) in limits {
                println!("{key}: {value}");
            }
            let (_, rows) = guard.list_held(account).await?;
            let count = |sent| {
                rows.iter()
                    .filter(|r| r.sent == sent)
                    .map(|r| {
                        1 + if r.kind == "removal" || r.kind == "upload" {
                            r.descendants
                        } else {
                            0
                        }
                    })
                    .sum::<u64>()
            };
            println!("Held: {}", count(false));
            println!("Already sent (recent): {}", count(true));
            let error = guard.get_error(account).await?;
            if !error.is_empty() {
                anyhow::bail!(error);
            }
        }
        ChangesCmd::Held { sent } => {
            let (token, rows) = guard.list_held(account).await?;
            if !sent {
                println!("Token: {token}");
            }
            for row in rows.into_iter().filter(|r| r.sent == sent) {
                println!(
                    "{}\t{}\t{}\t{} bytes → {}\t{}\t{}\tdirectory={} descendants={} old={}",
                    row.item,
                    row.kind,
                    row.path,
                    row.bytes,
                    row.after_bytes,
                    row.since,
                    row.state,
                    row.directory,
                    row.descendants,
                    row.old_path
                );
            }
        }
        ChangesCmd::Allow { token, kinds } => {
            print_result(guard.allow_held(account, &token, &kinds).await?)?
        }
        ChangesCmd::Restore { token, kinds } => {
            print_result(guard.restore_held(account, &token, &kinds).await?)?
        }
        ChangesCmd::Upload { path } => print_result(guard.upload_local(account, &path).await?)?,
        ChangesCmd::Enable => guard.set_enabled(true).await?,
        ChangesCmd::Disable => guard.set_enabled(false).await?,
        ChangesCmd::Thresholds { values } => {
            if values.is_empty() {
                let (_, limits) = guard.get_settings().await?;
                for (key, value) in limits {
                    println!("{key}={value}");
                }
            } else {
                let parsed = values
                    .iter()
                    .map(|v| {
                        let (key, value) = v
                            .split_once('=')
                            .ok_or_else(|| anyhow::anyhow!("expected NAME=VALUE: {v}"))?;
                        Ok((key.to_owned(), value.parse::<u64>()?))
                    })
                    .collect::<anyhow::Result<BTreeMap<_, _>>>()?;
                guard.set_thresholds(parsed).await?;
            }
        }
    }
    Ok(())
}

fn print_result(result: konedrive_dbus::rows::GuardResult) -> anyhow::Result<()> {
    println!(
        "done={} pending-offline={} failed={} kept-both-versions={}",
        result.done, result.pending_offline, result.failed, result.kept_both_versions
    );
    for (_, path, reason) in result.failures {
        eprintln!("{path}: {reason}");
    }
    anyhow::ensure!(result.failed == 0, "some changes remain on hold");
    Ok(())
}
