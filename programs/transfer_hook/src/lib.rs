use anchor_lang::prelude::*;
use anchor_lang::solana_program::{program::invoke_signed, system_instruction};
use anchor_spl::token_interface::{Mint, TokenAccount};
use spl_tlv_account_resolution::{
    account::ExtraAccountMeta, seeds::Seed, state::ExtraAccountMetaList,
};
use spl_transfer_hook_interface::instruction::{ExecuteInstruction, TransferHookInstruction};

// ⚠️ REPLACE after `anchor keys list` (see BUILD_CHECKLIST step 3)
declare_id!("Dszak9xWCHKeMKbNWx27u4mpo5ADFwqmC4iBS2w3bccZ");

/// RATE LIMIT HOOK (slide 6: "Rate Limits — cap the amount that can be
/// transferred in a window").
///
/// PER WALLET: each sender may move at most `LIMIT_PER_WINDOW` tokens per
/// `WINDOW_SECONDS`. Each wallet's window state lives in its own PDA at
/// ["rate-limit", mint, owner] — like every bank customer having their own
/// daily limit. Exceeding it errors → the ENTIRE transfer reverts
/// (slide 5: "If the hook returns an error, the entire transfer reverts").
/// Trade-off: each wallet needs a one-time `initialize_rate_limit` call
/// before its first send (hooks can't create accounts mid-transfer).
const WINDOW_SECONDS: i64 = 86_400; // 24h window
const LIMIT_PER_WINDOW: u64 = 500_000; // 0.5 tokens at 6 decimals — small so tests can hit it

#[program]
pub mod transfer_hook {
    use super::*;

    /// Called ONCE after mint creation. Creates the ExtraAccountMetaList PDA
    /// at ["extra-account-metas", mint] and the rate-limit state PDA.
    pub fn initialize_extra_account_meta_list(
        ctx: Context<InitializeExtraAccountMetaList>,
    ) -> Result<()> {
        // Declare the ONE extra account the hook needs on every transfer:
        // the PER-WALLET rate-limit PDA at ["rate-limit", mint, owner].
        // Execute account order: 0=source, 1=mint, 2=destination, 3=owner, 4=metalist —
        // so the seeds reference index 1 (mint) and index 3 (the SENDER's owner).
        // Token-2022's resolver derives the right PDA for whoever is sending.
        let account_metas = vec![ExtraAccountMeta::new_with_seeds(
            &[
                Seed::Literal {
                    bytes: b"rate-limit".to_vec(),
                },
                Seed::AccountKey { index: 1 },
                Seed::AccountKey { index: 3 },
            ],
            false, // is_signer
            true,  // is_writable
        )
        .map_err(|_| TransferHookError::MetaResolution)?];

        let mint = ctx.accounts.mint.key();
        let (_, bump) = Pubkey::find_program_address(
            &[b"extra-account-metas", mint.as_ref()],
            ctx.program_id,
        );
        let signer_seeds: &[&[&[u8]]] = &[&[b"extra-account-metas", mint.as_ref(), &[bump]]];

        // Allocate + fund the ExtraAccountMetaList PDA (manual CPI — TLV data
        // can't use Anchor's declarative `init`).
        let account_size = ExtraAccountMetaList::size_of(account_metas.len())
            .map_err(|_| TransferHookError::MetaResolution)? as u64;
        let lamports = Rent::get()?.minimum_balance(account_size as usize);

        // Raw invoke_signed instead of Anchor's CpiContext helper — avoids
        // cross-version type friction and keeps error conversion clean.
        invoke_signed(
            &system_instruction::create_account(
                &ctx.accounts.payer.key(),
                &ctx.accounts.extra_account_meta_list.key(),
                lamports,
                account_size,
                ctx.program_id,
            ),
            &[
                ctx.accounts.payer.to_account_info(),
                ctx.accounts.extra_account_meta_list.to_account_info(),
                ctx.accounts.system_program.to_account_info(),
            ],
            signer_seeds,
        )?;

        ExtraAccountMetaList::init::<ExecuteInstruction>(
            &mut ctx.accounts.extra_account_meta_list.try_borrow_mut_data()?,
            &account_metas,
        )
        .map_err(|_| TransferHookError::MetaResolution)?;

        Ok(())
    }

    /// One-time registration per wallet: creates that wallet's rate-limit PDA.
    /// Must be called before the wallet's FIRST send (the hook cannot create
    /// accounts mid-transfer — no payer signs at CPI depth). Anyone can pay
    /// to register any owner.
    pub fn initialize_rate_limit(ctx: Context<InitializeRateLimit>) -> Result<()> {
        let rate_limit = &mut ctx.accounts.rate_limit;
        rate_limit.window_start = Clock::get()?.unix_timestamp;
        rate_limit.window_total = 0;
        Ok(())
    }

    /// The hook body. Token-2022 CPIs into this on EVERY transfer of the mint.
    pub fn transfer_hook(ctx: Context<TransferHook>, amount: u64) -> Result<()> {
        // SECURITY (slide 9): only run inside a genuine transfer. Without this,
        // anyone could call execute directly and burn through the window quota
        // (or reset state) without moving tokens.
        check_is_transferring(&ctx)?;

        let rate_limit = &mut ctx.accounts.rate_limit;
        let now = Clock::get()?.unix_timestamp;

        // New window? Reset. (Same Clock-elapsed pattern as SubSol's process_payment.)
        if now - rate_limit.window_start >= WINDOW_SECONDS {
            rate_limit.window_start = now;
            rate_limit.window_total = 0;
        }

        let new_total = rate_limit
            .window_total
            .checked_add(amount)
            .ok_or(TransferHookError::Overflow)?;

        // Over the cap → error → Token-2022 reverts the WHOLE transfer.
        require!(
            new_total <= LIMIT_PER_WINDOW,
            TransferHookError::RateLimitExceeded
        );

        rate_limit.window_total = new_total;
        msg!(
            "Rate limit OK: {} / {} used this window",
            new_total,
            LIMIT_PER_WINDOW
        );
        Ok(())
    }

    /// Token-2022 invokes the hook with the spl-transfer-hook-interface
    /// `Execute` discriminator, which Anchor's dispatch doesn't recognize.
    /// This fallback catches the raw instruction and routes it to
    /// `transfer_hook`. (Official Anchor workaround; version-sensitive.)
    pub fn fallback<'info>(
        program_id: &'info Pubkey,
        accounts: &'info [AccountInfo<'info>],
        data: &[u8],
    ) -> Result<()> {
        let instruction = TransferHookInstruction::unpack(data)
            .map_err(|_| TransferHookError::MetaResolution)?;
        match instruction {
            TransferHookInstruction::Execute { amount } => {
                // amount_bytes must live for 'info to match the generated handler.
                // Box::leak is safe here: Solana's allocator is reset after each
                // instruction, so this is freed when the invocation ends.
                let amount_bytes: &'info [u8] = Box::leak(Box::new(amount.to_le_bytes()));
                __private::__global::transfer_hook(program_id, accounts, amount_bytes)
            }
            _ => Err(ProgramError::InvalidInstructionData.into()),
        }
    }
}

/// Reads the source token account's TransferHookAccount extension and confirms
/// `transferring == true` — proving Token-2022 set it mid-transfer (slide 9).
fn check_is_transferring(ctx: &Context<TransferHook>) -> Result<()> {
    use spl_token_2022::extension::{
        transfer_hook::TransferHookAccount, BaseStateWithExtensions, StateWithExtensions,
    };
    use spl_token_2022::state::Account as Token2022Account;

    let source_info = ctx.accounts.source_token.to_account_info();
    let data = source_info.try_borrow_data()?;
    let token_account = StateWithExtensions::<Token2022Account>::unpack(&data)
        .map_err(|_| TransferHookError::MetaResolution)?;
    let extension = token_account
        .get_extension::<TransferHookAccount>()
        .map_err(|_| TransferHookError::MetaResolution)?;

    if !bool::from(extension.transferring) {
        return err!(TransferHookError::NotTransferring);
    }
    Ok(())
}

#[derive(Accounts)]
pub struct InitializeExtraAccountMetaList<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,

    /// CHECK: PDA is created + written manually in the handler.
    #[account(
        mut,
        seeds = [b"extra-account-metas", mint.key().as_ref()],
        bump
    )]
    pub extra_account_meta_list: AccountInfo<'info>,

    pub mint: InterfaceAccount<'info, Mint>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct InitializeRateLimit<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,

    pub mint: InterfaceAccount<'info, Mint>,

    /// CHECK: the wallet being registered; only its key seeds the PDA.
    pub owner: UncheckedAccount<'info>,

    #[account(
        init,
        payer = payer,
        space = 8 + RateLimit::INIT_SPACE,
        seeds = [b"rate-limit", mint.key().as_ref(), owner.key().as_ref()],
        bump
    )]
    pub rate_limit: Account<'info, RateLimit>,

    pub system_program: Program<'info, System>,
}

// Account order MUST match the interface: source, mint, destination, owner,
// extra_account_meta_list, then the extras from the list (rate_limit).
#[derive(Accounts)]
pub struct TransferHook<'info> {
    pub source_token: InterfaceAccount<'info, TokenAccount>,
    pub mint: InterfaceAccount<'info, Mint>,
    pub destination_token: InterfaceAccount<'info, TokenAccount>,
    /// CHECK: source owner, not read here.
    pub owner: UncheckedAccount<'info>,
    /// CHECK: ExtraAccountMetaList PDA.
    #[account(seeds = [b"extra-account-metas", mint.key().as_ref()], bump)]
    pub extra_account_meta_list: UncheckedAccount<'info>,
    // Per-wallet: seeded by the SENDER's owner key. Musa exhausting his window
    // no longer touches yours.
    #[account(
        mut,
        seeds = [b"rate-limit", mint.key().as_ref(), owner.key().as_ref()],
        bump
    )]
    pub rate_limit: Account<'info, RateLimit>,
}

#[account]
#[derive(InitSpace)]
pub struct RateLimit {
    /// When the current window opened (unix timestamp).
    pub window_start: i64,
    /// Tokens moved so far in this window.
    pub window_total: u64,
}

#[error_code]
pub enum TransferHookError {
    #[msg("Hook can only be invoked from a real transfer")]
    NotTransferring,
    #[msg("Transfer exceeds the rate limit for this window")]
    RateLimitExceeded,
    #[msg("Arithmetic overflow")]
    Overflow,
    #[msg("Failed to resolve or read extra-account metadata")]
    MetaResolution,
}
