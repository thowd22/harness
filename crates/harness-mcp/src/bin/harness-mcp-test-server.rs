//! Minimal MCP server over stdio for tests. See
//! `harness_mcp::test_server` for the tools and flags.

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let code = harness_mcp::test_server::main_from_args().await;
    std::process::exit(code);
}
