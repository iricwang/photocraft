#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "../codex_bridge/mod.rs"]
mod codex_bridge;

fn main() {
    match run() {
        Ok(code) => std::process::exit(code),
        Err(message) => {
            eprintln!("PhotoCraft Codex connection: {message}");
            std::process::exit(1);
        }
    }
}

fn run() -> Result<i32, String> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let mode = args.first().and_then(|s| s.to_str()).unwrap_or("");
    if matches!(mode, "--canvas-mcp-live" | "--asset-mcp-live") {
        let path = args.get(1).map(std::path::PathBuf::from).ok_or("Missing live context path")?;
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|e| e.to_string())?;
        if mode == "--canvas-mcp-live" {
            runtime.block_on(photocraft_automation::canvas_mcp::serve_live(path))?;
        } else {
            runtime.block_on(photocraft_automation::asset_mcp::serve_live(path))?;
        }
        return Ok(0);
    }
    codex_bridge::run(args)
}
