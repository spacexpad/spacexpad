use anchor_lang::prelude::*;

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq)]
pub enum LaunchStatus {
    Minting = 0,
    Completed = 1,
    Graduated = 2,
}

#[account]
pub struct GlobalConfig {
    pub admin: Pubkey,
    pub treasury_wallet: Pubkey,
    pub gate_signer: Pubkey,
    pub platform_token_mint: Pubkey,
    pub platform_pool_id: Pubkey,
    pub total_launches: u64,
    pub bump: u8,
    pub buyback_vault_bump: u8,
    /// Total SOL released from the buyback vault for manual buybacks.
    pub total_buyback_sol_spent: u64,
    /// Total platform tokens burned through manual_buyback_and_burn.
    pub total_platform_burned: u64,
    /// Whether permissionless launch creation is open. When false, only
    /// the admin (contract deployer) or a gate co-signed creation may
    /// create a launch. Defaults to false (closed).
    pub public_launches_enabled: bool,
}

impl GlobalConfig {
    pub const LEN: usize = 8 + 32 * 5 + 8 + 1 + 1 + 8 + 8 + 1;
}

/// Dynamic on-chain whitelist entry for stock token dividend assets
/// (e.g., NVDAx, TSLAx, AAPLx, MSTRx, SPCX). Admin can add new assets anytime.
#[account]
pub struct WhitelistedAsset {
    pub asset_mint: Pubkey,
    pub symbol: [u8; 12],
    pub decimals: u8,
    pub enabled: bool,
    pub added_at: i64,
    pub bump: u8,
}

impl WhitelistedAsset {
    pub const LEN: usize = 8 + 32 + 12 + 1 + 1 + 8 + 1;
}

#[account]
pub struct LaunchPool {
    pub mint: Pubkey,
    pub creator: Pubkey,
    pub marketing_wallet: Pubkey,
    /// Pubkey::default() = Native SOL dividends; otherwise a whitelisted stock token mint
    pub reward_mint: Pubkey,
    pub total_shares: u64,
    pub sold_shares: u64,
    pub price_per_share: u64,
    pub tokens_per_share: u64,
    pub total_sol_raised: u64,
    pub status: LaunchStatus,
    pub is_official: bool,
    pub raydium_pool_id: Pubkey,
    pub created_at: i64,
    pub graduated_at: i64,
    pub bump: u8,
    pub sol_vault_bump: u8,
}

impl LaunchPool {
    pub const LEN: usize = 8 + 32 * 5 + 8 * 5 + 1 + 1 + 8 * 2 + 1 + 1;
}

#[account]
pub struct MinterRecord {
    pub pool: Pubkey,
    pub user: Pubkey,
    pub shares_bought: u64,
    pub bonus_shares: u64,
    pub has_recorded_invite: bool,
    pub has_claimed: bool,
    pub reward_debt: u128,
    pub bump: u8,
}

impl MinterRecord {
    pub const LEN: usize = 8 + 32 * 2 + 8 * 2 + 1 + 1 + 16 + 1;
}

#[account]
pub struct DividendVault {
    pub pool: Pubkey,
    pub reward_mint: Pubkey,
    pub total_reward_deposited: u64,
    pub acc_reward_per_token: u128,
    pub bump: u8,
}

impl DividendVault {
    pub const LEN: usize = 8 + 32 * 2 + 8 + 16 + 1;
}

/// Second-asset dividend debt for dual-asset launches (v4). A separate PDA
/// so existing MinterRecords (fixed LEN) stay valid: the second leg of
/// claim_dividend reads/updates this record instead of MinterRecord.
#[account]
pub struct MinterReward2 {
    pub pool: Pubkey,
    pub user: Pubkey,
    pub reward_debt: u128,
    pub bump: u8,
}

impl MinterReward2 {
    pub const LEN: usize = 8 + 32 * 2 + 16 + 1;
}
