//! Lesson 4: send tokens to another account with a public P2ID note.
//!
//! Run: `cargo run --release --example 04_send_public [-- <sender-account-id>]`
//!   Without an argument, the sender is the tracked account with the most fee tokens.
//!
//! Characters:
//! - Alice: your funded account from lesson 3. She sends.
//! - Bob:   a NEW wallet this script creates. He receives. He is not on chain yet.
//!
//! What happens:
//! 1. Alice builds a P2ID note: "these assets, for account Bob". It is PUBLIC, so the full note
//!    (assets, script, inputs) goes on chain.
//! 2. Alice's transaction moves the assets out of her vault INTO the note. The tokens now sit
//!    in the note, owned by no account. Alice pays a fee for this transaction.
//! 3. Bob's side syncs, finds the note by Bob's tag, and consumes it. That is Bob's first
//!    transaction, so it deploys Bob's account. Bob pays its fee from the tokens he just got.
//!
//! This is the core Miden pattern. A transfer is TWO transactions, one per account:
//!   Alice: account -> note        Bob: note -> account
//! Each account only ever changes its own state. Nobody touches someone else's account.
//!
//! Things to notice in the output:
//! - Alice loses `amount + her fee`. Bob ends with `amount - his fee`.
//! - Both transactions are independent. Alice's is done before Bob even syncs. Bob could
//!   consume the note tomorrow.
//!
//! Try next:
//! - Send to a friend's account ID instead of Bob (replace `bob.id()`).
//! - Make Bob's account `AccountType::Private` and compare what midenscan shows for him.

use std::time::{Duration, Instant};

use miden_client::account::AccountType;
use miden_client::address::NetworkId;
use miden_client::asset::FungibleAsset;
use miden_client::note::{Note, NoteType};
use miden_client::transaction::{PaymentNoteDescription, TransactionRequestBuilder};
use miden_by_example::{
    fee_token, new_wallet, parse_account_id, testnet_client, token_balance, wait_for_commit,
    wait_for_consumable_notes,
};

/// One token: the fee token has 6 decimals.
const AMOUNT: u64 = 1_000_000;
const TIMEOUT: Duration = Duration::from_secs(3 * 60);

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut client = testnet_client().await?;
    client.sync_state().await?;
    let token = fee_token(&client).await?;

    // Pick Alice: the given account, or the tracked account with the most fee tokens.
    let alice = match std::env::args().nth(1) {
        Some(text) => parse_account_id(&text)?,
        None => {
            let mut best = None;
            for (header, _) in client.get_account_headers().await? {
                let balance = token_balance(&client, header.id(), token).await?;
                if balance > best.map_or(0, |(_, b)| b) {
                    best = Some((header.id(), balance));
                }
            }
            best.map(|(id, _)| id)
                .ok_or_else(|| anyhow::anyhow!("no funded account: run lesson 3"))?
        }
    };
    let alice_before = token_balance(&client, alice, token).await?;
    anyhow::ensure!(
        alice_before > AMOUNT,
        "Alice has {alice_before}, needs more than {AMOUNT}"
    );

    // Create Bob. He exists only in this client's store for now.
    let bob = new_wallet(&mut client, AccountType::Public).await?;
    println!(
        "alice: {}  ({})",
        alice,
        alice.to_bech32(NetworkId::Testnet)
    );
    println!(
        "bob:   {}  ({})",
        bob.id(),
        bob.id().to_bech32(NetworkId::Testnet)
    );
    println!("alice balance before: {alice_before}");

    // ALICE'S TRANSACTION: vault -> note
    // --------------------------------------------------------------------------------------------
    let asset = FungibleAsset::new(token, AMOUNT)?;
    let payment = PaymentNoteDescription::new(vec![asset.into()], alice, bob.id());
    let request = TransactionRequestBuilder::new().build_pay_to_id(
        payment,
        NoteType::Public,
        client.rng(),
    )?;

    // The request already contains the full note it will create. Look at it before sending.
    let note: Note = request.expected_output_own_notes().remove(0);
    println!("\nnote to create: {}", note.id().to_hex());
    println!(
        "  tag:       {}  (first 14 bits of Bob's ID)",
        note.metadata().tag()
    );
    println!("  recipient: {}", note.recipient().digest().to_hex());

    let started = Instant::now();
    let alice_tx = client.submit_new_transaction(alice, request).await?;
    println!(
        "alice tx {} submitted in {:.1?}",
        alice_tx,
        started.elapsed()
    );
    let block = wait_for_commit(&mut client, alice_tx, TIMEOUT).await?;
    let alice_after = token_balance(&client, alice, token).await?;
    println!("alice tx committed in block {block}");
    println!(
        "alice balance after:  {alice_after}  (sent {AMOUNT}, fee {})",
        alice_before - alice_after - AMOUNT
    );

    // BOB'S TRANSACTION: note -> vault (also deploys Bob)
    // --------------------------------------------------------------------------------------------
    println!("\nbob syncs and looks for notes he can consume...");
    let records = wait_for_consumable_notes(&mut client, bob.id(), TIMEOUT).await?;
    let notes: Vec<Note> = records
        .into_iter()
        .map(TryInto::try_into)
        .collect::<Result<_, _>>()?;
    println!(
        "bob found {} note(s), first: {}",
        notes.len(),
        notes[0].id().to_hex()
    );

    let request = TransactionRequestBuilder::new().build_consume_notes(notes)?;
    let bob_tx = client.submit_new_transaction(bob.id(), request).await?;
    let block = wait_for_commit(&mut client, bob_tx, TIMEOUT).await?;
    let bob_after = token_balance(&client, bob.id(), token).await?;
    println!("bob tx {bob_tx} committed in block {block}");
    println!(
        "bob balance after:    {bob_after}  (received {AMOUNT}, fee {})",
        AMOUNT - bob_after
    );

    Ok(())
}
