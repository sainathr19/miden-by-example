//! Lesson 7: create your own token. A faucet is just an account with different components.
//!
//! Run: `cargo run --release --example 07_own_faucet [-- <alice-account-id>]`
//!   Change `SYMBOL`, `NAME`, `DECIMALS` below to name your token.
//!
//! On Miden there is no token contract that keeps a balance table. A token is:
//! - a FAUCET account that is allowed to create ("mint") assets of its own kind, and
//! - assets that carry the faucet's account ID. The faucet ID IS the token ID.
//!
//! Balances live in each holder's own vault, not in the faucet. The faucet only tracks how much
//! it has issued. That is why you could hold the fee token in lesson 3 without the fee faucet
//! ever touching your account.
//!
//! The faucet account is built from components, like the wallet in lesson 2:
//! - `AuthSingleSig`      who may run the faucet's transactions (your new key)
//! - `FungibleFaucet`     the token: name, symbol, decimals, max supply; the mint/burn code
//! - `TokenPolicyManager` extra rules for mint and burn (here: no extra rules)
//! - `BasicWallet`        lets the faucet RECEIVE assets, so it can be funded with fee tokens
//!
//! What happens (4 transactions):
//! 1. Create the faucet locally (not on chain yet).
//! 2. Alice sends the faucet some fee tokens. Minting costs a fee, and the faucet pays it.
//! 3. The faucet consumes that note. Its first transaction, so this deploys it.
//! 4. The faucet mints tokens for Alice. Minting creates a P2ID note that carries NEW assets.
//!    The kernel checks that a faucet can only mint assets with its own ID.
//! 5. Alice consumes the mint note. Her vault now holds two kinds of tokens.
//!
//! Try next: send some of your token to Bob with lesson 4's code, and view the faucet on
//! midenscan.

use std::time::Duration;

use miden_client::account::component::{
    BasicWallet, BurnPolicy, FungibleFaucet, MintPolicy, TokenName, TokenPolicyManager,
};
use miden_client::account::{AccountBuilder, AccountBuilderSchemaCommitmentExt, AccountType};
use miden_client::address::NetworkId;
use miden_client::asset::{AssetAmount, FungibleAsset, TokenSymbol};
use miden_client::auth::{Approver, AuthSchemeId, AuthSecretKey, AuthSingleSig};
use miden_client::keystore::Keystore;
use miden_client::note::{Note, NoteType};
use miden_client::transaction::{PaymentNoteDescription, TransactionRequestBuilder};
use miden_by_example::{
    fee_token, parse_account_id, richest_account, testnet_client, token_balance, wait_for_commit,
    wait_for_consumable_notes,
};

const SYMBOL: &str = "PLAY";
const NAME: &str = "Playground Token";
const DECIMALS: u8 = 6;
const MAX_SUPPLY: u64 = 1_000_000_000_000; // 1,000,000 tokens at 6 decimals
const MINT_AMOUNT: u64 = 500_000_000; // 500 tokens
const FEE_FUNDING: u64 = 1_000_000; // 1 fee token, enough for several faucet transactions
const TIMEOUT: Duration = Duration::from_secs(3 * 60);

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut client = testnet_client().await?;
    client.sync_state().await?;
    let fee = fee_token(&client).await?;
    let alice = match std::env::args().nth(1) {
        Some(text) => parse_account_id(&text)?,
        None => richest_account(&client, fee).await?,
    };

    // STEP 1: build the faucet account from components.
    // --------------------------------------------------------------------------------------------
    let key = AuthSecretKey::new_falcon512_poseidon2();
    let auth = AuthSingleSig::new(Approver::new(
        key.public_key().to_commitment(),
        AuthSchemeId::Falcon512Poseidon2,
    ));
    let token_component = FungibleFaucet::builder()
        .name(TokenName::new(NAME)?)
        .symbol(TokenSymbol::new(SYMBOL)?)
        .decimals(DECIMALS)
        .max_supply(AssetAmount::new(MAX_SUPPLY)?)
        .build()?;
    let policies = TokenPolicyManager::builder()
        .active_mint_policy(MintPolicy::allow_all())
        .active_burn_policy(BurnPolicy::allow_all())
        .build();

    let seed: [u8; 32] = rand::random();
    let faucet_account = AccountBuilder::new(seed)
        .account_type(AccountType::Public)
        .with_component(auth)
        .with_component(token_component)
        .with_component(BasicWallet)
        .with_components(policies)
        .build_with_schema_commitment()?;
    let faucet = faucet_account.id();

    let keystore = client.authenticator().expect("keystore is set").clone();
    keystore.add_key(&key, faucet).await?;
    client.add_account(&faucet_account, false).await?;

    println!("token {SYMBOL} ({NAME}), {DECIMALS} decimals");
    println!(
        "faucet = token ID: {faucet}  ({})",
        faucet.to_bech32(NetworkId::Testnet)
    );
    println!(
        "faucet procedures: {}",
        faucet_account.code().num_procedures()
    );
    println!("faucet storage slots:");
    for slot in faucet_account.storage().slots() {
        println!("  {}", slot.name());
    }

    // STEP 2: Alice funds the faucet with fee tokens.
    // --------------------------------------------------------------------------------------------
    let payment = PaymentNoteDescription::new(
        vec![FungibleAsset::new(fee, FEE_FUNDING)?.into()],
        alice,
        faucet,
    );
    let request = TransactionRequestBuilder::new().build_pay_to_id(
        payment,
        NoteType::Public,
        client.rng(),
    )?;
    let tx = client.submit_new_transaction(alice, request).await?;
    wait_for_commit(&mut client, tx, TIMEOUT).await?;
    println!("\nalice sent the faucet {FEE_FUNDING} fee tokens");

    // STEP 3: the faucet consumes the funding note. This deploys the faucet.
    // --------------------------------------------------------------------------------------------
    let records = wait_for_consumable_notes(&mut client, faucet, TIMEOUT).await?;
    let notes: Vec<Note> = records
        .into_iter()
        .map(TryInto::try_into)
        .collect::<Result<_, _>>()?;
    let request = TransactionRequestBuilder::new().build_consume_notes(notes)?;
    let tx = client.submit_new_transaction(faucet, request).await?;
    let block = wait_for_commit(&mut client, tx, TIMEOUT).await?;
    println!("faucet deployed in block {block}");

    // STEP 4: the faucet mints new tokens into a P2ID note for Alice.
    // --------------------------------------------------------------------------------------------
    let minted = FungibleAsset::new(faucet, MINT_AMOUNT)?;
    let request = TransactionRequestBuilder::new().build_mint_fungible_asset(
        minted,
        alice,
        NoteType::Public,
        client.rng(),
    )?;
    let tx = client.submit_new_transaction(faucet, request).await?;
    let block = wait_for_commit(&mut client, tx, TIMEOUT).await?;
    println!("minted {MINT_AMOUNT} {SYMBOL} base units for alice in block {block}");

    // STEP 5: Alice consumes the mint note.
    // --------------------------------------------------------------------------------------------
    let records = wait_for_consumable_notes(&mut client, alice, TIMEOUT).await?;
    let notes: Vec<Note> = records
        .into_iter()
        .map(TryInto::try_into)
        .collect::<Result<_, _>>()?;
    let request = TransactionRequestBuilder::new().build_consume_notes(notes)?;
    let tx = client.submit_new_transaction(alice, request).await?;
    wait_for_commit(&mut client, tx, TIMEOUT).await?;

    println!("\nalice's vault now:");
    println!("  fee token: {}", token_balance(&client, alice, fee).await?);
    println!(
        "  {SYMBOL}:      {}",
        token_balance(&client, alice, faucet).await?
    );
    println!(
        "faucet's own vault (fee tokens left): {}",
        token_balance(&client, faucet, fee).await?
    );

    Ok(())
}
