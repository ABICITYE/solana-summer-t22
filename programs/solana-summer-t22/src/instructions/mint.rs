use anchor_lang::prelude::*;
use anchor_spl::{token_2022::Token2022, token_interface::Mint};

use crate::{error::ErrorCode, state::Config};

#[derive(Accounts)]
pub struct InitializeMint<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,

    #[account(
        seeds = [b"config"],
        bump,
        constraint = config.admin == payer.key() @ ErrorCode::Unauthorized,
    )]
    pub config: Account<'info, Config>,

    /// CHECK: PDA used as the mint authority and permanent delegate.
    #[account(
        seeds = [b"authority"],
        bump,
    )]
    pub authority: UncheckedAccount<'info>,

    #[account(
        init,
        payer = payer,
        mint::decimals = 6,
        mint::authority = authority,
        mint::token_program = token_program,
        extensions::permanent_delegate::delegate = authority,
        extensions::transfer_hook::program_id = crate::TRANSFER_HOOK_PROGRAM_ID,
        extensions::transfer_hook::authority = authority,
    )]
    pub mint: InterfaceAccount<'info, Mint>,

    pub token_program: Program<'info, Token2022>,
    pub system_program: Program<'info, System>,
}

pub fn handler(_ctx: Context<InitializeMint>) -> Result<()> {
    Ok(())
}
