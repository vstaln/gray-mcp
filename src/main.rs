use std::io::IsTerminal;
use std::time::Duration;

struct StderrLog;

impl log::Log for StderrLog {
    fn enabled(&self, meta: &log::Metadata) -> bool {
        meta.level() <= log::Level::Warn
    }

    fn log(&self, record: &log::Record) {
        if self.enabled(record.metadata()) {
            eprintln!("{} {}", record.level(), record.args());
        }
    }

    fn flush(&self) {}
}

fn main() -> anyhow::Result<()> {
    static LOG: StderrLog = StderrLog;
    let _ = log::set_logger(&LOG);
    log::set_max_level(log::LevelFilter::Warn);
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("manifest") {
        println!("{}", serde_json::to_string(&gray_mcp::manifest())?);
        return Ok(());
    }
    let rt = tokio::runtime::Runtime::new()?;
    if gray_mcp::is_sidecar_invocation(&args, std::io::stdin().is_terminal()) {
        let res = rt.block_on(gray_mcp::sidecar::run());
        let code = match &res {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("gray-mcp sidecar: {e:#}");
                1
            }
        };
        rt.shutdown_timeout(Duration::from_millis(800));
        std::process::exit(code);
    }
    rt.block_on(gray_mcp::cli::run(args))
}
