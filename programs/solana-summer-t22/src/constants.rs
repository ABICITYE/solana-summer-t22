use anchor_lang::prelude::*;

#[constant]
pub const SEED: &str = "anchor";

/// Only this address is allowed to initialize a `Config`.
pub const ADMIN: Pubkey = pubkey!("AHYic562KhgtAEkb1rSesqS87dFYRcfXb4WwWus3Zc9C");

/// The transfer hook program that this mint's TransferHook extension points at.
pub const TRANSFER_HOOK_PROGRAM_ID: Pubkey =
    pubkey!("Dszak9xWCHKeMKbNWx27u4mpo5ADFwqmC4iB52w3bccZ");
