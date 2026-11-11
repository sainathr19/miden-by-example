//! Lesson 10: write your own Miden Assembly (MASM) and run it against your account.
//!
//! Run: `cargo run --release --example 10_masm_scripts [-- <account-id>]`
//! Costs NOTHING: no part of this lesson submits a transaction.
//!
//! A transaction script is the "main function" of a transaction. Every transaction you made so
//! far had one: the client generated it (for example, "create this P2ID note"). Here you write
//! your own. MASM is a stack machine language: instructions push and pop field elements on a
//! stack, and procedures say in comments what the stack looks like before and after.
//!
//! Part A: a read-only script, run with `execute_program`. It reads values that the protocol
//!   library exposes (account ID, reference block, timestamp), does some math, and leaves results
//!   on the stack. The client returns the final stack. No proof, no fee, nothing is sent.
//!
//! Part B: a script that fails on purpose with YOUR error message. The error travels from the
//!   VM back to Rust, so you can see exactly which assertion failed.
//!
//! Part C: a dry run of a full transaction with `execute_transaction`. It runs everything a real
//!   transaction runs, including your account's auth component (fee + nonce + signature), but it
//!   stops before proving and sending. You see what WOULD change.
//!
//! Something to notice: your script cannot read your balance directly. Reading the vault is
//! allowed only from the account's own code ("account context"), and `BasicWallet` exports no
//! "get balance" procedure. A script can only call what the account's components export. That
//! is the account's API. A custom component could add a getter.
//!
//! Read alongside:
//! - [`tx.masm`] and [`active_account.masm`]: the procedures used here, with their stack
//!   signatures.
//! - [`tx_scripts/`]: real scripts the client uses.
//!
//! [`tx.masm`]: https://github.com/0xMiden/protocol/blob/v0.18.0/crates/miden-protocol/asm/protocol/src/tx.masm
//! [`active_account.masm`]: https://github.com/0xMiden/protocol/blob/v0.18.0/crates/miden-protocol/asm/protocol/src/active_account.masm
//! [`tx_scripts/`]: https://github.com/0xMiden/protocol/blob/v0.18.0/crates/miden-standards/asm/standards/tx_scripts

use std::collections::BTreeMap;

use miden_by_example::{fee_token, pick_account, richest_account, testnet_client};
use miden_client::account::AccountId;
use miden_client::transaction::{RawOutputNote, TransactionRequestBuilder};
use miden_client::vm::AdviceInputs;

/// Part A. The stack is a list of field elements; the left end is the top.
/// A transaction script starts with 16 elements on the stack: its argument word plus padding.
/// `truncate_stack` cuts the stack back to 16 at the end, keeping the top 16.
const READ_ONLY_SCRIPT: &str = "
use miden::core::sys
use miden::protocol::active_account
use miden::protocol::tx

@transaction_script
pub proc main
    # some plain arithmetic first: 6 * 7
    push.6 push.7 mul
    # => [42]

    exec.tx::get_block_timestamp
    # => [timestamp, 42]

    exec.tx::get_reference_block_number
    # => [block_num, timestamp, 42]

    exec.active_account::get_id
    # => [id_suffix, id_prefix, block_num, timestamp, 42]

    exec.sys::truncate_stack
end
";

/// Part B. `{min_block}` is filled in from Rust before compiling.
/// `gte` pops [b, a] and pushes 1 if a >= b. `assert` fails if the top is not 1.
const FAILING_SCRIPT: &str = "
use miden::protocol::tx

const ERR_TOO_EARLY = \"miden-by-example: the reference block is below the required minimum\"

@transaction_script
pub proc main
    exec.tx::get_reference_block_number
    # => [block_num]

    push.{min_block}
    # => [min_block, block_num]

    gte
    # => [block_num >= min_block]

    assert.err=ERR_TOO_EARLY
    # => []
end
";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut client = testnet_client().await?;
    client.sync_state().await?;
    let account: AccountId = match std::env::args().nth(1) {
        Some(text) => pick_account(&client, Some(text)).await?,
        None => richest_account(&client, fee_token(&client).await?).await?,
    };
    let sync_height = client.get_sync_height().await?;
    println!("account: {account}   synced to block {sync_height}");

    // PART A: a read-only script.
    // --------------------------------------------------------------------------------------------
    let script = client.code_builder().compile_tx_script(READ_ONLY_SCRIPT)?;
    let stack = client
        .execute_program(account, script, AdviceInputs::default(), BTreeMap::new())
        .await?;

    println!("\n== part A: read-only script, final stack (top first):");
    let labels = ["id suffix", "id prefix", "ref block", "timestamp", "6 * 7"];
    for (label, felt) in labels.iter().zip(stack.iter()) {
        println!("  {label:<10} {}", felt.as_canonical_u64());
    }
    println!(
        "  check: id matches = {}, block matches = {}",
        stack[0] == account.suffix() && stack[1] == account.prefix().as_felt(),
        stack[2].as_canonical_u64() == u64::from(sync_height.as_u32()),
    );

    // PART B: a script that fails with your own error message.
    // --------------------------------------------------------------------------------------------
    let min_block = sync_height.as_u32() + 1_000_000;
    let code = FAILING_SCRIPT.replace("{min_block}", &min_block.to_string());
    let script = client.code_builder().compile_tx_script(&code)?;

    println!("\n== part B: assert that the block is at least {min_block}:");
    match client
        .execute_program(account, script, AdviceInputs::default(), BTreeMap::new())
        .await
    {
        Ok(_) => println!("  unexpected: the assertion passed"),
        Err(err) => {
            println!("  failed as expected:");
            let mut current: Option<&dyn std::error::Error> = Some(&err);
            while let Some(cause) = current {
                println!("    {cause}");
                current = cause.source();
            }
        }
    }

    // PART C: dry-run a full transaction that uses the part A script.
    // --------------------------------------------------------------------------------------------
    let script = client.code_builder().compile_tx_script(READ_ONLY_SCRIPT)?;
    let request = TransactionRequestBuilder::new()
        .custom_script(script)
        .build()?;
    let result = client.execute_transaction(account, request).await?;

    println!("\n== part C: dry run of a full transaction (executed, NOT proved, NOT sent)");
    let before = client
        .get_account(account)
        .await?
        .expect("account is tracked")
        .nonce();
    let patch = result.account_patch();
    println!(
        "  nonce:        {before} -> {}  (the auth component bumps it on every transaction)",
        patch
            .final_nonce()
            .map_or("unchanged".to_string(), |n| n.to_string())
    );
    println!("  output notes: {}", result.created_notes().num_notes());
    for note in result.created_notes().iter() {
        let kind = match note {
            RawOutputNote::Full(_) => "full",
            RawOutputNote::Partial(_) => "partial",
        };
        for asset in note.assets().iter() {
            println!(
                "    {kind} note {}...: carries {} of {}  <- the transaction fee",
                &note.id().to_hex()[..18],
                asset.unwrap_fungible().amount(),
                asset.faucet_id()
            );
        }
    }
    println!("  nothing was submitted: your account and balance are unchanged");

    Ok(())
}
