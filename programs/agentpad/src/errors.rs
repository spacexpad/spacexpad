use anchor_lang::prelude::*;

#[error_code]
pub enum AgentPadError {
    #[msg("Unauthorized caller for this instruction.")]
    Unauthorized,
    #[msg("Invalid AgentGateSigner co-signature. Only verified Solana Agent Wallets may participate.")]
    InvalidAgentGateSignature,
    #[msg("Total shares must be at least 1,000 and a multiple of 1,000.")]
    InvalidTotalShares,
    #[msg("Price per share must be greater than zero.")]
    InvalidSharePrice,
    #[msg("Shares count must be greater than zero.")]
    InvalidSharesCount,
    #[msg("Launch pool is not currently in Minting status.")]
    PoolNotMinting,
    #[msg("Launch pool has not reached 100% graduation yet.")]
    PoolNotCompleted,
    #[msg("Launch pool has not graduated to Raydium CPMM yet.")]
    PoolNotGraduated,
    #[msg("Requested shares exceed remaining available shares.")]
    ExceedsRemainingShares,
    #[msg("Requested shares exceed your current personal cap (1% base + 1 share per invited Agent).")]
    ExceedsPersonalCap,
    #[msg("Cannot invite yourself.")]
    SelfInviteNotAllowed,
    #[msg("Insufficient shares held for refund.")]
    InsufficientSharesForRefund,
    #[msg("Tokens have already been claimed for this wallet.")]
    AlreadyClaimed,
    #[msg("No tokens available to claim.")]
    NothingToClaim,
    #[msg("Token balance is below the 200,000 (0.02%) minimum threshold for dividends.")]
    BelowDividendThreshold,
    #[msg("No pending dividends to claim.")]
    NoDividendsToClaim,
    #[msg("Arithmetic overflow occurred.")]
    MathOverflow,
    #[msg("Platform token mint is not configured yet.")]
    PlatformTokenNotConfigured,
    #[msg("Selected dividend reward asset is not whitelisted or is currently disabled.")]
    DividendAssetNotWhitelisted,
    #[msg("Provided reward token account does not match the pool's configured reward_mint.")]
    InvalidRewardMintAccount,
    #[msg("Mint does not carry a 1% Token-2022 TransferFee extension.")]
    TransferFeeNotConfigured,
    #[msg("Mint authority must be the launch pool PDA.")]
    InvalidMintAuthority,
    #[msg("Mint must be fresh (zero supply) at launch creation.")]
    MintSupplyNotZero,
    #[msg("Launch pool already has a Raydium pool.")]
    PoolAlreadyHasRaydiumPool,
    #[msg("A Raydium account does not match its expected PDA.")]
    InvalidRaydiumAccount,
    #[msg("The Raydium amm config forbids pool creation.")]
    RaydiumPoolDisabled,
    #[msg("Not enough SOL left in the vault for the Raydium pool plus its creation fee.")]
    InsufficientLpSol,
    #[msg("token_0_mint must be the smaller pubkey of the pair.")]
    InvalidTokenOrder,
    #[msg("Mint decimals must be 6.")]
    InvalidMintDecimals,
    #[msg("Buyback vault holds fewer platform tokens than requested to burn.")]
    InsufficientBuybackBalance,
    #[msg("Public launch creation is currently disabled by the admin.")]
    LaunchesDisabled,
    #[msg("Token metadata name (max 32), symbol (max 10) or URI (max 200) is too long.")]
    InvalidMetadata,
    #[msg("The metadata account is not the Metaplex PDA for this launch mint, or the pool mint does not match.")]
    InvalidMetadataAccount,
    #[msg("Second dividend asset is invalid (duplicates the first asset, or SOL paired with SOL).")]
    InvalidSecondRewardMint,
    #[msg("LP staging account is not the expected PDA for this launch pool, or is not a valid Token-2022 account for the launch mint owned by the pool.")]
    InvalidLpStagingAccount,
    #[msg("Creator LP token account is not the expected associated token account for the LP mint owned by the pool.")]
    InvalidCreatorLpToken,
    #[msg("Total raise is too small: the 60% LP share would not cover the Raydium pool creation fee plus rent buffer.")]
    RaiseTooSmallForPoolCreation,
    #[msg("Minter record PDA does not match the expected address or its data is invalid.")]
    InvalidMinterRecord,
    #[msg("Dividend vault does not hold enough SOL to pay the pending dividend.")]
    InsufficientDividendBalance,
    #[msg("Mint does not carry the AgentPad real-time burn transfer hook.")]
    TransferHookNotConfigured,
}
