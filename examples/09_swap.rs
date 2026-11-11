//! Lesson 9: a SWAP note. Alice sells her PLAY token for fee tokens, and Bob fills the order.
//!
//! Run: `cargo run --release --example 09_swap`
//! Needs: Alice holds the token from lesson 7, and a Bob in `data/testnet-bob` holds fee tokens
//! (lessons 5 and 8 leave several).
//!
//! A SWAP note is an order that lives on chain:
//!   "This note holds 10 PLAY. ANY account may take them, IF it pays 0.05 fee tokens to Alice."
//!
//! The SWAP script runs inside the CONSUMER's transaction, so the swap is atomic:
//! - it moves the 10 PLAY into the consumer's vault, and
//! - it makes the consumer create a "payback" P2ID note for Alice that carries 0.05 fee tokens.
//!
//! If the consumer cannot pay, the script fails and nothing happens. Nobody trusts anybody.
//!
//! What happens (3 transactions, 2 clients):
//! 1. Bob's client SUBSCRIBES to the swap note's tag. Sync only looks forward from the last
//!    synced block, so Bob must subscribe before the order lands on chain.
//!    (Swap tags come from the asset pair, not from an account. A trading bot would subscribe to
//!    the tags of the markets it cares about.)
//! 2. Alice creates the SWAP note: her 10 PLAY move from her vault into the note.
//! 3. Bob finds the order, consumes it: he gets 10 PLAY, and his transaction creates the payback
//!    note for Alice with 0.05 fee tokens.
//! 4. Alice consumes the payback note.
//!
//! The payback note is PRIVATE, yet Alice finds it without any delivery. Why: the SWAP note fixed
//! every detail of the payback note in advance (assets, script, inputs, serial number). Alice's
//! client stored those details as an "expected future note" when she created the order. When a
//! note with matching ID appears on chain, her client recognises it.
//!
//! Limits of this SWAP note (see [`swap.masm`](https://github.com/0xMiden/protocol/blob/v0.18.0/crates/miden-standards/asm/standards/notes/swap.masm)):
//! - All or nothing. PSWAP (partially fillable swap) lets several takers fill parts of an order.
//! - No reclaim path. If nobody fills it, the offered tokens stay in the note.
//! - Anyone who sees a public order can fill it. First valid transaction wins.

use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use miden_by_example::{
    TestnetClient, bob_data_dir, fee_token, richest_account, testnet_client, testnet_client_at,
    token_balance, wait_for_commit, wait_for_consumable_notes,
};
use miden_client::account::AccountId;
use miden_client::asset::FungibleAsset;
use miden_client::note::{Note, NoteType};
use miden_client::store::NoteFilter;
use miden_client::transaction::{SwapTransactionData, TransactionRequestBuilder};

/// 10 PLAY (6 decimals).
const OFFER: u64 = 10_000_000;
/// 0.05 fee token (6 decimals).
const REQUEST: u64 = 50_000;
const FEE_MARGIN: u64 = 2_000;
const TIMEOUT: Duration = Duration::from_secs(3 * 60);

/// Finds a tracked account that holds at least `amount` of some token other than the fee token.
/// Returns `(account, token)`.
///
/// This picks the holder of your lesson 7 token. It does not pick the faucet itself: a faucet
/// mints its token into notes, so the token never sits in the faucet's own vault.
async fn find_token_holder(
    client: &TestnetClient,
    fee: AccountId,
    amount: u64,
) -> anyhow::Result<(AccountId, AccountId)> {
    for (header, _) in client.get_account_headers().await? {
        for asset in client.get_account_vault(header.id()).await?.assets() {
            if asset.is_fungible()
                && asset.faucet_id() != fee
                && asset.unwrap_fungible().amount().as_u64() >= amount
            {
                return Ok((header.id(), asset.faucet_id()));
            }
        }
    }
    bail!("no tracked account holds {amount} of a token besides the fee token: run lesson 7 first")
}

/// Syncs until the note is committed in this client's store.
async fn wait_for_committed_note(client: &mut TestnetClient, note: &Note) -> anyhow::Result<Note> {
    let started = Instant::now();
    loop {
        client.sync_state().await?;
        let committed = client.get_input_notes(NoteFilter::Committed).await?;
        if let Some(record) = committed
            .into_iter()
            .find(|record| record.id() == Some(note.id()))
        {
            return Ok(record.try_into()?);
        }
        anyhow::ensure!(
            started.elapsed() < TIMEOUT,
            "note {} never arrived",
            note.id().to_hex()
        );
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut alice_client = testnet_client().await?;
    let mut bob_client = testnet_client_at(bob_data_dir()).await?;
    alice_client.sync_state().await?;
    bob_client.sync_state().await?;

    let fee = fee_token(&alice_client).await?;
    let (alice, play) = find_token_holder(&alice_client, fee, OFFER).await?;
    let alice_fee = token_balance(&alice_client, alice, fee).await?;
    anyhow::ensure!(
        alice_fee >= FEE_MARGIN,
        "alice has {alice_fee} fee tokens, too few for 2 fees"
    );
    let bob = richest_account(&bob_client, fee)
        .await
        .context("no funded Bob: run lesson 5")?;
    let bob_fee = token_balance(&bob_client, bob, fee).await?;
    anyhow::ensure!(
        bob_fee >= REQUEST + FEE_MARGIN,
        "bob has {bob_fee} fee tokens, needs more"
    );

    let before = (
        token_balance(&alice_client, alice, play).await?,
        token_balance(&alice_client, alice, fee).await?,
        token_balance(&bob_client, bob, play).await?,
        bob_fee,
    );
    println!("alice {alice}: {} PLAY, {} fee", before.0, before.1);
    println!("bob   {bob}: {} PLAY, {} fee", before.2, before.3);
    println!("order: alice gives {OFFER} PLAY ({play}), wants {REQUEST} fee tokens");

    // STEP 1: Alice prepares the order. Bob subscribes to its tag BEFORE it lands on chain.
    // --------------------------------------------------------------------------------------------
    let swap = SwapTransactionData::new(
        alice,
        FungibleAsset::new(play, OFFER)?.into(),
        FungibleAsset::new(fee, REQUEST)?.into(),
    );
    let request = TransactionRequestBuilder::new().build_swap(
        &swap,
        NoteType::Public,  // the order itself: public, so takers can find it
        NoteType::Private, // the payback note: private, Alice already knows its details
        alice_client.rng(),
    )?;
    let swap_note = request.expected_output_own_notes().remove(0);
    let tag = swap_note.metadata().tag();
    bob_client.add_note_tag(tag).await?;
    println!(
        "\nswap note {} has tag {tag}; bob subscribed to it",
        swap_note.id().to_hex()
    );

    // STEP 2: Alice posts the order. Her PLAY moves into the note.
    // --------------------------------------------------------------------------------------------
    let tx = alice_client.submit_new_transaction(alice, request).await?;
    let block = wait_for_commit(&mut alice_client, tx, TIMEOUT).await?;
    println!("alice posted the order in block {block}");

    // STEP 3: Bob finds the order and fills it.
    // --------------------------------------------------------------------------------------------
    let order = wait_for_committed_note(&mut bob_client, &swap_note).await?;
    println!("bob found the order through his tag subscription");
    let request = TransactionRequestBuilder::new().build_consume_notes(vec![order])?;
    let tx = bob_client.submit_new_transaction(bob, request).await?;
    let block = wait_for_commit(&mut bob_client, tx, TIMEOUT).await?;
    println!("bob filled it in block {block}: he got the PLAY and created the payback note");

    // STEP 4: Alice's client recognises the private payback note and she consumes it.
    // --------------------------------------------------------------------------------------------
    let records = wait_for_consumable_notes(&mut alice_client, alice, TIMEOUT).await?;
    println!(
        "alice found {} note(s) to consume, including the payback",
        records.len()
    );
    let notes: Vec<Note> = records
        .into_iter()
        .map(TryInto::try_into)
        .collect::<Result<_, _>>()?;
    let request = TransactionRequestBuilder::new().build_consume_notes(notes)?;
    let tx = alice_client.submit_new_transaction(alice, request).await?;
    wait_for_commit(&mut alice_client, tx, TIMEOUT).await?;

    // RESULT
    // --------------------------------------------------------------------------------------------
    let after = (
        token_balance(&alice_client, alice, play).await?,
        token_balance(&alice_client, alice, fee).await?,
        token_balance(&bob_client, bob, play).await?,
        token_balance(&bob_client, bob, fee).await?,
    );
    println!("\n              PLAY                    fee token");
    println!(
        "alice  {:>10} -> {:<10}   {:>8} -> {:<8}",
        before.0, after.0, before.1, after.1
    );
    println!(
        "bob    {:>10} -> {:<10}   {:>8} -> {:<8}",
        before.2, after.2, before.3, after.3
    );
    println!("(fee-token changes include the transaction fees: alice paid 2, bob paid 1)");

    Ok(())
}
