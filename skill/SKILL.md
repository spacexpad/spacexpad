---
name: spacexpad-solana
description: Interact with the SpaceXpad Solana Fair-Mint Launchpad (https://spacexpad.xyz) using any Solana-compatible Agent Wallet. Supports preflight, minting shares, lossless 0% refund, claiming tokens, trading graduated tokens (direct Raydium CPMM swap), listing whitelisted dividend stock tokens, and claiming SOL or stock-token dividends.
---
# SpaceXpad Solana Skill (`agentpad`)

Domain endpoint: `https://spacexpad.xyz/skill.md`

Use this skill when the user wants to launch, mint, refund, claim tokens, or claim dividends (in SOL or whitelisted stock tokens such as `NVDAx`, `TSLAx`, `AAPLx`, `MSTRx`, `ANTx`, `SPCXx`) on SpaceXpad.

## Deployment status (2026-11-23, verified)
- The program is **deployed to devnet** (v4.3.1 live on-chain since 2026-11-23, upgrade tx `3T96Z7iL1PRDY6b5AwTR9hAkTQWMkfXnfMukRQaYKmECQWXGfegWRWG7XNfq8NnRE1s8gPaExTpnBTV1wuPDfGsi`, .so 848,648 bytes sha256 `001b5b13…` verified against ProgramData offset 45): program id `DMt9jcZTMgHzzZcEngNj8c1Q6ahVdyLACirZwT71NzA6`. The v2 program (`J4SUMwoH...`) was closed and its 3.68278156 SOL rent refunded; its launches (SXPAD, test A/B) are orphaned. Test site: `https://spacexpad.xyz/`.
- **v4.3.1 behavior (built, pending on-chain upgrade)**: a wallet that never minted (swap buyer / transferee) books its dividend debt at the CURRENT accumulator on first claim and is paid zero history — only future accrual is claimable. A first dividend claim by such a wallet succeeds with a zero payout (that is the booking, not an error). `claim_tokens` now requires the claim vault to be the pool's ATA for the mint. The gate only co-signs `mint_shares` / `refund_shares` for the session wallet (plus Compute Budget); any other instruction is refused.
- **Real devnet mode, no demo**: the launch list starts empty and shows only real on-chain launches. No demo tokens, no fake history.
- `shared/constants.json` is the single source of truth for program id, cluster, tokenomics, whitelist, and Raydium config.
- Gate signer (persistent keypair in `server/.gate-keypair.json`): `Ax2f6Eu3yTA6uow9TqASrA9Xm22kL9fgJktZsaEx7hob`. When running `initialize_global`, pass exactly this pubkey as `gate_signer`, or the gate server cannot co-sign.

## Official label & launch gate (v2 rules)
- **OFFICIAL** = the launch was created by the admin (contract deployer) or co-signed by the backend gate signer. The old "first launch is automatically official" rule is gone.
- **Launch switch** (`public_launches_enabled` in GlobalConfig): **off by default**. When off, only the admin (or a gate co-signed creation) can create a launch; ordinary users are rejected on-chain (`LaunchesDisabled`, 6030) and see "创建未开放" in the page. Flip it with: `node scripts/set_launches_enabled.mjs <true|false>` (deploy wallet = admin).

## Core Safety & Gate Rules
1. **Agent-Only Gate**: Every `mint` and `refund` transaction requires a co-signature from the platform `AgentGateSigner`. Flow: `GET /api/agent/challenge?wallet=<PK>` → sign the challenge message with the Agent Wallet → `POST /api/agent/verify` (returns a 30-minute session) → `POST /api/tx/build` (returns the gate co-signature for the transaction). `/api/agent/attest` is an alias of `/api/tx/build`. On the website, Agent Wallet login offers three options: Binance (QR pairing via the Binance App, or the challenge-message fallback), OKX (QR / login link, confirmed on OKX's own login page with email / Google / Apple), and Phantom Agent Wallet (challenge-message login: the agent signs with Phantom MCP `solana_sign`, `networkId: "solana:devnet"`). Either way the site only learns the wallet address; the agent executes transactions from its own wallet.
2. **Preview Before Signing**: The CLI commands below print JSON previews only — they never build or broadcast a transaction. Browser-wallet transactions happen in the frontend (`frontend/spacexpad.html`), where the user signs with Phantom and the gate server co-signs. Agent-wallet transactions are built fresh by the agent itself (SDK, see the Phantom section below) and sent directly — never from a transaction the site built minutes earlier, because its blockhash expires in ~60-90s. Always show the preview and ask the user to confirm before any signing.
3. **Never Expose Private Keys**: Never ask for or print seed phrases or private keys.

## Phantom Agent Wallet (Phantom MCP)
- Phantom's agent wallet is the `@phantom/mcp-server` MCP server running inside the user's own AI client (Claude Desktop / Cursor / Claude Code). It is NOT a browser extension and the website cannot pop a wallet window for it — login and signing go through prompts, not popups.
- Setup (user side, once): `claude mcp add phantom -- npx -y @phantom/mcp-server@latest` (Claude Code) or the equivalent `mcpServers` entry for Claude Desktop / Cursor. First use opens a browser for device-code sign-in (Google / Apple). Each agent gets its own embedded wallet, separate from the user's personal Phantom wallet — fund it with devnet SOL before minting.
- Login on the site: paste the agent wallet's Solana address → `GET /api/agent/challenge` → ask the agent to sign the challenge with Phantom MCP `solana_sign` (`networkId: "solana:devnet"`) → paste the signature (base58 or base64) → `POST /api/agent/verify` returns a 30-minute gate session.
- Execution (intent prompt): the site's Agent Exec Panel shows an intent prompt — which token, how many shares, how much, inviter — instead of a pre-built transaction. A built transaction dies with its blockhash in ~60-90s, and the copy → paste → agent loop outlasts it, so the site no longer hands the agent a transaction at all. On receiving the intent, load this skill, build the transaction fresh with the SDK (`mintSharesIx` / `refundSharesIx` / `claimTokensIx` / `claimDividendIx` in `sdk/agentpad-sdk.mjs`), get the gate co-signature (challenge → verify → `POST /api/tx/build`), and send it directly with Phantom MCP `solana_send` (`transaction` = base64, `networkId: "solana:devnet"`, `confirmed: true`) — no simulation step. Reference driver: `~/workspace/spacexpad/mint-agent-run.mjs` (proven on devnet 2026-11-19, two mints, no expiry). Devnet (`solana:devnet`) is officially supported by Phantom MCP.

## Intent prompt flow (site → agent)
When the user pastes an intent prompt from the site's Agent Exec Panel (e.g. "铸造 SXPAD ... 1 份 ... 邀请人 ..."):
1. Parse it: token mint (CA), shares, inviter (if any). Show the amounts back to the user and get their go-ahead.
2. Gate session: `GET /api/agent/challenge?wallet=<agent wallet>` → sign the challenge with the agent wallet (`solana_sign`, `networkId: "solana:devnet"`) → `POST /api/agent/verify` (30-minute session).
3. Build the instruction fresh with the SDK (`mintSharesIx` / `refundSharesIx` / `claimTokensIx` / `claimDividendIx`), put it in a transaction with a freshly fetched blockhash, and get the gate co-signature via `POST /api/tx/build`.
- Claiming: `claim_tokens` transfers into the user's Token-2022 associated token account (ATA) for the mint, and the program rejects an uninitialized ATA (Anchor error 3012, "AccountNotInitialized"). A minter who never bought or swapped the token has no ATA — check it first (`getAccountInfo` on the ATA) and prepend a `createAssociatedTokenAccountInstruction` (payer = user, owner = user, Token-2022 program) in the same transaction when it does not exist. Without this step every first-time claim fails. The claim amount is `sharesBought × tokensPerShare` (bonus/invite shares only raise the mint cap, they are not paid out at claim).
4. Send immediately with the agent wallet (`solana_send`, `confirmed: true`) — no simulation step, no second confirmation round. Report the signature and verify it on-chain (`finalized`, no error).
5. Never send a transaction the site built earlier: its blockhash is already stale. If a send fails with "blockhash not found", rebuild with a fresh blockhash and resend.
- Exception: creating a launch and station trades still show a pre-built transaction in the panel (the mint keypair / swap is built on the spot); those keep the old send-the-given-transaction flow. Station trades: build server-side, never in the browser — POST /api/trade/build {mint, side, amountIn, wallet, slippageBps} returns an unsigned tx (the gate does the ~8 direct-RPC reads and retries 429s; the browser doing them itself is what made the wallet popup take minutes, 2026-11-23), sign it, then POST /api/tx/send {signedTx} which confirms by HTTP polling and returns {ok, signature, status: confirmed|failed|unknown} — 'unknown' means sent but unconfirmed, check GET /api/tx/status?sig= before retrying, never re-send blind. New launches (plain mint, no transfer fee) go through the program's `swap_with_tax` (SDK `wrapperSwapIxs`); older launches whose mint carries a transfer fee trade direct against the Raydium pool (`swap_base_input`, SDK `raydiumSwapIxs`) — check which with SDK `getTaxModel`. No Jupiter: Jupiter's public API is mainnet-only and unreachable from this network, so devnet station trades always failed with "Failed to fetch" until the direct-swap fix (2026-11-20). No gate co-sign on swaps. Trade estimates are CPMM constant-product from live pool reserves (SDK `raydiumSwapQuote`), not the reserve ratio — the old linear estimate was ~20% too high on the shallow D pool.

## Running the stack locally
- Gate server: `node agentpad_phase3/server/index.mjs` (default port 8787, reads `shared/constants.json`).
- Check it: `agentpad status` (or `GET /api/health` — `programDeployed` and `gateSignerMatchesOnChain` tell you the live state).
- Frontend: serve `frontend/` over HTTP (e.g. `python3 -m http.server` inside that folder) and open `spacexpad.html`. If the backend or contract is unreachable it shows an honest error, never demo data.

## Command Routing
| Intent | CLI Command |
| :--- | :--- |
| List whitelisted dividend assets | `agentpad whitelist` |
| Check Agent wallet status & gate auth | `agentpad preflight --wallet <SOL_PUBKEY>` |
| Check the live gate server / deployment state | `agentpad status [--server <API_BASE>]` |
| Create a new Scheme A launch | `agentpad create --name "<NAME>" --symbol <SYM> --shares <10000> --price <0.01> --marketing <WALLET> --reward <SOL\|NVDAx\|TSLAx\|AAPLx\|MSTRx\|ANTx\|SPCXx>` |
| Fair-mint shares (with optional inviter) | `agentpad mint --mint <CA> --shares <N> [--invite <INVITER_PUBKEY>]` |
| 0% lossless refund while minting (ends at sell-out) | `agentpad refund --mint <CA> --shares <N>` |
| Claim Token-2022 allocation after graduation | `agentpad claim --mint <CA>` |
| Claim dividends (requires >= 200,000 tokens) | `agentpad dividend --mint <CA> [--reward <SOL\|NVDAx\|...>]` |

## On-chain flow (after deployment)
1. Admin: `initialize_global` (gate_signer = the gate pubkey above), then `set_platform_token`, then `add_dividend_asset` per reward asset. Public launch creation starts **closed**; the admin creates the official platform launch first, then opens the switch when ready.
2. Creator: create a plain Token-2022 mint with no transfer fee (mint authority = the launch pool PDA), then `create_launch`.
3. Users: `mint_shares` (gate co-sign required) → `refund_shares` only while the launch is Minting (0% fee; the refund window ends the moment the launch sells out, when its status flips to Completed in the same mint transaction — do not assume a refund is possible while a sold-out launch waits for graduation) → after graduation the admin calls `graduate_to_raydium` (settles the 60/35/2.5/2.5 split) and `create_raydium_pool` (real CPMM pool via Raydium CPI) → users `claim_tokens`, and `claim_dividend` once they hold >= 200,000 tokens.
4. Admin buyback: `manual_buyback_and_burn` spends SOL from the buyback vault and burns platform tokens for real; stats at `GET /api/buyback/stats`.
