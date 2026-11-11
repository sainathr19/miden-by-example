//! Shared setup for the examples in `examples/`.
//!
//! Every script talks to Miden testnet through one local client. The client keeps its state in
//! `data/testnet/`:
//! - `store.sqlite3` holds your accounts, notes, transactions and the chain data you need.
//! - `keys/` holds the secret keys of your accounts.
//!
//! Delete `data/testnet/` to start over with an empty client.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use miden_client::Client;
use miden_client::account::component::BasicWallet;
use miden_client::account::{
    Account, AccountBuilder, AccountBuilderSchemaCommitmentExt, AccountId, AccountType,
};
use miden_client::address::NetworkId;
use miden_client::auth::{Approver, AuthSchemeId, AuthSecretKey, AuthSingleSig};
use miden_client::block::BlockNumber;
use miden_client::builder::ClientBuilder;
use miden_client::keystore::{FilesystemKeyStore, Keystore};
use miden_client::note::NoteConsumptionStatus;
use miden_client::store::{InputNoteRecord, TransactionFilter};
use miden_client::sync::SyncSummary;
use miden_client::transaction::{TransactionId, TransactionStatus};
use miden_client_sqlite_store::SqliteStore;

/// How often the waiting helpers sync.
const POLL_EVERY: Duration = Duration::from_secs(5);

/// The client type every script uses: the default client with keys stored as files.
pub type TestnetClient = Client<FilesystemKeyStore>;

/// Returns the folder that holds the client state.
pub fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("data")
        .join("testnet")
}

/// Returns the folder of a second, separate client ("Bob's device"), used from lesson 5 on.
pub fn bob_data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("data")
        .join("testnet-bob")
}

/// Builds a client for testnet with its state in `data/testnet/`.
///
/// `ClientBuilder::for_testnet` sets three things for you:
/// - the node RPC endpoint (`https://rpc.testnet.miden.io`),
/// - a remote transaction prover (`https://tx-prover.testnet.miden.io`),
/// - the note transport service for private notes.
///
/// You supply the store (where state lives) and the keystore (where keys live).
pub async fn testnet_client() -> anyhow::Result<TestnetClient> {
    testnet_client_at(data_dir()).await
}

/// Builds a testnet client whose store and keystore live in `dir`.
///
/// Two clients with different folders share nothing: not accounts, not notes, not keys. They act
/// like two people on two devices.
pub async fn testnet_client_at(dir: PathBuf) -> anyhow::Result<TestnetClient> {
    std::fs::create_dir_all(&dir).context("create the data folder")?;

    let store = SqliteStore::new(dir.join("store.sqlite3"))
        .await
        .context("open the SQLite store")?;

    let client = ClientBuilder::<FilesystemKeyStore>::for_testnet()
        .store(Arc::new(store))
        .filesystem_keystore(dir.join("keys"))?
        .build()
        .await
        .context("build the client")?;

    Ok(client)
}

/// Parses an account ID written in hex (`0x...`) or bech32 (`mtst1...`).
pub fn parse_account_id(text: &str) -> anyhow::Result<AccountId> {
    if text.starts_with("0x") {
        Ok(AccountId::from_hex(text)?)
    } else {
        let (_network, id) = AccountId::from_bech32(text)?;
        Ok(id)
    }
}

/// Returns the account a script should use.
///
/// - If `arg` is given, it is parsed as an account ID.
/// - Otherwise, if the client tracks exactly one account, that account is used.
/// - Otherwise the tracked accounts are printed and an error asks you to pick one.
pub async fn pick_account(
    client: &TestnetClient,
    arg: Option<String>,
) -> anyhow::Result<AccountId> {
    if let Some(text) = arg {
        return parse_account_id(&text);
    }

    let headers = client.get_account_headers().await?;
    match headers.as_slice() {
        [] => anyhow::bail!("no accounts tracked yet: run 02_new_wallet first"),
        [(header, _)] => Ok(header.id()),
        many => {
            println!("this client tracks {} accounts:", many.len());
            for (header, _) in many {
                println!(
                    "  {}  ({})",
                    header.id(),
                    header.id().to_bech32(NetworkId::Testnet)
                );
            }
            anyhow::bail!(
                "pass the one to use: cargo run --release --example <name> -- <account-id>"
            )
        }
    }
}

/// Returns the faucet ID of the token that pays transaction fees on this network.
///
/// The fee token is part of the protocol configuration that every block header commits to.
pub async fn fee_token(client: &TestnetClient) -> anyhow::Result<AccountId> {
    let header = client.get_latest_block_header().await?;
    let config = client
        .get_protocol_config(header.protocol_config_commitment())
        .await?;
    Ok(config.fee_asset_id().faucet_id())
}

/// Creates a wallet account with one Falcon key, exactly like lesson 2, and tracks it.
///
/// The account exists only in the local store until its first transaction.
pub async fn new_wallet(
    client: &mut TestnetClient,
    account_type: AccountType,
) -> anyhow::Result<Account> {
    Ok(new_wallet_with_key(client, account_type).await?.0)
}

/// Same as [`new_wallet`], and also returns the secret key (for example, to back it up).
pub async fn new_wallet_with_key(
    client: &mut TestnetClient,
    account_type: AccountType,
) -> anyhow::Result<(Account, AuthSecretKey)> {
    let secret_key = AuthSecretKey::new_falcon512_poseidon2();
    let auth = AuthSingleSig::new(Approver::new(
        secret_key.public_key().to_commitment(),
        AuthSchemeId::Falcon512Poseidon2,
    ));
    let seed: [u8; 32] = rand::random();

    let account = AccountBuilder::new(seed)
        .account_type(account_type)
        .with_component(auth)
        .with_component(BasicWallet)
        .build_with_schema_commitment()?;

    let keystore = client
        .authenticator()
        .expect("testnet_client sets a keystore")
        .clone();
    keystore.add_key(&secret_key, account.id()).await?;
    client.add_account(&account, false).await?;
    Ok((account, secret_key))
}

/// Returns the tracked account that holds the most of `token`, or an error if none holds any.
pub async fn richest_account(
    client: &TestnetClient,
    token: AccountId,
) -> anyhow::Result<AccountId> {
    let mut best: Option<(AccountId, u64)> = None;
    for (header, _) in client.get_account_headers().await? {
        let balance = token_balance(client, header.id(), token).await?;
        if balance > best.map_or(0, |(_, b)| b) {
            best = Some((header.id(), balance));
        }
    }
    best.map(|(id, _)| id)
        .context("no funded account: run lesson 3 first")
}

/// Returns how many base units of `token` (a faucet ID) the account holds, per the local store.
pub async fn token_balance(
    client: &TestnetClient,
    account_id: AccountId,
    token: AccountId,
) -> anyhow::Result<u64> {
    let vault = client.get_account_vault(account_id).await?;
    Ok(vault
        .assets()
        .filter(|asset| asset.is_fungible() && asset.faucet_id() == token)
        .map(|asset| asset.unwrap_fungible().amount().as_u64())
        .sum())
}

/// Syncs until the client has seen block `height` (blocks come about every 3 seconds).
pub async fn wait_until_block(
    client: &mut TestnetClient,
    height: BlockNumber,
) -> anyhow::Result<()> {
    loop {
        let summary = client.sync_state().await?;
        if summary.block_num >= height {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
}

/// Syncs until the transaction is committed, and returns its block.
///
/// Fails if the node discards the transaction or nothing happens before `timeout`.
pub async fn wait_for_commit(
    client: &mut TestnetClient,
    tx_id: TransactionId,
    timeout: Duration,
) -> anyhow::Result<BlockNumber> {
    let started = Instant::now();
    loop {
        client.sync_state().await?;
        let records = client
            .get_transactions(TransactionFilter::Ids(vec![tx_id]))
            .await?;
        match records.first().map(|record| &record.status) {
            Some(TransactionStatus::Committed { block_number, .. }) => return Ok(*block_number),
            Some(TransactionStatus::Discarded(cause)) => bail!("transaction discarded: {cause:?}"),
            _ if started.elapsed() > timeout => {
                bail!("transaction still pending after {timeout:?}")
            }
            _ => tokio::time::sleep(POLL_EVERY).await,
        }
    }
}

/// Syncs until `account_id` has at least one note it can consume now, and returns those notes.
///
/// Accepts `ConsumableWithAuthorization`: screening runs without keys, so a signature-protected
/// account never gets plain `Consumable` (see lesson 3). Skips notes that are time-locked.
pub async fn wait_for_consumable_notes(
    client: &mut TestnetClient,
    account_id: AccountId,
    timeout: Duration,
) -> anyhow::Result<Vec<InputNoteRecord>> {
    let started = Instant::now();
    loop {
        client.sync_state().await?;
        let ready: Vec<InputNoteRecord> = client
            .get_consumable_notes(Some(account_id))
            .await?
            .into_iter()
            .filter(|(_, statuses)| {
                statuses.iter().any(|(id, status)| {
                    *id == account_id
                        && matches!(
                            status,
                            NoteConsumptionStatus::Consumable
                                | NoteConsumptionStatus::ConsumableWithAuthorization
                        )
                })
            })
            .map(|(record, _)| record)
            .collect();

        if !ready.is_empty() {
            return Ok(ready);
        }
        if started.elapsed() > timeout {
            bail!("no consumable note for {account_id} after {timeout:?}");
        }
        tokio::time::sleep(POLL_EVERY).await;
    }
}

/// Prints what one `sync_state` call changed.
pub fn print_sync_summary(summary: &SyncSummary) {
    println!("synced to block {}", summary.block_num);
    println!(
        "  new public notes:       {}",
        summary.new_public_notes.len()
    );
    println!(
        "  new private notes:      {}",
        summary.new_private_notes.len()
    );
    println!(
        "  committed notes:        {}",
        summary.committed_notes.len()
    );
    println!("  consumed notes:         {}", summary.consumed_notes.len());
    println!(
        "  updated accounts:       {}",
        summary.updated_accounts.len()
    );
    println!(
        "  locked accounts:        {}",
        summary.locked_accounts.len()
    );
    println!(
        "  committed transactions: {}",
        summary.committed_transactions.len()
    );
}
