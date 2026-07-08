//! Rate-limit transfer hook integration tests.
//! Place at: programs/solana-summer-t22/tests/transfer_hook.rs

mod common;
use common::*;

use anchor_lang::solana_program::instruction::AccountMeta;
use solana_account::Account;
use spl_token_2022::{
    extension::{
        transfer_hook::TransferHookAccount,
        BaseStateWithExtensionsMut, ExtensionType, StateWithExtensionsMut,
    },
    state::{Account as T2022Account, AccountState},
};

pub const HOOK_SO: &[u8] =
    include_bytes!("../../../target/deploy/transfer_hook.so");

const LIMIT_PER_WINDOW: u64 = 500_000;

fn extra_meta_list_pda(mint: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[b"extra-account-metas", mint.as_ref()],
        &transfer_hook::ID,
    )
    .0
}

fn rate_limit_pda(mint: &Pubkey, owner: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[b"rate-limit", mint.as_ref(), owner.as_ref()],
        &transfer_hook::ID,
    )
    .0
}

/// Creates a Token-2022 token account WITH the TransferHookAccount extension.
/// common::fund_token_account creates a base 165-byte account without it,
/// which Token-2022 rejects when the mint has a transfer hook.
fn fund_hooked_account(
    svm: &mut LiteSVM,
    mint: &Pubkey,
    owner: &Pubkey,
    amount: u64,
) -> Pubkey {
    let address = Keypair::new().pubkey();
    let len = ExtensionType::try_calculate_account_len::<T2022Account>(&[
        ExtensionType::TransferHookAccount,
    ])
    .unwrap();
    let mut data = vec![0u8; len];
    {
        let mut state =
            StateWithExtensionsMut::<T2022Account>::unpack_uninitialized(&mut data).unwrap();
        state.init_extension::<TransferHookAccount>(true).unwrap();
        state.base = T2022Account {
            mint: mint.to_bytes().into(),
            owner: owner.to_bytes().into(),
            amount,
            state: AccountState::Initialized,
            ..Default::default()
        };
        state.pack_base();
        state.init_account_type().unwrap();
    }
    let account = Account {
        lamports: svm.minimum_balance_for_rent_exemption(data.len()),
        data,
        owner: TOKEN_2022_ID,
        executable: false,
        rent_epoch: 0,
    };
    svm.set_account(address, account).unwrap();
    address
}

fn init_extra_account_meta_list(svm: &mut LiteSVM, payer: &Keypair, mint: &Pubkey) {
    let ix = Instruction {
        program_id: transfer_hook::ID,
        accounts: transfer_hook::accounts::InitializeExtraAccountMetaList {
            payer: payer.pubkey(),
            extra_account_meta_list: extra_meta_list_pda(mint),
            mint: *mint,
            system_program: system_program::ID,
        }
        .to_account_metas(None),
        data: transfer_hook::instruction::InitializeExtraAccountMetaList {}.data(),
    };
    let t = tx(&svm, &[ix], &payer.pubkey(), &[payer]);
    svm.send_transaction(t).expect("init ELAM failed");
}

fn init_rate_limit(svm: &mut LiteSVM, payer: &Keypair, mint: &Pubkey, owner: &Pubkey) {
    let ix = Instruction {
        program_id: transfer_hook::ID,
        accounts: transfer_hook::accounts::InitializeRateLimit {
            payer: payer.pubkey(),
            mint: *mint,
            owner: *owner,
            rate_limit: rate_limit_pda(mint, owner),
            system_program: system_program::ID,
        }
        .to_account_metas(None),
        data: transfer_hook::instruction::InitializeRateLimit {}.data(),
    };
    let t = tx(&svm, &[ix], &payer.pubkey(), &[payer]);
    svm.send_transaction(t).expect("init rate limit failed");
}

/// Builds a hooked transfer_checked with extra hook accounts appended.
fn hooked_transfer_ix(
    mint: &Pubkey,
    from: &Pubkey,
    to: &Pubkey,
    owner: &Pubkey,
    amount: u64,
) -> Instruction {
    // Build transfer_checked by hand with the repo's Instruction/AccountMeta
    // (avoids a solana-instruction version collision from the spl helper).
    // TransferChecked = instruction discriminator 12, then amount (u64 LE) + decimals (u8).
    let mut data = vec![12u8];
    data.extend_from_slice(&amount.to_le_bytes());
    data.push(6u8); // decimals
    let mut accounts = vec![
        AccountMeta::new(*from, false),
        AccountMeta::new_readonly(*mint, false),
        AccountMeta::new(*to, false),
        AccountMeta::new_readonly(*owner, true),
    ];
    // Standard spl-transfer-hook-interface resolver order:
    // resolved extras, then hook program, then ELAM PDA.
    accounts.extend(vec![
        AccountMeta::new_readonly(transfer_hook::ID, false),
        AccountMeta::new_readonly(extra_meta_list_pda(mint), false),
        AccountMeta::new(rate_limit_pda(mint, owner), false),
    ]);
    Instruction {
        program_id: TOKEN_2022_ID,
        accounts,
        data,
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// TEST 1: Mint + ELAM PDA + per-wallet rate-limit all initialize.
// ─────────────────────────────────────────────────────────────────────────────
#[test]
fn hook_initializes() {
    let (mut svm, admin, program_id) = setup();
    svm.add_program(transfer_hook::ID, HOOK_SO).unwrap();
    init_config(&mut svm, &admin, program_id);
    let mint = init_mint(&mut svm, &admin, program_id);
    init_extra_account_meta_list(&mut svm, &admin, &mint.pubkey());

    assert!(
        svm.get_account(&extra_meta_list_pda(&mint.pubkey())).is_some(),
        "ELAM PDA missing"
    );

    let owner = funded_keypair(&mut svm);
    init_rate_limit(&mut svm, &admin, &mint.pubkey(), &owner.pubkey());

    let rl_acc = svm
        .get_account(&rate_limit_pda(&mint.pubkey(), &owner.pubkey()))
        .expect("rate limit account missing");
    let rl =
        transfer_hook::RateLimit::try_deserialize(&mut rl_acc.data.as_slice()).unwrap();
    assert_eq!(rl.window_total, 0);
}

// ──────────────────────────────────────────────────────────────────────────────
// TEST 2: SECURITY — calling execute directly without a real transfer fails.
// The transferring-flag check (slide 9) doing its job.
// ─────────────────────────────────────────────────────────────────────────────
#[test]
fn direct_execute_is_rejected() {
    let (mut svm, admin, program_id) = setup();
    svm.add_program(transfer_hook::ID, HOOK_SO).unwrap();
    init_config(&mut svm, &admin, program_id);
    let mint = init_mint(&mut svm, &admin, program_id);
    init_extra_account_meta_list(&mut svm, &admin, &mint.pubkey());

    let owner = funded_keypair(&mut svm);
    init_rate_limit(&mut svm, &admin, &mint.pubkey(), &owner.pubkey());
    let from = fund_hooked_account(&mut svm, &mint.pubkey(), &owner.pubkey(), 1_000_000);
    let to = fund_hooked_account(&mut svm, &mint.pubkey(), &Pubkey::new_unique(), 0);

    let ix = Instruction {
        program_id: transfer_hook::ID,
        accounts: transfer_hook::accounts::TransferHook {
            source_token: from,
            mint: mint.pubkey(),
            destination_token: to,
            owner: owner.pubkey(),
            extra_account_meta_list: extra_meta_list_pda(&mint.pubkey()),
            rate_limit: rate_limit_pda(&mint.pubkey(), &owner.pubkey()),
        }
        .to_account_metas(None),
        data: transfer_hook::instruction::TransferHook { amount: 100_000 }.data(),
    };
    let t = tx(&svm, &[ix], &owner.pubkey(), &[&owner]);
    assert!(
        svm.send_transaction(t).is_err(),
        "direct execute must fail when source is not mid-transfer"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// TEST 3: Transfer under the limit succeeds; hook records usage.
// ─────────────────────────────────────────────────────────────────────────────
#[test]
fn transfer_under_limit_succeeds() {
    let (mut svm, admin, program_id) = setup();
    svm.add_program(transfer_hook::ID, HOOK_SO).unwrap();
    init_config(&mut svm, &admin, program_id);
    let mint = init_mint(&mut svm, &admin, program_id);
    init_extra_account_meta_list(&mut svm, &admin, &mint.pubkey());

    let owner = funded_keypair(&mut svm);
    init_rate_limit(&mut svm, &admin, &mint.pubkey(), &owner.pubkey());
    let from = fund_hooked_account(&mut svm, &mint.pubkey(), &owner.pubkey(), 1_000_000);
    let to = fund_hooked_account(&mut svm, &mint.pubkey(), &Pubkey::new_unique(), 0);

    let ix = hooked_transfer_ix(&mint.pubkey(), &from, &to, &owner.pubkey(), 400_000);
    let t = tx(&svm, &[ix], &owner.pubkey(), &[&owner]);
    svm.send_transaction(t).expect("hooked transfer failed");

    assert_eq!(token_amount(&svm, &from), 600_000);
    assert_eq!(token_amount(&svm, &to), 400_000);

    let rl_acc = svm
        .get_account(&rate_limit_pda(&mint.pubkey(), &owner.pubkey()))
        .unwrap();
    let rl = transfer_hook::RateLimit::try_deserialize(&mut rl_acc.data.as_slice()).unwrap();
    assert_eq!(rl.window_total, 400_000);
}

// ─────────────────────────────────────────────────────────────────────────────
// TEST 4: THE MONEY TEST — exceeding the limit reverts the WHOLE transfer.
// Balances stay untouched. Hook veto power (slide 5).
// ─────────────────────────────────────────────────────────────────────────────
#[test]
fn transfer_over_limit_reverts() {
    let (mut svm, admin, program_id) = setup();
    svm.add_program(transfer_hook::ID, HOOK_SO).unwrap();
    init_config(&mut svm, &admin, program_id);
    let mint = init_mint(&mut svm, &admin, program_id);
    init_extra_account_meta_list(&mut svm, &admin, &mint.pubkey());

    let owner = funded_keypair(&mut svm);
    init_rate_limit(&mut svm, &admin, &mint.pubkey(), &owner.pubkey());
    let from = fund_hooked_account(&mut svm, &mint.pubkey(), &owner.pubkey(), 1_000_000);
    let to = fund_hooked_account(&mut svm, &mint.pubkey(), &Pubkey::new_unique(), 0);

    // First: 400k of 500k window. Fine.
    let ix = hooked_transfer_ix(&mint.pubkey(), &from, &to, &owner.pubkey(), 400_000);
    let t = tx(&svm, &[ix], &owner.pubkey(), &[&owner]);
    svm.send_transaction(t).expect("first transfer failed");

    // Second: 200k more = 600k total > 500k cap. Must revert entirely.
    let ix = hooked_transfer_ix(&mint.pubkey(), &from, &to, &owner.pubkey(), 200_000);
    let t = tx(&svm, &[ix], &owner.pubkey(), &[&owner]);
    assert!(
        svm.send_transaction(t).is_err(),
        "transfer over window limit must revert"
    );

    // Balances unchanged from after the first transfer.
    assert_eq!(token_amount(&svm, &from), 600_000);
    assert_eq!(token_amount(&svm, &to), 400_000);

    // Window usage unchanged.
    let rl_acc = svm
        .get_account(&rate_limit_pda(&mint.pubkey(), &owner.pubkey()))
        .unwrap();
    let rl = transfer_hook::RateLimit::try_deserialize(&mut rl_acc.data.as_slice()).unwrap();
    assert_eq!(rl.window_total, 400_000);
    let _ = LIMIT_PER_WINDOW;
}

// ─────────────────────────────────────────────────────────────────────────────
// TEST 5: Musa exhausting his window does NOT block Chidi.
// Each wallet has its own independent rate-limit PDA (slide 6: Rate Limits).
// ─────────────────────────────────────────────────────────────────────────────
#[test]
fn limits_are_per_wallet() {
    let (mut svm, admin, program_id) = setup();
    svm.add_program(transfer_hook::ID, HOOK_SO).unwrap();
    init_config(&mut svm, &admin, program_id);
    let mint = init_mint(&mut svm, &admin, program_id);
    init_extra_account_meta_list(&mut svm, &admin, &mint.pubkey());

    let musa = funded_keypair(&mut svm);
    let chidi = funded_keypair(&mut svm);
    init_rate_limit(&mut svm, &admin, &mint.pubkey(), &musa.pubkey());
    init_rate_limit(&mut svm, &admin, &mint.pubkey(), &chidi.pubkey());

    let musa_acc = fund_hooked_account(&mut svm, &mint.pubkey(), &musa.pubkey(), 1_000_000);
    let chidi_acc = fund_hooked_account(&mut svm, &mint.pubkey(), &chidi.pubkey(), 1_000_000);
    let sink = fund_hooked_account(&mut svm, &mint.pubkey(), &Pubkey::new_unique(), 0);

    // Musa uses his entire window.
    let ix = hooked_transfer_ix(&mint.pubkey(), &musa_acc, &sink, &musa.pubkey(), 500_000);
    let t = tx(&svm, &[ix], &musa.pubkey(), &[&musa]);
    svm.send_transaction(t).expect("musa max transfer failed");

    // Musa now blocked.
    let ix = hooked_transfer_ix(&mint.pubkey(), &musa_acc, &sink, &musa.pubkey(), 1);
    let t = tx(&svm, &[ix], &musa.pubkey(), &[&musa]);
    assert!(svm.send_transaction(t).is_err(), "musa should be over his limit");

    // Chidi unaffected — his window is completely independent.
    let ix = hooked_transfer_ix(&mint.pubkey(), &chidi_acc, &sink, &chidi.pubkey(), 300_000);
    let t = tx(&svm, &[ix], &chidi.pubkey(), &[&chidi]);
    svm.send_transaction(t).expect("chidi must not be blocked by musa");

    assert_eq!(token_amount(&svm, &chidi_acc), 700_000);
}
