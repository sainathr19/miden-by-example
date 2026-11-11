//! Lesson 2: create a wallet account.
//!
//! Run: `cargo run --release --example 02_new_wallet`
//!
//! Creates a PUBLIC wallet account, saves its key, adds it to the client, and prints its ID in
//! hex and in bech32 (the `mtst1...` form the faucet website accepts).
//!
//! An account is made of components. A wallet needs two:
//! - an auth component, which decides who may change the account (here: one signature key),
//! - `BasicWallet`, which lets the account receive and send assets.
//!
//! Every run creates a NEW account. The client keeps all of them.
//!
//! Questions to answer when it runs:
//! - Run `01_sync` after this. Does the node know about your account yet? Why not?
//!   (Hint: an account exists on chain only after its first transaction.)
//! - The ID was fixed by the seed and the code before anything touched the network. How?
//!
//! Code to read: `crates/rust-client/src/account/mod.rs`, `add_account`.

use miden_client::account::component::BasicWallet;
use miden_client::account::{AccountBuilder, AccountBuilderSchemaCommitmentExt, AccountType};
use miden_client::address::NetworkId;
use miden_client::auth::{Approver, AuthSchemeId, AuthSecretKey, AuthSingleSig};
use miden_client::keystore::Keystore;
use miden_by_example::{data_dir, testnet_client};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut client = testnet_client().await?;
    client.sync_state().await?;

    // 1. Generate a key pair on this machine. The secret key never leaves the keystore.
    let secret_key = AuthSecretKey::new_falcon512_poseidon2();

    // 2. The auth component: "a transaction is valid if the key with this hash signed it".
    //    The account stores only the hash (commitment) of the public key, plus the scheme id.
    let auth = AuthSingleSig::new(Approver::new(
        secret_key.public_key().to_commitment(),
        AuthSchemeId::Falcon512Poseidon2,
    ));

    // 3. A random seed. The account ID is derived from this seed and the account's code and
    //    initial storage, so the ID exists before the network has seen anything.
    let seed: [u8; 32] = rand::random();

    // 4. Assemble the account from its components.
    let account = AccountBuilder::new(seed)
        .account_type(AccountType::Public)
        .with_component(auth)
        .with_component(BasicWallet)
        .build_with_schema_commitment()?;

    // 5. Save the secret key. During every transaction the auth code asks the client for a
    //    signature, and the client finds the secret key here by its public-key hash.
    let keystore = client
        .authenticator()
        .expect("testnet_client sets a keystore")
        .clone();
    keystore.add_key(&secret_key, account.id()).await?;

    // 6. Track the account. This writes it to the local SQLite store only. Nothing is sent to
    //    the node.
    client.add_account(&account, false).await?;

    // 7. Look at what was created.
    println!("account created and tracked locally");
    println!("  id (hex):     {}", account.id());
    println!(
        "  id (bech32):  {}",
        account.id().to_bech32(NetworkId::Testnet)
    );
    println!(
        "  nonce:        {}  (0 = never used on chain)",
        account.nonce()
    );
    println!("  is new:       {}", account.is_new());
    println!("  procedures:   {}", account.code().num_procedures());
    println!("  storage slots:");
    for slot in account.storage().slots() {
        println!("    {}", slot.name());
    }
    println!("  key saved in: {}", data_dir().join("keys").display());

    let tracked = client.get_account_headers().await?;
    println!("accounts tracked by this client: {}", tracked.len());

    Ok(())
}
