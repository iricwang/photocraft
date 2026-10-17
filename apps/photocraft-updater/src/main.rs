//! Release tooling and the detached installer share the same validated Rust implementation.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]
use photocraft_updater::{Result, *};
use std::{path::Path, time::Duration};

fn run() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let arg = |n: usize| -> Result<&str> { args.get(n).map(String::as_str).ok_or_else(|| "Missing argument; run photocraft-updater help".into()) };
    match arg(0)? {
        "init" if args.len() == 5 => init(Path::new(arg(1)?), arg(2)?.parse()?, arg(3)?, arg(4)?)?,
        "inventory" if args.len() == 2 => println!("{}", serde_json::to_string_pretty(&snapshot(Path::new(arg(1)?))?)?),
        "package" if args.len() == 7 => {
            let feed = package(Path::new(arg(1)?), Path::new(arg(2)?), Path::new(arg(3)?), arg(4)?, arg(6)?)?;
            let path = Path::new(arg(5)?);
            let mut feed = if path.exists() {
                let mut previous: Feed = read_json(path)?;
                if previous.schema != 1 || previous.channel != feed.channel || previous.platform != feed.platform {
                    return Err("Existing feed belongs to another channel/platform".into());
                }
                previous.releases.retain(|r| !feed.releases.iter().any(|new| new.from == r.from && new.base_tree == r.base_tree));
                previous.latest = previous.latest.max(feed.latest);
                previous.releases.extend(feed.releases);
                previous
            } else {
                feed
            };
            feed.releases.sort_by_key(|r| (r.from, r.to));
            if feed.releases.len() > 256 {
                return Err("Feed exceeds 256 transitions; start a new channel".into());
            }
            write_json(path, &feed)?;
            println!("{}", serde_json::to_string_pretty(&feed)?);
        }
        "check" if args.len() == 2 || args.len() == 3 => {
            let app = Path::new(arg(1)?);
            let feed = args.get(2).cloned().unwrap_or(config(app)?.feed);
            println!("{}", serde_json::to_string_pretty(&check(app, &feed)?)?);
        }
        "prepare" if args.len() == 2 || args.len() == 3 => {
            let app = Path::new(arg(1)?);
            let feed = args.get(2).cloned().unwrap_or(config(app)?.feed);
            let release = check(app, &feed)?.ok_or("Already up to date")?;
            println!("{}", prepare(app, &release)?.display());
        }
        "recover" if args.len() == 2 => recover(Path::new(arg(1)?))?,
        "install" if args.len() == 2 => install(Path::new(arg(1)?), Duration::from_secs(60))?,
        "bump" if args.len() == 3 => {
            let path = Path::new(arg(1)?);
            let mut channel: serde_json::Value = read_json(path)?;
            let revision =
                channel.get("revision").and_then(serde_json::Value::as_u64).ok_or("Invalid channel revision")?.checked_add(1).ok_or("Revision overflow")?;
            channel["revision"] = revision.into();
            channel["notes"] = arg(2)?.into();
            write_json(path, &channel)?;
            println!("patch.{revision}");
        }
        "help" | "--help" | "-h" => println!(
            "PhotoCraft incremental updater (macOS bundles)\n\
init APP REVISION FEED_URL VERSION      Embed config before signing\n\
inventory APP                           Print exact baseline inventory\n\
package BASE TARGET PACKAGE URL FEED NOTES  Create changed-file package and append feed history\n\
check APP [FEED_URL]                    Find the next compatible patch\n\
prepare APP [FEED_URL]                  Download, verify, stage; prints ticket path\n\
install TICKET                         Wait for app exit, install and restart, verify health\n\
recover TICKET                         Restore a retained exact baseline\n\
bump CHANNEL_JSON NOTES                Increment the tracked release revision"
        ),
        _ => return Err("Unknown command/argument count; run photocraft-updater help".into()),
    }
    Ok(())
}
fn main() {
    if let Err(error) = run() {
        eprintln!("PhotoCraft patch: {error}");
        std::process::exit(1);
    }
}
