//! Standalone `vcad-mcp` binary (same as `vcad mcp`).

fn main() {
    let opts = match vcad_mcp::cli::parse_args(std::env::args().skip(1)) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
    let server = if std::env::var_os("VCAD_MCP_TEST_UPSTREAM").is_some() {
        // Test hook: act as the "node" upstream in the proxy round-trip test,
        // serving one tool the core doesn't have.
        let s = vcad_mcp::cli::headless_server(&vcad_mcp::cli::McpOptions {
            no_proxy: true,
            ..opts.clone()
        });
        s.register(vcad_mcp::test_support::upstream_echo_tool());
        s.register(vcad_mcp::test_support::upstream_touch_tool());
        Some(s)
    } else {
        None
    };
    let result = match server {
        Some(s) => {
            vcad_mcp::transport::run_stdio(&s);
            Ok(())
        }
        None => vcad_mcp::cli::run(opts),
    };
    if let Err(e) = result {
        eprintln!("vcad-mcp: {e}");
        std::process::exit(1);
    }
}
