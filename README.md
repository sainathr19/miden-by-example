# Miden by Example

[![CI](https://github.com/sainathr19/miden-by-example/actions/workflows/ci.yml/badge.svg)](https://github.com/sainathr19/miden-by-example/actions/workflows/ci.yml)

Twelve runnable Rust programs that walk through the [Miden](https://miden.xyz) rollup on testnet, one concept at a time. They cover accounts, notes, public and private transfers, private accounts, your own token, timelocks, swaps, Miden Assembly and the raw node RPC.

Each example is a single file, and its header explains what happens and why. They build on each other and use [`miden-client`](https://crates.io/crates/miden-client) 0.18.

## Quick start

You need Rust 1.98.1 or newer. `rust-toolchain.toml` pins 1.99.1, and rustup installs it on the first build.

```bash
git clone https://github.com/sainathr19/miden-by-example
cd miden-by-example
cargo run --release --example 01_sync
```

Always use `--release`, because the Miden VM is very slow in debug builds. The first build takes a few minutes.

To make transactions, you need testnet tokens:

1. Run `02_new_wallet` and copy the `mtst1...` account ID it prints.
2. Paste the ID into the [testnet faucet](https://faucet.testnet.miden.io/) and choose **Send Public Note**.
3. Run `03_fund_and_consume` to consume the note. This also deploys your account.

The faucet sends the token that testnet charges fees in, so a new account can pay for its own first transaction.

## The examples

| # | Example | What it shows |
|---|---|---|
| 00 | `00_network_info` | Which token pays fees, read from the protocol config that every block header commits to |
| 01 | `01_sync` | Building a client, pinning genesis, and what a sync fetches when nothing is tracked |
| 02 | `02_new_wallet` | An account is components plus a seed, and its ID exists before the network sees it |
| 03 | `03_fund_and_consume` | The first transaction deploys the account and pays its fee from the note it consumes |
| 04 | `04_send_public` | A transfer is two transactions, one per account: the sender's vault to a note, then the note to the receiver's vault |
| 05 | `05_send_private` | Private notes with two separate clients: the receiver sees nothing until the details arrive off-chain |
| 06 | `06_private_account` | The node holds only a commitment. The example backs up, "loses the device" and restores |
| 07 | `07_own_faucet` | A token is a faucet account, and the faucet ID is the token ID. Mint, fund and deploy |
| 08 | `08_timelock` | P2IDE timelock and reclaim, checked against the transaction's reference block |
| 09 | `09_swap` | A SWAP note is an on-chain order filled atomically by the taker. The private payback note needs no delivery |
| 10 | `10_masm_scripts` | Your own Miden Assembly: a read-only script, a custom assertion error, and a dry run of a full transaction |
| 11 | `11_raw_rpc` | No client and no store: block headers, blocks, notes by tag, and nullifier lookups by prefix |

## Things worth knowing

The examples surfaced these protocol behaviours. Each one is shown in the example noted.

- **Screening a note can never return plain `Consumable` for a signature-protected account.** Consumability checks run without your keys, so a note you can spend shows up as `ConsumableWithAuthorization` (03).
- **Fees track the size of the proof, not each operation.** One run paid 119 = 7 × 17, consistent with `base_fee × (log2(cycles) + 1)` (03, 10).
- **Every transaction pays its fee by creating a small fee note.** The auth component also bumps the nonce, even when nothing else changes (10).
- **A private note's details never reach the chain.** They travel through the note-transport mailbox, which stores them in plaintext in this version (05).
- **A private account backup goes stale after the account's next transaction.** If you restore an old backup, the commitment no longer matches the chain and sync locks the account (06).
- **A tag is only the first 14 bits of an account ID.** Many accounts share each tag, so your client receives strangers' notes too. Nullifier lookups send only a 16-bit prefix (11).
- **The private payback note of a swap is found without delivery.** The SWAP note fixes its details in advance, so the creator already knows it (09).

## How it is organised

```
src/lib.rs     shared helpers: testnet client, account picking, waiting for commits and notes
examples/      one file per lesson
data/          local client state: SQLite stores and secret keys (git-ignored)
```

Some examples use a second, fully separate client in `data/testnet-bob/` (and `data/testnet-carol/`) to act as another person on another device. Delete `data/` to start over.

## Versions and caveats

- Built against `miden-client` 0.18.0 and a testnet node running 0.18.x. Miden is moving fast: VM 1.0 changes the native hash function, so expect breaking changes on the next releases.
- **Testnet only.** `data/` holds real secret keys for your testnet accounts and is never committed. Do not reuse these keys anywhere else.

## License

MIT
