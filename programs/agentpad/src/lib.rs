use anchor_lang::prelude::*;
use anchor_lang::solana_program::instruction::{AccountMeta, Instruction};
use anchor_lang::solana_program::program::{invoke, invoke_signed};
use anchor_lang::solana_program::system_instruction;
use anchor_lang::solana_program::program_option::COption;
use anchor_lang::solana_program::sysvar::rent::Rent;
use anchor_lang::system_program;
use anchor_spl::associated_token::AssociatedToken;
use anchor_spl::token::Token;
use anchor_spl::token_2022::{self, Burn, MintTo, Token2022, TransferChecked};
use anchor_spl::token_interface::{self, Mint, TokenAccount, TokenInterface};
use spl_token_2022::extension::{
    transfer_fee::TransferFeeConfig, BaseStateWithExtensions, ExtensionType,
    StateWithExtensions,
};

pub mod constants;
pub mod errors;
pub mod state;

use constants::*;
use errors::*;
use state::*;

declare_id!("DMt9jcZTMgHzzZcEngNj8c1Q6ahVdyLACirZwT71NzA6");

#[program]
pub mod agentpad {
    use super::*;

    /// Initialize global configuration and the non-withdrawable BuybackVault PDA.
    pub fn initialize_global(
        ctx: Context<InitializeGlobal>,
        treasury_wallet: Pubkey,
        gate_signer: Pubkey,
    ) -> Result<()> {
        let cfg = &mut ctx.accounts.global_config;
        cfg.admin = ctx.accounts.admin.key();
        cfg.treasury_wallet = treasury_wallet;
        cfg.gate_signer = gate_signer;
        cfg.platform_token_mint = Pubkey::default();
        cfg.platform_pool_id = Pubkey::default();
        cfg.total_launches = 0;
        cfg.bump = ctx.bumps.global_config;
        cfg.buyback_vault_bump = ctx.bumps.buyback_vault;
        cfg.total_buyback_sol_spent = 0;
        cfg.total_platform_burned = 0;
        cfg.public_launches_enabled = false;
        Ok(())
    }

    /// Open or close permissionless launch creation. Admin only; the admin
    /// (and gate co-signed creations) are never restricted by the switch.
    pub fn set_public_launches_enabled(
        ctx: Context<AdminOnly>,
        enabled: bool,
    ) -> Result<()> {
        ctx.accounts.global_config.public_launches_enabled = enabled;
        Ok(())
    }

    /// Create the Metaplex Token Metadata account for a launch's mint so
    /// Raydium / DexScreener / wallets show the token's name, symbol and
    /// icon. Admin or gate signer (v4.2.9: the gate service calls this at
    /// graduation, when the backend registry already holds the
    /// normalized icon and the token JSON). The launch-pool PDA is the
    /// mint authority, so the program signs for it; the caller is the
    /// update authority, so the URI can later be re-pointed through the
    /// metadata program (UpdateMetadataAccountV2) by whoever created it.
    /// The rent is paid by a separate `payer` (v4.2.10: the graduator),
    /// so the gate signer needs no SOL of its own. Create-only: if the
    /// metadata account already exists the metadata program rejects the
    /// call.
    pub fn set_token_metadata(
        ctx: Context<SetTokenMetadata>,
        name: String,
        symbol: String,
        uri: String,
    ) -> Result<()> {
        // Caller must be the admin or the gate signer.
        require!(
            ctx.accounts.authority.key() == ctx.accounts.global_config.admin
                || ctx.accounts.authority.key() == ctx.accounts.global_config.gate_signer,
            AgentPadError::Unauthorized
        );
        require!(
            name.len() <= 32 && symbol.len() <= 10 && uri.len() <= 200,
            AgentPadError::InvalidMetadata
        );
        let pool = &ctx.accounts.launch_pool;
        let mint_key = ctx.accounts.mint.key();
        require!(
            pool.mint == mint_key,
            AgentPadError::InvalidMetadataAccount
        );
        let (metadata_pda, _) = Pubkey::find_program_address(
            &[
                b"metadata",
                METAPLEX_METADATA_PROGRAM_ID.as_ref(),
                mint_key.as_ref(),
            ],
            &METAPLEX_METADATA_PROGRAM_ID,
        );
        require!(
            metadata_pda == ctx.accounts.metadata.key(),
            AgentPadError::InvalidMetadataAccount
        );
        // The token program must be Token-2022 (our mints are Token-2022).
        require!(
            ctx.accounts.token_program.key() == anchor_spl::token_2022::ID,
            AgentPadError::InvalidMetadataAccount
        );

        // Metaplex `Create` (discriminator 42) — the NEW Token Metadata API.
        // CreateMetadataAccountV3 (discriminator 33) is REJECTED for
        // Token-2022 mints: the on-chain program routes legacy instructions
        // through `incompatible_accounts`, which returns true for any account
        // owned by Token-2022, and fails with 0x99 InstructionNotSupported
        // (verified 2026-11-19 against mpl-token-metadata main).
        //
        // DISCRIMINANT ENCODING (got this wrong the first time):
        // `MetadataInstruction`, `CreateArgs` and `TokenStandard` are all
        // `#[repr(C)]` + Borsh, and Borsh serializes their discriminants as
        // a SINGLE u8 — not u32. Proof on-chain: the V3 attempt sent
        // discriminator 33 as one byte and reached the handler (failed later
        // at 0x99); the first Create attempt sent the CreateArgs variant as
        // u32, so the program read the name length (9) as the variant and
        // failed with InvalidInstructionData.
        // Layout: u8 42 | u8 CreateArgs::V1 (0) | name, symbol, uri
        // (strings) | seller_fee_basis_points u16, creators None,
        // primary_sale_happened false, is_mutable true,
        // token_standard Fungible (=2, u8), collection None, uses None,
        // collection_details None, rule_set None, decimals None,
        // print_supply None.
        let mut data: Vec<u8> =
            Vec::with_capacity(64 + name.len() + symbol.len() + uri.len());
        data.push(42u8);
        data.push(0u8); // CreateArgs::V1
        for s in [&name, &symbol, &uri] {
            data.extend_from_slice(&(s.len() as u32).to_le_bytes());
            data.extend_from_slice(s.as_bytes());
        }
        data.extend_from_slice(&0u16.to_le_bytes()); // seller_fee_basis_points
        data.push(0u8); // creators: None
        data.push(0u8); // primary_sale_happened: false
        data.push(1u8); // is_mutable: true
        data.push(2u8); // token_standard: Fungible
        data.push(0u8); // collection: None
        data.push(0u8); // uses: None
        data.push(0u8); // collection_details: None
        data.push(0u8); // rule_set: None
        data.push(0u8); // decimals: None (mint already exists)
        data.push(0u8); // print_supply: None

        let pool_seeds: &[&[u8]] = &[
            b"launch_pool",
            mint_key.as_ref(),
            &[pool.bump],
        ];
        invoke_signed(
            &Instruction {
                program_id: METAPLEX_METADATA_PROGRAM_ID,
                accounts: vec![
                    // 0: metadata (writable)
                    AccountMeta::new(ctx.accounts.metadata.key(), false),
                    // 1: master_edition placeholder = metadata program itself
                    AccountMeta::new_readonly(METAPLEX_METADATA_PROGRAM_ID, false),
                    // 2: mint (writable, not signer — already exists)
                    AccountMeta::new(ctx.accounts.mint.key(), false),
                    // 3: mint authority = pool PDA (signer via invoke_signed)
                    AccountMeta::new_readonly(ctx.accounts.launch_pool.key(), true),
                    // 4: payer = payer (signer, v4.2.10: the graduator)
                    AccountMeta::new(ctx.accounts.payer.key(), true),
                    // 5: update authority = authority (signer)
                    AccountMeta::new_readonly(ctx.accounts.authority.key(), true),
                    // 6: system program
                    AccountMeta::new_readonly(system_program::ID, false),
                    // 7: sysvar instructions
                    AccountMeta::new_readonly(
                        anchor_lang::solana_program::sysvar::instructions::ID,
                        false,
                    ),
                    // 8: spl token program = Token-2022
                    AccountMeta::new_readonly(anchor_spl::token_2022::ID, false),
                ],
                data,
            },
            &[
                ctx.accounts.metadata.to_account_info(),
                ctx.accounts.metadata_program.to_account_info(),
                ctx.accounts.mint.to_account_info(),
                ctx.accounts.launch_pool.to_account_info(),
                ctx.accounts.payer.to_account_info(),
                ctx.accounts.authority.to_account_info(),
                ctx.accounts.system_program.to_account_info(),
                ctx.accounts.sysvar_instructions.to_account_info(),
                ctx.accounts.token_program.to_account_info(),
            ],
            &[pool_seeds],
        )?;
        Ok(())
    }

    /// Set the official platform token mint & Raydium pool after genesis launch graduates.
    pub fn set_platform_token(
        ctx: Context<AdminOnly>,
        platform_token_mint: Pubkey,
        platform_pool_id: Pubkey,
    ) -> Result<()> {
        let cfg = &mut ctx.accounts.global_config;
        cfg.platform_token_mint = platform_token_mint;
        cfg.platform_pool_id = platform_pool_id;
        Ok(())
    }

    /// Add a new stock token to the dividend reward whitelist (e.g., NVDAx, TSLAx, AAPLx, MSTRx, SPCX).
    /// Can be called anytime by admin to expand supported dividend assets.
    pub fn add_dividend_asset(
        ctx: Context<AddDividendAsset>,
        symbol: String,
        decimals: u8,
    ) -> Result<()> {
        let entry = &mut ctx.accounts.whitelisted_asset;
        entry.asset_mint = ctx.accounts.asset_mint.key();
        let mut sym_bytes = [0u8; 12];
        let src = symbol.as_bytes();
        let len = src.len().min(12);
        sym_bytes[..len].copy_from_slice(&src[..len]);
        entry.symbol = sym_bytes;
        entry.decimals = decimals;
        entry.enabled = true;
        entry.added_at = Clock::get()?.unix_timestamp;
        entry.bump = ctx.bumps.whitelisted_asset;

        emit!(DividendAssetWhitelistedEvent {
            asset_mint: entry.asset_mint,
            symbol,
            decimals,
            enabled: true,
        });
        Ok(())
    }

    /// Enable or disable an existing whitelisted dividend stock token.
    pub fn set_dividend_asset_status(
        ctx: Context<ToggleDividendAsset>,
        enabled: bool,
    ) -> Result<()> {
        ctx.accounts.whitelisted_asset.enabled = enabled;
        Ok(())
    }

    /// Permissionless token launch creation.
    /// Creator selects `reward_mint`: None = Native SOL, Some(mint) = Whitelisted stock token.
    /// `reward_mint_2` (v4) is the optional second dividend asset: when set,
    /// dividends split 50/50 between the two assets. None = single-asset
    /// launch (unchanged behavior). A launch is tagged `is_official = true`
    /// when its creator is the admin (contract deployer) or when the backend
    /// gate signer co-signed the creation. When `public_launches_enabled`
    /// is false, only the admin or a gate co-signed creation may create a
    /// launch at all.
    pub fn create_launch(
        ctx: Context<CreateLaunch>,
        total_shares: u64,
        price_per_share: u64,
        marketing_wallet: Pubkey,
        reward_mint: Option<Pubkey>,
        reward_mint_2: Option<Pubkey>,
    ) -> Result<()> {
        require!(
            total_shares >= MIN_TOTAL_SHARES
                && total_shares <= MAX_TOTAL_SHARES
                && total_shares % SHARES_STEP == 0,
            AgentPadError::InvalidTotalShares
        );
        require!(price_per_share > 0, AgentPadError::InvalidSharePrice);

        // v4.2.7: minimum-raise check — the 60% LP share of the total
        // raise must cover the Raydium pool-creation fee plus the rent
        // buffer, or the launch could never graduate to a pool (launch
        // A: 1 SOL raise, 0.6 SOL LP share < 1.1 SOL needed). The fee is
        // read from the Raydium amm_config, the same source
        // `create_raydium_pool` uses, so the two can never drift apart.
        // v4.2.11: the graduator operations reserve (0.02 SOL) is added
        // here too, so the LP share must cover fee + rent buffer +
        // reserve (~1.12 SOL total).
        // The amm_config PDA is verified against its owning program
        // (the Raydium CPMM program): a fake config would need a program
        // that owns an account at that PDA address, and the SDK always
        // passes the real config.
        let create_pool_fee = {
            let amm_config_info = &ctx.accounts.amm_config;
            let (amm_config_key, _) = Pubkey::find_program_address(
                &[b"amm_config", &RAYDIUM_AMM_CONFIG_INDEX.to_be_bytes()],
                amm_config_info.owner,
            );
            require_keys_eq!(
                amm_config_info.key(),
                amm_config_key,
                AgentPadError::InvalidRaydiumAccount
            );
            let config_data = amm_config_info.try_borrow_data()?;
            require!(
                config_data.len() >= 44,
                AgentPadError::InvalidRaydiumAccount
            );
            require!(config_data[9] == 0, AgentPadError::RaydiumPoolDisabled);
            u64::from_le_bytes(
                config_data[36..44]
                    .try_into()
                    .map_err(|_| AgentPadError::InvalidRaydiumAccount)?,
            )
        };
        let total_raise = total_shares
            .checked_mul(price_per_share)
            .ok_or(AgentPadError::MathOverflow)?;
        let lp_sol = total_raise
            .checked_mul(GRAD_LP_SOL_BPS)
            .ok_or(AgentPadError::MathOverflow)?
            .checked_div(10_000)
            .ok_or(AgentPadError::MathOverflow)?;
        require!(
            lp_sol
                > create_pool_fee + RAYDIUM_POOL_RENT_BUFFER + GRADUATOR_RESERVE_LAMPORTS,
            AgentPadError::RaiseTooSmallForPoolCreation
        );

        // The mint must be a fresh Token-2022 mint (6 decimals, zero supply)
        // whose mint authority is this launch pool, carrying the 1% transfer
        // fee. Otherwise graduate_to_raydium could never mint and the "1%
        // tax" would be unenforced. The SDK creates such mints
        // (see sdk/agentpad-sdk.mjs `createLaunchMint`).
        let mint_acc = &ctx.accounts.mint;
        require!(
            mint_acc.decimals == TOKEN_DECIMALS,
            AgentPadError::InvalidMintDecimals
        );
        require!(mint_acc.supply == 0, AgentPadError::MintSupplyNotZero);
        require!(
            mint_acc.mint_authority == COption::Some(ctx.accounts.launch_pool.key()),
            AgentPadError::InvalidMintAuthority
        );
        {
            let mint_info = mint_acc.to_account_info();
            let mint_data = mint_info.try_borrow_data()?;
            let mint_state =
                StateWithExtensions::<spl_token_2022::state::Mint>::unpack(&mint_data)
                    .map_err(|_| AgentPadError::TransferFeeNotConfigured)?;
            let fee_config = mint_state
                .get_extension::<TransferFeeConfig>()
                .map_err(|_| AgentPadError::TransferFeeNotConfigured)?;
            require!(
                fee_config.newer_transfer_fee.transfer_fee_basis_points == TRANSFER_FEE_BPS.into(),
                AgentPadError::TransferFeeNotConfigured
            );
            // v4.2.13: the transfer-hook requirement is removed (the hook
            // cannot burn from the source account — the token program
            // passes it readonly — so the 20% burn goes back to the
            // crank). No hook extension is required on the mint.
        }

        let resolved_reward_mint = match reward_mint {
            None => Pubkey::default(),
            Some(rm) => {
                let wl = ctx
                    .accounts
                    .whitelisted_asset
                    .as_ref()
                    .ok_or(AgentPadError::DividendAssetNotWhitelisted)?;
                require!(
                    wl.enabled && wl.asset_mint == rm,
                    AgentPadError::DividendAssetNotWhitelisted
                );
                rm
            }
        };

        // Second dividend asset (v4): None = single-asset launch. Some(m)
        // must differ from the first asset; a stock token must be
        // whitelisted + enabled, SOL (default pubkey) is always allowed
        // as long as the first asset is not also SOL.
        let resolved_reward_mint_2 = match reward_mint_2 {
            None => None,
            Some(rm2) => {
                if rm2 == Pubkey::default() {
                    require!(
                        resolved_reward_mint != Pubkey::default(),
                        AgentPadError::InvalidSecondRewardMint
                    );
                    Some(rm2)
                } else {
                    require!(
                        rm2 != resolved_reward_mint,
                        AgentPadError::InvalidSecondRewardMint
                    );
                    let wl2 = ctx
                        .accounts
                        .whitelisted_asset_2
                        .as_ref()
                        .ok_or(AgentPadError::DividendAssetNotWhitelisted)?;
                    require!(
                        wl2.enabled && wl2.asset_mint == rm2,
                        AgentPadError::DividendAssetNotWhitelisted
                    );
                    Some(rm2)
                }
            }
        };

        let cfg = &mut ctx.accounts.global_config;
        // Launch gate: when public creation is closed, only the admin
        // (contract deployer) or a backend/gate co-signed creation may
        // create a launch. The same two identities are what make a launch
        // official.
        let is_admin = ctx.accounts.creator.key() == cfg.admin;
        let gate_signed = match &ctx.accounts.gate_signer {
            Some(gs) => gs.is_signer && gs.key() == cfg.gate_signer,
            None => false,
        };
        if !cfg.public_launches_enabled {
            require!(is_admin || gate_signed, AgentPadError::LaunchesDisabled);
        }
        let is_official = is_admin || gate_signed;
        cfg.total_launches = cfg
            .total_launches
            .checked_add(1)
            .ok_or(AgentPadError::MathOverflow)?;

        let tokens_per_share = PUBLIC_MINT_SUPPLY_RAW
            .checked_div(total_shares)
            .ok_or(AgentPadError::MathOverflow)?;

        let pool = &mut ctx.accounts.launch_pool;
        pool.mint = ctx.accounts.mint.key();
        pool.creator = ctx.accounts.creator.key();
        pool.marketing_wallet = marketing_wallet;
        pool.reward_mint = resolved_reward_mint;
        pool.total_shares = total_shares;
        pool.sold_shares = 0;
        pool.price_per_share = price_per_share;
        pool.tokens_per_share = tokens_per_share;
        pool.total_sol_raised = 0;
        pool.status = LaunchStatus::Minting;
        pool.is_official = is_official;
        pool.raydium_pool_id = Pubkey::default();
        pool.created_at = Clock::get()?.unix_timestamp;
        pool.graduated_at = 0;
        pool.bump = ctx.bumps.launch_pool;
        pool.sol_vault_bump = ctx.bumps.sol_vault;

        let div_vault = &mut ctx.accounts.dividend_vault;
        div_vault.pool = pool.key();
        div_vault.reward_mint = resolved_reward_mint;
        div_vault.total_reward_deposited = 0;
        div_vault.acc_reward_per_token = 0;
        div_vault.bump = ctx.bumps.dividend_vault;

        // Second dividend vault (v4): only when a second asset was chosen.
        // Manual creation: Anchor 0.30 `init_if_needed` on an
        // `Option<Account>` crashes this instruction (BPF null-pointer
        // read), so create the account with a SystemProgram CPI and write
        // the DividendVault state by hand.
        if let Some(rm2) = resolved_reward_mint_2 {
            let v2_info = ctx
                .accounts
                .dividend_vault_2
                .as_ref()
                .ok_or(AgentPadError::InvalidSecondRewardMint)?;
            let (expected_pda, v2_bump) = Pubkey::find_program_address(
                &[b"dividend_vault_2", pool.key().as_ref()],
                ctx.program_id,
            );
            require_keys_eq!(
                v2_info.key(),
                expected_pda,
                AgentPadError::InvalidSecondRewardMint
            );
            if v2_info.lamports() == 0 {
                let rent = Rent::get()?;
                let lamports = rent.minimum_balance(DividendVault::LEN);
                // `invoke_signed` with the vault-2 PDA seeds (the same
                // pattern Anchor's own `init` uses for launch_pool /
                // dividend_vault): a plain `invoke` of create_account for
                // this PDA fails with "signer privilege escalated"
                // (devnet-verified 2026-11-19) — the runtime wants the PDA
                // authorized as a signer via its seeds.
                let pool_key = pool.key();
                let v2_seeds: &[&[u8]] = &[
                    b"dividend_vault_2",
                    pool_key.as_ref(),
                    &[v2_bump],
                ];
                let create_ix = system_instruction::create_account(
                    &ctx.accounts.creator.key(),
                    &v2_info.key(),
                    lamports,
                    DividendVault::LEN as u64,
                    ctx.program_id,
                );
                invoke_signed(
                    &create_ix,
                    &[
                        ctx.accounts.creator.to_account_info(),
                        v2_info.to_account_info(),
                    ],
                    &[v2_seeds],
                )?;
            }
            let v2_state = DividendVault {
                pool: pool.key(),
                reward_mint: rm2,
                total_reward_deposited: 0,
                acc_reward_per_token: 0,
                bump: v2_bump,
            };
            let mut v2_data = v2_info.try_borrow_mut_data()?;
            v2_state.try_serialize(&mut &mut v2_data[..])?;
        }

        emit!(LaunchCreatedEvent {
            mint: pool.mint,
            creator: pool.creator,
            reward_mint: resolved_reward_mint,
            reward_mint_2: resolved_reward_mint_2,
            total_shares,
            price_per_share,
            is_official,
        });

        Ok(())
    }

    /// Agent-only fair mint. Requires backend AgentGateSigner co-signature.
    /// Initial cap = 1% of total_shares; +1 share per invited new Agent.
    pub fn mint_shares(ctx: Context<MintShares>, shares_count: u64) -> Result<()> {
        require!(shares_count > 0, AgentPadError::InvalidSharesCount);
        require_keys_eq!(
            ctx.accounts.agent_gate_signer.key(),
            ctx.accounts.global_config.gate_signer,
            AgentPadError::InvalidAgentGateSignature
        );

        let pool = &mut ctx.accounts.launch_pool;
        require!(
            pool.status == LaunchStatus::Minting,
            AgentPadError::PoolNotMinting
        );

        let remaining = pool
            .total_shares
            .checked_sub(pool.sold_shares)
            .ok_or(AgentPadError::MathOverflow)?;
        require!(
            shares_count <= remaining,
            AgentPadError::ExceedsRemainingShares
        );

        let record = &mut ctx.accounts.minter_record;
        if record.user == Pubkey::default() {
            record.pool = pool.key();
            record.user = ctx.accounts.user.key();
            record.shares_bought = 0;
            // NOTE: bonus_shares is deliberately NOT reset here. A record
            // can be created first as an inviter (init_if_needed in
            // MintShares) with bonus_shares > 0 and user == default;
            // resetting it when the inviter later mints would wipe the
            // invite bonus they already earned.
            record.has_recorded_invite = false;
            record.has_claimed = false;
            record.reward_debt = 0;
            record.bump = ctx.bumps.minter_record;
        }

        let base_cap = pool
            .total_shares
            .checked_mul(INITIAL_CAP_BPS)
            .ok_or(AgentPadError::MathOverflow)?
            .checked_div(10_000)
            .ok_or(AgentPadError::MathOverflow)?;
        let personal_cap = base_cap
            .checked_add(record.bonus_shares)
            .ok_or(AgentPadError::MathOverflow)?;
        let new_user_shares = record
            .shares_bought
            .checked_add(shares_count)
            .ok_or(AgentPadError::MathOverflow)?;
        require!(
            new_user_shares <= personal_cap,
            AgentPadError::ExceedsPersonalCap
        );

        // First-time referral boost (+1 share to inviter, no clawback on refund)
        if !record.has_recorded_invite {
            record.has_recorded_invite = true;
            if let Some(inviter_rec) = &mut ctx.accounts.inviter_record {
                require!(
                    inviter_rec.user != ctx.accounts.user.key(),
                    AgentPadError::SelfInviteNotAllowed
                );
                inviter_rec.bonus_shares = inviter_rec
                    .bonus_shares
                    .checked_add(1)
                    .ok_or(AgentPadError::MathOverflow)?;
            }
        }

        let sol_cost = shares_count
            .checked_mul(pool.price_per_share)
            .ok_or(AgentPadError::MathOverflow)?;

        system_program::transfer(
            CpiContext::new(
                ctx.accounts.system_program.to_account_info(),
                system_program::Transfer {
                    from: ctx.accounts.user.to_account_info(),
                    to: ctx.accounts.sol_vault.to_account_info(),
                },
            ),
            sol_cost,
        )?;

        record.shares_bought = new_user_shares;
        pool.sold_shares = pool
            .sold_shares
            .checked_add(shares_count)
            .ok_or(AgentPadError::MathOverflow)?;
        pool.total_sol_raised = pool
            .total_sol_raised
            .checked_add(sol_cost)
            .ok_or(AgentPadError::MathOverflow)?;

        if pool.sold_shares == pool.total_shares {
            pool.status = LaunchStatus::Completed;
        }

        emit!(SharesMintedEvent {
            mint: pool.mint,
            user: ctx.accounts.user.key(),
            shares_count,
            sol_cost,
            sold_shares: pool.sold_shares,
        });

        Ok(())
    }

    /// 0% fee lossless principal refund anytime before graduation.
    pub fn refund_shares(ctx: Context<RefundShares>, shares_count: u64) -> Result<()> {
        require!(shares_count > 0, AgentPadError::InvalidSharesCount);
        require_keys_eq!(
            ctx.accounts.agent_gate_signer.key(),
            ctx.accounts.global_config.gate_signer,
            AgentPadError::InvalidAgentGateSignature
        );

        let pool = &mut ctx.accounts.launch_pool;
        require!(
            pool.status == LaunchStatus::Minting,
            AgentPadError::PoolNotMinting
        );

        let record = &mut ctx.accounts.minter_record;
        require!(
            record.shares_bought >= shares_count,
            AgentPadError::InsufficientSharesForRefund
        );

        let refund_lamports = shares_count
            .checked_mul(pool.price_per_share)
            .ok_or(AgentPadError::MathOverflow)?;

        let mint_key = pool.mint;
        let vault_seeds: &[&[u8]] = &[b"sol_vault", mint_key.as_ref(), &[pool.sol_vault_bump]];
        system_program::transfer(
            CpiContext::new_with_signer(
                ctx.accounts.system_program.to_account_info(),
                system_program::Transfer {
                    from: ctx.accounts.sol_vault.to_account_info(),
                    to: ctx.accounts.user.to_account_info(),
                },
                &[vault_seeds],
            ),
            refund_lamports,
        )?;

        record.shares_bought = record
            .shares_bought
            .checked_sub(shares_count)
            .ok_or(AgentPadError::MathOverflow)?;
        pool.sold_shares = pool
            .sold_shares
            .checked_sub(shares_count)
            .ok_or(AgentPadError::MathOverflow)?;
        pool.total_sol_raised = pool
            .total_sol_raised
            .checked_sub(refund_lamports)
            .ok_or(AgentPadError::MathOverflow)?;

        emit!(SharesRefundedEvent {
            mint: pool.mint,
            user: ctx.accounts.user.key(),
            shares_count,
            refund_lamports,
        });

        Ok(())
    }

    /// Graduate completed launch: mint 625M to claim vault & 375M to LP,
    /// split raised SOL (60% LP, 35% Creator, 2.5% BuybackVault, 2.5% Treasury),
    /// and execute Raydium CPMM 1:1 pool creation CPI.
    ///
    /// v4.2 idempotency: if the pool already graduated, this is a no-op
    /// (`Ok(())`). A retry (e.g. after a client timeout) must not fail
    /// or double-mint. Reachable because `lp_token_staging` is a raw
    /// `AccountInfo` (v4.2.1, see below) — no Anchor init runs on it at
    /// account-validation time, so a retry reaches this check.
    pub fn graduate_to_raydium(ctx: Context<GraduateToRaydium>) -> Result<()> {
        if ctx.accounts.launch_pool.status == LaunchStatus::Graduated {
            return Ok(());
        }
        let pool = &mut ctx.accounts.launch_pool;
        require!(
            pool.status == LaunchStatus::Completed,
            AgentPadError::PoolNotCompleted
        );

        let total_sol = pool.total_sol_raised;
        let lp_sol = total_sol
            .checked_mul(GRAD_LP_SOL_BPS)
            .ok_or(AgentPadError::MathOverflow)?
            .checked_div(10_000)
            .ok_or(AgentPadError::MathOverflow)?;
        let creator_sol = total_sol
            .checked_mul(GRAD_CREATOR_SOL_BPS)
            .ok_or(AgentPadError::MathOverflow)?
            .checked_div(10_000)
            .ok_or(AgentPadError::MathOverflow)?;
        let buyback_sol = total_sol
            .checked_mul(GRAD_BUYBACK_SOL_BPS)
            .ok_or(AgentPadError::MathOverflow)?
            .checked_div(10_000)
            .ok_or(AgentPadError::MathOverflow)?;
        let treasury_sol = total_sol
            .checked_sub(lp_sol)
            .and_then(|v| v.checked_sub(creator_sol))
            .and_then(|v| v.checked_sub(buyback_sol))
            .ok_or(AgentPadError::MathOverflow)?;

        let mint_key = pool.mint;
        let pool_seeds: &[&[u8]] = &[b"launch_pool", mint_key.as_ref(), &[pool.bump]];
        let vault_seeds: &[&[u8]] = &[b"sol_vault", mint_key.as_ref(), &[pool.sol_vault_bump]];

        let public_mint_amount = pool
            .total_shares
            .checked_mul(pool.tokens_per_share)
            .ok_or(AgentPadError::MathOverflow)?;
        let dust_remainder = PUBLIC_MINT_SUPPLY_RAW
            .checked_sub(public_mint_amount)
            .ok_or(AgentPadError::MathOverflow)?;
        let lp_mint_amount = LP_TOKEN_SUPPLY_RAW
            .checked_add(dust_remainder)
            .ok_or(AgentPadError::MathOverflow)?;

        // v4.2.1: create `lp_token_staging` by hand. Anchor 0.30's
        // `init_if_needed` on a Token-2022 account is broken: the
        // generated code captures `owner_program` BEFORE the create /
        // InitializeAccount3 CPIs and then runs its post-init
        // `ConstraintTokenTokenProgram` check against that stale value, so
        // a freshly created staging account always fails validation and
        // graduation can never succeed (devnet-verified: create_account
        // and InitializeAccount3 both succeed, the instruction then dies
        // before the first MintTo below). The struct field is therefore a
        // raw `AccountInfo` — no Anchor init or deserialization runs on
        // it at account-validation time — and this block does what the
        // constraint used to do, with fresh reads at each step: PDA
        // check, create-if-absent, initialize, verify.
        //
        // Idempotency: (a) an already-graduated pool returned Ok(())
        // above, before this block runs at all; (b) an ungraduated pool
        // whose staging account already exists (e.g. a prior call that
        // failed after InitializeAccount3 but before MintTo) skips
        // creation and continues below — the verify block at the end of
        // this section is the only gate on that path.
        let lp_info = ctx.accounts.lp_token_staging.to_account_info();
        let pool_key = pool.key();
        let (expected_lp, lp_bump) = Pubkey::find_program_address(
            &[b"lp_staging", pool_key.as_ref()],
            ctx.program_id,
        );
        require_keys_eq!(
            lp_info.key(),
            expected_lp,
            AgentPadError::InvalidLpStagingAccount
        );
        let lp_seeds: &[&[u8]] = &[b"lp_staging", pool_key.as_ref(), &[lp_bump]];
        if lp_info.data_len() == 0 {
            // Space: exactly what Anchor's token `init` computes for this
            // mint — `ExtensionType::try_calculate_account_len::<Account>`
            // over the mint's required init account extensions. Our mints
            // always carry TransferFeeConfig (enforced in create_launch),
            // so the required extension is TransferFeeAmount and the size
            // is 165 (Account::LEN) + 1 (AccountType) + 12 (4-byte TLV
            // header + 8-byte TransferFeeAmount) = 178 bytes — the same
            // space the v4.2 init CPI requested in simulation.
            let lp_space = {
                let mint_info = ctx.accounts.mint.to_account_info();
                let mint_data = mint_info.try_borrow_data()?;
                let mint_state =
                    StateWithExtensions::<spl_token_2022::state::Mint>::unpack(&mint_data)
                        .map_err(|_| AgentPadError::TransferFeeNotConfigured)?;
                let mint_extensions = mint_state.get_extension_types()?;
                let required =
                    ExtensionType::get_required_init_account_extensions(&mint_extensions);
                ExtensionType::try_calculate_account_len::<
                    spl_token_2022::state::Account,
                >(&required)?
            };
            let rent = Rent::get()?;
            if lp_info.lamports() == 0 {
                // Fresh PDA: SystemProgram create_account, signed via
                // seeds — a PDA cannot sign a client transaction (same
                // pattern as the manual dividend_vault_2 creation in
                // create_launch).
                let create_ix = system_instruction::create_account(
                    &ctx.accounts.payer.key(),
                    &lp_info.key(),
                    rent.minimum_balance(lp_space),
                    lp_space as u64,
                    &ctx.accounts.token_program.key(),
                );
                invoke_signed(
                    &create_ix,
                    &[ctx.accounts.payer.to_account_info(), lp_info.clone()],
                    &[lp_seeds],
                )?;
            } else {
                // Funded-but-uninitialized PDA (e.g. someone transferred
                // SOL to the deterministic staging address): a plain
                // create_account would fail with AccountAlreadyInUse, so
                // mirror Anchor's own generated `init` fallback — fund to
                // rent-exemption, allocate, assign to the token program.
                // (Anchor's extra `payer != account` guard is omitted:
                // the payer must be a transaction signer and this PDA can
                // never sign, so the two keys cannot be equal.)
                let required_lamports = rent
                    .minimum_balance(lp_space)
                    .max(1)
                    .saturating_sub(lp_info.lamports());
                if required_lamports > 0 {
                    system_program::transfer(
                        CpiContext::new(
                            ctx.accounts.system_program.to_account_info(),
                            system_program::Transfer {
                                from: ctx.accounts.payer.to_account_info(),
                                to: lp_info.clone(),
                            },
                        ),
                        required_lamports,
                    )?;
                }
                let alloc_ix =
                    system_instruction::allocate(&lp_info.key(), lp_space as u64);
                invoke_signed(&alloc_ix, &[lp_info.clone()], &[lp_seeds])?;
                let assign_ix = system_instruction::assign(
                    &lp_info.key(),
                    &ctx.accounts.token_program.key(),
                );
                invoke_signed(&assign_ix, &[lp_info.clone()], &[lp_seeds])?;
            }
            // Same CPI Anchor's token `init` runs: InitializeAccount3
            // (mint + owner = launch_pool PDA). No rent sysvar needed.
            let cpi_program = ctx.accounts.token_program.to_account_info();
            let accounts = token_interface::InitializeAccount3 {
                account: lp_info.clone(),
                mint: ctx.accounts.mint.to_account_info(),
                authority: pool.to_account_info(),
            };
            let cpi_ctx = CpiContext::new(cpi_program, accounts);
            token_interface::initialize_account3(cpi_ctx)?;
        }
        // Verify the (possibly just created) staging account with a fresh
        // read: correct mint + authority = launch pool. This replaces the
        // `token::mint` / `token::authority` constraints the struct used
        // to carry, and also guards the already-exists path.
        {
            let lp_data = lp_info.try_borrow_data()?;
            let lp_state =
                StateWithExtensions::<spl_token_2022::state::Account>::unpack(&lp_data)
                    .map_err(|_| AgentPadError::InvalidLpStagingAccount)?;
            require!(
                lp_state.base.mint == mint_key,
                AgentPadError::InvalidLpStagingAccount
            );
            require!(
                lp_state.base.owner == pool_key,
                AgentPadError::InvalidLpStagingAccount
            );
        }

        token_2022::mint_to(
            CpiContext::new_with_signer(
                ctx.accounts.token_program.to_account_info(),
                MintTo {
                    mint: ctx.accounts.mint.to_account_info(),
                    to: ctx.accounts.claim_token_vault.to_account_info(),
                    authority: pool.to_account_info(),
                },
                &[pool_seeds],
            ),
            public_mint_amount,
        )?;

        token_2022::mint_to(
            CpiContext::new_with_signer(
                ctx.accounts.token_program.to_account_info(),
                MintTo {
                    mint: ctx.accounts.mint.to_account_info(),
                    to: ctx.accounts.lp_token_staging.to_account_info(),
                    authority: pool.to_account_info(),
                },
                &[pool_seeds],
            ),
            lp_mint_amount,
        )?;

        system_program::transfer(
            CpiContext::new_with_signer(
                ctx.accounts.system_program.to_account_info(),
                system_program::Transfer {
                    from: ctx.accounts.sol_vault.to_account_info(),
                    to: ctx.accounts.marketing_wallet.to_account_info(),
                },
                &[vault_seeds],
            ),
            creator_sol,
        )?;
        system_program::transfer(
            CpiContext::new_with_signer(
                ctx.accounts.system_program.to_account_info(),
                system_program::Transfer {
                    from: ctx.accounts.sol_vault.to_account_info(),
                    to: ctx.accounts.buyback_vault.to_account_info(),
                },
                &[vault_seeds],
            ),
            buyback_sol,
        )?;
        system_program::transfer(
            CpiContext::new_with_signer(
                ctx.accounts.system_program.to_account_info(),
                system_program::Transfer {
                    from: ctx.accounts.sol_vault.to_account_info(),
                    to: ctx.accounts.treasury_wallet.to_account_info(),
                },
                &[vault_seeds],
            ),
            treasury_sol,
        )?;

        pool.status = LaunchStatus::Graduated;
        pool.graduated_at = Clock::get()?.unix_timestamp;

        emit!(PoolGraduatedEvent {
            mint: pool.mint,
            lp_sol,
            lp_tokens: lp_mint_amount,
            creator_sol,
            buyback_sol,
            treasury_sol,
        });

        Ok(())
    }

    /// Minters actively claim their Token-2022 allocation after Raydium graduation.
    pub fn claim_tokens(ctx: Context<ClaimTokens>) -> Result<()> {
        let pool = &ctx.accounts.launch_pool;
        require!(
            pool.status == LaunchStatus::Graduated,
            AgentPadError::PoolNotGraduated
        );

        let record = &mut ctx.accounts.minter_record;
        require!(!record.has_claimed, AgentPadError::AlreadyClaimed);
        require!(record.shares_bought > 0, AgentPadError::NothingToClaim);

        let amount = record
            .shares_bought
            .checked_mul(pool.tokens_per_share)
            .ok_or(AgentPadError::MathOverflow)?;

        let mint_key = pool.mint;
        let pool_seeds: &[&[u8]] = &[b"launch_pool", mint_key.as_ref(), &[pool.bump]];

        token_2022::transfer_checked(
            CpiContext::new_with_signer(
                ctx.accounts.token_program.to_account_info(),
                TransferChecked {
                    from: ctx.accounts.claim_token_vault.to_account_info(),
                    to: ctx.accounts.user_token_account.to_account_info(),
                    authority: pool.to_account_info(),
                    mint: ctx.accounts.mint.to_account_info(),
                },
                &[pool_seeds],
            ),
            amount,
            TOKEN_DECIMALS,
        )?;

        record.has_claimed = true;
        Ok(())
    }

    /// Distribute harvested 1% bidirectional tax (5-2-2-1):
    /// - 20% project token burned in-place
    /// - 50% swapped to the pool's chosen `reward_mint` (Native SOL or whitelisted stock token)
    /// - 20% Creator SOL + 5% BuybackVault SOL + 5% Treasury SOL
    pub fn distribute_harvested_tax(
        mut ctx: Context<DistributeHarvestedTax>,
        burn_token_amount: u64,
        dividend_reward_amount: u64,
        creator_and_platform_sol_amount: u64,
    ) -> Result<()> {
        // Scoped borrow: the pool reference must not stay live for the whole
        // handler.
        {
            let status = ctx.accounts.launch_pool.status;
            require!(
                status == LaunchStatus::Graduated,
                AgentPadError::PoolNotGraduated
            );
        }

        // The rest lives in `distribute_inner` (free function below): this
        // instruction's 20-account Context plus the burn/deposit/transfer
        // temporaries exceed the BPF 4096-byte stack frame when kept in one
        // function (v4 build: 4416, over by 320). The helper takes only a
        // `&mut Context` pointer, so its own frame stays small.
        distribute_inner(
            &mut ctx.accounts,
            burn_token_amount,
            dividend_reward_amount,
            creator_and_platform_sol_amount,
        )
    }

    /// Harvest withheld 1% transfer fees into the harvester's token account.
    /// The pool PDA is the mint's withdraw authority (set at mint creation),
    /// so only the program can sign this CPI — the harvester triggers it and
    /// pays the tx fee, the tokens land in their ATA. The crank is expected
    /// to call `HarvestWithheldTokensToMint` (permissionless, off-chain) first
    /// to move fees from individual token accounts into the mint; this
    /// instruction then withdraws the mint's accumulated total in one go.
    /// Anyone can trigger it; the pool must be Graduated.
    pub fn harvest_withheld_fees(ctx: Context<HarvestWithheldFees>) -> Result<()> {
        require!(
            ctx.accounts.launch_pool.status == LaunchStatus::Graduated,
            AgentPadError::PoolNotGraduated
        );
        let pool_mint = ctx.accounts.launch_pool.mint;
        let pool_bump = ctx.accounts.launch_pool.bump;
        let ix = spl_token_2022::extension::transfer_fee::instruction::withdraw_withheld_tokens_from_mint(
            &ctx.accounts.token_program.key(),
            &ctx.accounts.mint.key(),
            &ctx.accounts.harvester_token_account.key(),
            &ctx.accounts.launch_pool.key(),
            &[],
        )?;
        invoke_signed(
            &ix,
            &[
                ctx.accounts.mint.to_account_info(),
                ctx.accounts.harvester_token_account.to_account_info(),
                ctx.accounts.launch_pool.to_account_info(),
            ],
            &[&[b"launch_pool", pool_mint.as_ref(), &[pool_bump]]],
        )?;
        Ok(())
    }

    /// Holders with >= 200,000 tokens (0.02% supply) actively claim dividends
    /// in the pool's configured reward asset (SOL or whitelisted stock token).
    /// Holders with >= 200,000 tokens (0.02% supply) actively claim dividends
    /// in the pool's configured reward asset(s) — one asset, or both halves
    /// of a 50/50 dual-asset launch (v4). Each leg tracks its own debt, so
    /// a user who claimed leg 1 cannot double-claim it via leg 2.
    pub fn claim_dividend(ctx: Context<ClaimDividend>) -> Result<()> {
        let user_balance = ctx.accounts.user_token_account.amount;
        require!(
            user_balance >= MIN_DIVIDEND_HOLDING_RAW,
            AgentPadError::BelowDividendThreshold
        );

        let pool = &ctx.accounts.launch_pool;
        let div_vault = &mut ctx.accounts.dividend_vault;

        // Minter record (v4.2.8): ensure it exists, creating it on first
        // claim via manual SystemProgram CPI. Swap buyers have no record;
        // without this their claim fails even though the frontend shows a
        // pending dividend. Manual CPI (not Anchor init_if_needed) because
        // init_if_needed corrupts the heap in this 16-account struct.
        let record_info = ctx.accounts.minter_record.to_account_info();
        let user_key = ctx.accounts.user.key();
        let mint_key = pool.mint;
        let (expected_minter, minter_bump) = Pubkey::find_program_address(
            &[b"minter", mint_key.as_ref(), user_key.as_ref()],
            ctx.program_id,
        );
        require_keys_eq!(
            record_info.key(),
            expected_minter,
            AgentPadError::InvalidMinterRecord
        );
        if record_info.lamports() == 0 {
            let rent = Rent::get()?;
            let lamports = rent.minimum_balance(MinterRecord::LEN);
            let minter_seeds: &[&[u8]] = &[
                b"minter",
                mint_key.as_ref(),
                user_key.as_ref(),
                &[minter_bump],
            ];
            let create_ix = system_instruction::create_account(
                &user_key,
                &record_info.key(),
                lamports,
                MinterRecord::LEN as u64,
                ctx.program_id,
            );
            invoke_signed(
                &create_ix,
                &[
                    ctx.accounts.user.to_account_info(),
                    record_info.clone(),
                ],
                &[minter_seeds],
            )?;
            let init_record = MinterRecord {
                pool: pool.key(),
                user: user_key,
                shares_bought: 0,
                bonus_shares: 0,
                has_recorded_invite: false,
                has_claimed: false,
                reward_debt: 0,
                bump: minter_bump,
            };
            let mut data = record_info.try_borrow_mut_data()?;
            init_record.try_serialize(&mut &mut data[..])?;
        }
        // Load the record (fresh or just-created) to read reward_debt.
        let mut record = MinterRecord::try_deserialize(
            &mut &record_info.data.borrow()[..],
        )
        .map_err(|_| AgentPadError::InvalidMinterRecord)?;

        // Leg 1 (unchanged math): pending from vault 1's accumulator.
        let accumulated = (user_balance as u128)
            .checked_mul(div_vault.acc_reward_per_token)
            .ok_or(AgentPadError::MathOverflow)?
            .checked_div(DIVIDEND_PRECISION)
            .ok_or(AgentPadError::MathOverflow)?;

        let pending = accumulated.saturating_sub(record.reward_debt) as u64;

        // Leg 2 (v4, dual-asset launches only): same math against vault 2's
        // accumulator and the separate MinterReward2 debt record.
        // v4.2.8 guard: Anchor 0.30 resolves the PROGRAM_ID placeholder for
        // this Option account to Some(garbage) instead of None, so a
        // single-asset claim would multiply by a garbage accumulator and
        // overflow. A real vault always has pool == launch_pool; only
        // process leg 2 when that holds.
        let (pending_2, accumulated_2) = match &ctx.accounts.dividend_vault_2 {
            Some(v2) if v2.pool == pool.key() => {
                let acc2 = (user_balance as u128)
                    .checked_mul(v2.acc_reward_per_token)
                    .ok_or(AgentPadError::MathOverflow)?
                    .checked_div(DIVIDEND_PRECISION)
                    .ok_or(AgentPadError::MathOverflow)?;
                let debt2 = ctx
                    .accounts
                    .minter_reward_2
                    .as_ref()
                    .map(|r| r.reward_debt)
                    .unwrap_or(0);
                (acc2.saturating_sub(debt2) as u64, acc2)
            }
            // None, or a placeholder/garbage vault whose pool doesn't match.
            _ => (0, 0),
        };

        // A dual-asset pool MUST carry the second debt record, otherwise
        // leg 2 could be claimed repeatedly (debt never recorded).
        // (Use the pool-match test, not is_some(): a PROGRAM_ID placeholder
        // resolves to Some(garbage) in Anchor 0.30.)
        let leg2_real = matches!(&ctx.accounts.dividend_vault_2, Some(v2) if v2.pool == pool.key());
        if leg2_real {
            require!(
                ctx.accounts.minter_reward_2.is_some(),
                AgentPadError::InvalidSecondRewardMint
            );
        }

        require!(
            pending > 0 || pending_2 > 0,
            AgentPadError::NoDividendsToClaim
        );

        record.reward_debt = accumulated;
        // Write the updated record back (it is an owned struct now, not an
        // Anchor Account — the manual-CPI path deserializes by hand).
        {
            let mut data = record_info.try_borrow_mut_data()?;
            record.try_serialize(&mut &mut data[..])?;
        }
        // Only touch the second debt record when leg 2 really ran (real
        // vault, pool matches). Otherwise minter_reward_2 is a PROGRAM_ID
        // placeholder and writing to it would fail.
        if leg2_real {
            if let Some(r2) = ctx.accounts.minter_reward_2.as_mut() {
                if r2.pool == Pubkey::default() {
                    r2.pool = pool.key();
                    r2.user = ctx.accounts.user.key();
                }
                r2.reward_debt = accumulated_2;
            }
        }

        let pool_key = pool.key();
        let div_seeds: &[&[u8]] = &[b"dividend_vault", pool_key.as_ref(), &[div_vault.bump]];

        pay_dividend_leg(
            pool.reward_mint,
            &div_vault.to_account_info(),
            div_seeds,
            pending,
            ctx.accounts.dividend_reward_ata.as_ref(),
            ctx.accounts.user_reward_ata.as_ref(),
            ctx.accounts.reward_mint_account.as_ref(),
            ctx.accounts.reward_token_program.as_ref(),
            &ctx.accounts.user.to_account_info(),
            &ctx.accounts.system_program,
        )?;

        if pending_2 > 0 {
            let v2 = ctx
                .accounts
                .dividend_vault_2
                .as_ref()
                .ok_or(AgentPadError::InvalidSecondRewardMint)?;
            let div2_seeds: &[&[u8]] =
                &[b"dividend_vault_2", pool_key.as_ref(), &[v2.bump]];
            pay_dividend_leg(
                v2.reward_mint,
                &v2.to_account_info(),
                div2_seeds,
                pending_2,
                ctx.accounts.dividend_reward_ata_2.as_ref(),
                ctx.accounts.user_reward_ata_2.as_ref(),
                ctx.accounts.reward_mint_account_2.as_ref(),
                ctx.accounts.reward_token_program_2.as_ref(),
                &ctx.accounts.user.to_account_info(),
                &ctx.accounts.system_program,
            )?;
        }

        Ok(())
    }

    /// Manual buyback-and-burn of the official Platform Token.
    ///
    /// Two legs, both executed on-chain by this instruction:
    /// 1. Release `amount_sol` from the BuybackVault PDA to the admin. The
    ///    admin swaps that SOL for platform tokens off-chain (Jupiter /
    ///    Raydium) and deposits the tokens into the vault's token account.
    ///    The release is admin-only and emitted, so every SOL out is
    ///    auditable.
    /// 2. Burn `burn_token_amount` platform tokens already held in the
    ///    vault's token account (authority = BuybackVault PDA).
    /// Totals are accumulated on GlobalConfig so the frontend burn page can
    /// read real numbers instead of hardcoded demo values.
    pub fn manual_buyback_and_burn(
        ctx: Context<ManualBuybackAndBurn>,
        amount_sol: u64,
        burn_token_amount: u64,
    ) -> Result<()> {
        let cfg = &mut ctx.accounts.global_config;
        require!(
            cfg.platform_token_mint != Pubkey::default(),
            AgentPadError::PlatformTokenNotConfigured
        );
        require!(
            ctx.accounts.platform_token_mint.key() == cfg.platform_token_mint,
            AgentPadError::PlatformTokenNotConfigured
        );
        require!(
            ctx.accounts.buyback_token_account.amount >= burn_token_amount,
            AgentPadError::InsufficientBuybackBalance
        );

        let vault_seeds: &[&[u8]] = &[b"buyback_vault", &[cfg.buyback_vault_bump]];

        if amount_sol > 0 {
            system_program::transfer(
                CpiContext::new_with_signer(
                    ctx.accounts.system_program.to_account_info(),
                    system_program::Transfer {
                        from: ctx.accounts.buyback_vault.to_account_info(),
                        to: ctx.accounts.admin.to_account_info(),
                    },
                    &[vault_seeds],
                ),
                amount_sol,
            )?;
        }

        if burn_token_amount > 0 {
            token_2022::burn(
                CpiContext::new_with_signer(
                    ctx.accounts.token_program.to_account_info(),
                    Burn {
                        mint: ctx.accounts.platform_token_mint.to_account_info(),
                        from: ctx.accounts.buyback_token_account.to_account_info(),
                        authority: ctx.accounts.buyback_vault.to_account_info(),
                    },
                    &[vault_seeds],
                ),
                burn_token_amount,
            )?;
        }

        cfg.total_buyback_sol_spent = cfg
            .total_buyback_sol_spent
            .checked_add(amount_sol)
            .ok_or(AgentPadError::MathOverflow)?;
        cfg.total_platform_burned = cfg
            .total_platform_burned
            .checked_add(burn_token_amount)
            .ok_or(AgentPadError::MathOverflow)?;

        emit!(PlatformBuybackBurnEvent {
            sol_spent: amount_sol,
            tokens_burned: burn_token_amount,
            timestamp: Clock::get()?.unix_timestamp,
        });
        Ok(())
    }

    /// Create the Raydium CPMM pool for a graduated launch.
    ///
    /// Settlement (`graduate_to_raydium`) must have run first: the LP token
    /// allocation sits in `lp_token_staging` and 60% of raised SOL sits in
    /// `sol_vault`. This instruction wraps that SOL into WSOL, pays the
    /// Raydium pool-creation fee (read from the amm config, taken from the
    /// vault — i.e. out of raised SOL), and CPIs Raydium CPMM `initialize`
    /// with the launch-pool PDA as creator. Account order and discriminator
    /// follow @raydium-io/raydium-sdk-v2 `makeCreateCpmmPoolInInstruction`.
    /// Permissionless and atomic: if the CPI fails the whole transaction
    /// rolls back, so no funds can get stuck half-wrapped.
    ///
    /// v4.2: (a) after a successful `initialize`, the LP tokens Raydium
    /// minted into `creator_lp_token` are burned in full (LP mint supply
    /// to zero) in the same transaction; (b) if the pool already exists
    /// (`raydium_pool_id` set — only written after a successful
    /// initialize + burn), a retry is a no-op (`Ok(())`).
    pub fn create_raydium_pool(ctx: Context<CreateRaydiumPool>) -> Result<()> {
        // v4.2.4: `launch_pool` is UncheckedAccount now (the
        // Account<LaunchPool> deserialization in try_accounts was the
        // largest single contributor to the try_accounts stack overflow:
        // 4264 bytes vs the 4096-byte BPF limit), so the pool is
        // deserialized inside the inner helper and the early
        // idempotency/status checks live there. The 25-account Context
        // itself is Boxed onto the heap.
        let mut accounts = Box::new(ctx.accounts);
        create_raydium_pool_inner(&mut accounts)
    }

}

/// `create_raydium_pool` body, extracted so the handler's stack frame
/// stays under the BPF 4096-byte limit (see the handler comment). Takes
/// only a `&mut` pointer to the accounts; the 25-account Context itself
/// is Boxed onto the heap by the handler, so it never sits on the stack.
fn create_raydium_pool_inner<'info>(
    accounts: &mut CreateRaydiumPool<'info>,
) -> Result<()> {
    // Deserialize the pool up front (launch_pool is UncheckedAccount
    // now). The borrow is dropped immediately; only plain values are
    // kept, so no borrow is held across the CPIs below.
    let (mint_key, pool_bump, pool_sol_vault_bump) = {
        let data = accounts.launch_pool.try_borrow_data()?;
        let pool = LaunchPool::try_deserialize(&mut &data[..])?;
        // v4.2 idempotency: raydium_pool_id is only written after a
        // successful initialize + LP burn, so a non-default value means
        // the pool already exists — a retry is a no-op, not an error.
        // (Moved here from the wrapper with the deserialization.)
        if pool.raydium_pool_id != Pubkey::default() {
            return Ok(());
        }
        require!(
            pool.status == LaunchStatus::Graduated,
            AgentPadError::PoolNotGraduated
        );
        require!(
            pool.raydium_pool_id == Pubkey::default(),
            AgentPadError::PoolAlreadyHasRaydiumPool
        );
        (pool.mint, pool.bump, pool.sol_vault_bump)
    };
        let wsol_key = WSOL_MINT;
        require!(mint_key != wsol_key, AgentPadError::InvalidTokenOrder);
        // Raydium orders the pair by pubkey: token_0 is the smaller one.
        let token_0_is_platform = mint_key.to_bytes() < wsol_key.to_bytes();
        let (token_0_mint, token_1_mint) = if token_0_is_platform {
            (mint_key, wsol_key)
        } else {
            (wsol_key, mint_key)
        };
        require_keys_eq!(
            accounts.token_0_mint.key(),
            token_0_mint,
            AgentPadError::InvalidTokenOrder
        );
        require_keys_eq!(
            accounts.token_1_mint.key(),
            token_1_mint,
            AgentPadError::InvalidTokenOrder
        );

        let raydium_program = accounts.raydium_program.key();
        let (amm_config_key, _) = Pubkey::find_program_address(
            &[b"amm_config", &RAYDIUM_AMM_CONFIG_INDEX.to_be_bytes()],
            &raydium_program,
        );
        require_keys_eq!(
            accounts.amm_config.key(),
            amm_config_key,
            AgentPadError::InvalidRaydiumAccount
        );
        let (authority_key, _) = Pubkey::find_program_address(
            &[b"vault_and_lp_mint_auth_seed"],
            &raydium_program,
        );
        require_keys_eq!(
            accounts.raydium_authority.key(),
            authority_key,
            AgentPadError::InvalidRaydiumAccount
        );
        let (pool_state_key, _) = Pubkey::find_program_address(
            &[
                b"pool",
                amm_config_key.as_ref(),
                token_0_mint.as_ref(),
                token_1_mint.as_ref(),
            ],
            &raydium_program,
        );
        require_keys_eq!(
            accounts.pool_state.key(),
            pool_state_key,
            AgentPadError::InvalidRaydiumAccount
        );
        let (lp_mint_key, _) = Pubkey::find_program_address(
            &[b"pool_lp_mint", pool_state_key.as_ref()],
            &raydium_program,
        );
        require_keys_eq!(
            accounts.lp_mint.key(),
            lp_mint_key,
            AgentPadError::InvalidRaydiumAccount
        );
        let (vault_0_key, _) = Pubkey::find_program_address(
            &[b"pool_vault", pool_state_key.as_ref(), token_0_mint.as_ref()],
            &raydium_program,
        );
        require_keys_eq!(
            accounts.token_0_vault.key(),
            vault_0_key,
            AgentPadError::InvalidRaydiumAccount
        );
        let (vault_1_key, _) = Pubkey::find_program_address(
            &[b"pool_vault", pool_state_key.as_ref(), token_1_mint.as_ref()],
            &raydium_program,
        );
        require_keys_eq!(
            accounts.token_1_vault.key(),
            vault_1_key,
            AgentPadError::InvalidRaydiumAccount
        );
        let (observation_key, _) = Pubkey::find_program_address(
            &[b"observation", pool_state_key.as_ref()],
            &raydium_program,
        );
        require_keys_eq!(
            accounts.observation_state.key(),
            observation_key,
            AgentPadError::InvalidRaydiumAccount
        );

        // AmmConfig layout (raydium-sdk-v2 CpmmConfigInfoLayout):
        //   8 discriminator | 1 bump | 1 disableCreatePool | 2 index |
        //   8 tradeFeeRate | 8 protocolFeeRate | 8 fundFeeRate |
        //   8 createPoolFee | 32 protocolOwner | 32 fundOwner | ...
        // Scoped in a block so the data borrow ends before the
        // `&mut accounts` call below (a `Ref` guard otherwise keeps the
        // borrow alive until the end of this scope).
        let create_pool_fee = {
            let config_data = accounts.amm_config.try_borrow_data()?;
            require!(
                config_data.len() >= 44,
                AgentPadError::InvalidRaydiumAccount
            );
            require!(config_data[9] == 0, AgentPadError::RaydiumPoolDisabled);
            u64::from_le_bytes(
                config_data[36..44]
                    .try_into()
                    .map_err(|_| AgentPadError::InvalidRaydiumAccount)?,
            )
        };

        // v4.2.7: verify the two funding token accounts with the
        // relaxed authority (launch_pool OR sol_vault) and create the
        // WSOL ATA on first use. This replaces the struct constraints
        // that used to reject retries after a successful pool creation.
        let lp_token_amount = prepare_pool_funding_accounts(accounts, &mint_key)?;
        require!(lp_token_amount > 0, AgentPadError::NothingToClaim);
        let vault_lamports = accounts.sol_vault.lamports();
        let wsol_amount = vault_lamports
            .checked_sub(
                create_pool_fee + RAYDIUM_POOL_RENT_BUFFER + GRADUATOR_RESERVE_LAMPORTS,
            )
            .ok_or(AgentPadError::InsufficientLpSol)?;
        require!(wsol_amount > 0, AgentPadError::InsufficientLpSol);
        let pool_seeds: &[&[u8]] = &[b"launch_pool", mint_key.as_ref(), &[pool_bump]];
        let vault_seeds: &[&[u8]] = &[b"sol_vault", mint_key.as_ref(), &[pool_sol_vault_bump]];

        // v4.2.11: operations reserve for the graduator (the tx payer).
        // Sent from the vault (the 60% LP share) before the WSOL wrap,
        // so the graduator is funded for this launch's metadata rent
        // (~0.0137 SOL) and tx fees, plus a surplus that accumulates
        // across graduations to cover the ongoing 6-hour crank fees.
        // No manual top-ups needed while launches keep graduating.
        system_program::transfer(
            CpiContext::new_with_signer(
                accounts.system_program.to_account_info(),
                system_program::Transfer {
                    from: accounts.sol_vault.to_account_info(),
                    to: accounts.payer.to_account_info(),
                },
                &[vault_seeds],
            ),
            GRADUATOR_RESERVE_LAMPORTS,
        )?;

        // 1. Wrap the LP SOL into WSOL. It stays in `pool_wsol_account`
        //    (its authority is moved to the sol vault in step 2, so the
        //    Raydium creator — the sol vault — owns it).
        //    NOTE (v4.2.4): the fee is NOT pre-funded into the launch-pool
        //    PDA any more. Raydium's initialize pays createPoolFee via a
        //    System Program `transfer` from the creator, and that
        //    instruction rejects a `from` account carrying data
        //    ("Transfer: `from` must not carry data"). The launch-pool PDA
        //    holds the 228-byte LaunchPool struct, so it can never be the
        //    creator. The sol vault is a data-less System-owned PDA, so it
        //    is the creator: it already holds the vault SOL (fee + rent +
        //    LP), and Raydium takes the fee straight from it.
        system_program::transfer(
            CpiContext::new_with_signer(
                accounts.system_program.to_account_info(),
                system_program::Transfer {
                    from: accounts.sol_vault.to_account_info(),
                    to: accounts.pool_wsol_account.to_account_info(),
                },
                &[vault_seeds],
            ),
            wsol_amount,
        )?;
        invoke(
            &Instruction {
                program_id: anchor_spl::token::ID,
                accounts: vec![AccountMeta::new(
                    accounts.pool_wsol_account.key(),
                    false,
                )],
                data: vec![17], // spl-token SyncNative
            },
            &[accounts.pool_wsol_account.to_account_info()],
        )?;

        // 2. Move both funding token accounts' authority from the
        //    launch-pool PDA to the sol vault, so the Raydium creator
        //    (the sol vault, step 3) owns them. The launch-pool PDA signs
        //    here as the current authority (pool_seeds).
        token_2022::set_authority(
            CpiContext::new_with_signer(
                accounts.token_program.to_account_info(),
                token_2022::SetAuthority {
                    account_or_mint: accounts.lp_token_staging.to_account_info(),
                    current_authority: accounts.launch_pool.to_account_info(),
                },
                &[pool_seeds],
            ),
            token_2022::spl_token_2022::instruction::AuthorityType::AccountOwner,
            Some(accounts.sol_vault.key()),
        )?;
        anchor_spl::token::set_authority(
            CpiContext::new_with_signer(
                accounts.token_classic_program.to_account_info(),
                anchor_spl::token::SetAuthority {
                    account_or_mint: accounts.pool_wsol_account.to_account_info(),
                    current_authority: accounts.launch_pool.to_account_info(),
                },
                &[pool_seeds],
            ),
            anchor_spl::token::spl_token::instruction::AuthorityType::AccountOwner,
            Some(accounts.sol_vault.key()),
        )?;

        // 3. CPI Raydium CPMM initialize. The sol-vault PDA is the creator
        //    and the authority of both funding token accounts.
        let (user_vault_0, user_vault_1, mint_program_0, mint_program_1, amount_0, amount_1) =
            if token_0_is_platform {
                (
                    accounts.lp_token_staging.to_account_info(),
                    accounts.pool_wsol_account.to_account_info(),
                    accounts.token_program.to_account_info(),
                    accounts.token_classic_program.to_account_info(),
                    lp_token_amount,
                    wsol_amount,
                )
            } else {
                (
                    accounts.pool_wsol_account.to_account_info(),
                    accounts.lp_token_staging.to_account_info(),
                    accounts.token_classic_program.to_account_info(),
                    accounts.token_program.to_account_info(),
                    wsol_amount,
                    lp_token_amount,
                )
            };

        let mut ix_data = Vec::with_capacity(32);
        ix_data.extend_from_slice(&RAYDIUM_INITIALIZE_DISCRIMINATOR);
        ix_data.extend_from_slice(&amount_0.to_le_bytes());
        ix_data.extend_from_slice(&amount_1.to_le_bytes());
        ix_data.extend_from_slice(&(Clock::get()?.unix_timestamp as u64).to_le_bytes());

        let raydium_ix = Instruction {
            program_id: raydium_program,
            accounts: vec![
                AccountMeta::new(accounts.sol_vault.key(), true),
                AccountMeta::new_readonly(accounts.amm_config.key(), false),
                AccountMeta::new_readonly(accounts.raydium_authority.key(), false),
                AccountMeta::new(accounts.pool_state.key(), false),
                AccountMeta::new_readonly(token_0_mint, false),
                AccountMeta::new_readonly(token_1_mint, false),
                AccountMeta::new(accounts.lp_mint.key(), false),
                AccountMeta::new(user_vault_0.key(), false),
                AccountMeta::new(user_vault_1.key(), false),
                AccountMeta::new(accounts.creator_lp_token.key(), false),
                AccountMeta::new(accounts.token_0_vault.key(), false),
                AccountMeta::new(accounts.token_1_vault.key(), false),
                AccountMeta::new(accounts.create_pool_fee_account.key(), false),
                AccountMeta::new(accounts.observation_state.key(), false),
                AccountMeta::new_readonly(anchor_spl::token::ID, false),
                AccountMeta::new_readonly(mint_program_0.key(), false),
                AccountMeta::new_readonly(mint_program_1.key(), false),
                AccountMeta::new_readonly(anchor_spl::associated_token::ID, false),
                AccountMeta::new_readonly(anchor_lang::system_program::ID, false),
                AccountMeta::new_readonly(anchor_lang::solana_program::sysvar::rent::ID, false),
            ],
            data: ix_data,
        };
        // v4.2.3: build the CPI account list on the heap, not the stack.
        // A 20-element `[AccountInfo; 20]` array literal costs ~1.1 KB of
        // stack on its own; together with this instruction's 25-account
        // Context it pushed the frame past the BPF 4096-byte limit and the
        // handler faulted with an access violation before any CPI
        // (devnet-verified, 2026-11-19). Pushing one AccountInfo at a time
        // into a Vec keeps only one element on the stack at once. Order
        // matches `raydium_ix`'s AccountMetas above, position for position.
        // v4.2.6: position 0 is the Raydium creator = the SOL vault, NOT
        // the launch pool. v4.2.5 changed the creator in `raydium_ix` but
        // left this push as launch_pool, so the CPI failed with
        // MissingAccount ("Instruction references an unknown account
        // <sol_vault>") — the runtime matches CPI metas to AccountInfos
        // by position, and sol_vault was never passed.
        let mut cpi_accounts: Vec<AccountInfo> = Vec::with_capacity(20);
        cpi_accounts.push(accounts.sol_vault.to_account_info());
        cpi_accounts.push(accounts.amm_config.to_account_info());
        cpi_accounts.push(accounts.raydium_authority.to_account_info());
        cpi_accounts.push(accounts.pool_state.to_account_info());
        cpi_accounts.push(accounts.token_0_mint.to_account_info());
        cpi_accounts.push(accounts.token_1_mint.to_account_info());
        cpi_accounts.push(accounts.lp_mint.to_account_info());
        cpi_accounts.push(user_vault_0.clone());
        cpi_accounts.push(user_vault_1.clone());
        cpi_accounts.push(accounts.creator_lp_token.to_account_info());
        cpi_accounts.push(accounts.token_0_vault.to_account_info());
        cpi_accounts.push(accounts.token_1_vault.to_account_info());
        cpi_accounts.push(accounts.create_pool_fee_account.to_account_info());
        cpi_accounts.push(accounts.observation_state.to_account_info());
        cpi_accounts.push(accounts.token_classic_program.to_account_info());
        cpi_accounts.push(mint_program_0.clone());
        cpi_accounts.push(mint_program_1.clone());
        cpi_accounts.push(accounts.associated_token_program.to_account_info());
        cpi_accounts.push(accounts.system_program.to_account_info());
        cpi_accounts.push(accounts.rent.to_account_info());
        invoke_signed(&raydium_ix, &cpi_accounts, &[vault_seeds])?;

        // 4. v4.2: burn the LP tokens Raydium just minted into
        //    `creator_lp_token`, in full, so the LP mint supply ends at
        //    zero and the pool can never be drained by withdrawing LP
        //    tokens. The LP mint is created by Raydium under the classic
        //    SPL Token program (not Token-2022 — the creator ATA was
        //    created with `token_classic_program`), so the burn CPI goes
        //    through `anchor_spl::token::burn` with the sol-vault PDA
        //    signing as the token-account authority via `vault_seeds`.
        //    Same transaction as the initialize above: if the burn fails
        //    the whole thing rolls back, so `raydium_pool_id` is never
        //    written for a pool whose LP was not burned.
        // v4.2.2: the account is now a raw AccountInfo (Raydium
        // created it during the CPI), so verify it and read its
        // balance with a manual unpack first.
        let lp_burn_amount = verify_creator_lp_token(accounts)?;
        if lp_burn_amount > 0 {
            burn_creator_lp(accounts, vault_seeds, lp_burn_amount)?;
        }

        // Write back raydium_pool_id. launch_pool is a plain AccountInfo
        // now (v4.2.4 stack fix), so there is no automatic Account exit
        // write-back: write the 32 bytes directly. LaunchPool layout: 8-byte
        // discriminator + 4 pubkeys (128) + 5 u64 (40) + status (1) +
        // is_official (1) = 178; raydium_pool_id occupies 178..210.
        {
            let mut data = accounts.launch_pool.try_borrow_mut_data()?;
            data[178..210].copy_from_slice(pool_state_key.as_ref());
        }

        emit!(RaydiumPoolCreatedEvent {
            mint: mint_key,
            pool_id: pool_state_key,
            lp_sol: wsol_amount,
            lp_tokens: lp_token_amount,
            token_0_mint,
            token_1_mint,
        });

        Ok(())
}

/// v4.2.7: verify the two pool-funding token accounts and create the
/// WSOL ATA on first use; returns the staged LP-token amount.
/// Replaces the `token::authority = launch_pool` /
/// `associated_token::authority = launch_pool` struct constraints,
/// which rejected retries after a successful pool creation (the
/// authority had moved to the sol vault) before the handler's
/// idempotency no-op could run. Both accounts now accept either
/// authority; anything else is rejected. Kept out of the handler so
/// the handler's stack frame stays small (same reason as
/// `verify_creator_lp_token` / `burn_creator_lp`).
fn prepare_pool_funding_accounts<'info>(
    accounts: &mut CreateRaydiumPool<'info>,
    mint_key: &Pubkey,
) -> Result<u64> {
    let pool_key = accounts.launch_pool.key();
    let vault_key = accounts.sol_vault.key();

    // lp_token_staging: a Token-2022 account for the launch mint,
    // owned by the launch pool (first call) or the sol vault (retry
    // after a successful set_authority). The base Account layout
    // (mint[0..32], owner[32..64], amount[64..72]) is the same for
    // Token and Token-2022.
    let lp_amount = {
        let lp_info = &accounts.lp_token_staging;
        require_keys_eq!(
            *lp_info.owner,
            token_2022::ID,
            AgentPadError::InvalidLpStagingAccount
        );
        let lp_data = lp_info.try_borrow_data()?;
        require!(
            lp_data.len() >= 72,
            AgentPadError::InvalidLpStagingAccount
        );
        let lp_mint = Pubkey::try_from(&lp_data[0..32])
            .map_err(|_| AgentPadError::InvalidLpStagingAccount)?;
        require_keys_eq!(lp_mint, *mint_key, AgentPadError::InvalidLpStagingAccount);
        let lp_owner = Pubkey::try_from(&lp_data[32..64])
            .map_err(|_| AgentPadError::InvalidLpStagingAccount)?;
        require!(
            lp_owner == pool_key || lp_owner == vault_key,
            AgentPadError::InvalidLpStagingAccount
        );
        u64::from_le_bytes(
            lp_data[64..72]
                .try_into()
                .map_err(|_| AgentPadError::InvalidLpStagingAccount)?,
        )
    };

    // pool_wsol_account: the WSOL ATA owned by the launch pool.
    // Create it on first use (payer funds the rent); on later calls
    // verify address / mint / owner (owner may be the launch pool or
    // the sol vault after set_authority).
    let wsol_info = accounts.pool_wsol_account.clone();
    let expected_wsol = anchor_spl::associated_token::get_associated_token_address(
        &pool_key,
        &WSOL_MINT,
    );
    require_keys_eq!(
        wsol_info.key(),
        expected_wsol,
        AgentPadError::InvalidRaydiumAccount
    );
    if wsol_info.data_is_empty() {
        anchor_spl::associated_token::create(CpiContext::new(
            accounts.associated_token_program.to_account_info(),
            anchor_spl::associated_token::Create {
                payer: accounts.payer.to_account_info(),
                associated_token: wsol_info.clone(),
                authority: accounts.launch_pool.to_account_info(),
                mint: accounts.wsol_mint.to_account_info(),
                system_program: accounts.system_program.to_account_info(),
                token_program: accounts.token_classic_program.to_account_info(),
            },
        ))?;
    } else {
        require_keys_eq!(
            *wsol_info.owner,
            anchor_spl::token::ID,
            AgentPadError::InvalidRaydiumAccount
        );
        let wsol_data = wsol_info.try_borrow_data()?;
        require!(
            wsol_data.len() == 165,
            AgentPadError::InvalidRaydiumAccount
        );
        let wsol_mint = Pubkey::try_from(&wsol_data[0..32])
            .map_err(|_| AgentPadError::InvalidRaydiumAccount)?;
        require_keys_eq!(wsol_mint, WSOL_MINT, AgentPadError::InvalidRaydiumAccount);
        let wsol_owner = Pubkey::try_from(&wsol_data[32..64])
            .map_err(|_| AgentPadError::InvalidRaydiumAccount)?;
        require!(
            wsol_owner == pool_key || wsol_owner == vault_key,
            AgentPadError::InvalidRaydiumAccount
        );
    }
    Ok(lp_amount)
}

/// Verify `creator_lp_token` after Raydium's `initialize` created it:
/// it must be the associated token account of the LP mint owned by the
/// sol vault (the Raydium creator, v4.2.4), and return its balance for
/// the burn step. Kept out of
/// the handler to keep the stack frame small (see `burn_creator_lp`).
/// Classic SPL Token accounts are a fixed 165-byte layout with no
/// extensions, so the fields are read straight from the bytes
/// (mint[0..32], owner[32..64], amount[64..72]) instead of pulling in
/// another unpack dependency.
fn verify_creator_lp_token<'info>(accounts: &CreateRaydiumPool<'info>) -> Result<u64> {
    let expected = anchor_spl::associated_token::get_associated_token_address(
        &accounts.sol_vault.key(),
        &accounts.lp_mint.key(),
    );
    require_keys_eq!(
        accounts.creator_lp_token.key(),
        expected,
        AgentPadError::InvalidCreatorLpToken
    );
    let data = accounts.creator_lp_token.try_borrow_data()?;
    require!(
        data.len() == 165,
        AgentPadError::InvalidCreatorLpToken
    );
    let mint = Pubkey::try_from(&data[0..32])
        .map_err(|_| AgentPadError::InvalidCreatorLpToken)?;
    let owner = Pubkey::try_from(&data[32..64])
        .map_err(|_| AgentPadError::InvalidCreatorLpToken)?;
    require_keys_eq!(mint, accounts.lp_mint.key(), AgentPadError::InvalidCreatorLpToken);
    require_keys_eq!(
        owner,
        accounts.sol_vault.key(),
        AgentPadError::InvalidCreatorLpToken
    );
    let amount = u64::from_le_bytes(
        data[64..72]
            .try_into()
            .map_err(|_| AgentPadError::InvalidCreatorLpToken)?,
    );
    Ok(amount)
}

/// Burn `amount` Raydium LP tokens from `creator_lp_token` (authority =
/// the sol-vault PDA, signing via `vault_seeds` — it is the Raydium
/// creator, v4.2.4). Kept out of the
/// `create_raydium_pool` handler so the handler's stack frame stays small
/// — the handler already carries the 24-account Context plus the Raydium
/// CPI temporaries (the v4 `distribute_harvested_tax` handler overflowed
/// the BPF 4096-byte frame at 4416 bytes for the same reason).
/// The LP mint lives under the classic SPL Token program, so this uses
/// `anchor_spl::token::burn` (not the Token-2022 `Burn` used elsewhere).
fn burn_creator_lp<'info>(
    accounts: &CreateRaydiumPool<'info>,
    vault_seeds: &[&[u8]],
    amount: u64,
) -> Result<()> {
    anchor_spl::token::burn(
        CpiContext::new_with_signer(
            accounts.token_classic_program.to_account_info(),
            anchor_spl::token::Burn {
                mint: accounts.lp_mint.to_account_info(),
                from: accounts.creator_lp_token.to_account_info(),
                authority: accounts.sol_vault.to_account_info(),
            },
            &[vault_seeds],
        ),
        amount,
    )
}

/// Deposit `amount` of the harvester's dividend share into one dividend
/// vault (vault 1 or vault 2 — same code path, v4). SOL goes by system
/// transfer; a stock token goes by transfer_checked from the
/// harvester's ATA to the vault's ATA. Updates the vault's accumulator
/// so holders' pending dividends grow pro-rata.
/// Body of `distribute_harvested_tax`, kept out of the instruction handler
/// so the handler's stack frame stays under the BPF 4096-byte limit (the
/// 20-account Context plus burn/deposit/transfer temporaries overflowed it
/// at 4416 bytes when inline).
fn distribute_inner<'info>(
    accounts: &mut DistributeHarvestedTax<'info>,
    burn_token_amount: u64,
    dividend_reward_amount: u64,
    creator_and_platform_sol_amount: u64,
) -> Result<()> {
    // 1. Direct 20% token burn
    if burn_token_amount > 0 {
        token_2022::burn(
            CpiContext::new(
                accounts.token_program.to_account_info(),
                Burn {
                    mint: accounts.mint.to_account_info(),
                    from: accounts.harvester_token_account.to_account_info(),
                    authority: accounts.harvester.to_account_info(),
                },
            ),
            burn_token_amount,
        )?;
    }

    // 2. Deposit the dividend share (50% of the harvested tax) into the
    // dividend vault(s), in the pool's chosen reward asset(s). Single-
    // asset launches: the whole amount goes to vault 1 (unchanged
    // behavior). Dual-asset launches (v4): fixed 50/50 split between
    // vault 1 and vault 2; an odd lamport goes to vault 1.
    let (div1, div2) = if accounts.dividend_vault_2.is_some() {
        (
            dividend_reward_amount / 2,
            dividend_reward_amount - dividend_reward_amount / 2,
        )
    } else {
        (dividend_reward_amount, 0)
    };
    deposit_dividend(
        &mut accounts.dividend_vault,
        div1,
        &accounts.harvester,
        accounts.harvester_reward_ata.as_ref(),
        accounts.dividend_reward_ata.as_ref(),
        accounts.reward_mint_account.as_ref(),
        accounts.reward_token_program.as_ref(),
        &accounts.system_program,
    )?;
    if div2 > 0 {
        let v2 = accounts
            .dividend_vault_2
            .as_mut()
            .ok_or(AgentPadError::InvalidSecondRewardMint)?;
        deposit_dividend(
            v2,
            div2,
            &accounts.harvester,
            accounts.harvester_reward_ata_2.as_ref(),
            accounts.dividend_reward_ata_2.as_ref(),
            accounts.reward_mint_account_2.as_ref(),
            accounts.reward_token_program_2.as_ref(),
            &accounts.system_program,
        )?;
    }

    // 3. Split remaining 30% SOL (20% Creator, 5% Buyback, 5% Treasury -> normalized out of 3000 bps)
    if creator_and_platform_sol_amount > 0 {
        let creator_sol = creator_and_platform_sol_amount
            .checked_mul(TAX_CREATOR_BPS)
            .ok_or(AgentPadError::MathOverflow)?
            .checked_div(3_000)
            .ok_or(AgentPadError::MathOverflow)?;
        let buyback_sol = creator_and_platform_sol_amount
            .checked_mul(TAX_BUYBACK_BPS)
            .ok_or(AgentPadError::MathOverflow)?
            .checked_div(3_000)
            .ok_or(AgentPadError::MathOverflow)?;
        let treasury_sol = creator_and_platform_sol_amount
            .checked_sub(creator_sol)
            .and_then(|v| v.checked_sub(buyback_sol))
            .ok_or(AgentPadError::MathOverflow)?;

        // Three separate transfers (not a [(dest, amt); 3] array): the
        // array would hold three AccountInfos live at once.
        if creator_sol > 0 {
            system_program::transfer(
                CpiContext::new(
                    accounts.system_program.to_account_info(),
                    system_program::Transfer {
                        from: accounts.harvester.to_account_info(),
                        to: accounts.marketing_wallet.to_account_info(),
                    },
                ),
                creator_sol,
            )?;
        }
        if buyback_sol > 0 {
            system_program::transfer(
                CpiContext::new(
                    accounts.system_program.to_account_info(),
                    system_program::Transfer {
                        from: accounts.harvester.to_account_info(),
                        to: accounts.buyback_vault.to_account_info(),
                    },
                ),
                buyback_sol,
            )?;
        }
        if treasury_sol > 0 {
            system_program::transfer(
                CpiContext::new(
                    accounts.system_program.to_account_info(),
                    system_program::Transfer {
                        from: accounts.harvester.to_account_info(),
                        to: accounts.treasury_wallet.to_account_info(),
                    },
                ),
                treasury_sol,
            )?;
        }
    }

    Ok(())
}

fn deposit_dividend<'info>(
    vault: &mut Account<'info, DividendVault>,
    amount: u64,
    harvester: &Signer<'info>,
    harvester_reward_ata: Option<&AccountInfo<'info>>,
    dividend_reward_ata: Option<&AccountInfo<'info>>,
    reward_mint_account: Option<&InterfaceAccount<'info, Mint>>,
    reward_token_program: Option<&AccountInfo<'info>>,
    system_program: &Program<'info, System>,
) -> Result<()> {
    if amount == 0 {
        return Ok(());
    }
    if vault.reward_mint == Pubkey::default() {
        system_program::transfer(
            CpiContext::new(
                system_program.to_account_info(),
                system_program::Transfer {
                    from: harvester.to_account_info(),
                    to: vault.to_account_info(),
                },
            ),
            amount,
        )?;
    } else {
        let src_ata = harvester_reward_ata
            .ok_or(AgentPadError::InvalidRewardMintAccount)?;
        let dst_ata = dividend_reward_ata
            .ok_or(AgentPadError::InvalidRewardMintAccount)?;
        let reward_mint_acc = reward_mint_account
            .ok_or(AgentPadError::InvalidRewardMintAccount)?;
        let reward_prog = reward_token_program
            .ok_or(AgentPadError::InvalidRewardMintAccount)?;

        require_keys_eq!(
            reward_mint_acc.key(),
            vault.reward_mint,
            AgentPadError::InvalidRewardMintAccount
        );
        token_interface::transfer_checked(
            CpiContext::new(
                reward_prog.to_account_info(),
                token_interface::TransferChecked {
                    from: src_ata.to_account_info(),
                    mint: reward_mint_acc.to_account_info(),
                    to: dst_ata.to_account_info(),
                    authority: harvester.to_account_info(),
                },
            ),
            amount,
            reward_mint_acc.decimals,
        )?;
    }

    vault.total_reward_deposited = vault
        .total_reward_deposited
        .checked_add(amount)
        .ok_or(AgentPadError::MathOverflow)?;

    let delta_acc = (amount as u128)
        .checked_mul(DIVIDEND_PRECISION)
        .ok_or(AgentPadError::MathOverflow)?
        .checked_div(TOTAL_SUPPLY_RAW as u128)
        .ok_or(AgentPadError::MathOverflow)?;
    vault.acc_reward_per_token = vault
        .acc_reward_per_token
        .checked_add(delta_acc)
        .ok_or(AgentPadError::MathOverflow)?;
    Ok(())
}

/// Pay one dividend leg (one vault, one asset) to the user. The vault
/// signs via its PDA seeds. `pending == 0` is a no-op so a dual-asset
/// claim still succeeds when only one leg has accrued anything.
fn pay_dividend_leg<'info>(
    vault_reward_mint: Pubkey,
    vault_info: &AccountInfo<'info>,
    seeds: &[&[u8]],
    pending: u64,
    dividend_reward_ata: Option<&InterfaceAccount<'info, TokenAccount>>,
    user_reward_ata: Option<&InterfaceAccount<'info, TokenAccount>>,
    reward_mint_account: Option<&InterfaceAccount<'info, Mint>>,
    reward_token_program: Option<&Interface<'info, TokenInterface>>,
    user_info: &AccountInfo<'info>,
    system_program: &Program<'info, System>,
) -> Result<()> {
    if pending == 0 {
        return Ok(());
    }
    if vault_reward_mint == Pubkey::default() {
        // SOL payout: the vault is a program-owned DATA account, and the
        // System Program refuses `transfer` from an account that carries
        // data ("Transfer: `from` must not carry data"). Move lamports
        // directly instead — the program owns the vault so it may debit it,
        // and crediting the user's system account keeps the total balanced.
        // (v4.2.8: this path was never reached before — the 3012
        // minter-record bug blocked every claim before payout.)
        require!(
            vault_info.lamports() >= pending,
            AgentPadError::InsufficientDividendBalance
        );
        **vault_info.try_borrow_mut_lamports()? -= pending;
        **user_info.try_borrow_mut_lamports()? += pending;
    } else {
        let src_ata = dividend_reward_ata
            .ok_or(AgentPadError::InvalidRewardMintAccount)?;
        let dst_ata = user_reward_ata
            .ok_or(AgentPadError::InvalidRewardMintAccount)?;
        let reward_mint_acc = reward_mint_account
            .ok_or(AgentPadError::InvalidRewardMintAccount)?;
        let reward_prog = reward_token_program
            .ok_or(AgentPadError::InvalidRewardMintAccount)?;

        require_keys_eq!(
            reward_mint_acc.key(),
            vault_reward_mint,
            AgentPadError::InvalidRewardMintAccount
        );

        token_interface::transfer_checked(
            CpiContext::new_with_signer(
                reward_prog.to_account_info(),
                token_interface::TransferChecked {
                    from: src_ata.to_account_info(),
                    mint: reward_mint_acc.to_account_info(),
                    to: dst_ata.to_account_info(),
                    authority: vault_info.to_account_info(),
                },
                &[seeds],
            ),
            pending,
            reward_mint_acc.decimals,
        )?;
    }
    Ok(())
}


// ==========================================
// ACCOUNT CONTEXTS
// ==========================================

#[derive(Accounts)]
pub struct InitializeGlobal<'info> {
    #[account(
        init,
        payer = admin,
        space = GlobalConfig::LEN,
        seeds = [b"global_config"],
        bump
    )]
    pub global_config: Account<'info, GlobalConfig>,
    #[account(mut, seeds = [b"buyback_vault"], bump)]
    pub buyback_vault: SystemAccount<'info>,
    #[account(mut)]
    pub admin: Signer<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct AdminOnly<'info> {
    #[account(
        mut,
        seeds = [b"global_config"],
        bump = global_config.bump,
        has_one = admin @ AgentPadError::Unauthorized
    )]
    pub global_config: Account<'info, GlobalConfig>,
    pub admin: Signer<'info>,
}

#[derive(Accounts)]
pub struct SetTokenMetadata<'info> {
    // No has_one constraint: the caller may be the admin OR the gate
    // signer; the handler checks the caller against both keys.
    #[account(
        seeds = [b"global_config"],
        bump = global_config.bump
    )]
    pub global_config: Account<'info, GlobalConfig>,
    /// CHECK: the launch's Token-2022 mint. Its mint authority is the
    /// launch pool PDA (enforced at create_launch); the metadata program
    /// validates the mint account itself. Must be mut: the new `Create`
    /// instruction takes the mint writable.
    #[account(mut)]
    pub mint: AccountInfo<'info>,
    #[account(
        seeds = [b"launch_pool", mint.key().as_ref()],
        bump = launch_pool.bump
    )]
    pub launch_pool: Account<'info, LaunchPool>,
    /// CHECK: Metaplex metadata PDA for this mint. Verified in the
    /// handler against the canonical PDA derivation; created by this
    /// instruction (must not exist yet).
    #[account(mut)]
    pub metadata: AccountInfo<'info>,
    /// The caller: admin or gate signer (checked in the handler). Becomes
    /// the metadata update authority. Does NOT pay: the rent comes from
    /// `payer` (v4.2.10), so the gate signer needs no SOL of its own.
    pub authority: Signer<'info>,
    /// Pays the metadata account rent (~0.0137 SOL). Any signer; in the
    /// graduation flow this is the graduator keypair, which is already
    /// funded and balance-monitored.
    #[account(mut)]
    pub payer: Signer<'info>,
    /// CHECK: Metaplex Token Metadata program.
    pub metadata_program: AccountInfo<'info>,
    pub system_program: Program<'info, System>,
    /// CHECK: instructions sysvar (required by the new `Create` ix).
    pub sysvar_instructions: AccountInfo<'info>,
    /// CHECK: Token-2022 program (verified in the handler).
    pub token_program: AccountInfo<'info>,
}

#[derive(Accounts)]
pub struct AddDividendAsset<'info> {
    #[account(
        seeds = [b"global_config"],
        bump = global_config.bump,
        has_one = admin @ AgentPadError::Unauthorized
    )]
    pub global_config: Account<'info, GlobalConfig>,
    pub asset_mint: InterfaceAccount<'info, Mint>,
    #[account(
        init,
        payer = admin,
        space = WhitelistedAsset::LEN,
        seeds = [b"whitelist_asset", asset_mint.key().as_ref()],
        bump
    )]
    pub whitelisted_asset: Account<'info, WhitelistedAsset>,
    #[account(mut)]
    pub admin: Signer<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct ToggleDividendAsset<'info> {
    #[account(
        seeds = [b"global_config"],
        bump = global_config.bump,
        has_one = admin @ AgentPadError::Unauthorized
    )]
    pub global_config: Account<'info, GlobalConfig>,
    #[account(mut, seeds = [b"whitelist_asset", whitelisted_asset.asset_mint.as_ref()], bump = whitelisted_asset.bump)]
    pub whitelisted_asset: Account<'info, WhitelistedAsset>,
    pub admin: Signer<'info>,
}

#[derive(Accounts)]
pub struct CreateLaunch<'info> {
    #[account(mut, seeds = [b"global_config"], bump = global_config.bump)]
    pub global_config: Account<'info, GlobalConfig>,
    #[account(mut)]
    pub mint: InterfaceAccount<'info, Mint>,
    pub whitelisted_asset: Option<Account<'info, WhitelistedAsset>>,
    #[account(
        init,
        payer = creator,
        space = LaunchPool::LEN,
        seeds = [b"launch_pool", mint.key().as_ref()],
        bump
    )]
    pub launch_pool: Account<'info, LaunchPool>,
    #[account(mut, seeds = [b"sol_vault", mint.key().as_ref()], bump)]
    pub sol_vault: SystemAccount<'info>,
    #[account(
        init,
        payer = creator,
        space = DividendVault::LEN,
        seeds = [b"dividend_vault", launch_pool.key().as_ref()],
        bump
    )]
    pub dividend_vault: Account<'info, DividendVault>,
    /// Whitelist entry for the optional second dividend asset (v4).
    pub whitelisted_asset_2: Option<Account<'info, WhitelistedAsset>>,
    /// Second dividend vault (v4, dual-asset launches only). Same
    /// DividendVault layout, different seeds; created only when a second
    /// reward asset is chosen. Raw AccountInfo on purpose: Anchor 0.30's
    /// `init_if_needed` on an `Option<Account>` crashes create_launch with
    /// a BPF null-pointer read (devnet-verified 2026-11-19: single- and
    /// dual-asset simulations both fail, mint_shares is unaffected), so the
    /// handler creates it with a manual SystemProgram CPI and writes the
    /// state itself. PDA + bump are verified in the handler.
    pub dividend_vault_2: Option<AccountInfo<'info>>,
    #[account(mut)]
    pub creator: Signer<'info>,
    pub system_program: Program<'info, System>,
    /// CHECK: Raydium amm_config for index 0 (v4.2.7). Verified in the
    /// handler against its PDA seeds (the owning program is the Raydium
    /// CPMM program); the pool-creation fee is read from its bytes
    /// 36..44 for the minimum-raise check, the same source
    /// `create_raydium_pool` uses.
    pub amm_config: AccountInfo<'info>,
    /// Optional backend co-signer. When present and its key equals the
    /// GlobalConfig gate_signer, the launch counts as official even if the
    /// creator is not the admin. Must be last so clients may omit it.
    pub gate_signer: Option<AccountInfo<'info>>,
}

#[derive(Accounts)]
pub struct MintShares<'info> {
    #[account(seeds = [b"global_config"], bump = global_config.bump)]
    pub global_config: Account<'info, GlobalConfig>,
    #[account(mut, seeds = [b"launch_pool", launch_pool.mint.as_ref()], bump = launch_pool.bump)]
    pub launch_pool: Account<'info, LaunchPool>,
    #[account(mut, seeds = [b"sol_vault", launch_pool.mint.as_ref()], bump = launch_pool.sol_vault_bump)]
    pub sol_vault: SystemAccount<'info>,
    #[account(
        init_if_needed,
        payer = user,
        space = MinterRecord::LEN,
        seeds = [b"minter", launch_pool.mint.as_ref(), user.key().as_ref()],
        bump
    )]
    pub minter_record: Account<'info, MinterRecord>,
    /// CHECK: raw inviter address, used only to derive the inviter's
    /// minter-record PDA. PROGRAM_ID when no inviter was given.
    pub inviter: AccountInfo<'info>,
    #[account(
        init_if_needed,
        payer = user,
        space = MinterRecord::LEN,
        seeds = [b"minter", launch_pool.mint.as_ref(), inviter.key().as_ref()],
        bump
    )]
    pub inviter_record: Option<Account<'info, MinterRecord>>,
    pub agent_gate_signer: Signer<'info>,
    #[account(mut)]
    pub user: Signer<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct RefundShares<'info> {
    #[account(seeds = [b"global_config"], bump = global_config.bump)]
    pub global_config: Account<'info, GlobalConfig>,
    #[account(mut, seeds = [b"launch_pool", launch_pool.mint.as_ref()], bump = launch_pool.bump)]
    pub launch_pool: Account<'info, LaunchPool>,
    #[account(mut, seeds = [b"sol_vault", launch_pool.mint.as_ref()], bump = launch_pool.sol_vault_bump)]
    pub sol_vault: SystemAccount<'info>,
    #[account(mut, seeds = [b"minter", launch_pool.mint.as_ref(), user.key().as_ref()], bump = minter_record.bump)]
    pub minter_record: Account<'info, MinterRecord>,
    pub agent_gate_signer: Signer<'info>,
    #[account(mut)]
    pub user: Signer<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct GraduateToRaydium<'info> {
    #[account(seeds = [b"global_config"], bump = global_config.bump)]
    pub global_config: Account<'info, GlobalConfig>,
    #[account(mut, seeds = [b"launch_pool", launch_pool.mint.as_ref()], bump = launch_pool.bump)]
    pub launch_pool: Account<'info, LaunchPool>,
    #[account(mut)]
    pub mint: InterfaceAccount<'info, Mint>,
    #[account(mut, seeds = [b"sol_vault", launch_pool.mint.as_ref()], bump = launch_pool.sol_vault_bump)]
    pub sol_vault: SystemAccount<'info>,
    #[account(mut, token::mint = mint, token::authority = launch_pool)]
    pub claim_token_vault: InterfaceAccount<'info, TokenAccount>,
    // LP staging is a PDA-owned token account, so no client can create it
    // (PDAs cannot sign a client-side createAccount). The program creates
    // it by hand inside `graduate_to_raydium`.
    // v4.2.1: raw `AccountInfo`, NOT `init_if_needed`. Anchor 0.30's
    // `init_if_needed` on a Token-2022 account captures `owner_program`
    // before the create/init CPIs and then fails the freshly created
    // account in its post-init check, so graduation could never succeed
    // (v4.2 regression, devnet-verified). A plain `init` is equally
    // wrong: it would fail an already-graduated retry at account
    // validation, before the handler's Graduated no-op. With a raw
    // account, no Anchor init/deserialization runs on it, the PDA is
    // verified and the account created-if-absent in the handler, and a
    // retry after graduation still reaches the handler's no-op.
    // The SDK derives the same address.
    #[account(mut)]
    pub lp_token_staging: AccountInfo<'info>,
    /// CHECK: Verified against launch_pool.marketing_wallet
    #[account(mut, address = launch_pool.marketing_wallet)]
    pub marketing_wallet: UncheckedAccount<'info>,
    #[account(mut, seeds = [b"buyback_vault"], bump = global_config.buyback_vault_bump)]
    pub buyback_vault: SystemAccount<'info>,
    /// CHECK: Verified against global_config.treasury_wallet
    #[account(mut, address = global_config.treasury_wallet)]
    pub treasury_wallet: UncheckedAccount<'info>,
    pub token_program: Program<'info, Token2022>,
    pub system_program: Program<'info, System>,
    #[account(mut)]
    pub payer: Signer<'info>,
}

#[derive(Accounts)]
pub struct ClaimTokens<'info> {
    #[account(seeds = [b"launch_pool", launch_pool.mint.as_ref()], bump = launch_pool.bump)]
    pub launch_pool: Account<'info, LaunchPool>,
    pub mint: InterfaceAccount<'info, Mint>,
    #[account(mut, seeds = [b"minter", launch_pool.mint.as_ref(), user.key().as_ref()], bump = minter_record.bump)]
    pub minter_record: Account<'info, MinterRecord>,
    #[account(mut, token::mint = mint, token::authority = launch_pool)]
    pub claim_token_vault: InterfaceAccount<'info, TokenAccount>,
    #[account(mut, token::mint = mint, token::authority = user)]
    pub user_token_account: InterfaceAccount<'info, TokenAccount>,
    pub user: Signer<'info>,
    pub token_program: Program<'info, Token2022>,
}

#[derive(Accounts)]
pub struct DistributeHarvestedTax<'info> {
    #[account(seeds = [b"global_config"], bump = global_config.bump)]
    pub global_config: Account<'info, GlobalConfig>,
    #[account(seeds = [b"launch_pool", launch_pool.mint.as_ref()], bump = launch_pool.bump)]
    pub launch_pool: Account<'info, LaunchPool>,
    #[account(mut)]
    pub mint: InterfaceAccount<'info, Mint>,
    #[account(mut, seeds = [b"dividend_vault", launch_pool.key().as_ref()], bump = dividend_vault.bump)]
    pub dividend_vault: Account<'info, DividendVault>,
    #[account(mut)]
    pub harvester_token_account: InterfaceAccount<'info, TokenAccount>,
    pub reward_mint_account: Option<InterfaceAccount<'info, Mint>>,
    /// Raw AccountInfo on purpose (not deserialized): the token accounts
    /// are only forwarded into the transfer_checked CPI, and deserializing
    /// all eight of them blows this instruction's BPF stack frame past
    /// 4096 bytes (v4 build: 4416). The CPI itself validates them.
    #[account(mut)]
    pub harvester_reward_ata: Option<AccountInfo<'info>>,
    #[account(mut)]
    pub dividend_reward_ata: Option<AccountInfo<'info>>,
    pub reward_token_program: Option<AccountInfo<'info>>,
    /// Second dividend vault (v4, dual-asset launches only).
    #[account(mut, seeds = [b"dividend_vault_2", launch_pool.key().as_ref()], bump)]
    pub dividend_vault_2: Option<Account<'info, DividendVault>>,
    pub reward_mint_account_2: Option<InterfaceAccount<'info, Mint>>,
    #[account(mut)]
    pub harvester_reward_ata_2: Option<AccountInfo<'info>>,
    #[account(mut)]
    pub dividend_reward_ata_2: Option<AccountInfo<'info>>,
    pub reward_token_program_2: Option<AccountInfo<'info>>,
    /// CHECK: Verified against launch_pool.marketing_wallet
    #[account(mut, address = launch_pool.marketing_wallet)]
    pub marketing_wallet: UncheckedAccount<'info>,
    #[account(mut, seeds = [b"buyback_vault"], bump = global_config.buyback_vault_bump)]
    pub buyback_vault: SystemAccount<'info>,
    /// CHECK: Verified against global_config.treasury_wallet
    #[account(mut, address = global_config.treasury_wallet)]
    pub treasury_wallet: UncheckedAccount<'info>,
    #[account(mut)]
    pub harvester: Signer<'info>,
    pub token_program: Program<'info, Token2022>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct HarvestWithheldFees<'info> {
    #[account(seeds = [b"global_config"], bump = global_config.bump)]
    pub global_config: Account<'info, GlobalConfig>,
    #[account(seeds = [b"launch_pool", launch_pool.mint.as_ref()], bump = launch_pool.bump)]
    pub launch_pool: Account<'info, LaunchPool>,
    #[account(mut, address = launch_pool.mint)]
    pub mint: InterfaceAccount<'info, Mint>,
    #[account(mut, token::mint = mint, token::authority = harvester)]
    pub harvester_token_account: InterfaceAccount<'info, TokenAccount>,
    #[account(mut)]
    pub harvester: Signer<'info>,
    pub token_program: Program<'info, Token2022>,
}

#[derive(Accounts)]
pub struct ClaimDividend<'info> {
    // Boxed (v4.2.8): init_if_needed on minter_record pushes try_accounts
    // over the 4K stack limit; Box moves these large accounts to the heap.
    #[account(seeds = [b"launch_pool", launch_pool.mint.as_ref()], bump = launch_pool.bump)]
    pub launch_pool: Account<'info, LaunchPool>,
    #[account(mut, seeds = [b"dividend_vault", launch_pool.key().as_ref()], bump = dividend_vault.bump)]
    pub dividend_vault: Account<'info, DividendVault>,
    /// Minter record (v4.2.8 fix): created on first claim. Swap buyers never
    /// minted, so they have no record — a plain Account would fail with
    /// AccountNotInitialized (3012) even though the frontend shows a pending
    /// dividend from their balance. Raw AccountInfo on purpose: Anchor 0.30
    /// `init_if_needed` in this 16-account struct corrupts the heap
    /// (access violation on first claim by a swap buyer). The handler
    /// creates the record via manual SystemProgram CPI on first claim.
    /// PDA is verified in the handler. reward_debt starts at 0.
    pub minter_record: UncheckedAccount<'info>,
    #[account(token::mint = launch_pool.mint, token::authority = user)]
    pub user_token_account: InterfaceAccount<'info, TokenAccount>,
    pub reward_mint_account: Option<InterfaceAccount<'info, Mint>>,
    #[account(mut)]
    pub dividend_reward_ata: Option<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub user_reward_ata: Option<InterfaceAccount<'info, TokenAccount>>,
    pub reward_token_program: Option<Interface<'info, TokenInterface>>,
    /// Second dividend vault (v4, dual-asset launches only).
    #[account(mut, seeds = [b"dividend_vault_2", launch_pool.key().as_ref()], bump)]
    pub dividend_vault_2: Option<Account<'info, DividendVault>>,
    /// Second-asset dividend debt record (v4). NOTE (v4.2.8): init_if_needed
    /// removed — with init_if_needed on minter_record too, ClaimDividend
    /// exceeds the 4K stack limit and Box<> triggers an access violation
    /// in Anchor 0.30. Dual-asset swap buyers cannot claim leg 2 until
    /// this is re-enabled with a proper stack fix; single-asset (D pool)
    /// is unaffected. Created on first dual claim.
    #[account(
        mut,
        seeds = [b"minter_reward_2", launch_pool.mint.as_ref(), user.key().as_ref()],
        bump
    )]
    pub minter_reward_2: Option<Account<'info, MinterReward2>>,
    pub reward_mint_account_2: Option<InterfaceAccount<'info, Mint>>,
    #[account(mut)]
    pub dividend_reward_ata_2: Option<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub user_reward_ata_2: Option<InterfaceAccount<'info, TokenAccount>>,
    pub reward_token_program_2: Option<Interface<'info, TokenInterface>>,
    #[account(mut)]
    pub user: Signer<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct ManualBuybackAndBurn<'info> {
    #[account(
        mut,
        seeds = [b"global_config"],
        bump = global_config.bump,
        has_one = admin @ AgentPadError::Unauthorized
    )]
    pub global_config: Account<'info, GlobalConfig>,
    #[account(mut, seeds = [b"buyback_vault"], bump = global_config.buyback_vault_bump)]
    pub buyback_vault: SystemAccount<'info>,
    #[account(mut)]
    pub admin: Signer<'info>,
    pub platform_token_mint: InterfaceAccount<'info, Mint>,
    #[account(mut, token::mint = platform_token_mint, token::authority = buyback_vault)]
    pub buyback_token_account: InterfaceAccount<'info, TokenAccount>,
    pub token_program: Program<'info, Token2022>,
    pub system_program: Program<'info, System>,
}

/// Accounts for `create_raydium_pool`. Raydium PDAs (amm_config, authority,
/// pool_state, lp_mint, vaults, observation) are plain unchecked accounts
/// whose addresses are verified against `find_program_address` in the
/// handler; they are created by the Raydium program during the CPI.
#[derive(Accounts)]
pub struct CreateRaydiumPool<'info> {
    #[account(seeds = [b"global_config"], bump)]
    pub global_config: UncheckedAccount<'info>,
    #[account(mut, seeds = [b"launch_pool", mint.key().as_ref()], bump)]
    pub launch_pool: AccountInfo<'info>,
    pub mint: UncheckedAccount<'info>,
    #[account(address = WSOL_MINT)]
    pub wsol_mint: UncheckedAccount<'info>,
    #[account(mut, seeds = [b"sol_vault", mint.key().as_ref()], bump)]
    pub sol_vault: SystemAccount<'info>,
    /// v4.2.7: raw AccountInfo now. The `token::mint = mint,
    /// token::authority = launch_pool` constraint broke retries after a
    /// successful pool creation (the authority had moved to the sol
    /// vault), failing try_accounts before the handler's idempotency
    /// no-op. Verified in the handler instead: mint = launch mint,
    /// owner = launch_pool or sol_vault.
    #[account(mut)]
    pub lp_token_staging: AccountInfo<'info>,
    /// v4.2.7: raw AccountInfo now, same reason as lp_token_staging
    /// (the `associated_token::authority = launch_pool` constraint
    /// failed retries after the authority moved to the sol vault).
    /// Created by the handler on first use (ATA of WSOL owned by the
    /// launch pool) and verified on every call: address = ATA, mint =
    /// WSOL, owner = launch_pool or sol_vault.
    #[account(mut)]
    pub pool_wsol_account: AccountInfo<'info>,
    /// CHECK: Created by Raydium's `initialize` CPI (the LP mint does not
    /// exist until that CPI runs, so no Anchor `init` constraint can
    /// create this account beforehand — v4.2's `init_if_needed` failed
    /// with `IncorrectProgramId` for exactly that reason). Verified in
    /// the handler after the CPI (ATA address, mint, owner).
    #[account(mut)]
    pub creator_lp_token: AccountInfo<'info>,
    /// CHECK: PDA verified in handler against the Raydium program.
    pub amm_config: AccountInfo<'info>,
    /// CHECK: PDA verified in handler against the Raydium program.
    pub raydium_authority: AccountInfo<'info>,
    /// CHECK: PDA verified in handler; created by Raydium during the CPI.
    #[account(mut)]
    pub pool_state: AccountInfo<'info>,
    /// CHECK: PDA verified in handler; created by Raydium during the CPI.
    #[account(mut)]
    pub lp_mint: AccountInfo<'info>,
    /// CHECK: Must be the smaller of (platform mint, WSOL); verified in handler.
    pub token_0_mint: AccountInfo<'info>,
    /// CHECK: Must be the larger of (platform mint, WSOL); verified in handler.
    pub token_1_mint: AccountInfo<'info>,
    /// CHECK: PDA verified in handler; created by Raydium during the CPI.
    #[account(mut)]
    pub token_0_vault: AccountInfo<'info>,
    /// CHECK: PDA verified in handler; created by Raydium during the CPI.
    #[account(mut)]
    pub token_1_vault: AccountInfo<'info>,
    /// CHECK: PDA verified in handler; created by Raydium during the CPI.
    #[account(mut)]
    pub observation_state: AccountInfo<'info>,
    /// CHECK: Raydium pool-creation fee collector (cluster-specific; the SDK
    /// passes the value from shared/constants.json).
    #[account(mut)]
    pub create_pool_fee_account: AccountInfo<'info>,
    pub token_program: Program<'info, Token2022>,
    pub token_classic_program: Program<'info, Token>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
    pub rent: Sysvar<'info, Rent>,
    /// CHECK: Raydium CPMM program id (cluster-specific; the SDK passes the
    /// value from shared/constants.json).
    pub raydium_program: AccountInfo<'info>,
    #[account(mut)]
    pub payer: Signer<'info>,
}

// ==========================================
// EVENTS
// ==========================================

#[event]
pub struct DividendAssetWhitelistedEvent {
    pub asset_mint: Pubkey,
    pub symbol: String,
    pub decimals: u8,
    pub enabled: bool,
}

#[event]
pub struct LaunchCreatedEvent {
    pub mint: Pubkey,
    pub creator: Pubkey,
    pub reward_mint: Pubkey,
    /// Second dividend asset for dual-asset launches (v4); None = single-asset.
    pub reward_mint_2: Option<Pubkey>,
    pub total_shares: u64,
    pub price_per_share: u64,
    pub is_official: bool,
}

#[event]
pub struct SharesMintedEvent {
    pub mint: Pubkey,
    pub user: Pubkey,
    pub shares_count: u64,
    pub sol_cost: u64,
    pub sold_shares: u64,
}

#[event]
pub struct SharesRefundedEvent {
    pub mint: Pubkey,
    pub user: Pubkey,
    pub shares_count: u64,
    pub refund_lamports: u64,
}

#[event]
pub struct PoolGraduatedEvent {
    pub mint: Pubkey,
    pub lp_sol: u64,
    pub lp_tokens: u64,
    pub creator_sol: u64,
    pub buyback_sol: u64,
    pub treasury_sol: u64,
}

#[event]
pub struct PlatformBuybackBurnEvent {
    pub sol_spent: u64,
    pub tokens_burned: u64,
    pub timestamp: i64,
}

#[event]
pub struct RaydiumPoolCreatedEvent {
    pub mint: Pubkey,
    pub pool_id: Pubkey,
    pub lp_sol: u64,
    pub lp_tokens: u64,
    pub token_0_mint: Pubkey,
    pub token_1_mint: Pubkey,
}
