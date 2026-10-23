//! `kill_all` signals every live server group in the process, so it runs
//! in its own test binary where no other test has servers.

mod common;

use common::{handle, stdio, wait_gone};
use serde_json::json;

#[tokio::test]
async fn kill_all_covers_exit_paths_without_destructors() {
    let h = handle("killall", stdio(&[]));
    let r = h.call_tool("spawn_grandchild", json!({})).await.unwrap();
    let grandchild: i32 = r.text().parse().unwrap();
    let server = h.pgid().await.unwrap();
    harness_mcp::kill_all();
    assert!(wait_gone(server).await);
    assert!(wait_gone(grandchild).await);
    // The handle notices and restarts on next use.
    assert_eq!(
        h.call_tool("echo", json!({"text": "again"}))
            .await
            .unwrap()
            .text(),
        "again"
    );
    h.shutdown().await;
}
