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
    state::{AccountState, Mint as MintState},
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
    let mut signers: Vec<&Keypair> = vec![payer];
    signers.extend_from_slice(extra);
    let mut tx = Transaction::new_unsigned(Message::new(ixs, Some(&payer.pubkey())));
    tx.try_sign(&signers, svm.latest_blockhash()).unwrap();
    if let Err(e) = svm.send_transaction(tx) {
        panic!("tx failed: {:#?}", e.meta.logs);
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