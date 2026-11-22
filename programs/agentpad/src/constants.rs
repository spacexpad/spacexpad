use anchor_lang::prelude::{pubkey, Pubkey};

pub const TOKEN_DECIMALS: u8 = 6;

/// 1,000,000,000 tokens with 6 decimals = 10^15 raw units
pub const TOTAL_SUPPLY_RAW: u64 = 1_000_000_000 * 1_000_000;

/// Scheme A: 62.5% Public Mint (625,000,000 tokens)
pub const PUBLIC_MINT_SUPPLY_RAW: u64 = 625_000_000 * 1_000_000;

/// Scheme A: 37.5% Raydium CPMM LP (375,000,000 tokens)
pub const LP_TOKEN_SUPPLY_RAW: u64 = 375_000_000 * 1_000_000;

/// Shares constraints: baseline 10,000, step 1,000
pub const SHARES_STEP: u64 = 1_000;
pub const MIN_TOTAL_SHARES: u64 = 1_000;
pub const MAX_TOTAL_SHARES: u64 = 50_000;

/// Initial per-wallet cap: 1% of total shares
pub const INITIAL_CAP_BPS: u64 = 100; // 1% = 100 / 10_000

/// Graduation SOL split (bps of 10,000)
/// 60% LP + 35% Creator + 2.5% BuybackVault + 2.5% Platform Treasury
pub const GRAD_LP_SOL_BPS: u64 = 6_000;
pub const GRAD_CREATOR_SOL_BPS: u64 = 3_500;
pub const GRAD_BUYBACK_SOL_BPS: u64 = 250;
pub const GRAD_TREASURY_SOL_BPS: u64 = 250;

/// Post-launch Token-2022 TransferFee: 1% (100 bps) bidirectional.
/// v4.2.13 reverts the v4.2.12 transfer-hook experiment: the hook cannot
/// burn from the source account (the token program passes it readonly to
/// the hook, so a Burn CPI is rejected with "writable privilege
/// escalated"), so the 20% burn goes back to the crank (every 5 minutes).
/// The full 1% is withheld again; the crank sells 80% and burns 20%.
/// v4.3.0: new launches NO LONGER carry this fee (create_launch does not
/// require it, the SDK does not create it). The 1% tax moved to the
/// `swap_with_tax` wrapper (WRAPPER_TAX_BPS below) so mint / claim / P2P
/// transfers are tax-free and only pool trades are taxed. This constant
/// stays for the legacy coins (XXX / D / SXPAD), whose mints already
/// carry the fee and whose crank pipeline (harvest_withheld_fees) is
/// unchanged.
pub const TRANSFER_FEE_BPS: u16 = 100;
pub const MAX_TRANSFER_FEE_RAW: u64 = 1_000_000_000 * 1_000_000;

/// v4.3.0: wrapper tax rate (100 bps = 1% of the token leg of a pool
/// trade routed through `swap_with_tax`). Same rate as the legacy mint
/// fee, but collected by the program into the per-launch tax vault
/// instead of by the token program on every transfer.
pub const WRAPPER_TAX_BPS: u64 = 100;

/// 5-2-2-1 Tax Split (out of 10,000 bps of collected tax)
/// 20% Burned by the crank (2,000 bps), remaining 80% withheld by the
/// TransferFee and swapped to SOL by the crank:
/// - 50% (5,000 bps) -> Holder Dividends
/// - 20% (2,000 bps) -> Creator Marketing
/// - 5%  (500 bps)   -> BuybackVault PDA
/// - 5%  (500 bps)   -> Platform Treasury
pub const TAX_DIVIDEND_BPS: u64 = 5_000;
pub const TAX_BURN_BPS: u64 = 2_000;
pub const TAX_CREATOR_BPS: u64 = 2_000;
pub const TAX_BUYBACK_BPS: u64 = 500;
pub const TAX_TREASURY_BPS: u64 = 500;

/// Minimum token balance required to claim SOL dividends:
/// 0.02% of 1,000,000,000 = 200,000 tokens
pub const MIN_DIVIDEND_HOLDING_RAW: u64 = 200_000 * 1_000_000;

/// Precision scaler for dividend-per-token accumulator
pub const DIVIDEND_PRECISION: u128 = 1_000_000_000_000;

/// Wrapped SOL mint (used as the SOL side of the Raydium CPMM pool).
pub const WSOL_MINT: Pubkey = pubkey!("So11111111111111111111111111111111111111112");

/// Raydium CPMM `initialize` discriminator, taken verbatim from
/// @raydium-io/raydium-sdk-v2 `makeCreateCpmmPoolInInstruction`
/// (NOT sha256("global:initialize") — Raydium hardcodes its own).
pub const RAYDIUM_INITIALIZE_DISCRIMINATOR: [u8; 8] = [175, 175, 109, 31, 13, 152, 155, 237];

/// Raydium CPMM amm-config index used for every pool (see SDK `getCreatePoolKeys`).
pub const RAYDIUM_AMM_CONFIG_INDEX: u16 = 0;

/// Raydium CPMM `swap_base_input` discriminator, verbatim from
/// @raydium-io/raydium-sdk-v2 `makeSwapCpmmBaseInInInstruction`
/// (same value the SDK's `createSwapBaseInputIx` uses). Used by the
/// v4.3.0 `swap_with_tax` wrapper, which CPIs into Raydium's swap.
pub const RAYDIUM_SWAP_BASE_INPUT_DISCRIMINATOR: [u8; 8] =
    [143, 190, 90, 218, 196, 30, 51, 222];

/// Raydium CPMM program ids (v4.3.1, review item 2): the only AMM
/// programs the pool-creation and swap paths may CPI into. The devnet
/// id only exists on devnet and the mainnet id only on mainnet, so
/// accepting both is safe on either cluster. The caller passes the
/// program id; without this whitelist a fake program would be used
/// for every PDA derivation and CPI.
pub const RAYDIUM_CPMM_PROGRAM_ID_DEVNET: Pubkey =
    pubkey!("CPMDWBwJDtYax9qW7AyRuVC19Cc4L4Vcy4n2BHAbHkCW");
pub const RAYDIUM_CPMM_PROGRAM_ID_MAINNET: Pubkey =
    pubkey!("CPMMoo8L3F4NbTegBCKVNunggL7H1ZpdTHKxQB5qKP1C");

/// SPL Associated Token program id (same on every cluster). Used by
/// the v4.3.1 claim-vault check: the claim vault must be the pool's
/// ATA for the mint.
pub const ASSOCIATED_TOKEN_PROGRAM_ID: Pubkey =
    pubkey!("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL");

/// Rent buffer (lamports) funded into the launch-pool PDA alongside the
/// Raydium pool-creation fee, so the PDA can pay the rent for the pool
/// accounts Raydium's `initialize` creates (pool state, LP mint, two
/// vaults, observation state, creator LP token account). Measured on
/// devnet 2026-11-19: exact rent for those accounts is ~0.0528 SOL;
/// 0.1 SOL leaves a comfortable margin. Any leftover stays in the PDA.
/// Note the devnet createPoolFee alone is 1 SOL, so a launch needs an
/// LP share (60% of raised) above ~1.1 SOL for pool creation to work.
pub const RAYDIUM_POOL_RENT_BUFFER: u64 = 100_000_000;

/// Operations reserve (lamports) sent to the graduator at pool creation
/// (v4.2.11). The graduator (the tx payer) funds every graduation step's
/// tx fees plus the metadata rent (~0.0137 SOL, v4.2.10) and the ongoing
/// 6-hour crank tx fees. Instead of manual top-ups, each graduation sets
/// aside this fixed reserve from the 60% LP share and sends it to the
/// graduator. 0.02 SOL covers the metadata rent and leaves ~0.006 SOL
/// surplus per graduation, which accumulates to cover crank fees
/// indefinitely. The LP side of the pool is reduced by the same 0.02 SOL.
/// The minimum-raise check (graduate_to_raydium) counts this reserve
/// alongside the pool fee and rent buffer, so a launch must raise enough
/// to cover all three.
pub const GRADUATOR_RESERVE_LAMPORTS: u64 = 20_000_000;

/// Metaplex Token Metadata program (same ID on devnet and mainnet).
/// Used by `set_token_metadata` to create the metadata account that
/// Raydium / DexScreener / wallets read for name, symbol and icon.
pub const METAPLEX_METADATA_PROGRAM_ID: Pubkey =
    pubkey!("metaqbxxUerdq28cj1RbAWkYQm3ybzjb6a8bt518x1s");
