//! A metadata-only sample. This program has no write, content or download call.

#[path = "../token_file.rs"]
mod token_file;

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use clap::Parser;
use konedrive_graph::drive::{DeltaFrom, DeltaNext, DriveClient};
use konedrive_graph::token::StaticToken;
use url::Url;

#[derive(Parser)]
#[command(name = "konedrive-metadata-probe")]
struct Args {
    /// Explicit expected drive id. A mismatch stops before enumeration.
    #[arg(long)]
    graph_drive: String,
    /// An owner-only 0600 token file. Use a read-only token.
    #[arg(long)]
    graph_token: PathBuf,
    /// At most this many distinct files and this many folders, in delta order.
    #[arg(long, default_value_t = 100, value_parser = clap::value_parser!(u32).range(1..=10000))]
    sample: u32,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let token = token_file::read_token(&args.graph_token)?;
    let drive = DriveClient::new(Url::parse("https://graph.microsoft.com/v1.0/")?, Arc::new(StaticToken::new(token)))?;
    if drive.drive_id().await? != args.graph_drive { return Err("drive id mismatch; nothing enumerated".into()); }
    let (files, hashed, folders, tagged, ended) = sample_metadata(&drive, args.sample).await?;
    println!("files sampled: {files}; quickXorHash present: {hashed}; absent: {}", files - hashed);
    println!("folders sampled: {folders}; cTag present: {tagged}; absent: {}", folders - tagged);
    println!("sample order: delta enumeration; not random; enumeration ended: {ended}; at most 1000 pages");
    Ok(())
}

/// Only metadata enumeration lives here; no content or write methods are reachable.
async fn sample_metadata(drive: &DriveClient, sample: u32) -> Result<(u32, u32, u32, u32, bool), konedrive_graph::drive::DriveError> {
    let mut seen = HashSet::new();
    let (mut files, mut hashed, mut folders, mut tagged) = (0, 0, 0, 0);
    let mut from = DeltaFrom::Start;
    let mut ended = false;
    for _ in 0..1000 {
        let page = drive.delta(&from).await?;
        for item in page.items {
            if item.deleted.is_some() || !seen.insert(item.id) { continue; }
            if item.folder.is_some() && folders < sample {
                folders += 1;
                tagged += u32::from(item.c_tag.as_deref().is_some_and(|tag| !tag.is_empty()));
            } else if item.file.is_some() && files < sample {
                files += 1;
                hashed += u32::from(item.file.as_ref().and_then(|file| file.hashes.as_ref()).and_then(|hashes| hashes.quick_xor_hash.as_deref()).is_some_and(|hash| !hash.is_empty()));
            }
        }
        if files == sample && folders == sample {
            ended = matches!(&page.next, DeltaNext::Done(_));
            break;
        }
        match page.next {
            DeltaNext::Page(next) => from = DeltaFrom::Link(next),
            DeltaNext::Done(_) => { ended = true; break; }
        }
    }
    Ok((files, hashed, folders, tagged, ended))
}

#[cfg(test)]
#[path = "metadata-probe/tests.rs"]
mod tests;
