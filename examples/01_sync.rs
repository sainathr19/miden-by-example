//! Lesson 1: connect to testnet and sync.
//!
//! Run: `cargo run --release --example 01_sync`
//!
//! What happens:
//! 1. The client opens (or creates) its SQLite store in `data/testnet/`.
//! 2. On the very first run it downloads the genesis block header and pins it. Every later request
//!    sends that genesis commitment, so a node from another network rejects the client.
//! 3. `sync_state` asks the node what changed between the local sync height and the chain tip.
//!
//! Things to try:
//! - Run it twice. The second run is fast because the client only fetches the new blocks.
//! - Delete `data/testnet/` and run it again. Sync starts from genesis.
//! - Open `data/testnet/store.sqlite3` with `sqlite3` and run `.tables`. That is everything the
//!   client remembers.
//!
//! Code to read next to this script (in the rust-sdk clone):
//! - `crates/rust-client/src/builder.rs`, `for_testnet` and `build`
//! - `crates/rust-client/src/sync/mod.rs`, `sync_state`

use miden_by_example::{print_sync_summary, testnet_client};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut client = testnet_client().await?;

    let before = client.get_sync_height().await?;
    println!("local sync height before: {before}");

    let accounts = client.get_account_headers().await?;
    println!("tracked accounts: {}", accounts.len());

    let summary = client.sync_state().await?;
    print_sync_summary(&summary);

    Ok(())
}
