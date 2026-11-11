//! Lesson 6: a PRIVATE account. What the chain stores, and what happens if you lose your device.
//!
//! Run: `cargo run --release --example 06_private_account [-- <alice-account-id>]`
//!
//! Characters:
//! - Alice: your funded public account (`data/testnet/`).
//! - Carol: a new PRIVATE wallet on her own client (`data/testnet-carol/`).
//! - "Carol's new phone": an empty client (`data/testnet-carol-restored/`). The script deletes
//!   and recreates this folder on every run.
//!
//! A public account keeps its full state on chain: code, storage, vault, nonce. A private account
//! keeps only a COMMITMENT on chain: one hash of its state. The state itself lives only on the
//! owner's device. Every transaction proves "old commitment -> new commitment is valid" without
//! showing the state.
//!
//! What happens:
//! 1. Carol creates a private wallet. Alice sends her 2 tokens with a public note. Carol consumes
//!    it, which deploys her account.
//! 2. The script asks the node directly for both accounts:
//!    - Alice (public): the node returns her header: nonce, vault root, storage root, code.
//!    - Carol (private): the node returns ONLY the commitment. It equals the hash of the state
//!      on Carol's device.
//! 3. Carol backs up her account and key to a file.
//! 4. Carol "loses her phone". A new, empty client tries to import her account by ID from the
//!    network. That fails: the network does not have her state.
//! 5. The new client restores from the backup file and syncs. The backup state matches the
//!    on-chain commitment, so the account works again.
//!
//! Account privacy and note privacy are separate dials. Carol's account is private, but the note
//! Alice sent is public, so anyone can see "2 tokens went to Carol's ID". Use a private note
//! (lesson 5) to hide that as well.
//!
//! Try next (a stale backup): after this script, make one more transaction with Carol on her
//! original client (for example, send 1 token back to Alice). Then restore the OLD backup into a
//! fresh client and sync. The backup commitment no longer matches the chain, and sync reports the
//! account as LOCKED. A backup of a private account is only good until the next transaction.

use std::path::PathBuf;
use std::time::Duration;

use miden_by_example::{
    fee_token, new_wallet_with_key, parse_account_id, richest_account, testnet_client,
    testnet_client_at, token_balance, wait_for_commit, wait_for_consumable_notes,
};
use miden_client::account::{AccountFile, AccountType};
use miden_client::asset::FungibleAsset;
use miden_client::keystore::Keystore;
use miden_client::note::{Note, NoteType};
use miden_client::rpc::domain::account::GetAccountRequest;
use miden_client::rpc::{Endpoint, GrpcClient, NodeRpcClient};
use miden_client::transaction::{PaymentNoteDescription, TransactionRequestBuilder};

const AMOUNT: u64 = 2_000_000;
const TIMEOUT: Duration = Duration::from_secs(3 * 60);

fn data(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("data")
        .join(name)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut alice_client = testnet_client().await?;
    let mut carol_client = testnet_client_at(data("testnet-carol")).await?;
    alice_client.sync_state().await?;
    carol_client.sync_state().await?;

    let token = fee_token(&alice_client).await?;
    let alice = match std::env::args().nth(1) {
        Some(text) => parse_account_id(&text)?,
        None => richest_account(&alice_client, token).await?,
    };

    // STEP 1: a private wallet for Carol, funded by Alice, deployed by its first transaction.
    // --------------------------------------------------------------------------------------------
    let (carol_account, carol_key) =
        new_wallet_with_key(&mut carol_client, AccountType::Private).await?;
    let carol = carol_account.id();
    println!("carol: {carol}  (PRIVATE, in data/testnet-carol)");

    let payment = PaymentNoteDescription::new(
        vec![FungibleAsset::new(token, AMOUNT)?.into()],
        alice,
        carol,
    );
    let request = TransactionRequestBuilder::new().build_pay_to_id(
        payment,
        NoteType::Public,
        alice_client.rng(),
    )?;
    let tx = alice_client.submit_new_transaction(alice, request).await?;
    wait_for_commit(&mut alice_client, tx, TIMEOUT).await?;
    println!("alice sent carol {AMOUNT}");

    let records = wait_for_consumable_notes(&mut carol_client, carol, TIMEOUT).await?;
    let notes: Vec<Note> = records
        .into_iter()
        .map(TryInto::try_into)
        .collect::<Result<_, _>>()?;
    let request = TransactionRequestBuilder::new().build_consume_notes(notes)?;
    let tx = carol_client.submit_new_transaction(carol, request).await?;
    let block = wait_for_commit(&mut carol_client, tx, TIMEOUT).await?;
    let carol_now = carol_client
        .get_account(carol)
        .await?
        .expect("carol is tracked");
    println!(
        "carol consumed it in block {block}: nonce {}, balance {}",
        carol_now.nonce(),
        token_balance(&carol_client, carol, token).await?
    );

    // STEP 2: what does the NETWORK know? Ask the node directly, with no local store involved.
    // --------------------------------------------------------------------------------------------
    let rpc = GrpcClient::new(&Endpoint::testnet(), 10_000);

    let (_, alice_proof) = rpc.get_account(GetAccountRequest::new(alice)).await?;
    println!("\nnode's view of ALICE (public):");
    println!(
        "  commitment: {}",
        alice_proof.account_commitment().to_hex()
    );
    match alice_proof.account_header() {
        Some(header) => println!(
            "  header:     nonce {}, vault root {}..., code {}...",
            header.nonce(),
            &header.vault_root().to_hex()[..18],
            &header.code_commitment().to_hex()[..18]
        ),
        None => println!("  header:     none"),
    }

    let (_, carol_proof) = rpc.get_account(GetAccountRequest::new(carol)).await?;
    println!("node's view of CAROL (private):");
    println!(
        "  commitment: {}",
        carol_proof.account_commitment().to_hex()
    );
    println!(
        "  header:     {}",
        if carol_proof.account_header().is_some() {
            "present"
        } else {
            "none: no state on chain"
        }
    );
    println!("carol's device, hash of her real state:");
    println!("  commitment: {}", carol_now.to_commitment().to_hex());
    println!(
        "  matches the chain: {}",
        carol_now.to_commitment() == carol_proof.account_commitment()
    );

    // STEP 3: Carol backs up her account AND her key. Without both, the account is gone.
    // --------------------------------------------------------------------------------------------
    let backup_path = data("carol-backup.mac");
    AccountFile::new(carol_now, vec![carol_key]).write(&backup_path)?;
    println!(
        "\nbackup written to {}  (it contains the SECRET KEY)",
        backup_path.display()
    );

    // STEP 4: a new phone. Can it get Carol's account from the network?
    // --------------------------------------------------------------------------------------------
    let restored_dir = data("testnet-carol-restored");
    if restored_dir.exists() {
        std::fs::remove_dir_all(&restored_dir)?;
    }
    let mut new_phone = testnet_client_at(restored_dir).await?;
    new_phone.sync_state().await?;
    match new_phone.import_account_by_id(carol).await {
        Ok(()) => println!("\nimport by ID: unexpectedly worked"),
        Err(err) => println!("\nimport by ID from the network: FAILED, as expected\n  {err}"),
    }

    // STEP 5: restore from the backup file instead.
    // --------------------------------------------------------------------------------------------
    let (account, keys) = AccountFile::read(&backup_path)?.into_parts();
    let keystore = new_phone.authenticator().expect("keystore is set").clone();
    for key in &keys {
        keystore.add_key(key, account.id()).await?;
    }
    new_phone.add_account(&account, false).await?;
    let summary = new_phone.sync_state().await?;
    println!("restored from backup and synced:");
    println!(
        "  balance: {}",
        token_balance(&new_phone, carol, token).await?
    );
    println!("  locked:  {}", summary.locked_accounts.contains(&carol));

    Ok(())
}
