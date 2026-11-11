//! Prints the network's fee settings: which token pays fees, and the base fee.
//!
//! Run: `cargo run --release --example 00_network_info`

use miden_by_example::testnet_client;
use miden_client::address::NetworkId;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut client = testnet_client().await?;
    client.sync_state().await?;

    let header = client.get_latest_block_header().await?;
    let config = client
        .get_protocol_config(header.protocol_config_commitment())
        .await?;
    let fee_faucet = config.fee_asset_id().faucet_id();

    println!("block:                 {}", header.block_num());
    println!("fee token faucet:      {fee_faucet}");
    println!(
        "fee token (bech32):    {}",
        fee_faucet.to_bech32(NetworkId::Testnet)
    );
    println!(
        "verification base fee: {}",
        header.fee_parameters().verification_base_fee()
    );
    Ok(())
}
