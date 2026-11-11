//! Lesson 8: time rules on notes. A timelock, and a reclaim.
//!
//! Run: `cargo run --release --example 08_timelock [-- [<alice-account-id>] [--reclaim-only]]`
//! Takes about 2 minutes: it waits for blocks on purpose. `--reclaim-only` runs only part B.
//!
//! P2IDE ("pay to ID, extended") is P2ID plus two optional block heights in the note's storage:
//! - timelock height: nobody can consume the note before this block.
//! - reclaim height: from this block on, the SENDER may take the note back if the target has not
//!   consumed it. The target can still consume it too: first one wins.
//!
//! The note script checks these against the transaction's REFERENCE block: the block your client
//! synced to when it executed the transaction. No clock, no oracle, just block numbers.
//! (See [`p2ide.masm`].)
//!
//! Part A, timelock:
//!
//! 1. Alice sends Bob 0.1 token, locked until "now + 10 blocks".
//! 2. Bob's screening says `ConsumableAfter(h)`.
//! 3. Bob tries to consume it early. The note script rejects it during LOCAL execution: nothing
//!    reaches the network, and no fee is paid.
//! 4. After block h, Bob consumes it.
//!
//! Part B, reclaim:
//!
//! 1. Alice sends Bob 0.1 token, reclaimable by Alice from "now + 10 blocks".
//! 2. Bob does nothing. Alice's own screening says `ConsumableAfter(h)` for HER.
//! 3. After block h, Alice consumes her own note: the tokens come back.
//!
//! This is the building block for escrow and for atomic swaps between chains: "claim before
//! block X, or the sender gets a refund after block X".
//!
//! [`p2ide.masm`]: https://github.com/0xMiden/protocol/blob/v0.18.0/crates/miden-standards/asm/standards/notes/p2ide.masm

use std::time::Duration;

use miden_by_example::{
    TestnetClient, bob_data_dir, fee_token, new_wallet, parse_account_id, richest_account,
    testnet_client, testnet_client_at, token_balance, wait_for_commit, wait_until_block,
};
use miden_client::account::{AccountId, AccountType};
use miden_client::asset::FungibleAsset;
use miden_client::block::BlockNumber;
use miden_client::note::{Note, NoteConsumptionStatus, NoteFile, NoteId, NoteType};
use miden_client::transaction::{PaymentNoteDescription, TransactionRequestBuilder};

/// 0.1 token per note. Small, but far more than the fee (~120) the receiver pays to consume it.
const AMOUNT: u64 = 100_000;
/// Room for Alice's own fees on top of the amounts she sends.
const FEE_MARGIN: u64 = 2_000;
const LOCK_BLOCKS: u32 = 10;
const TIMEOUT: Duration = Duration::from_secs(3 * 60);

/// Syncs until `account` has the note, and returns the account's consumption status for it.
async fn status_of(
    client: &mut TestnetClient,
    account: AccountId,
    note_id: NoteId,
) -> anyhow::Result<NoteConsumptionStatus> {
    let started = std::time::Instant::now();
    loop {
        client.sync_state().await?;
        for (record, statuses) in client.get_consumable_notes(Some(account)).await? {
            if record.id() == Some(note_id)
                && let Some((_, status)) = statuses.into_iter().find(|(id, _)| *id == account)
            {
                return Ok(status);
            }
        }
        anyhow::ensure!(
            started.elapsed() < TIMEOUT,
            "note {} never showed up",
            note_id.to_hex()
        );
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
}

/// Builds a request that consumes one tracked note.
async fn consume_request(
    client: &TestnetClient,
    note_id: NoteId,
) -> anyhow::Result<miden_client::transaction::TransactionRequest> {
    let record = client
        .get_input_note(note_id)
        .await?
        .expect("note is tracked");
    let note: Note = record.try_into()?;
    Ok(TransactionRequestBuilder::new().build_consume_notes(vec![note])?)
}

/// Alice sends a P2IDE note to Bob and waits for the commit. Returns the note ID.
async fn send_p2ide(
    client: &mut TestnetClient,
    alice: AccountId,
    bob: AccountId,
    token: AccountId,
    timelock: Option<BlockNumber>,
    reclaim: Option<BlockNumber>,
) -> anyhow::Result<NoteId> {
    let mut payment =
        PaymentNoteDescription::new(vec![FungibleAsset::new(token, AMOUNT)?.into()], alice, bob);
    if let Some(height) = timelock {
        payment = payment.with_timelock_height(height);
    }
    if let Some(height) = reclaim {
        payment = payment.with_reclaim_height(height);
    }
    let request = TransactionRequestBuilder::new().build_pay_to_id(
        payment,
        NoteType::Public,
        client.rng(),
    )?;
    let note_id = request.expected_output_own_notes()[0].id();
    let tx = client.submit_new_transaction(alice, request).await?;
    wait_for_commit(client, tx, TIMEOUT).await?;
    Ok(note_id)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut alice_client = testnet_client().await?;
    let mut bob_client = testnet_client_at(bob_data_dir()).await?;
    alice_client.sync_state().await?;
    bob_client.sync_state().await?;

    let args: Vec<String> = std::env::args().skip(1).collect();
    let reclaim_only = args.iter().any(|arg| arg == "--reclaim-only");
    let token = fee_token(&alice_client).await?;
    let alice = match args.iter().find(|arg| !arg.starts_with("--")) {
        Some(text) => parse_account_id(text)?,
        None => richest_account(&alice_client, token).await?,
    };

    // Check the balance first, so the lesson does not stop halfway.
    let parts = if reclaim_only { 1 } else { 2 };
    let needed = parts * AMOUNT + FEE_MARGIN;
    let balance = token_balance(&alice_client, alice, token).await?;
    anyhow::ensure!(
        balance >= needed,
        "alice has {balance} but needs {needed}. Request more from the faucet, then run:\n  \
         cargo run --release --example 03_fund_and_consume -- {alice}"
    );

    let bob = new_wallet(&mut bob_client, AccountType::Public).await?.id();
    println!("alice: {alice}  (balance {balance})\nbob:   {bob} (new, in data/testnet-bob)");

    if !reclaim_only {
        part_a_timelock(&mut alice_client, &mut bob_client, alice, bob, token).await?;
    }
    part_b_reclaim(&mut alice_client, alice, bob, token).await
}

/// Prints an error and every error that caused it. The innermost cause usually names the exact
/// check that failed, for example the note script's assertion.
fn print_error_chain(err: &dyn std::error::Error) {
    println!("  {err}");
    let mut current = err.source();
    while let Some(cause) = current {
        println!("    caused by: {cause}");
        current = cause.source();
    }
}

async fn part_a_timelock(
    alice_client: &mut TestnetClient,
    bob_client: &mut TestnetClient,
    alice: AccountId,
    bob: AccountId,
    token: AccountId,
) -> anyhow::Result<()> {
    let now = alice_client.get_sync_height().await?;
    let unlock_at = now + LOCK_BLOCKS;
    println!("\n== part A: timelock. now = block {now}, unlocks at block {unlock_at}");

    let note_a = send_p2ide(alice_client, alice, bob, token, Some(unlock_at), None).await?;
    println!("alice sent note {}", note_a.to_hex());

    let status = status_of(bob_client, bob, note_a).await?;
    println!("bob's screening says: {status:?}");

    println!(
        "bob tries to consume it now (block {})...",
        bob_client.get_sync_height().await?
    );
    let request = consume_request(bob_client, note_a).await?;
    match bob_client.submit_new_transaction(bob, request).await {
        Ok(tx) => println!("  unexpected: accepted as {tx}"),
        Err(err) => {
            println!("  REJECTED during local execution, nothing was sent:");
            print_error_chain(&err);
        }
    }

    println!("waiting for block {unlock_at}...");
    wait_until_block(bob_client, unlock_at).await?;
    let status = status_of(bob_client, bob, note_a).await?;
    println!("bob's screening now says: {status:?}");
    let request = consume_request(bob_client, note_a).await?;
    let tx = bob_client.submit_new_transaction(bob, request).await?;
    let block = wait_for_commit(bob_client, tx, TIMEOUT).await?;
    println!(
        "bob consumed it in block {block}. bob balance: {}",
        token_balance(bob_client, bob, token).await?
    );
    Ok(())
}

async fn part_b_reclaim(
    alice_client: &mut TestnetClient,
    alice: AccountId,
    bob: AccountId,
    token: AccountId,
) -> anyhow::Result<()> {
    let now = alice_client.get_sync_height().await?;
    let reclaim_at = now + LOCK_BLOCKS;
    println!("\n== part B: reclaim. now = block {now}, alice may reclaim from block {reclaim_at}");

    let alice_before = token_balance(alice_client, alice, token).await?;
    let note_b = send_p2ide(alice_client, alice, bob, token, None, Some(reclaim_at)).await?;
    println!(
        "alice sent note {} to bob; bob will ignore it",
        note_b.to_hex()
    );

    // Alice's client usually tracks this note already, because Alice can consume it too. If it
    // does not, import it by ID. That works because the note is public.
    if alice_client.get_input_note(note_b).await?.is_none() {
        alice_client
            .import_notes(&[NoteFile::NoteId(note_b)])
            .await?;
    }
    let status = status_of(alice_client, alice, note_b).await?;
    println!("alice's screening of her OWN note: {status:?}");

    println!("waiting for block {reclaim_at}...");
    wait_until_block(alice_client, reclaim_at).await?;
    let request = consume_request(alice_client, note_b).await?;
    let tx = alice_client.submit_new_transaction(alice, request).await?;
    let block = wait_for_commit(alice_client, tx, TIMEOUT).await?;
    let alice_after = token_balance(alice_client, alice, token).await?;
    println!("alice reclaimed it in block {block}");
    println!(
        "alice balance {alice_before} -> {alice_after}: the {AMOUNT} came back; she lost only two fees ({})",
        alice_before - alice_after
    );

    Ok(())
}
