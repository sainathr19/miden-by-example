//! Lesson 3: get tokens from the faucet and make your first transaction.
//!
//! Run: `cargo run --release --example 03_fund_and_consume [-- <account-id>]`
//!   (the account ID is needed only if the client tracks more than one account)
//!
//! Before or while it runs:
//!   open https://faucet.testnet.miden.io/, paste the bech32 ID the script prints, and click
//!   "Send Public Note".
//!
//! What happens:
//! 1. The faucet creates a PUBLIC P2ID note ("pay to ID") addressed to your account. Your account
//!    still does not exist on chain. The note only names its ID.
//! 2. The script syncs until the note arrives. Sync finds it through your account's note tag.
//! 3. The script consumes the note. This is your account's FIRST transaction, so it also deploys
//!    the account (nonce 0 -> 1).
//! 4. Your client executes the transaction locally, the testnet prover proves it, and the node
//!    receives only the proof.
//! 5. The auth component pays the fee out of the tokens the note just moved into the vault.
//!    Testnet fees are paid in the same token the faucet sends.
//! 6. The script syncs until the transaction is committed, then prints the vault and the fee.
//!
//! Code to read:
//! - [`note_screener.rs`](https://github.com/0xMiden/rust-sdk/blob/v0.18.0/crates/rust-client/src/note/note_screener.rs) (why the note is "consumable")
//! - [`transaction/mod.rs`](https://github.com/0xMiden/rust-sdk/blob/v0.18.0/crates/rust-client/src/transaction/mod.rs), `submit_new_transaction`
//! - [`p2id.masm`](https://github.com/0xMiden/protocol/blob/v0.18.0/crates/miden-standards/asm/standards/notes/p2id.masm) (the note's script)

use std::time::{Duration, Instant};

use anyhow::bail;
use miden_by_example::{fee_token, pick_account, testnet_client};
use miden_client::address::NetworkId;
use miden_client::note::{Note, NoteConsumptionStatus};
use miden_client::store::TransactionFilter;
use miden_client::transaction::{TransactionRequestBuilder, TransactionStatus};

const POLL_EVERY: Duration = Duration::from_secs(5);
const WAIT_FOR_NOTE: Duration = Duration::from_secs(10 * 60);
const WAIT_FOR_COMMIT: Duration = Duration::from_secs(3 * 60);

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut client = testnet_client().await?;
    client.sync_state().await?;

    let account_id = pick_account(&client, std::env::args().nth(1)).await?;
    let fee_token = fee_token(&client).await?;

    println!("account:   {account_id}");
    println!("bech32:    {}", account_id.to_bech32(NetworkId::Testnet));
    println!("fee token: {fee_token}");
    println!();
    println!("Request a PUBLIC note for the bech32 ID at https://faucet.testnet.miden.io/");

    // STEP 1: sync until a note this account can consume shows up.
    // --------------------------------------------------------------------------------------------
    let started = Instant::now();
    let records = loop {
        let summary = client.sync_state().await?;
        let ready: Vec<_> = client
            .get_consumable_notes(Some(account_id))
            .await?
            .into_iter()
            // Keep only notes this account can consume NOW. A note can also be consumable only
            // after some block (a timelock), and such a note would make the transaction fail.
            //
            // Screening runs WITHOUT your keys, so it cannot produce the signature your auth
            // component asks for. For a signature-protected account, a note that is fine to
            // consume therefore comes back as `ConsumableWithAuthorization`: "this works once you
            // sign". Plain `Consumable` appears only for accounts that need no signature.
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
            println!("\nnote arrived (synced to block {})", summary.block_num);
            break ready;
        }
        if started.elapsed() > WAIT_FOR_NOTE {
            bail!(
                "no consumable note after {:?}; did the faucet request go through?",
                WAIT_FOR_NOTE
            );
        }
        print!(".");
        use std::io::Write;
        std::io::stdout().flush()?;
        tokio::time::sleep(POLL_EVERY).await;
    };

    // STEP 2: look at the notes. A note is a small container: assets + a script + inputs.
    // --------------------------------------------------------------------------------------------
    let mut received_fee_token = 0u64;
    let mut notes: Vec<Note> = Vec::new();
    for record in records {
        println!(
            "note {}",
            record.id().map(|id| id.to_hex()).unwrap_or_default()
        );
        for asset in record.assets().iter() {
            let amount = asset.unwrap_fungible().amount().as_u64();
            println!("  carries {amount} of token {}", asset.faucet_id());
            if asset.faucet_id() == fee_token {
                received_fee_token += amount;
            }
        }
        notes.push(record.try_into()?);
    }

    // STEP 3: build the request, then execute, prove and submit it.
    // --------------------------------------------------------------------------------------------
    let account_before = client
        .get_account(account_id)
        .await?
        .expect("account is tracked");
    println!("\nnonce before: {}", account_before.nonce());

    let request = TransactionRequestBuilder::new().build_consume_notes(notes)?;

    let submit_started = Instant::now();
    let tx_id = client.submit_new_transaction(account_id, request).await?;
    println!(
        "submitted tx {} in {:.1?} (execute + remote prove + send)",
        tx_id,
        submit_started.elapsed()
    );

    // STEP 4: sync until the node includes the transaction in a block.
    // --------------------------------------------------------------------------------------------
    let commit_started = Instant::now();
    loop {
        client.sync_state().await?;
        let records = client
            .get_transactions(TransactionFilter::Ids(vec![tx_id]))
            .await?;
        match records.first().map(|record| &record.status) {
            Some(TransactionStatus::Committed { block_number, .. }) => {
                println!(
                    "committed in block {block_number} after {:.1?}",
                    commit_started.elapsed()
                );
                break;
            }
            Some(TransactionStatus::Discarded(cause)) => {
                bail!("transaction was discarded: {cause:?}")
            }
            _ if commit_started.elapsed() > WAIT_FOR_COMMIT => {
                bail!("transaction still pending after {:?}", WAIT_FOR_COMMIT)
            }
            _ => tokio::time::sleep(POLL_EVERY).await,
        }
    }

    // STEP 5: the account now exists on chain. Look at it.
    // --------------------------------------------------------------------------------------------
    let account_after = client
        .get_account(account_id)
        .await?
        .expect("account is tracked");
    println!("\nnonce after:  {}", account_after.nonce());
    println!("vault:");
    let mut balance_fee_token = 0u64;
    for asset in client.get_account_vault(account_id).await?.assets() {
        let amount = asset.unwrap_fungible().amount().as_u64();
        println!("  {amount} of token {}", asset.faucet_id());
        if asset.faucet_id() == fee_token {
            balance_fee_token = amount;
        }
    }
    println!(
        "fee paid:     {} (received {received_fee_token}, kept {balance_fee_token})",
        received_fee_token.saturating_sub(balance_fee_token)
    );

    Ok(())
}
