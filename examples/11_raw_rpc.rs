//! Lesson 11: talk to the node directly. No store, no keys, no sync: raw RPC.
//!
//! Run: `cargo run --release --example 11_raw_rpc [-- <account-id>]`
//! Read-only and free.
//!
//! This is how Miden looks to a relayer, an indexer, or an explorer like midenscan. The local
//! store is used only to remember IDs from earlier lessons (Alice's ID, a note she consumed).
//! Every question below goes straight to the node over gRPC. The full API is in
//! [`rpc.proto`](https://github.com/0xMiden/node/blob/v0.18.2/proto/proto/rpc.proto).
//!
//! What it asks:
//! 1. Status: node version and chain tip.
//! 2. The latest block header: the whole chain state compressed into a few roots.
//! 3. Alice's transaction history (`SyncTransactions`).
//! 4. One full block: its transactions, new notes, spent nullifiers, validator signatures.
//! 5. Every note under Alice's tag (`SyncNotes`). Public notes come with details, private notes
//!    only with metadata. Other accounts share the tag, so not all of them are Alice's.
//! 6. One public and one private note by ID (`GetNotesById`).
//! 7. "Has this note been spent, and when?" (`SyncNullifiers`, by 16-bit prefix).
//!
//! What the node does NOT have: private account state, private note details, your keys.
//! Everything private stays on the devices of the people involved.

use std::collections::BTreeSet;

use miden_client::account::AccountId;
use miden_client::block::BlockNumber;
use miden_client::note::NoteType;
use miden_client::rpc::domain::note::FetchedNote;
use miden_client::rpc::{Endpoint, GrpcClient, NodeRpcClient};
use miden_client::store::{InputNoteState, NoteFilter};
use miden_by_example::{fee_token, parse_account_id, richest_account, testnet_client};

/// How far back to look, in blocks. Blocks come about every 3 seconds, so this is about 2 days.
const LOOKBACK: u32 = 60_000;

fn short(hex: String) -> String {
    format!("{}...", &hex[..14])
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // IDs from earlier lessons, read from the local store. No network call here.
    let local = testnet_client().await?;
    let alice: AccountId = match std::env::args().nth(1) {
        Some(text) => parse_account_id(&text)?,
        None => richest_account(&local, fee_token(&local).await?).await?,
    };

    // A bare gRPC client. Like the full client, pin the genesis commitment first: every request
    // then says which chain it expects.
    let rpc = GrpcClient::new(&Endpoint::testnet(), 10_000);
    let (genesis, _) = rpc
        .get_block_header_by_number(Some(BlockNumber::from(0u32)), false)
        .await?;
    rpc.set_genesis_commitment(genesis.commitment()).await?;

    // 1. STATUS
    // --------------------------------------------------------------------------------------------
    let status = rpc.get_status_unversioned().await?;
    println!("== 1. node status");
    println!(
        "  version {}, chain tip {}",
        status.version, status.chain_tip
    );

    // 2. LATEST BLOCK HEADER
    // --------------------------------------------------------------------------------------------
    let (tip, _) = rpc.get_block_header_by_number(None, false).await?;
    println!("\n== 2. latest block header: block {}", tip.block_num());
    println!("  timestamp        {}", tip.timestamp());
    println!(
        "  account root     {}  <- every account's commitment, in one tree",
        short(tip.account_root().to_hex())
    );
    println!(
        "  nullifier root   {}  <- every spent note, in one tree",
        short(tip.nullifier_root().to_hex())
    );
    println!(
        "  note root        {}  <- notes created in THIS block",
        short(tip.note_root().to_hex())
    );
    println!(
        "  chain commitment {}  <- all previous block headers (the MMR)",
        short(tip.chain_commitment().to_hex())
    );
    println!(
        "  base fee         {}",
        tip.fee_parameters().verification_base_fee()
    );

    let from = BlockNumber::from(tip.block_num().as_u32().saturating_sub(LOOKBACK));
    let to = tip.block_num();

    // 3. ALICE'S TRANSACTIONS
    // --------------------------------------------------------------------------------------------
    let txs = rpc.sync_transactions(from, to, vec![alice]).await?;
    println!(
        "\n== 3. transactions of {alice} in blocks {from}..{to}: {}",
        txs.len()
    );
    for tx in &txs {
        println!(
            "  block {}  tx {}  output notes: {}",
            tx.block_num,
            short(tx.transaction_header.id().to_hex()),
            tx.output_notes.len()
        );
    }

    // 4. ONE FULL BLOCK
    // --------------------------------------------------------------------------------------------
    if let Some(last) = txs.last() {
        let (block, _proof) = rpc.get_block_by_number(last.block_num, false).await?;
        let body = block.body();
        println!(
            "\n== 4. block {} (the block of Alice's latest transaction)",
            last.block_num
        );
        println!("  validator signatures: {}", block.signatures().len());
        println!(
            "  transactions:         {}",
            body.transactions().as_slice().len()
        );
        for header in body.transactions().as_slice() {
            let marker = if header.account_id() == alice {
                "  <- alice"
            } else {
                ""
            };
            println!(
                "    tx {} by account {}{marker}",
                short(header.id().to_hex()),
                header.account_id()
            );
        }
        println!("  new notes:            {}", body.output_notes().count());
        println!(
            "  spent nullifiers:     {}",
            body.created_nullifiers().len()
        );
        println!("  updated accounts:     {}", body.updated_accounts().len());
    }

    // 5. EVERY NOTE UNDER ALICE'S TAG
    // --------------------------------------------------------------------------------------------
    let alice_tag = miden_client::note::NoteTag::with_account_target(alice);
    let blocks = rpc
        .sync_notes(from, to, &BTreeSet::from([alice_tag]))
        .await?;
    let notes: Vec<_> = blocks
        .iter()
        .flat_map(|block| block.notes.values())
        .collect();
    let public = notes
        .iter()
        .filter(|note| note.note_type() == NoteType::Public)
        .count();
    println!(
        "\n== 5. notes with alice's tag {alice_tag}: {} ({public} public, {} private)",
        notes.len(),
        notes.len() - public
    );
    for note in notes.iter().take(12) {
        println!(
            "  {}  {:?}  from {}",
            short(note.note_id().to_hex()),
            note.note_type(),
            note.sender()
        );
    }

    // 6. ONE PUBLIC AND ONE PRIVATE NOTE BY ID
    // --------------------------------------------------------------------------------------------
    println!("\n== 6. the same question for a public and a private note:");
    let mut wanted = Vec::new();
    for kind in [NoteType::Public, NoteType::Private] {
        if let Some(note) = notes.iter().find(|note| note.note_type() == kind) {
            wanted.push(*note.note_id());
        }
    }
    for fetched in rpc.get_notes_by_id(&wanted).await? {
        match fetched {
            FetchedNote::Public(note, _proof) => println!(
                "  PUBLIC  {}: full note. {} asset(s), script root {}, storage items {}",
                short(note.id().to_hex()),
                note.assets().iter().count(),
                short(note.script().root().to_hex()),
                note.recipient().storage().num_items()
            ),
            FetchedNote::Private(id, metadata, _attachments, _proof) => println!(
                "  PRIVATE {}: metadata only (sender {}, tag {}). No assets, no script.",
                short(id.to_hex()),
                metadata.sender(),
                metadata.tag()
            ),
        }
    }

    // 7. HAS THIS NOTE BEEN SPENT?
    // --------------------------------------------------------------------------------------------
    // Take a note that THIS client consumed in an earlier lesson. Its nullifier needs the note's
    // secret details, so only someone who knows the note can compute it. The node only ever sees
    // the nullifier itself.
    //
    // Why not any consumed note: the store also holds notes "consumed externally". Some of them
    // are strangers' private notes that the note-transport mailbox delivered because their tag
    // shares the first 14 bits with one of your accounts. They can be weeks old, outside the
    // LOOKBACK window.
    let consumed = local.get_input_notes(NoteFilter::Consumed).await?;
    let own = consumed.iter().filter(|record| {
        matches!(
            record.state(),
            InputNoteState::ConsumedAuthenticatedLocal(_)
                | InputNoteState::ConsumedUnauthenticatedLocal(_)
        )
    });
    if let Some(nullifier) = own.filter_map(|record| record.nullifier()).next_back() {
        println!("\n== 7. is nullifier {} spent?", short(nullifier.to_hex()));
        println!(
            "  the request sends only its 16-bit prefix: {:#06x}",
            nullifier.prefix()
        );
        let updates = rpc.sync_nullifiers(&[nullifier.prefix()], from, to).await?;
        println!(
            "  the node returned {} nullifier(s) with that prefix",
            updates.len()
        );
        match updates.iter().find(|update| update.nullifier == nullifier) {
            Some(update) => println!("  ours is among them: spent in block {}", update.block_num),
            None => println!("  ours is not among them in this range"),
        }
    }

    Ok(())
}
