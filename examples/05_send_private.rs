//! Lesson 5: send tokens with a PRIVATE note, between two separate clients.
//!
//! Run: `cargo run --release --example 05_send_private [-- <alice-account-id>]`
//!
//! Characters:
//! - Alice: your funded account, in your usual client (`data/testnet/`).
//! - Bob: a new wallet in a SEPARATE client (`data/testnet-bob/`): his own store, his own keys.
//!   Think "Bob's phone". The two clients share nothing.
//!
//! Why two clients: if Alice and Bob lived in one client, that client would already know the
//! note, because it created it. Then you would never see the real problem private notes have.
//!
//! The problem: a private note puts only its ID and metadata (sender, tag, type) on chain. The
//! details (assets, script, inputs, serial number) never leave Alice's device. Bob cannot spend
//! a note he cannot see. So the details must travel to Bob OFF-CHAIN.
//!
//! What happens:
//! 1. Bob's client creates Bob's wallet. Bob "tells" Alice his account ID.
//! 2. Alice's client creates a PRIVATE P2ID note for Bob and commits it.
//! 3. Bob syncs. The node shows him a note header under his tag, but no details. His client
//!    cannot use it and ignores it. Bob sees NOTHING.
//! 4. Alice sends the note details to Bob through the note transport service: a mailbox server
//!    keyed by tag. She attaches the note's inclusion proof, so the mailbox accepts only notes
//!    that really exist on chain.
//! 5. Bob syncs again. Sync also checks the mailbox, finds the note, verifies it against the
//!    chain, and stores it. Now Bob can consume it.
//!
//! Notes:
//! - The mailbox is one channel. Any channel works: a file (`NoteFile`), a chat message, your
//!   own server. The chain only needs the consuming transaction to be valid.
//! - Today the mailbox stores note details in PLAINTEXT. End-to-end encryption is planned, not
//!   built (see `send_private_note_with_proof` in the client).
//! - Open the note on midenscan. It shows "Private", and no assets or script.

use std::time::{Duration, Instant};

use anyhow::Context;
use miden_by_example::{
    bob_data_dir, fee_token, new_wallet, parse_account_id, richest_account, testnet_client,
    testnet_client_at, token_balance, wait_for_commit, wait_for_consumable_notes,
};
use miden_client::account::AccountType;
use miden_client::address::{Address, NetworkId};
use miden_client::asset::FungibleAsset;
use miden_client::note::{Note, NoteType};
use miden_client::transaction::{PaymentNoteDescription, TransactionRequestBuilder};

/// One token: the fee token has 6 decimals.
const AMOUNT: u64 = 1_000_000;
const TIMEOUT: Duration = Duration::from_secs(3 * 60);

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Two independent clients.
    let mut alice_client = testnet_client().await?;
    let mut bob_client = testnet_client_at(bob_data_dir()).await?;
    alice_client.sync_state().await?;
    bob_client.sync_state().await?;

    let token = fee_token(&alice_client).await?;
    let alice = match std::env::args().nth(1) {
        Some(text) => parse_account_id(&text)?,
        None => richest_account(&alice_client, token).await?,
    };

    // STEP 1: Bob makes a wallet on HIS client and shares only his ID.
    // --------------------------------------------------------------------------------------------
    let bob = new_wallet(&mut bob_client, AccountType::Public).await?.id();
    println!("alice: {alice}  (in data/testnet)");
    println!(
        "bob:   {bob}  (in data/testnet-bob)  {}",
        bob.to_bech32(NetworkId::Testnet)
    );

    // STEP 2: Alice creates a PRIVATE note for Bob.
    // --------------------------------------------------------------------------------------------
    let alice_before = token_balance(&alice_client, alice, token).await?;
    let payment =
        PaymentNoteDescription::new(vec![FungibleAsset::new(token, AMOUNT)?.into()], alice, bob);
    let request = TransactionRequestBuilder::new().build_pay_to_id(
        payment,
        NoteType::Private,
        alice_client.rng(),
    )?;
    let note_id = request.expected_output_own_notes()[0].id();

    let alice_tx = alice_client.submit_new_transaction(alice, request).await?;
    let block = wait_for_commit(&mut alice_client, alice_tx, TIMEOUT).await?;
    println!(
        "\nalice sent a PRIVATE note {} in block {block}",
        note_id.to_hex()
    );
    println!(
        "  see it: https://testnet.midenscan.com/note/{}",
        note_id.to_hex()
    );
    println!(
        "  alice balance {} -> {}",
        alice_before,
        token_balance(&alice_client, alice, token).await?
    );

    // STEP 3: Bob syncs BEFORE delivery. He has no way to use the note.
    // --------------------------------------------------------------------------------------------
    let summary = bob_client.sync_state().await?;
    let bob_notes = bob_client.get_consumable_notes(Some(bob)).await?;
    println!("\nbob syncs before delivery:");
    println!("  new private notes: {}", summary.new_private_notes.len());
    println!(
        "  consumable notes:  {}   <- the note is on chain, but Bob cannot see it",
        bob_notes.len()
    );

    // STEP 4: Alice delivers the note details through the note transport mailbox.
    // --------------------------------------------------------------------------------------------
    // The mailbox wants proof that the note is on chain. Alice's client gets that inclusion
    // proof during a sync after the commit, so sync until the output note record has it.
    let started = Instant::now();
    let (note, proof) = loop {
        let record = alice_client
            .get_output_note(note_id)
            .await?
            .context("note is tracked")?;
        if let Some(proof) = record.inclusion_proof().cloned() {
            let note: Note = record.try_into()?;
            break (note, proof);
        }
        anyhow::ensure!(
            started.elapsed() < TIMEOUT,
            "no inclusion proof after {TIMEOUT:?}"
        );
        tokio::time::sleep(Duration::from_secs(3)).await;
        alice_client.sync_state().await?;
    };
    alice_client
        .send_private_note_with_proof(note, &Address::new(bob), proof)
        .await?;
    println!("\nalice put the note details in the mailbox for Bob's tag");

    // STEP 5: Bob syncs again. Sync also reads the mailbox.
    // --------------------------------------------------------------------------------------------
    let records = wait_for_consumable_notes(&mut bob_client, bob, TIMEOUT).await?;
    println!(
        "bob's sync fetched the note from the mailbox: {} note(s) consumable",
        records.len()
    );

    let notes: Vec<Note> = records
        .into_iter()
        .map(TryInto::try_into)
        .collect::<Result<_, _>>()?;
    let request = TransactionRequestBuilder::new().build_consume_notes(notes)?;
    let bob_tx = bob_client.submit_new_transaction(bob, request).await?;
    let block = wait_for_commit(&mut bob_client, bob_tx, TIMEOUT).await?;
    println!(
        "bob consumed it in block {block}; bob balance: {}",
        token_balance(&bob_client, bob, token).await?
    );

    Ok(())
}
