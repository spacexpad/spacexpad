/**
 * agentpad SDK — single client for the SpaceXpad Solana launchpad.
 *
 * Everything in this file is derived from two sources of truth and nothing else:
 *   1. `shared/constants.json` — numbers, mints, program ids, seeds.
 *   2. `idl/agentpad.json` — instruction names, account order, arg order.
 * A test (`tests/verify_integration.mjs`) re-derives the Anchor discriminators
 * from the instruction names and checks them against the constants below,
 * and re-checks every PDA seed against lib.rs.
 *
 * Works in Node (>=18) and in the browser (bundled with esbuild, see
 * `sdk/browser-entry.mjs`). No `node:` imports here on purpose.
 */
import {
  PublicKey,
  SystemProgram,
  SYSVAR_RENT_PUBKEY,
  SYSVAR_INSTRUCTIONS_PUBKEY,
  Transaction,
  TransactionInstruction,
} from '@solana/web3.js';
// `Buffer` is a Node global, but NOT a browser global: the browser bundle
// must import it explicitly or every decode/encode call throws
// "Buffer is not defined" in a real browser (found 2026-11-19 via the page
// badge). The `buffer` npm package is bundled by esbuild for the browser.
import { Buffer } from 'buffer';
import CONSTANTS from '../shared/constants.json' with { type: 'json' };

// --- Token programs, hand-rolled (no @solana/spl-token dependency) ---
// The spl-token ESM build imports node:crypto, which esbuild cannot bundle
// for the browser. The handful of instructions this SDK needs are encoded
// manually below; tests/verify_integration.mjs checks every encoding
// byte-for-byte against @solana/spl-token in Node.
export const TOKEN_PROGRAM_ID = new PublicKey('TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA');
export const TOKEN_2022_PROGRAM_ID = new PublicKey('TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb');
export const ASSOCIATED_TOKEN_PROGRAM_ID = new PublicKey('ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL');
/** Mint account size with a TransferFeeConfig extension (== spl-token getMintLen). */
export const MINT_LEN_WITH_TRANSFER_FEE = 278;

/** Mint account size for a plain Token-2022 mint with no extensions (v4.3.0: new launches carry no transfer fee). */
export const MINT_LEN_PLAIN = 82;

/** v4.3.0 wrapper tax: 1% of the token leg of a pool trade routed through `swap_with_tax`. */
export const WRAPPER_TAX_BPS = 100;

// --- Tiny base58 encoder (no extra dependency) ---
// The RPC `getProgramAccounts` memcmp filter expects base58 bytes; the
// SDK previously passed base64, which devnet rejects with INVALID_PARAMS.
const B58_ALPHABET = '123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz';
export function base58Encode(bytes) {
  let num = 0n;
  for (const b of bytes) num = (num << 8n) | BigInt(b);
  let out = '';
  while (num > 0n) { out = B58_ALPHABET[Number(num % 58n)] + out; num /= 58n; }
  for (const b of bytes) { if (b === 0) out = '1' + out; else break; }
  return out || '1';
}

export function getAssociatedTokenAddressSync(mint, owner, allowOwnerOffCurve = false, programId = TOKEN_PROGRAM_ID) {
  return PublicKey.findProgramAddressSync(
    [new PublicKey(owner).toBuffer(), new PublicKey(programId).toBuffer(), new PublicKey(mint).toBuffer()],
    ASSOCIATED_TOKEN_PROGRAM_ID,
  )[0];
}
export function createInitializeMint2Instruction(mint, decimals, mintAuthority, freezeAuthority, programId) {
  // spl-token packs COption<Pubkey> as a 1-byte flag + 32-byte pubkey
  // (zeros when None) — NOT borsh's 4-byte discriminant.
  const freeze = freezeAuthority === null || freezeAuthority === undefined
    ? Buffer.concat([Buffer.from([0]), Buffer.alloc(32, 0)])
    : Buffer.concat([Buffer.from([1]), Buffer.from(new PublicKey(freezeAuthority).toBuffer())]);
  return new TransactionInstruction({
    programId: new PublicKey(programId),
    keys: [{ pubkey: new PublicKey(mint), isSigner: false, isWritable: true }],
    data: Buffer.concat([Buffer.from([20, decimals & 0xff]), Buffer.from(new PublicKey(mintAuthority).toBuffer()), freeze]),
  });
}
// spl-token-2022 layout: [26 TransferFeeExtension][0 InitializeTransferFeeConfig]
// [COption<Pubkey> config authority][COption<Pubkey> withdraw authority]
// [u16 basis points][u64 maximum fee]. COption = 1-byte flag + 32-byte pubkey.
function coptionPubkey(pk) {
  if (pk === null || pk === undefined) return Buffer.from([0]);
  return Buffer.concat([Buffer.from([1]), Buffer.from(new PublicKey(pk).toBuffer())]);
}
export function createInitializeTransferFeeConfigInstruction(mint, configAuthority, withdrawAuthority, basisPoints, maxFee, programId) {
  const bps = Buffer.alloc(2);
  bps.writeUInt16LE(basisPoints);
  const fee = Buffer.alloc(8);
  fee.writeBigUInt64LE(BigInt(maxFee));
  return new TransactionInstruction({
    programId: new PublicKey(programId),
    keys: [{ pubkey: new PublicKey(mint), isSigner: false, isWritable: true }],
    data: Buffer.concat([
      Buffer.from([26, 0]),
      coptionPubkey(configAuthority),
      coptionPubkey(withdrawAuthority),
      bps,
      fee,
    ]),
  });
}
// TransferHook SetAuthority (classic token layout): [6 SetAuthority]
// [0 MintTokens][COption<Pubkey> new authority (1-byte flag + 32 bytes)].
export function createSetAuthorityInstruction(mint, currentAuthority, newAuthority, programId) {
  return new TransactionInstruction({
    programId: new PublicKey(programId),
    keys: [
      { pubkey: new PublicKey(mint), isSigner: false, isWritable: true },
      { pubkey: new PublicKey(currentAuthority), isSigner: true, isWritable: false },
    ],
    data: Buffer.concat([
      Buffer.from([6, 0]),
      coptionPubkey(newAuthority),
    ]),
  });
}
export function createAssociatedTokenAccountInstruction(payer, associatedToken, owner, mint, programId = TOKEN_PROGRAM_ID) {
  return new TransactionInstruction({
    programId: ASSOCIATED_TOKEN_PROGRAM_ID,
    keys: [
      { pubkey: new PublicKey(payer), isSigner: true, isWritable: true },
      { pubkey: new PublicKey(associatedToken), isSigner: false, isWritable: true },
      { pubkey: new PublicKey(owner), isSigner: false, isWritable: false },
      { pubkey: new PublicKey(mint), isSigner: false, isWritable: false },
      { pubkey: SystemProgram.programId, isSigner: false, isWritable: false },
      { pubkey: new PublicKey(programId), isSigner: false, isWritable: false },
    ],
    data: Buffer.alloc(0),
  });
}
// --- WSOL wrap/unwrap (classic Token program, same hand-rolled style) ---
// SyncNative = instruction 17: turns lamports sent to a WSOL account into
// wrapped SOL. CloseAccount = instruction 9: closes a token account and
// returns its lamports (rent + any wrapped SOL) to the destination; closing
// a WSOL account is the standard unwrap.
export function createSyncNativeInstruction(account) {
  return new TransactionInstruction({
    programId: TOKEN_PROGRAM_ID,
    keys: [{ pubkey: new PublicKey(account), isSigner: false, isWritable: true }],
    data: Buffer.from([17]),
  });
}
export function createCloseAccountInstruction(account, destination, owner) {
  return new TransactionInstruction({
    programId: TOKEN_PROGRAM_ID,
    keys: [
      { pubkey: new PublicKey(account), isSigner: false, isWritable: true },
      { pubkey: new PublicKey(destination), isSigner: false, isWritable: true },
      { pubkey: new PublicKey(owner), isSigner: true, isWritable: false },
    ],
    data: Buffer.from([9]),
  });
}

export { CONSTANTS };
export const PROGRAM_ID = new PublicKey(CONSTANTS.programId);
export const WSOL_MINT = new PublicKey(CONSTANTS.wsolMint);
export const TOKEN_2022_PROGRAM = TOKEN_2022_PROGRAM_ID;
/** Metaplex Token Metadata program (same ID on devnet and mainnet). */
export const METAPLEX_METADATA_PROGRAM_ID = new PublicKey('metaqbxxUerdq28cj1RbAWkYQm3ybzjb6a8bt518x1s');

/** Anchor `global:<ix>` discriminators = sha256("global:<ix>")[0:8]. */
export const DISCRIMINATORS = {
  initialize_global: '2fe10f705633bee7',
  set_platform_token: '1627d16a183ffefe',
  set_public_launches_enabled: '7372248af1c6dbe4',
  set_token_metadata: 'da7e7ac1dc956727',
  add_dividend_asset: '61ff16bd12f4d40c',
  set_dividend_asset_status: 'fe6fdbe07b173923',
  create_launch: 'efdfff8627797f3e',
  mint_shares: '18c48400b79ed88e',
  refund_shares: '6bf48c23cb1f5655',
  graduate_to_raydium: '3bef23390dd928c3',
  create_raydium_pool: '412d774dccb25402',
  claim_tokens: '6cd8d2e700d42a40',
  distribute_harvested_tax: '09b9d7a0bb043346',
  harvest_withheld_fees: '5181ab2eac2c6051',
  swap_with_tax: '69cfba047d4d7fc7',
  withdraw_tax: 'dc9654c7031ddf60',
  claim_dividend: '0f1dcf7899b2a45b',
  manual_buyback_and_burn: '3639e5aa9809fe7f',
};

/** Anchor `account:<Name>` discriminators = sha256("account:<Name>")[0:8]. */
export const ACCOUNT_DISCRIMINATORS = {
  GlobalConfig: '95089ccaa0fcb0d9',
  WhitelistedAsset: '6f9f0d2fd3b40614',
  LaunchPool: '1d41bdd84381b33d',
  MinterRecord: '621de86f44e73e0b',
  DividendVault: '326a86702bcfc896',
  MinterReward2: 'f44d2656c0bc3ca3',
};

/** Raydium CPMM `initialize` discriminator, verbatim from raydium-sdk-v2. */
export const RAYDIUM_INITIALIZE_DISCRIMINATOR = 'afaf6d1f0d989bed';

export function raydiumConfig(cluster = CONSTANTS.cluster) {
  return CONSTANTS.raydium[cluster] ?? CONSTANTS.raydium.devnet;
}
export function rpcUrl(cluster = CONSTANTS.cluster) {
  return CONSTANTS.rpc[cluster] ?? CONSTANTS.rpc.devnet;
}

// ---------------------------------------------------------------------------
// Exact decimal math (never floats for money)
// ---------------------------------------------------------------------------
/** "0.01" -> 10_000_000n lamports. Throws on >9 decimals or bad input. */
export function solToLamports(sol) {
  const s = String(sol).trim();
  if (!/^\d+(\.\d{1,9})?$/.test(s)) throw new Error(`Invalid SOL amount: ${sol}`);
  const [whole, frac = ''] = s.split('.');
  return BigInt(whole) * 1_000_000_000n + BigInt(frac.padEnd(9, '0') || '0');
}
export function lamportsToSol(lamports) {
  const v = BigInt(lamports);
  const whole = v / 1_000_000_000n;
  const frac = (v % 1_000_000_000n).toString().padStart(9, '0').replace(/0+$/, '');
  return frac ? `${whole}.${frac}` : `${whole}`;
}
export function tokensPerShare(totalShares) {
  return Math.floor(CONSTANTS.token.publicMintSupply / Number(totalShares));
}
export function personalCapShares(totalShares, bonusShares = 0) {
  return Math.floor(Number(totalShares) * CONSTANTS.shares.initialCapBps / 10_000) + Number(bonusShares);
}
export function graduationSplitSol(totalRaisedLamports) {
  const total = BigInt(totalRaisedLamports);
  const s = CONSTANTS.graduationSolSplitBps;
  return {
    lp: total * BigInt(s.lp) / 10_000n,
    creator: total * BigInt(s.creator) / 10_000n,
    buyback: total * BigInt(s.buyback) / 10_000n,
    treasury: total - (total * BigInt(s.lp) / 10_000n) - (total * BigInt(s.creator) / 10_000n) - (total * BigInt(s.buyback) / 10_000n),
  };
}
export function resolveDividendAsset(symbolInput) {
  const sym = (symbolInput || 'SOL').trim();
  return CONSTANTS.dividendWhitelist.find((a) => a.symbol.toLowerCase() === sym.toLowerCase() && a.enabled) || null;
}

// ---------------------------------------------------------------------------
// Borsh-lite encoders (Anchor argument layout, little-endian)
// ---------------------------------------------------------------------------
function u64(v) {
  const b = Buffer.alloc(8);
  b.writeBigUInt64LE(BigInt(v));
  return b;
}
function u128(v) {
  const b = Buffer.alloc(16);
  b.writeBigUInt128LE(BigInt(v));
  return b;
}
function u8(v) {
  return Buffer.from([v & 0xff]);
}
function bool(v) {
  return Buffer.from([v ? 1 : 0]);
}
function pubkey(pk) {
  return Buffer.from(new PublicKey(pk).toBuffer());
}
function str(s) {
  const body = Buffer.from(String(s), 'utf8');
  const len = Buffer.alloc(4);
  len.writeUInt32LE(body.length);
  return Buffer.concat([len, body]);
}
function optPubkey(pk) {
  if (pk === null || pk === undefined) return Buffer.from([0]);
  return Buffer.concat([Buffer.from([1]), pubkey(pk)]);
}
function ixData(name, ...parts) {
  return Buffer.concat([Buffer.from(DISCRIMINATORS[name], 'hex'), ...parts]);
}
function meta(pubkeyValue, isSigner, isWritable) {
  return { pubkey: new PublicKey(pubkeyValue), isSigner, isWritable };
}
/** Anchor `Option<Account>` placeholder: the program id itself means None. */
function optAccount(pk) {
  return pk === null || pk === undefined ? PROGRAM_ID : new PublicKey(pk);
}

// ---------------------------------------------------------------------------
// PDAs (seeds mirror programs/agentpad/src/lib.rs)
// ---------------------------------------------------------------------------
const S = CONSTANTS.seeds;
export function globalConfigPda() {
  return PublicKey.findProgramAddressSync([Buffer.from(S.globalConfig)], PROGRAM_ID)[0];
}
export function buybackVaultPda() {
  return PublicKey.findProgramAddressSync([Buffer.from(S.buybackVault)], PROGRAM_ID)[0];
}
export function launchPoolPda(mint) {
  return PublicKey.findProgramAddressSync([Buffer.from(S.launchPool), new PublicKey(mint).toBuffer()], PROGRAM_ID)[0];
}
export function solVaultPda(mint) {
  return PublicKey.findProgramAddressSync([Buffer.from(S.solVault), new PublicKey(mint).toBuffer()], PROGRAM_ID)[0];
}
export function dividendVaultPda(pool) {
  return PublicKey.findProgramAddressSync([Buffer.from(S.dividendVault), new PublicKey(pool).toBuffer()], PROGRAM_ID)[0];
}
// v4: second dividend vault (dual-asset launches) — same DividendVault
// layout, different seeds. Its reward_mint records the second asset.
export function dividendVault2Pda(pool) {
  return PublicKey.findProgramAddressSync([Buffer.from(S.dividendVault2), new PublicKey(pool).toBuffer()], PROGRAM_ID)[0];
}
// v4.3.0: tax vault — PDA token account for the launch mint, owned by the
// launch pool. The wrapper routes 1% of every pool trade here.
export function taxVaultPda(mint) {
  return PublicKey.findProgramAddressSync([Buffer.from(S.taxVault), new PublicKey(mint).toBuffer()], PROGRAM_ID)[0];
}
export function minterPda(mint, user) {
  return PublicKey.findProgramAddressSync(
    [Buffer.from(S.minter), new PublicKey(mint).toBuffer(), new PublicKey(user).toBuffer()], PROGRAM_ID)[0];
}
// v4: second-asset dividend debt record (dual-asset launches).
export function minterReward2Pda(mint, user) {
  return PublicKey.findProgramAddressSync(
    [Buffer.from(S.minterReward2), new PublicKey(mint).toBuffer(), new PublicKey(user).toBuffer()], PROGRAM_ID)[0];
}
export function whitelistAssetPda(assetMint) {
  return PublicKey.findProgramAddressSync(
    [Buffer.from(S.whitelistAsset), new PublicKey(assetMint).toBuffer()], PROGRAM_ID)[0];
}

export function metadataPda(mint) {
  return PublicKey.findProgramAddressSync(
    [Buffer.from('metadata'), METAPLEX_METADATA_PROGRAM_ID.toBuffer(), new PublicKey(mint).toBuffer()],
    METAPLEX_METADATA_PROGRAM_ID)[0];
}
export function raydiumPdas(mint, cluster = CONSTANTS.cluster) {
  const cfg = raydiumConfig(cluster);
  const raydiumProgram = new PublicKey(cfg.cpmmProgram);
  const ammConfig = PublicKey.findProgramAddressSync(
    [Buffer.from('amm_config'), Buffer.from([0, Number(CONSTANTS.raydium.ammConfigIndex)])], raydiumProgram)[0];
  const authority = PublicKey.findProgramAddressSync(
    [Buffer.from('vault_and_lp_mint_auth_seed')], raydiumProgram)[0];
  const mintPk = new PublicKey(mint);
  const [token0, token1] = Buffer.compare(mintPk.toBuffer(), WSOL_MINT.toBuffer()) < 0
    ? [mintPk, WSOL_MINT] : [WSOL_MINT, mintPk];
  const poolState = PublicKey.findProgramAddressSync(
    [Buffer.from('pool'), ammConfig.toBuffer(), token0.toBuffer(), token1.toBuffer()], raydiumProgram)[0];
  const lpMint = PublicKey.findProgramAddressSync(
    [Buffer.from('pool_lp_mint'), poolState.toBuffer()], raydiumProgram)[0];
  const vault0 = PublicKey.findProgramAddressSync(
    [Buffer.from('pool_vault'), poolState.toBuffer(), token0.toBuffer()], raydiumProgram)[0];
  const vault1 = PublicKey.findProgramAddressSync(
    [Buffer.from('pool_vault'), poolState.toBuffer(), token1.toBuffer()], raydiumProgram)[0];
  const observation = PublicKey.findProgramAddressSync(
    [Buffer.from('observation'), poolState.toBuffer()], raydiumProgram)[0];
  return {
    raydiumProgram, ammConfig, authority, poolState, lpMint,
    vault0, vault1, observation, token0, token1,
    feeAccount: new PublicKey(cfg.feeAccount),
    token0IsPlatform: token0.equals(mintPk),
  };
}

// v4.2.7 minimum-raise check (client-side mirror of the on-chain check
// in `create_launch` — the program is the authority, this is only the
// friendly early error). The fee is read from the Raydium amm_config
// account (bytes 36..44, same layout the program reads), never
// hardcoded: devnet is 1 SOL, mainnet 0.15 SOL (both chain-verified).
export async function getCreatePoolFeeLamports(connection, cluster = CONSTANTS.cluster) {
  const cfg = raydiumConfig(cluster);
  const raydiumProgram = new PublicKey(cfg.cpmmProgram);
  const [ammConfig] = PublicKey.findProgramAddressSync(
    [Buffer.from('amm_config'), Buffer.from([0, Number(CONSTANTS.raydium.ammConfigIndex)])], raydiumProgram);
  const info = await connection.getAccountInfo(ammConfig);
  if (!info || info.data.length < 44) throw new Error('Raydium amm_config not found on-chain');
  return Number(info.data.readBigUInt64LE(36));
}

// Pure check: total raise (shares x price) * 60% LP share must be
// strictly greater than fee + rent buffer. Shares and price are NOT
// individually limited — only the total raise matters (e.g. 1000
// shares @ 0.002 SOL passes on devnet, 1000 @ 0.001 does not).
export function checkMinRaise(totalShares, pricePerShareLamports, feeLamports) {
  const totalRaise = BigInt(totalShares) * BigInt(pricePerShareLamports);
  const lpShare = totalRaise * BigInt(CONSTANTS.graduationSolSplitBps.lp) / 10000n;
  const required = BigInt(feeLamports) + BigInt(CONSTANTS.raydium.poolRentBufferLamports);
  return {
    ok: lpShare > required,
    totalRaiseLamports: totalRaise,
    lpShareLamports: lpShare,
    requiredLamports: required,
  };
}

// ---------------------------------------------------------------------------
// Raydium CPMM direct swap — station buy/sell (2026-11-20)
// ---------------------------------------------------------------------------
// The station used to fetch a swap transaction from Jupiter
// (quote-api.jup.ag /v6/quote + /v6/swap). That API is mainnet-only and
// unreachable from this network, so devnet station trades always died
// with "Failed to fetch". The swap is now built directly against the
// Raydium CPMM pool (swap_base_input) — no third party involved.
// Mainnet keeps the Jupiter flow in the wiring until a mainnet pool
// exists to test against.

/** swap_base_input discriminator (raydium-sdk-v2 anchorDataBuf). */
export const RAYDIUM_SWAP_BASE_INPUT_DISCRIMINATOR = [143, 190, 90, 218, 196, 30, 51, 222];
/** CPMM fee denominator (raydium-sdk-v2 FEE_RATE_DENOMINATOR_VALUE). */
export const CPMM_FEE_DENOMINATOR = 1000000n;
/** Legacy launch mints carry a 100bps TransferFee (v4.2.x). New launches
 * (v4.3.0+) are plain mints with no fee; their 1% is the wrapper tax
 * (WRAPPER_TAX_BPS), collected by `swap_with_tax` into the tax vault. */
export const PLATFORM_TRANSFER_FEE_BPS = 100;
/** Default slippage for station trades (matches the old Jupiter flow). */
export const TRADE_SLIPPAGE_BPS = 100;

/** Ceiling division for non-negative BigInts. */
function ceilDiv(a, b) {
  return (a + b - 1n) / b;
}

/**
 * CPMM swap_base_input quote (pure BigInt, mirrors the on-chain program):
 *   tradeFee = ceil(amountIn * tradeFeeRate / 1e6)
 *   amountInAfterFee = amountIn - tradeFee
 *   newOutputReserve = ceil(inputReserve * outputReserve / (inputReserve + amountInAfterFee))
 *   amountOut = outputReserve - newOutputReserve
 *   netOut = amountOut * (10000 - transferFeeBps) / 10000   (Token-2022 fee on the output leg)
 * `tradeFeeRate` defaults to 2500 (0.25%, the devnet amm_config rate);
 * raydiumSwapIxs always reads the live rate for the real transaction.
 * All amounts are raw (smallest-unit) BigInts.
 */
export function raydiumSwapQuote({ inputReserveRaw, outputReserveRaw, amountInRaw, tradeFeeRate = 2500n, transferFeeBps = 0 }) {
  const amountIn = BigInt(amountInRaw);
  const inputReserve = BigInt(inputReserveRaw);
  const outputReserve = BigInt(outputReserveRaw);
  if (amountIn <= 0n) throw new Error('amountIn must be positive');
  if (inputReserve <= 0n || outputReserve <= 0n) throw new Error('pool reserves must be positive');
  const tradeFee = ceilDiv(amountIn * BigInt(tradeFeeRate), CPMM_FEE_DENOMINATOR);
  const amountInAfterFee = amountIn - tradeFee;
  const invariant = inputReserve * outputReserve;
  const newOutputReserve = ceilDiv(invariant, inputReserve + amountInAfterFee);
  const amountOut = outputReserve - newOutputReserve;
  if (amountOut <= 0n) throw new Error('swap output is zero (amount too small or pool drained)');
  const netOut = amountOut * (10000n - BigInt(transferFeeBps)) / 10000n;
  return { tradeFeeRaw: tradeFee, amountInAfterFeeRaw: amountInAfterFee, amountOutRaw: amountOut, netOutRaw: netOut };
}

/** Read the Raydium amm_config fee rates (offsets verified on devnet, see scripts/check_amm_config.mjs). */
export async function getAmmConfigFees(connection, cluster = CONSTANTS.cluster) {
  const cfg = raydiumConfig(cluster);
  const raydiumProgram = new PublicKey(cfg.cpmmProgram);
  const [ammConfig] = PublicKey.findProgramAddressSync(
    [Buffer.from('amm_config'), Buffer.from([0, Number(CONSTANTS.raydium.ammConfigIndex)])], raydiumProgram);
  const info = await connection.getAccountInfo(ammConfig);
  if (!info || info.data.length < 44) throw new Error('Raydium amm_config not found on-chain');
  const d = info.data;
  return {
    ammConfig,
    tradeFeeRate: d.readBigUInt64LE(12),
    protocolFeeRate: d.readBigUInt64LE(20),
    fundFeeRate: d.readBigUInt64LE(28),
    createPoolFee: d.readBigUInt64LE(36),
  };
}

/**
 * Read a Raydium CPMM pool state. Offsets are empirical (verified against
 * the D pool EuUp3qdRPvLJjZvnjMvoVQyj1UQ7V9qdxSpptqQoWnQp on 2026-11-20):
 * amm_config 8..40, owner 40..72, vaultA 72..104, vaultB 104..136,
 * lpMint 136..168, mintA 168..200, mintB 200..232, programA 232..264,
 * programB 264..296, observation 296..328, decA byte 331, decB byte 332.
 * "A" is the first mint/vault/program triple, "B" the second; the caller
 * maps them to input/output by mint.
 */
export async function getRaydiumPoolState(connection, poolId) {
  if (!poolId) throw new Error('Raydium pool id missing (launch may not be graduated yet)');
  const info = await connection.getAccountInfo(new PublicKey(poolId));
  if (!info) throw new Error(`Raydium pool not found: ${poolId}`);
  const d = Buffer.from(info.data);
  const pk = (off) => new PublicKey(d.subarray(off, off + 32));
  return {
    poolId: new PublicKey(poolId),
    ammConfig: pk(8),
    owner: pk(40),
    vaultA: pk(72),
    vaultB: pk(104),
    lpMint: pk(136),
    mintA: pk(168),
    mintB: pk(200),
    programA: pk(232),
    programB: pk(264),
    observation: pk(296),
    decA: d[331],
    decB: d[332],
  };
}

/**
 * swap_base_input instruction (raydium-sdk-v2 makeSwapCpmmBaseInInInstruction).
 * Account order + flags follow the Raydium CPMM program exactly:
 * payer (signer), authority, config, pool (w), user input (w), user output (w),
 * input vault (w), output vault (w), input token program, output token program,
 * input mint, output mint, observation (w). Data = discriminator + u64 amountIn
 * + u64 amountOutMin.
 */
export function createSwapBaseInputIx({ poolState, authority, raydiumProgram, inputMint, outputMint, inputTokenAccount, outputTokenAccount, amountInRaw, amountOutMinRaw, payer }) {
  const inMint = new PublicKey(inputMint);
  const outMint = new PublicKey(outputMint);
  const inIsA = inMint.equals(poolState.mintA);
  const outIsA = outMint.equals(poolState.mintA);
  if (!inIsA && !inMint.equals(poolState.mintB)) throw new Error('input mint is not in this pool');
  if (!outIsA && !outMint.equals(poolState.mintB)) throw new Error('output mint is not in this pool');
  const inputVault = inIsA ? poolState.vaultA : poolState.vaultB;
  const outputVault = outIsA ? poolState.vaultA : poolState.vaultB;
  const inputTokenProgram = inIsA ? poolState.programA : poolState.programB;
  const outputTokenProgram = outIsA ? poolState.programA : poolState.programB;
  const data = Buffer.alloc(24);
  Buffer.from(RAYDIUM_SWAP_BASE_INPUT_DISCRIMINATOR).copy(data, 0);
  data.writeBigUInt64LE(BigInt(amountInRaw), 8);
  data.writeBigUInt64LE(BigInt(amountOutMinRaw), 16);
  return new TransactionInstruction({
    programId: new PublicKey(raydiumProgram),
    keys: [
      meta(payer, true, false),
      meta(authority, false, false),
      meta(poolState.ammConfig, false, false),
      meta(poolState.poolId, false, true),
      meta(inputTokenAccount, false, true),
      meta(outputTokenAccount, false, true),
      meta(inputVault, false, true),
      meta(outputVault, false, true),
      meta(inputTokenProgram, false, false),
      meta(outputTokenProgram, false, false),
      meta(inputMint, false, false),
      meta(outputMint, false, false),
      meta(poolState.observation, false, true),
    ],
    data,
  });
}

/**
 * v4.3.0: which tax model a launch uses.
 *   'legacy'  — the mint carries a TransferFeeConfig (100bps, v4.2.x
 *               launches); trades go straight to the Raydium pool and the
 *               crank harvests the withheld fee.
 *   'wrapper' — plain mint, no extensions (v4.3.0+ launches); trades must
 *               go through `swap_with_tax`, which routes 1% of the token
 *               leg into the launch's tax vault for the crank to collect.
 * Detection is by mint account length: this platform only ever creates
 * plain mints (82 bytes) or fee mints (278 bytes). spl-token cannot be
 * imported in the browser bundle (it pulls node:crypto), so the extension
 * TLV is not parsed here.
 */
export async function getTaxModel(connection, mint) {
  const info = await connection.getAccountInfo(new PublicKey(mint), 'confirmed');
  if (!info) throw new Error(`mint not found: ${mint}`);
  return info.data.length === MINT_LEN_PLAIN ? 'wrapper' : 'legacy';
}

/**
 * Build the full instruction list for a station trade on a v4.3.0+
 * (wrapper) launch, routed through the program's `swap_with_tax` so 1%
 * of the token leg lands in the launch's tax vault. `amountIn` is in
 * human units (SOL for a buy, tokens for a sell). Returns the same shape
 * as raydiumSwapIxs.
 *
 * Buy (SOL -> token): pool outputs amountOut, the user keeps 99%
 * (1% stays in the tax vault); amountOutMin is the NET minimum and the
 * program grosses it back up internally.
 * Sell (token -> SOL): 1% of amountIn goes to the tax vault, 99%
 * enters the pool; amountOutMin is the SOL minimum, no tax on the SOL leg.
 */
export async function wrapperSwapIxs({ connection, poolId, mint, side, amountIn, slippageBps = TRADE_SLIPPAGE_BPS, payer, cluster = CONSTANTS.cluster }) {
  if (side !== 'buy' && side !== 'sell') throw new Error(`side must be 'buy' or 'sell', got ${side}`);
  const payerPk = new PublicKey(payer);
  const mintPk = new PublicKey(mint);
  const poolState = await getRaydiumPoolState(connection, poolId);
  const r = raydiumPdas(mint, cluster);
  if (!r.poolState.equals(poolState.poolId)) throw new Error('pool id does not match the mint (wrong pool?)');
  const fees = await getAmmConfigFees(connection, cluster);
  const isBuy = side === 'buy';
  const inputMint = isBuy ? WSOL_MINT : mintPk;
  const outputMint = isBuy ? mintPk : WSOL_MINT;
  const inputDec = inputMint.equals(poolState.mintA) ? poolState.decA : poolState.decB;
  const inputVault = inputMint.equals(poolState.mintA) ? poolState.vaultA : poolState.vaultB;
  const outputVault = outputMint.equals(poolState.mintA) ? poolState.vaultA : poolState.vaultB;
  const amountInRaw = BigInt(Math.round(Number(amountIn) * 10 ** inputDec));
  if (amountInRaw <= 0n) throw new Error('amount must be positive');
  const [inBal, outBal] = await Promise.all([
    connection.getTokenAccountBalance(inputVault),
    connection.getTokenAccountBalance(outputVault),
  ]);
  const inputReserveRaw = BigInt(inBal.value.amount);
  const outputReserveRaw = BigInt(outBal.value.amount);
  // Quote: the mint carries no transfer fee, so the pool math uses
  // transferFeeBps=0; the 1% wrapper tax is applied on the net amount.
  let quoteInputRaw = amountInRaw;
  if (!isBuy) quoteInputRaw = amountInRaw * (10000n - BigInt(WRAPPER_TAX_BPS)) / 10000n;
  const quote = raydiumSwapQuote({ inputReserveRaw, outputReserveRaw, amountInRaw: quoteInputRaw, tradeFeeRate: fees.tradeFeeRate, transferFeeBps: 0 });
  const netOutRaw = isBuy
    ? quote.netOutRaw * (10000n - BigInt(WRAPPER_TAX_BPS)) / 10000n
    : quote.netOutRaw;
  const amountOutMinRaw = netOutRaw * (10000n - BigInt(slippageBps)) / 10000n;
  // Token accounts (same as raydiumSwapIxs).
  const wsolAta = getAssociatedTokenAddressSync(WSOL_MINT, payerPk, false, TOKEN_PROGRAM_ID);
  const tokenAta = getAssociatedTokenAddressSync(mintPk, payerPk, false, TOKEN_2022_PROGRAM_ID);
  const inputAta = isBuy ? wsolAta : tokenAta;
  const outputAta = isBuy ? tokenAta : wsolAta;
  const ixs = [];
  const [wsolInfo, tokenInfo] = await Promise.all([
    connection.getAccountInfo(wsolAta),
    connection.getAccountInfo(tokenAta),
  ]);
  if (!wsolInfo) ixs.push(createAssociatedTokenAccountInstruction(payerPk, wsolAta, payerPk, WSOL_MINT, TOKEN_PROGRAM_ID));
  if (isBuy && !tokenInfo) ixs.push(createAssociatedTokenAccountInstruction(payerPk, tokenAta, payerPk, mintPk, TOKEN_2022_PROGRAM_ID));
  if (!isBuy && !tokenInfo) throw new Error('no token account — nothing to sell');
  if (isBuy) {
    ixs.push(SystemProgram.transfer({ fromPubkey: payerPk, toPubkey: wsolAta, lamports: Number(amountInRaw) }));
    ixs.push(createSyncNativeInstruction(wsolAta));
  }
  ixs.push(swapWithTaxIx({
    mint, user: payerPk, userTokenAccount: tokenAta, userWsolAccount: wsolAta,
    poolState, isBuy, amountIn: amountInRaw, amountOutMin: amountOutMinRaw, cluster,
  }));
  ixs.push(createCloseAccountInstruction(wsolAta, payerPk, payerPk));
  return { ixs, quote, amountOutMinRaw, inputAta, outputAta, poolState, inputMint, outputMint, amountInRaw };
}

/**
 * Build the full instruction list for a station trade directly against the
 * Raydium CPMM pool (no Jupiter). `amountIn` is in human units (SOL for a
 * buy, tokens for a sell); the function converts with the pool's decimals.
 * Returns { ixs, quote, amountOutMinRaw, inputAta, outputAta, poolState,
 * inputMint, outputMint, amountInRaw }.
 *
 * Buy (SOL -> token):
 *   [create WSOL ATA?] [create token ATA?] transfer SOL -> WSOL ATA,
 *   SyncNative, swap, close WSOL ATA (unwrap + reclaim rent).
 * Sell (token -> SOL):
 *   [create WSOL ATA?] swap, close WSOL ATA (unwrap the received SOL).
 *
 * The 1% platform transfer fee lands on the token leg: on a buy the user
 * receives amountOut * 0.99; on a sell the vault receives amountIn * 0.99,
 * so the sell quote uses the post-fee input (conservative — the on-chain
 * program may credit the full input, in which case the user gets slightly
 * more than quoted and amountOutMin still holds).
 */
export async function raydiumSwapIxs({ connection, poolId, mint, side, amountIn, slippageBps = TRADE_SLIPPAGE_BPS, payer, cluster = CONSTANTS.cluster, transferFeeBps = PLATFORM_TRANSFER_FEE_BPS }) {
  if (side !== 'buy' && side !== 'sell') throw new Error(`side must be 'buy' or 'sell', got ${side}`);
  const payerPk = new PublicKey(payer);
  const mintPk = new PublicKey(mint);
  const poolState = await getRaydiumPoolState(connection, poolId);
  const r = raydiumPdas(mint, cluster);
  if (!r.poolState.equals(poolState.poolId)) throw new Error('pool id does not match the mint (wrong pool?)');
  const fees = await getAmmConfigFees(connection, cluster);
  const isBuy = side === 'buy';
  const inputMint = isBuy ? WSOL_MINT : mintPk;
  const outputMint = isBuy ? mintPk : WSOL_MINT;
  const inputDec = inputMint.equals(poolState.mintA) ? poolState.decA : poolState.decB;
  const inputVault = inputMint.equals(poolState.mintA) ? poolState.vaultA : poolState.vaultB;
  const outputVault = outputMint.equals(poolState.mintA) ? poolState.vaultA : poolState.vaultB;
  const amountInRaw = BigInt(Math.round(Number(amountIn) * 10 ** inputDec));
  if (amountInRaw <= 0n) throw new Error('amount must be positive');
  const [inBal, outBal] = await Promise.all([
    connection.getTokenAccountBalance(inputVault),
    connection.getTokenAccountBalance(outputVault),
  ]);
  const inputReserveRaw = BigInt(inBal.value.amount);
  const outputReserveRaw = BigInt(outBal.value.amount);
  // Quote: the transfer fee is on the token leg.
  let quoteInputRaw = amountInRaw;
  let quoteFeeBps = 0;
  if (isBuy) {
    quoteFeeBps = transferFeeBps; // fee on the output (token) leg
  } else {
    // Fee on the input (token) leg: the vault only receives 99% of amountIn.
    quoteInputRaw = amountInRaw * (10000n - BigInt(transferFeeBps)) / 10000n;
  }
  const quote = raydiumSwapQuote({ inputReserveRaw, outputReserveRaw, amountInRaw: quoteInputRaw, tradeFeeRate: fees.tradeFeeRate, transferFeeBps: quoteFeeBps });
  const amountOutMinRaw = quote.netOutRaw * (10000n - BigInt(slippageBps)) / 10000n;
  // Token accounts.
  const wsolAta = getAssociatedTokenAddressSync(WSOL_MINT, payerPk, false, TOKEN_PROGRAM_ID);
  const tokenAta = getAssociatedTokenAddressSync(mintPk, payerPk, false, TOKEN_2022_PROGRAM_ID);
  const inputAta = isBuy ? wsolAta : tokenAta;
  const outputAta = isBuy ? tokenAta : wsolAta;
  const ixs = [];
  const [wsolInfo, tokenInfo] = await Promise.all([
    connection.getAccountInfo(wsolAta),
    connection.getAccountInfo(tokenAta),
  ]);
  if (!wsolInfo) ixs.push(createAssociatedTokenAccountInstruction(payerPk, wsolAta, payerPk, WSOL_MINT, TOKEN_PROGRAM_ID));
  if (isBuy && !tokenInfo) ixs.push(createAssociatedTokenAccountInstruction(payerPk, tokenAta, payerPk, mintPk, TOKEN_2022_PROGRAM_ID));
  if (!isBuy && !tokenInfo) throw new Error('no token account — nothing to sell');
  if (isBuy) {
    ixs.push(SystemProgram.transfer({ fromPubkey: payerPk, toPubkey: wsolAta, lamports: Number(amountInRaw) }));
    ixs.push(createSyncNativeInstruction(wsolAta));
  }
  ixs.push(createSwapBaseInputIx({
    poolState, authority: r.authority, raydiumProgram: r.raydiumProgram,
    inputMint, outputMint, inputTokenAccount: inputAta, outputTokenAccount: outputAta,
    amountInRaw, amountOutMinRaw, payer: payerPk,
  }));
  ixs.push(createCloseAccountInstruction(wsolAta, payerPk, payerPk));
  return { ixs, quote, amountOutMinRaw, inputAta, outputAta, poolState, inputMint, outputMint, amountInRaw };
}

// ---------------------------------------------------------------------------
// Instruction builders (keys + args follow idl/agentpad.json exactly)
// ---------------------------------------------------------------------------
export function initializeGlobalIx({ admin, treasuryWallet, gateSigner }) {
  return new TransactionInstruction({
    programId: PROGRAM_ID,
    keys: [
      meta(globalConfigPda(), false, true),
      meta(buybackVaultPda(), false, true),
      meta(admin, true, true),
      meta(SystemProgram.programId, false, false),
    ],
    data: ixData('initialize_global', pubkey(treasuryWallet), pubkey(gateSigner)),
  });
}

export function setPlatformTokenIx({ admin, platformTokenMint, platformPoolId }) {
  return new TransactionInstruction({
    programId: PROGRAM_ID,
    keys: [meta(globalConfigPda(), false, true), meta(admin, true, false)],
    data: ixData('set_platform_token', pubkey(platformTokenMint), pubkey(platformPoolId)),
  });
}

export function addDividendAssetIx({ admin, assetMint, symbol, decimals }) {
  return new TransactionInstruction({
    programId: PROGRAM_ID,
    keys: [
      meta(globalConfigPda(), false, false),
      meta(assetMint, false, false),
      meta(whitelistAssetPda(assetMint), false, true),
      meta(admin, true, true),
      meta(SystemProgram.programId, false, false),
    ],
    data: ixData('add_dividend_asset', str(symbol), u8(decimals)),
  });
}

export function setDividendAssetStatusIx({ admin, assetMint, enabled }) {
  return new TransactionInstruction({
    programId: PROGRAM_ID,
    keys: [
      meta(globalConfigPda(), false, false),
      meta(whitelistAssetPda(assetMint), false, true),
      meta(admin, true, false),
    ],
    data: ixData('set_dividend_asset_status', bool(enabled)),
  });
}

export function setPublicLaunchesEnabledIx({ admin, enabled }) {
  return new TransactionInstruction({
    programId: PROGRAM_ID,
    keys: [meta(globalConfigPda(), false, true), meta(admin, true, false)],
    data: ixData('set_public_launches_enabled', bool(enabled)),
  });
}

export function createSetTokenMetadataIx({ authority, payer, admin, mint, name, symbol, uri }) {
  // v4.2.9: the caller may be the admin OR the gate signer (the `authority`
  // account). `admin` is kept as a deprecated alias for older callers.
  // v4.2.10: the metadata rent is paid by a separate `payer` (defaults to
  // the authority for single-signer callers like the admin script); the
  // graduator passes itself as payer and the gate signer as authority.
  // Accounts must match the Anchor SetTokenMetadata struct order:
  // global_config, mint (mut), launch_pool, metadata (mut), authority,
  // payer, metadata_program, system_program, sysvar_instructions,
  // token_program.
  const caller = authority ?? admin;
  const rentPayer = payer ?? caller;
  return new TransactionInstruction({
    programId: PROGRAM_ID,
    keys: [
      meta(globalConfigPda(), false, false),
      meta(mint, false, true),
      meta(launchPoolPda(mint), false, false),
      meta(metadataPda(mint), false, true),
      meta(caller, true, false),
      meta(rentPayer, true, true),
      meta(METAPLEX_METADATA_PROGRAM_ID, false, false),
      meta(SystemProgram.programId, false, false),
      meta(SYSVAR_INSTRUCTIONS_PUBKEY, false, false),
      meta(TOKEN_2022_PROGRAM_ID, false, false),
    ],
    data: ixData('set_token_metadata', str(name), str(symbol), str(uri)),
  });
}

// v4: `rewardMint2` — null = no second asset; 'SOL' = SOL as second asset;
// a mint address = whitelisted stock token as second asset. Dividends then
// split 50/50 between the two vaults. Account order MUST match the Anchor
// CreateLaunch struct in lib.rs.
export function createLaunchIx({ creator, mint, totalShares, pricePerShareLamports, marketingWallet, rewardMint, rewardMint2 = null, gateSigner = null }) {
  const pool = launchPoolPda(mint);
  const whitelist = rewardMint
    ? whitelistAssetPda(rewardMint)
    : PROGRAM_ID; // Option<Account> None placeholder
  const hasSecond = rewardMint2 !== null && rewardMint2 !== undefined;
  const secondIsSol = rewardMint2 === 'SOL';
  const whitelist2 = hasSecond && !secondIsSol ? whitelistAssetPda(rewardMint2) : PROGRAM_ID;
  const divVault2 = hasSecond ? dividendVault2Pda(pool) : PROGRAM_ID;
  const rewardMint2Arg = !hasSecond ? null : (secondIsSol ? '11111111111111111111111111111111' : rewardMint2);
  const keys = [
    meta(globalConfigPda(), false, true),
    meta(mint, false, true),
    meta(whitelist, false, false),
    meta(pool, false, true),
    meta(solVaultPda(mint), false, true),
    meta(dividendVaultPda(pool), false, true),
    meta(whitelist2, false, false),
    meta(divVault2, false, true),
    // v4.3.0: tax vault PDA (created by the program in create_launch).
    meta(taxVaultPda(mint), false, true),
    meta(creator, true, true),
    meta(SystemProgram.programId, false, false),
    // v4.3.0: Token-2022 program (InitializeAccount3 for the tax vault).
    meta(TOKEN_2022_PROGRAM_ID, false, false),
    // v4.2.7: the Raydium amm_config (read-only). The program reads the
    // pool-creation fee from it for the minimum-raise check, so it must
    // sit before the trailing gate_signer.
    meta(raydiumPdas(mint).ammConfig, false, false),
  ];
  // Trailing gate_signer is Option<AccountInfo> in the program, but Anchor
  // still requires the key to be present in the account list even when it
  // is None — pass the program id as the placeholder (Anchor treats it as
  // None, same pattern as the whitelist placeholder above). Omitting the
  // meta entirely makes the program fail with NotEnoughAccountKeys (3005),
  // which is what the live site hit on 2026-11-19.
  keys.push(meta(gateSigner || PROGRAM_ID, !!gateSigner, false));
  return new TransactionInstruction({
    programId: PROGRAM_ID,
    keys,
    data: ixData(
      'create_launch',
      u64(totalShares),
      u64(pricePerShareLamports),
      pubkey(marketingWallet),
      optPubkey(rewardMint),
      optPubkey(rewardMint2Arg),
    ),
  });
}

export function mintSharesIx({ user, mint, sharesCount, gateSigner, inviter = null }) {
  const pool = launchPoolPda(mint);
  // Account order MUST match the Anchor MintShares struct in lib.rs:
  // [4] inviter (raw pubkey, readonly), [5] inviter_record (PDA, writable).
  // The IDL had them swapped (inviter_record first) and the SDK followed
  // it — on-chain Anchor then read the inviter's raw key as the record and
  // failed ConstraintSeeds (found 2026-11-19 via simulate_invite).
  return new TransactionInstruction({
    programId: PROGRAM_ID,
    keys: [
      meta(globalConfigPda(), false, false),
      meta(pool, false, true),
      meta(solVaultPda(mint), false, true),
      meta(minterPda(mint, user), false, true),
      meta(inviter ? new PublicKey(inviter) : PROGRAM_ID, false, false),
      meta(inviter ? minterPda(mint, inviter) : PROGRAM_ID, false, true),
      meta(gateSigner, true, false),
      meta(user, true, true),
      meta(SystemProgram.programId, false, false),
    ],
    data: ixData('mint_shares', u64(sharesCount)),
  });
}

export function refundSharesIx({ user, mint, sharesCount, gateSigner }) {
  const pool = launchPoolPda(mint);
  return new TransactionInstruction({
    programId: PROGRAM_ID,
    keys: [
      meta(globalConfigPda(), false, false),
      meta(pool, false, true),
      meta(solVaultPda(mint), false, true),
      meta(minterPda(mint, user), false, true),
      meta(gateSigner, true, false),
      meta(user, true, true),
      meta(SystemProgram.programId, false, false),
    ],
    data: ixData('refund_shares', u64(sharesCount)),
  });
}

export function graduateIx({ mint, marketingWallet, treasuryWallet, payer }) {
  const { pool, claimVault, lpStaging } = graduateAccounts(mint);
  return new TransactionInstruction({
    programId: PROGRAM_ID,
    keys: [
      meta(globalConfigPda(), false, false),
      meta(pool, false, true),
      meta(mint, false, true),
      meta(solVaultPda(mint), false, true),
      meta(claimVault, false, true),
      meta(lpStaging, false, true),
      meta(marketingWallet, false, true),
      meta(buybackVaultPda(), false, true),
      meta(treasuryWallet, false, true),
      meta(TOKEN_2022_PROGRAM_ID, false, false),
      meta(SystemProgram.programId, false, false),
      meta(payer, true, true),
    ],
    data: ixData('graduate_to_raydium'),
  });
}

/**
 * The two token accounts `graduate_to_raydium` uses, derived exactly the
 * way the program constrains them (token::authority = launch_pool):
 * - claimVault: the pool's ATA for the mint (anyone can create it, the ATA
 *   program needs no signature from the PDA owner).
 * - lpStaging: PDA ['lp_staging', pool] — no client can create it, so the
 *   program creates it by hand inside `graduate_to_raydium` (v4.2.1:
 *   manual create + InitializeAccount3; Anchor 0.30 `init_if_needed` on
 *   a Token-2022 account is broken, and plain `init` would fail a retry
 *   after graduation at account validation instead of reaching the
 *   handler's no-op).
 */
export function graduateAccounts(mint) {
  const pool = launchPoolPda(mint);
  const mintPk = new PublicKey(mint);
  // Two distinct PDAs owned by the pool: claim vault is the pool's ATA,
  // LP staging is a second token account seeded off the pool + mint.
  const claimVault = getAssociatedTokenAddressSync(mintPk, pool, true, TOKEN_2022_PROGRAM_ID);
  const [lpStaging] = PublicKey.findProgramAddressSync(
    [Buffer.from('lp_staging'), pool.toBuffer()], PROGRAM_ID);
  return { pool, claimVault, lpStaging };
}

export function createRaydiumPoolIx({ mint, payer, cluster }) {
  const { pool, lpStaging } = graduateAccounts(mint);
  const r = raydiumPdas(mint, cluster);
  const poolWsol = getAssociatedTokenAddressSync(WSOL_MINT, pool, true, TOKEN_PROGRAM_ID);
  // v4.2.6: the Raydium creator is the SOL vault (v4.2.5), so the LP-token
  // ATA is owned by the vault, not the launch pool. v4.2.5 left this as
  // the pool's ATA, which Raydium's `init` (authority = creator) rejects.
  const solVault = solVaultPda(mint);
  const creatorLp = getAssociatedTokenAddressSync(r.lpMint, solVault, true, TOKEN_PROGRAM_ID);
  return new TransactionInstruction({
    programId: PROGRAM_ID,
    keys: [
      meta(globalConfigPda(), false, false),
      meta(pool, false, true),
      meta(mint, false, false),
      meta(WSOL_MINT, false, false),
      meta(solVaultPda(mint), false, true),
      meta(lpStaging, false, true),
      meta(poolWsol, false, true),
      meta(creatorLp, false, true),
      meta(r.ammConfig, false, false),
      meta(r.authority, false, false),
      meta(r.poolState, false, true),
      meta(r.lpMint, false, true),
      meta(r.token0, false, false),
      meta(r.token1, false, false),
      meta(r.vault0, false, true),
      meta(r.vault1, false, true),
      meta(r.observation, false, true),
      meta(r.feeAccount, false, true),
      meta(TOKEN_2022_PROGRAM_ID, false, false),
      meta(TOKEN_PROGRAM_ID, false, false),
      meta(ASSOCIATED_TOKEN_PROGRAM_ID, false, false),
      meta(SystemProgram.programId, false, false),
      meta(SYSVAR_RENT_PUBKEY, false, false),
      meta(r.raydiumProgram, false, false),
      meta(payer, true, true),
    ],
    data: ixData('create_raydium_pool'),
  });
}

export function claimTokensIx({ user, mint }) {
  const pool = launchPoolPda(mint);
  const { claimVault } = graduateAccounts(mint);
  const userAta = getAssociatedTokenAddressSync(new PublicKey(mint), new PublicKey(user), false, TOKEN_2022_PROGRAM_ID);
  return new TransactionInstruction({
    programId: PROGRAM_ID,
    keys: [
      meta(pool, false, false),
      meta(mint, false, false),
      meta(minterPda(mint, user), false, true),
      meta(claimVault, false, true),
      meta(userAta, false, true),
      meta(user, true, false),
      meta(TOKEN_2022_PROGRAM_ID, false, false),
    ],
    data: ixData('claim_tokens'),
  });
}

export function claimDividendIx({ user, mint, rewardMint = null, rewardMint2 = null, rewardTokenProgram = TOKEN_2022_PROGRAM_ID }) {
  const pool = launchPoolPda(mint);
  const divVault = dividendVaultPda(pool);
  const userAta = getAssociatedTokenAddressSync(new PublicKey(mint), new PublicKey(user), false, TOKEN_2022_PROGRAM_ID);
  const keys = [
    meta(pool, false, false),
    meta(divVault, false, true),
    meta(minterPda(mint, user), false, true),
    meta(userAta, false, false),
  ];
  if (rewardMint) {
    const divRewardAta = getAssociatedTokenAddressSync(new PublicKey(rewardMint), divVault, true, rewardTokenProgram);
    const userRewardAta = getAssociatedTokenAddressSync(new PublicKey(rewardMint), new PublicKey(user), false, rewardTokenProgram);
    keys.push(
      meta(rewardMint, false, false),
      meta(divRewardAta, false, true),
      meta(userRewardAta, false, true),
      meta(rewardTokenProgram, false, false),
    );
  } else {
    keys.push(
      meta(PROGRAM_ID, false, false),
      meta(PROGRAM_ID, false, true),
      meta(PROGRAM_ID, false, true),
      meta(PROGRAM_ID, false, false),
    );
  }
  // Leg 2 (v4, dual-asset launches): rewardMint2 null = single-asset,
  // 'SOL' = SOL as second asset, a mint address = stock token.
  if (rewardMint2 !== null && rewardMint2 !== undefined) {
    const divVault2 = dividendVault2Pda(pool);
    keys.push(
      meta(divVault2, false, true),
      meta(minterReward2Pda(mint, user), false, true),
    );
    if (rewardMint2 === 'SOL') {
      keys.push(
        meta(PROGRAM_ID, false, false),
        meta(PROGRAM_ID, false, true),
        meta(PROGRAM_ID, false, true),
        meta(PROGRAM_ID, false, false),
      );
    } else {
      const divRewardAta2 = getAssociatedTokenAddressSync(new PublicKey(rewardMint2), divVault2, true, rewardTokenProgram);
      const userRewardAta2 = getAssociatedTokenAddressSync(new PublicKey(rewardMint2), new PublicKey(user), false, rewardTokenProgram);
      keys.push(
        meta(rewardMint2, false, false),
        meta(divRewardAta2, false, true),
        meta(userRewardAta2, false, true),
        meta(rewardTokenProgram, false, false),
      );
    }
  } else {
    keys.push(
      meta(PROGRAM_ID, false, false),
      meta(PROGRAM_ID, false, true),
      meta(PROGRAM_ID, false, false),
      meta(PROGRAM_ID, false, true),
      meta(PROGRAM_ID, false, true),
      meta(PROGRAM_ID, false, false),
    );
  }
  keys.push(meta(user, true, true), meta(SystemProgram.programId, false, false));
  return new TransactionInstruction({ programId: PROGRAM_ID, keys, data: ixData('claim_dividend') });
}

export function manualBuybackAndBurnIx({ admin, platformTokenMint, amountSolLamports, burnTokenAmount }) {
  const vault = buybackVaultPda();
  const vaultAta = getAssociatedTokenAddressSync(new PublicKey(platformTokenMint), vault, true, TOKEN_2022_PROGRAM_ID);
  return new TransactionInstruction({
    programId: PROGRAM_ID,
    keys: [
      meta(globalConfigPda(), false, true),
      meta(vault, false, true),
      meta(admin, true, true),
      meta(platformTokenMint, false, false),
      meta(vaultAta, false, true),
      meta(TOKEN_2022_PROGRAM_ID, false, false),
      meta(SystemProgram.programId, false, false),
    ],
    data: ixData('manual_buyback_and_burn', u64(amountSolLamports), u64(burnTokenAmount)),
  });
}

// ---------------------------------------------------------------------------
// distribute_harvested_tax (harvester brings the 1% tax they collected)
// ---------------------------------------------------------------------------
export function distributeHarvestedTaxIx({
  harvester, mint, burnTokenAmount, dividendRewardAmount, creatorAndPlatformSolAmount,
  marketingWallet, treasuryWallet, rewardMint = null, rewardMint2 = null, rewardTokenProgram = TOKEN_2022_PROGRAM_ID,
}) {
  const pool = launchPoolPda(mint);
  const divVault = dividendVaultPda(pool);
  const harvesterAta = getAssociatedTokenAddressSync(new PublicKey(mint), new PublicKey(harvester), false, TOKEN_2022_PROGRAM_ID);
  const keys = [
    meta(globalConfigPda(), false, false),
    meta(pool, false, false),
    meta(mint, false, true),
    meta(divVault, false, true),
    meta(harvesterAta, false, true),
  ];
  if (rewardMint) {
    const harvesterRewardAta = getAssociatedTokenAddressSync(new PublicKey(rewardMint), new PublicKey(harvester), false, rewardTokenProgram);
    const divRewardAta = getAssociatedTokenAddressSync(new PublicKey(rewardMint), divVault, true, rewardTokenProgram);
    keys.push(
      meta(rewardMint, false, false),
      meta(harvesterRewardAta, false, true),
      meta(divRewardAta, false, true),
      meta(rewardTokenProgram, false, false),
    );
  } else {
    keys.push(meta(PROGRAM_ID, false, false), meta(PROGRAM_ID, false, true), meta(PROGRAM_ID, false, true), meta(PROGRAM_ID, false, false));
  }
  // Leg 2 (v4, dual-asset launches): rewardMint2 null = single-asset,
  // 'SOL' = SOL as second asset, a mint address = stock token.
  if (rewardMint2 !== null && rewardMint2 !== undefined) {
    const divVault2 = dividendVault2Pda(pool);
    keys.push(meta(divVault2, false, true));
    if (rewardMint2 === 'SOL') {
      keys.push(meta(PROGRAM_ID, false, false), meta(PROGRAM_ID, false, true), meta(PROGRAM_ID, false, true), meta(PROGRAM_ID, false, false));
    } else {
      const harvesterRewardAta2 = getAssociatedTokenAddressSync(new PublicKey(rewardMint2), new PublicKey(harvester), false, rewardTokenProgram);
      const divRewardAta2 = getAssociatedTokenAddressSync(new PublicKey(rewardMint2), divVault2, true, rewardTokenProgram);
      keys.push(
        meta(rewardMint2, false, false),
        meta(harvesterRewardAta2, false, true),
        meta(divRewardAta2, false, true),
        meta(rewardTokenProgram, false, false),
      );
    }
  } else {
    keys.push(
      meta(PROGRAM_ID, false, false),
      meta(PROGRAM_ID, false, false),
      meta(PROGRAM_ID, false, true),
      meta(PROGRAM_ID, false, true),
      meta(PROGRAM_ID, false, false),
    );
  }
  keys.push(
    meta(marketingWallet, false, true),
    meta(buybackVaultPda(), false, true),
    meta(treasuryWallet, false, true),
    meta(harvester, true, true),
    meta(TOKEN_2022_PROGRAM_ID, false, false),
    meta(SystemProgram.programId, false, false),
  );
  return new TransactionInstruction({
    programId: PROGRAM_ID,
    keys,
    data: ixData('distribute_harvested_tax', u64(burnTokenAmount), u64(dividendRewardAmount), u64(creatorAndPlatformSolAmount)),
  });
}

// harvest_withheld_fees (v4.2.8): withdraw the mint's accumulated withheld
// 1% transfer fees to the harvester's token account. The pool PDA is the
// withdraw authority, so only the program can sign this CPI. The crank is
// expected to call HarvestWithheldTokensToMint (permissionless, off-chain)
// first to move fees from individual token accounts into the mint; this
// instruction then withdraws the mint's accumulated total in one go.
// Anyone can trigger it; the pool must be Graduated.
export function harvestWithheldFeesIx({ harvester, mint }) {
  const pool = launchPoolPda(mint);
  const harvesterAta = getAssociatedTokenAddressSync(new PublicKey(mint), new PublicKey(harvester), false, TOKEN_2022_PROGRAM_ID);
  const keys = [
    meta(globalConfigPda(), false, false),
    meta(pool, false, false),
    meta(mint, false, true),
    meta(harvesterAta, false, true),
    meta(harvester, true, true),
    meta(TOKEN_2022_PROGRAM_ID, false, false),
  ];
  return new TransactionInstruction({
    programId: PROGRAM_ID,
    keys,
    data: ixData('harvest_withheld_fees'),
  });
}

// v4.3.0 wrapper swap: route a pool trade through the program so 1% of
// the token leg lands in the launch's tax vault. Account order MUST match
// the Anchor SwapWithTax struct in lib.rs. `poolState` is the object from
// getRaydiumPoolState (vaults/observation come from it, not from seeds).
// amountOutMin is the NET minimum (after the 1% wrapper tax); the program
// grosses it back up internally before calling the pool.
export function swapWithTaxIx({ mint, user, userTokenAccount, userWsolAccount, poolState, isBuy, amountIn, amountOutMin, cluster = CONSTANTS.cluster }) {
  const mintPk = new PublicKey(mint);
  const r = raydiumPdas(mint, cluster);
  if (!r.poolState.equals(poolState.poolId)) throw new Error('pool id does not match the mint (wrong pool?)');
  const tokenIsA = mintPk.equals(poolState.mintA);
  const tokenVault = tokenIsA ? poolState.vaultA : poolState.vaultB;
  const wsolVault = tokenIsA ? poolState.vaultB : poolState.vaultA;
  return new TransactionInstruction({
    programId: PROGRAM_ID,
    keys: [
      meta(launchPoolPda(mint), false, false),
      meta(mint, false, false),
      meta(taxVaultPda(mint), false, true),
      meta(user, true, true),
      meta(userTokenAccount, false, true),
      meta(userWsolAccount, false, true),
      meta(WSOL_MINT, false, false),
      meta(r.raydiumProgram, false, false),
      meta(r.authority, false, false),
      meta(poolState.ammConfig, false, false),
      meta(poolState.poolId, false, true),
      meta(tokenVault, false, true),
      meta(wsolVault, false, true),
      meta(poolState.observation, false, true),
      meta(TOKEN_2022_PROGRAM_ID, false, false),
      meta(TOKEN_PROGRAM_ID, false, false),
    ],
    data: ixData('swap_with_tax', bool(isBuy), u64(amountIn), u64(amountOutMin)),
  });
}

// v4.3.0: sweep the whole tax vault into the harvester's ATA (crank).
// Zero balance is a no-op success on-chain.
export function withdrawTaxIx({ mint, harvester, harvesterTokenAccount }) {
  return new TransactionInstruction({
    programId: PROGRAM_ID,
    keys: [
      meta(launchPoolPda(mint), false, false),
      meta(mint, false, false),
      meta(taxVaultPda(mint), false, true),
      meta(harvesterTokenAccount, false, true),
      meta(harvester, true, true),
      meta(TOKEN_2022_PROGRAM_ID, false, false),
    ],
    data: ixData('withdraw_tax'),
  });
}

// ---------------------------------------------------------------------------
// Token-2022 mint creation: plain mint, NO transfer fee (v4.3.0).
// What create_launch needs: supply == 0, mint authority == pool PDA.
// The 1% tax moved to the `swap_with_tax` wrapper, so mint / claim / P2P
// transfers are tax-free and only pool trades are taxed. Legacy coins
// (XXX / D / SXPAD) keep their fee-carrying mints (created before v4.3.0);
// their crank pipeline (harvest_withheld_fees) is unchanged.
// ---------------------------------------------------------------------------
export async function createLaunchMintInstructions({ connection, payer, mintKeypair }) {
  const mintLen = MINT_LEN_PLAIN;
  const rent = await connection.getMinimumBalanceForRentExemption(mintLen);
  const pool = launchPoolPda(mintKeypair.publicKey);
  const payerPk = new PublicKey(payer);
  return {
    mint: mintKeypair.publicKey,
    pool,
    instructions: [
      SystemProgram.createAccount({
        fromPubkey: payerPk,
        newAccountPubkey: mintKeypair.publicKey,
        space: mintLen,
        lamports: rent,
        programId: TOKEN_2022_PROGRAM_ID,
      }),
      createInitializeMint2Instruction(
        mintKeypair.publicKey,
        CONSTANTS.token.decimals,
        pool, // mint authority: the launch pool PDA (graduate mints from it)
        null,
        TOKEN_2022_PROGRAM_ID,
      ),
    ],
  };
}

// ---------------------------------------------------------------------------
// Account decoders (layouts mirror programs/agentpad/src/state.rs)
// ---------------------------------------------------------------------------
function readPubkey(buf, off) {
  return new PublicKey(buf.subarray(off, off + 32)).toBase58();
}
function readU64(buf, off) {
  return buf.readBigUInt64LE(off);
}
function readI64(buf, off) {
  return buf.readBigInt64LE(off);
}
function readU128(buf, off) {
  // Node's Buffer and the browser `buffer` polyfill have NO 128-bit read:
  // readBigUInt128LE does not exist, so the old call threw inside
  // decodeMinterRecord / decodeDividendVault. That silently zeroed
  // userShares (refreshToken swallows the throw), which left the refund tab
  // at "已购 0 份" and its input disabled (2026-11-19). Compose from two u64.
  const lo = buf.readBigUInt64LE(off);
  const hi = buf.readBigUInt64LE(off + 8);
  return (hi << 64n) | lo;
}
export function decodeGlobalConfig(buf) {
  const b = Buffer.from(buf);
  if (b.length < 194) throw new Error(`GlobalConfig too short: ${b.length}`);
  return {
    admin: readPubkey(b, 8), treasuryWallet: readPubkey(b, 40), gateSigner: readPubkey(b, 72),
    platformTokenMint: readPubkey(b, 104), platformPoolId: readPubkey(b, 136),
    totalLaunches: readU64(b, 168), bump: b[176], buybackVaultBump: b[177],
    totalBuybackSolSpent: readU64(b, 178), totalPlatformBurned: readU64(b, 186),
    // Added with the v2 redeploy (LEN 194 -> 195). Old 194-byte buffers
    // read undefined here -> false, so the check below stays safe.
    publicLaunchesEnabled: b[194] === 1,
  };
}
export function decodeLaunchPool(buf) {
  const b = Buffer.from(buf);
  if (b.length < 228) throw new Error(`LaunchPool too short: ${b.length}`);
  return {
    mint: readPubkey(b, 8), creator: readPubkey(b, 40), marketingWallet: readPubkey(b, 72),
    rewardMint: readPubkey(b, 104),
    totalShares: readU64(b, 136), soldShares: readU64(b, 144),
    pricePerShare: readU64(b, 152), tokensPerShare: readU64(b, 160), totalSolRaised: readU64(b, 168),
    status: ['Minting', 'Completed', 'Graduated'][b[176]] ?? `unknown(${b[176]})`,
    isOfficial: b[177] === 1, raydiumPoolId: readPubkey(b, 178),
    createdAt: readI64(b, 210), graduatedAt: readI64(b, 218),
    bump: b[226], solVaultBump: b[227],
  };
}
export function decodeMinterRecord(buf) {
  const b = Buffer.from(buf);
  if (b.length < 107) throw new Error(`MinterRecord too short: ${b.length}`);
  return {
    pool: readPubkey(b, 8), user: readPubkey(b, 40),
    sharesBought: readU64(b, 72), bonusShares: readU64(b, 80),
    hasRecordedInvite: b[88] === 1, hasClaimed: b[89] === 1,
    rewardDebt: readU128(b, 90), bump: b[106],
  };
}
export function decodeDividendVault(buf) {
  const b = Buffer.from(buf);
  if (b.length < 97) throw new Error(`DividendVault too short: ${b.length}`);
  return {
    pool: readPubkey(b, 8), rewardMint: readPubkey(b, 40),
    totalRewardDeposited: readU64(b, 72), accRewardPerToken: readU128(b, 80), bump: b[96],
  };
}
export function decodeMinterReward2(buf) {
  // v4: pool(32) + user(32) + rewardDebt u128(16) + bump(1) = 89 bytes.
  const b = Buffer.from(buf);
  if (b.length < 89) throw new Error(`MinterReward2 too short: ${b.length}`);
  return {
    pool: readPubkey(b, 8), user: readPubkey(b, 40),
    rewardDebt: readU128(b, 72), bump: b[88],
  };
}
export function decodeWhitelistedAsset(buf) {
  const b = Buffer.from(buf);
  if (b.length < 63) throw new Error(`WhitelistedAsset too short: ${b.length}`);
  return {
    assetMint: readPubkey(b, 8),
    symbol: Buffer.from(b.subarray(40, 52)).toString('utf8').replace(/\0+$/, ''),
    decimals: b[52], enabled: b[53] === 1, addedAt: readI64(b, 54), bump: b[62],
  };
}

// ---------------------------------------------------------------------------
// RPC reads
// ---------------------------------------------------------------------------
async function getAccountData(connection, pubkey) {
  const info = await connection.getAccountInfo(new PublicKey(pubkey));
  return info ? Buffer.from(info.data) : null;
}
export async function getGlobalConfig(connection) {
  const data = await getAccountData(connection, globalConfigPda());
  return data ? decodeGlobalConfig(data) : null;
}
export async function getLaunchPool(connection, mint) {
  const data = await getAccountData(connection, launchPoolPda(mint));
  return data ? decodeLaunchPool(data) : null;
}
export async function getMinterRecord(connection, mint, user) {
  const data = await getAccountData(connection, minterPda(mint, user));
  return data ? decodeMinterRecord(data) : null;
}

export async function countMinters(connection, mint) {
  // Distinct wallets that minted this token (the holders count). Filter
  // MinterRecord accounts by their pool field (offset 8 = pool PDA).
  const pool = launchPoolPda(mint);
  const disc = base58Encode(Buffer.from(ACCOUNT_DISCRIMINATORS.MinterRecord, 'hex'));
  const accounts = await connection.getProgramAccounts(PROGRAM_ID, {
    filters: [
      { memcmp: { offset: 0, bytes: disc } },
      { memcmp: { offset: 8, bytes: pool.toBase58() } },
      { dataSize: 107 },
    ],
  });
  return accounts.length;
}

export async function listMinters(connection, mint) {
  // Same scan as countMinters, but returns the wallets so callers can
  // union them with on-chain token holders (a minter who has not claimed
  // yet holds shares, not tokens — without this they are invisible in
  // the holders count). Only wallets with a live claim on tokens count:
  // sharesBought + bonusShares > 0 (a fully-refunded record stays on
  // chain with 0 shares and must not count). MinterRecord layout:
  // pool @8 (32), user @40 (32), sharesBought @72 (u64),
  // bonusShares @80 (u64).
  const pool = launchPoolPda(mint);
  const disc = base58Encode(Buffer.from(ACCOUNT_DISCRIMINATORS.MinterRecord, 'hex'));
  const accounts = await connection.getProgramAccounts(PROGRAM_ID, {
    filters: [
      { memcmp: { offset: 0, bytes: disc } },
      { memcmp: { offset: 8, bytes: pool.toBase58() } },
      { dataSize: 107 },
    ],
  });
  const out = [];
  for (const a of accounts) {
    const b = Buffer.from(a.account.data);
    if (b.length < 107) continue;
    const shares = Number(readU64(b, 72)) + Number(readU64(b, 80));
    if (shares > 0) out.push(readPubkey(b, 40));
  }
  return out;
}

export async function listMinterRecords(connection) {
  // All MinterRecord accounts in ONE program scan. The launch-list cache
  // groups them by pool server-side (holders per mint), instead of one
  // getProgramAccounts scan per token from the browser.
  const disc = base58Encode(Buffer.from(ACCOUNT_DISCRIMINATORS.MinterRecord, 'hex'));
  const accounts = await connection.getProgramAccounts(PROGRAM_ID, {
    filters: [{ memcmp: { offset: 0, bytes: disc } }],
  });
  return accounts.map((a) => decodeMinterRecord(a.account.data));
}
export async function getDividendVault(connection, mint) {
  const pool = launchPoolPda(mint);
  const data = await getAccountData(connection, dividendVaultPda(pool));
  return data ? decodeDividendVault(data) : null;
}
export async function getDividendVault2(connection, mint) {
  // v4: null when the launch is single-asset (no vault-2 account exists).
  const pool = launchPoolPda(mint);
  const data = await getAccountData(connection, dividendVault2Pda(pool));
  return data ? decodeDividendVault(data) : null;
}
export async function getMinterReward2(connection, mint, user) {
  const data = await getAccountData(connection, minterReward2Pda(mint, user));
  return data ? decodeMinterReward2(data) : null;
}
export async function listLaunches(connection) {
  const disc = base58Encode(Buffer.from(ACCOUNT_DISCRIMINATORS.LaunchPool, 'hex'));
  const accounts = await connection.getProgramAccounts(PROGRAM_ID, {
    filters: [{ memcmp: { offset: 0, bytes: disc } }],
  });
  return accounts.map((a) => ({ address: a.pubkey.toBase58(), ...decodeLaunchPool(a.account.data) }));
}
export async function listWhitelistedAssets(connection) {
  const disc = base58Encode(Buffer.from(ACCOUNT_DISCRIMINATORS.WhitelistedAsset, 'hex'));
  const accounts = await connection.getProgramAccounts(PROGRAM_ID, {
    filters: [{ memcmp: { offset: 0, bytes: disc } }],
  });
  return accounts.map((a) => ({ address: a.pubkey.toBase58(), ...decodeWhitelistedAsset(a.account.data) }));
}

// ---------------------------------------------------------------------------
// Gate flow client (talks to server/index.mjs)
// ---------------------------------------------------------------------------
async function apiJson(apiBase, path, init) {
  const res = await fetch(`${apiBase}${path}`, {
    ...init,
    headers: { 'content-type': 'application/json', ...(init?.headers || {}) },
  });
  const body = await res.json().catch(() => ({}));
  if (!res.ok) throw new Error(body.error || `API ${path} failed: ${res.status}`);
  return body;
}
export async function requestChallenge(apiBase, wallet) {
  return apiJson(apiBase, `/api/agent/challenge?wallet=${encodeURIComponent(wallet)}`);
}
export async function verifyChallenge(apiBase, wallet, signatureBase64) {
  return apiJson(apiBase, '/api/agent/verify', {
    method: 'POST',
    body: JSON.stringify({ wallet, signature: signatureBase64 }),
  });
}
export async function requestCoSign(apiBase, sessionToken, messageBase64) {
  return apiJson(apiBase, '/api/tx/build', {
    method: 'POST',
    body: JSON.stringify({ sessionToken, message: messageBase64 }),
  });
}
/** Add the gate co-signature to a transaction the wallet will also sign. */
export async function coSignTransaction(apiBase, sessionToken, tx, gateSignerPubkey) {
  const message = tx.compileMessage().serialize().toString('base64');
  const { coSignature } = await requestCoSign(apiBase, sessionToken, message);
  tx.addSignature(new PublicKey(gateSignerPubkey), Buffer.from(coSignature, 'base64'));
  return tx;
}
export async function getServerConfig(apiBase) {
  return apiJson(apiBase, '/api/config');
}
