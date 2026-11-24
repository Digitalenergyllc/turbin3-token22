use anchor_lang::{
    prelude::Pubkey,
    solana_program::{instruction::Instruction, system_program},
    InstructionData, ToAccountMetas,
};
use anchor_spl::token_interface::spl_token_2022::{
    extension::{
        default_account_state::DefaultAccountState, metadata_pointer::MetadataPointer,
        transfer_fee::TransferFeeConfig, BaseStateWithExtensions, ExtensionType,
        StateWithExtensions,
    },
    instruction::{initialize_account3, mint_to},
    state::{Account as TokenAccountState, AccountState, Mint as MintState},
};
use litesvm::LiteSVM;
use solana_keypair::Keypair;
use solana_message::Message;
use solana_signer::Signer;
use solana_transaction::Transaction;
use t22::{accounts, instruction, ID};

const TOKEN_2022_PROGRAM_ID: Pubkey = anchor_spl::token_interface::spl_token_2022::ID;
const DECIMALS: u8 = 6;
const FEE_BPS: u16 = 50; // 0.5% issuer fee
const MAX_FEE: u64 = 5_000_000; // 5 tokens

fn setup() -> (LiteSVM, Keypair) {
    let mut svm = LiteSVM::new();
    let payer = Keypair::new();
    svm.airdrop(&payer.pubkey(), 10_000_000_000).unwrap();
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/deploy/t22.so");
    svm.add_program_from_file(ID, path).unwrap();
    (svm, payer)
}

fn send(svm: &mut LiteSVM, payer: &Keypair, ixs: &[Instruction], extra: &[&Keypair]) {
    svm.expire_blockhash();
    let mut signers: Vec<&Keypair> = vec![payer];
    signers.extend_from_slice(extra);
    let mut tx = Transaction::new_unsigned(Message::new(ixs, Some(&payer.pubkey())));
    tx.try_sign(&signers, svm.latest_blockhash()).unwrap();
    if let Err(e) = svm.send_transaction(tx) {
        panic!("tx failed: {:?}\nlogs: {:#?}", e.err, e.meta.logs);
    }
}

fn create_stablecoin(svm: &mut LiteSVM, payer: &Keypair) -> Keypair {
    let mint = Keypair::new();
    let ix = Instruction {
        program_id: ID,
        accounts: accounts::CreateStablecoinMint {
            payer: payer.pubkey(),
            mint: mint.pubkey(),
            token_program: TOKEN_2022_PROGRAM_ID,
            system_program: system_program::ID,
        }
        .to_account_metas(None),
        data: instruction::CreateStablecoinMint {
            decimals: DECIMALS,
            basis_points: FEE_BPS,
            maximum_fee: MAX_FEE,
            name: "Remit USD".to_string(),
            symbol: "rUSD".to_string(),
            uri: "https://example.com/rusd.json".to_string(),
        }
        .data(),
    };
    send(svm, payer, &[ix], &[&mint]);
    mint
}
fn send_expect_fail(svm: &mut LiteSVM, payer: &Keypair, ixs: &[Instruction], extra: &[&Keypair]) -> Vec<String> {
    svm.expire_blockhash();
    let mut signers: Vec<&Keypair> = vec![payer];
    signers.extend_from_slice(extra);
    let mut tx = Transaction::new_unsigned(Message::new(ixs, Some(&payer.pubkey())));
    tx.try_sign(&signers, svm.latest_blockhash()).unwrap();
    match svm.send_transaction(tx) {
        Ok(_) => panic!("expected failure, but it succeeded"),
        Err(e) => e.meta.logs,
    }
}

/// Create a token account sized for whatever this mint's extensions require.
fn create_holder(svm: &mut LiteSVM, payer: &Keypair, mint: &Pubkey, owner: &Pubkey) -> Pubkey {
    let mint_data = svm.get_account(mint).unwrap().data;
    let mint_state = StateWithExtensions::<MintState>::unpack(&mint_data).unwrap();
    let required = ExtensionType::get_required_init_account_extensions(
        &mint_state.get_extension_types().unwrap(),
    );
    let space = ExtensionType::try_calculate_account_len::<TokenAccountState>(&required).unwrap();

    let ta = Keypair::new();
    let lamports = svm.minimum_balance_for_rent_exemption(space);
    send(
        svm,
        payer,
        &[
            solana_system_interface::instruction::create_account(
                &payer.pubkey(), &ta.pubkey(), lamports, space as u64, &TOKEN_2022_PROGRAM_ID,
            ),
            initialize_account3(&TOKEN_2022_PROGRAM_ID, &ta.pubkey(), mint, owner).unwrap(),
        ],
        &[&ta],
    );
    ta.pubkey()
}

fn read_holder(svm: &LiteSVM, account: &Pubkey) -> (AccountState, u64) {
    let data = svm.get_account(account).unwrap().data;
    let state = StateWithExtensions::<TokenAccountState>::unpack(&data).unwrap();
    (state.base.state, state.base.amount)
}

fn kyc_thaw_ix(token_account: &Pubkey, mint: &Pubkey, freeze_authority: &Pubkey) -> Instruction {
    Instruction {
        program_id: ID,
        accounts: accounts::KycThaw {
            token_account: *token_account,
            mint: *mint,
            freeze_authority: *freeze_authority,
            token_program: TOKEN_2022_PROGRAM_ID,
        }
        .to_account_metas(None),
        data: instruction::KycThaw {}.data(),
    }
}
#[test]
fn stablecoin_mint_stacks_four_extensions_plus_metadata() {
    let (mut svm, payer) = setup();
    let mint = create_stablecoin(&mut svm, &payer);

    let account = svm.get_account(&mint.pubkey()).unwrap();
    let state = StateWithExtensions::<MintState>::unpack(&account.data).unwrap();

    // Extensions sit in the account in the order they were initialized.
    assert_eq!(
        state.get_extension_types().unwrap(),
        vec![
            ExtensionType::TransferFeeConfig,
            ExtensionType::MetadataPointer,
            ExtensionType::DefaultAccountState,
            ExtensionType::MintCloseAuthority,
            ExtensionType::TokenMetadata,
        ]
    );

    // Base mint fields
    assert_eq!(state.base.decimals, DECIMALS);
    assert_eq!(Option::<Pubkey>::from(state.base.freeze_authority), Some(payer.pubkey()));

    // Fee config
    let fee = state.get_extension::<TransferFeeConfig>().unwrap();
    assert_eq!(u16::from(fee.newer_transfer_fee.transfer_fee_basis_points), FEE_BPS);

    // Metadata pointer names the mint itself
    let ptr = state.get_extension::<MetadataPointer>().unwrap();
    assert_eq!(Option::<Pubkey>::from(ptr.metadata_address), Some(mint.pubkey()));

    // New accounts start frozen
    let das = state.get_extension::<DefaultAccountState>().unwrap();
    assert_eq!(das.state, AccountState::Frozen as u8);
}

#[test]
fn new_account_is_frozen_until_kyc_thaw() {
    let (mut svm, payer) = setup();
    let mint = create_stablecoin(&mut svm, &payer).pubkey();
    let alice = Keypair::new();
    let alice_ta = create_holder(&mut svm, &payer, &mint, &alice.pubkey());

    // Born frozen: the mint's default applies to every new account.
    assert_eq!(read_holder(&svm, &alice_ta).0, AccountState::Frozen);

    // Frozen accounts can't receive tokens, not even a mint.
    let mint_ix = mint_to(&TOKEN_2022_PROGRAM_ID, &mint, &alice_ta, &payer.pubkey(), &[], 1_000_000_000).unwrap();
    let logs = send_expect_fail(&mut svm, &payer, &[mint_ix.clone()], &[]);
    assert!(logs.iter().any(|l| l.contains("frozen")), "{:?}", logs);

    // KYC clears: the freeze authority thaws Alice's account only.
    send(&mut svm, &payer, &[kyc_thaw_ix(&alice_ta, &mint, &payer.pubkey())], &[]);
    assert_eq!(read_holder(&svm, &alice_ta).0, AccountState::Initialized);

    // Now she can receive.
    send(&mut svm, &payer, &[mint_ix], &[]);
    assert_eq!(read_holder(&svm, &alice_ta).1, 1_000_000_000);

    // The mint-level default is unchanged: the next account is still born frozen.
    let bob = Keypair::new();
    let bob_ta = create_holder(&mut svm, &payer, &mint, &bob.pubkey());
    assert_eq!(read_holder(&svm, &bob_ta).0, AccountState::Frozen);
}

#[test]
fn only_freeze_authority_can_thaw() {
    let (mut svm, payer) = setup();
    let mint = create_stablecoin(&mut svm, &payer).pubkey();
    let alice = Keypair::new();
    let alice_ta = create_holder(&mut svm, &payer, &mint, &alice.pubkey());

    // Alice tries to approve her own KYC.
    svm.airdrop(&alice.pubkey(), 1_000_000_000).unwrap();
    let logs = send_expect_fail(&mut svm, &alice, &[kyc_thaw_ix(&alice_ta, &mint, &alice.pubkey())], &[]);
    assert!(logs.iter().any(|l| l.contains("owner does not match")), "{:?}", logs);
    assert_eq!(read_holder(&svm, &alice_ta).0, AccountState::Frozen);
}