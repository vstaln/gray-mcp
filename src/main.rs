use std::io::IsTerminal;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("manifest") {
        println!("{}", serde_json::to_string(&gray_mcp::manifest())?);
        return Ok(());
    }
    let rt = tokio::runtime::Runtime::new()?;
    if gray_mcp::is_sidecar_invocation(&args, std::io::stdin().is_terminal()) {
        return rt.block_on(gray_mcp::sidecar::run());
    }
    rt.block_on(gray_mcp::cli::run(args))
}
