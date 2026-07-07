pub mod constants;
pub mod error;
pub mod instructions;
pub mod state;

use anchor_lang::prelude::*;

pub use constants::*;
pub use instructions::*;
pub use state::*;

declare_id!("FkaFBQKxZWYCzGqSC3xsUggiT8E9Swsopsu2gQJH3pi");

#[program]
pub mod solana_summer_t22 {
    use super::*;

    pub fn initialize(ctx: Context<Initialize>) -> Result<()> {
        initialize::handler(ctx)
    }

    pub fn initialize_mint(ctx: Context<InitializeMint>) -> Result<()> {
        mint::handler(ctx)
    }

    pub fn propose_admin(ctx: Context<ProposeAdmin>, new_admin: Pubkey) -> Result<()> {
        propose_admin::handler(ctx, new_admin)
    }

    pub fn confirm_admin(ctx: Context<ConfirmAdmin>) -> Result<()> {
        confirm_admin::handler(ctx)
    }

    pub fn transfer(ctx: Context<Transfer>, amount: u64) -> Result<()> {
        transfer::handler(ctx, amount)
    }

    pub fn burn(ctx: Context<Burn>, amount: u64) -> Result<()> {
        burn::handler(ctx, amount)
    }

    pub fn forced_transfer(ctx: Context<ForcedTransfer>, amount: u64) -> Result<()> {
        forced_transfer::handler(ctx, amount)
    }

    pub fn forced_burn(ctx: Context<ForcedBurn>, amount: u64) -> Result<()> {
        forced_burn::handler(ctx, amount)
    }
}
