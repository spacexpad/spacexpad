# SpaceXpad

Solana fair-mint launchpad for agent wallets. Devnet only.

SpaceXpad lets anyone launch a token with a fair mint: a fixed share price, a
per-wallet cap (1% of supply + 1 share per referred first-time minter), a 1%
Token-2022 transfer tax split 50% dividends / 20% creator marketing / 20%
burn / 5% project / 5% buyback, and graduation into a Raydium CPMM pool once
the mint sells out. Live site: https://spacexpad.xyz

This repository is the open-source part of SpaceXpad. It contains everything
needed to verify the on-chain program and to build against it. The backend
gate server, the web frontend, and all private keys are not in this repo.

## Deployed program (devnet)

- Program ID: `DMt9jcZTMgHzzZcEngNj8c1Q6ahVdyLACirZwT71NzA6`
- ProgramData: `64FRm9rvR2kbBN4PEfuUpW8mkYkJPGdN78jctoGKCJmb`
- Version: v4.3.1 in this source. The on-chain program is still v4.3.0 (`.so` 837,608 bytes, sha256 `acaa7ce0…`); the v4.3.1 upgrade is pending. This source builds to 848,648 bytes, sha256 `001b5b13…` — that hash must match the chain after the upgrade lands.
- Cluster: devnet only. No mainnet deployment exists.

## Verify the deployed program matches this source

The program is the trust anchor: anyone can rebuild it from this source and
compare the resulting `.so` hash with the bytes stored on-chain.

```sh
cargo build-sbf --manifest-path programs/agentpad/Cargo.toml
# compare the sha256 of the built .so with the on-chain ProgramData account:
# read ProgramData from offset 45, take as many bytes as the local .so,
# and hash them. The two hashes must match.
```

Build notes: Anchor 0.30.1, Rust 1.82.0 for the program build
(`cargo-build-sbf --manifest-path programs/agentpad/Cargo.toml`). Do not
run a full `cargo metadata` on this workspace (it upgrades `Cargo.lock`
to v4, which the Solana 1.18 BPF toolchain cannot read). `anchor build`
currently cannot generate the IDL on any installed toolchain
(ark-bn254's MontFp proc-macro panics on 1.81/1.82/1.85; 1.79 is too
old for wasm-bindgen), so `idl/agentpad.json` is hand-maintained:
address + new error entries only (the account list is unchanged).

## What's in this repo

| Path | What it is |
| --- | --- |
| `programs/agentpad/` | The Solana program (Anchor/Rust): `lib.rs`, `state.rs`, `errors.rs`, `constants.rs`. The source of truth. |
| `idl/agentpad.json` | Anchor IDL for the program (hand-maintained while `anchor build` cannot generate it; see build notes). |
| `sdk/` | JavaScript SDK (`agentpad-sdk.mjs`, `browser-entry.mjs`) for building mint / refund / claim / swap transactions. |
| `shared/constants.json` | Single source of truth for program id, cluster, tokenomics, fee splits, and the dividend whitelist. |
| `skill/` | `SKILL.md` — the agent skill for interacting with SpaceXpad (also served at https://spacexpad.xyz/skill.md). |
| `docs/` | Design notes: minimum-raise check, mainnet readiness, high-concurrency deploy plan. |

## What's NOT in this repo (and why)

- **Gate server** — the backend that co-signs launches (the `OFFICIAL` label
  and the public-launch gate). It holds a persistent signing key; publishing
  it would let anyone mint the official label. The gate's public key is in
  `shared/constants.json` (`gateSignerPubkey`).
- **Web frontend** — the site at spacexpad.xyz. Trust comes from the on-chain
  program, not from the UI.
- **Private keys** — deployer, gate signer, graduator. They never enter any
  repository.

## Key parameters

- Total supply: 1,000,000,000 (6 decimals); 625M public mint / 375M LP.
- Total shares per launch: 1,000–50,000 (step 1,000).
- Personal cap: 100 bps of total shares + 1 per referred first-time minter.
- Graduation split: LP 60% / creator 35% / buyback 2.5% / treasury 2.5%.
- Minimum raise check: `total_shares x price x 60%` must cover the Raydium
  pool-creation fee + 0.1 SOL (error 6036 `RaiseTooSmallForPoolCreation`).
- Dividend claim requires holding >= 200,000 tokens.

## License

MIT. See `LICENSE`.
