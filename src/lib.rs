use anchor_lang::prelude::AccountInfo;
use anchor_lang::AccountDeserialize;
use anchor_spl::token_interface::spl_token_2022::extension::transfer_fee::TransferFeeConfig;
use anchor_spl::token_interface::spl_token_2022::extension::{
    BaseStateWithExtensions, StateWithExtensions,
};
use anyhow::Result;
use hyperplane::curve::base::SwapCurve;
use hyperplane::curve::calculator::TradeDirection;
use hyperplane::state::{SwapPool, SwapState};
use hyperplane::utils::seeds::SWAP_CURVE;

use jupiter_core::amm::{AccountMap, Amm, KeyedAccount, Swap};
use solana_sdk::pubkey;
use solana_sdk::{instruction::AccountMeta, pubkey::Pubkey};

use anchor_spl::token::TokenAccount;
use jupiter_core::amm::{Quote, QuoteParams, SwapAndAccountMetas, SwapParams};

pub const CLOCK_PUBLIC_KEY: Pubkey = pubkey!("SysvarC1ock11111111111111111111111111111111");

#[derive(Clone, Debug)]
pub struct JupiterRarefish {
    market_key: Pubkey,
    pool: SwapPool,
    token_a_vault: Option<TokenAccount>,
    token_b_vault: Option<TokenAccount>,
    token_a_mint_raw: Option<solana_sdk::account::Account>,
    token_b_mint_raw: Option<solana_sdk::account::Account>,
    curve: Option<SwapCurve>,
    last_epoch: u64,
    /// Will always be "Rarefish"
    label: String,
    /// The pubkey of the Rarefish program
    program_id: Pubkey,
}

impl JupiterRarefish {
    pub fn new_from_keyed_account(keyed_account: &KeyedAccount) -> Result<Self> {
        let pool: SwapPool =
            AccountDeserialize::try_deserialize(&mut keyed_account.account.data.as_ref()).unwrap();
        Ok(Self {
            market_key: keyed_account.key,
            label: "Rarefish".into(),
            program_id: hyperplane::id(),
            pool,
            token_a_vault: None,
            token_b_vault: None,
            token_a_mint_raw: None,
            token_b_mint_raw: None,
            curve: None,
            last_epoch: 0,
        })
    }

    pub fn pool_curve(&self) -> Pubkey {
        Pubkey::find_program_address(&[SWAP_CURVE, self.market_key.as_ref()], &self.program_id).0
    }
}

impl Amm for JupiterRarefish {
    fn program_id(&self) -> Pubkey {
        self.program_id
    }

    fn from_keyed_account(keyed_account: &KeyedAccount) -> Result<Self> {
        JupiterRarefish::new_from_keyed_account(keyed_account)
    }

    fn label(&self) -> String {
        self.label.clone()
    }

    fn key(&self) -> Pubkey {
        self.market_key
    }

    fn get_reserve_mints(&self) -> Vec<Pubkey> {
        vec![self.pool.token_a_mint, self.pool.token_b_mint]
    }

    fn get_accounts_to_update(&self) -> Vec<Pubkey> {
        vec![
            self.pool.token_a_vault,
            self.pool.token_b_vault,
            self.pool_curve(),
            CLOCK_PUBLIC_KEY,
            // These 2 accounts are only needed at the start
            self.pool.token_a_mint,
            self.pool.token_b_mint,
        ]
    }

    fn update(&mut self, accounts_map: &AccountMap) -> Result<()> {
        let curve_key = self.pool_curve();
        let mut curve_account = accounts_map.get(&curve_key).unwrap().clone();

        self.token_a_mint_raw = accounts_map
            .get(&self.pool.token_a_mint)
            .map(|account| account.clone());
        self.token_b_mint_raw = accounts_map
            .get(&self.pool.token_b_mint)
            .map(|account| account.clone());
        self.last_epoch = accounts_map
            .get(&CLOCK_PUBLIC_KEY)
            .map(|account| u64::from_le_bytes(account.data[16..24].try_into().unwrap()))
            .unwrap_or(0);

        self.token_a_vault = accounts_map.get(&self.pool.token_a_vault).map(|account| {
            let mut data = &account.data[..TokenAccount::LEN];
            TokenAccount::try_deserialize(&mut data).unwrap()
        });
        self.token_b_vault = accounts_map.get(&self.pool.token_b_vault).map(|account| {
            let mut data = &account.data[..TokenAccount::LEN];
            TokenAccount::try_deserialize(&mut data).unwrap()
        });
        let curve_account_info = AccountInfo::new(
            &curve_key,
            false,
            false,
            &mut curve_account.lamports,
            &mut curve_account.data,
            &curve_account.owner,
            curve_account.executable,
            curve_account.rent_epoch,
        );
        self.curve = Some(hyperplane::curve!(curve_account_info, self.pool));
        Ok(())
    }

    fn quote(&self, quote_params: &QuoteParams) -> Result<Quote> {
        let a_to_b = quote_params.input_mint == self.pool.token_a_mint;
        let source_mint_raw = if a_to_b {
            self.token_a_mint_raw.as_ref().unwrap()
        } else {
            self.token_b_mint_raw.as_ref().unwrap()
        };
        let source_mint =
            StateWithExtensions::<anchor_spl::token_2022::spl_token_2022::state::Mint>::unpack(
                &source_mint_raw.data,
            )?;
        // Handle the case when the source token has a transfer fee
        let actual_amount_in =
            if let Ok(transfer_fee_config) = source_mint.get_extension::<TransferFeeConfig>() {
                hyperplane::swap::utils::sub_input_transfer_fees_with_config(
                    &transfer_fee_config,
                    &self.pool.fees,
                    quote_params.amount,
                    false, // You should pass true here if your frontend takes a fee
                    self.last_epoch,
                )?
            } else {
                quote_params.amount
            };

        let (token_a_amount, token_b_amount) = match (&self.token_a_vault, &self.token_b_vault) {
            (Some(token_a_vault), Some(token_b_vault)) => {
                (token_a_vault.amount, token_b_vault.amount)
            }
            _ => panic!("These token accounts should be updated first"),
        };
        let (trade_direction, source_amount, destination_amount) = if a_to_b {
            (TradeDirection::AtoB, token_a_amount, token_b_amount)
        } else {
            (TradeDirection::BtoA, token_b_amount, token_a_amount)
        };
        let result = self.curve.as_ref().map(|curve| {
            curve.swap(
                u128::from(actual_amount_in),
                u128::from(source_amount),
                u128::from(destination_amount),
                trade_direction,
                self.pool.fees(),
            )
        });
        match result {
            Some(Ok(result)) => {
                // Handle the case when destination token has a transfer fee
                let destination_mint_raw = if a_to_b {
                    self.token_b_mint_raw.as_ref().unwrap()
                } else {
                    self.token_a_mint_raw.as_ref().unwrap()
                };
                let destination_mint = StateWithExtensions::<
                    anchor_spl::token_2022::spl_token_2022::state::Mint,
                >::unpack(&destination_mint_raw.data)?;
                let transfer_fee = if let Ok(transfer_fee_config) =
                    destination_mint.get_extension::<TransferFeeConfig>()
                {
                    transfer_fee_config
                        .calculate_epoch_fee(
                            self.last_epoch,
                            result.destination_amount_swapped as u64,
                        )
                        .unwrap()
                } else {
                    0
                };
                Ok(Quote {
                    out_amount: result.destination_amount_swapped as u64 - transfer_fee,
                    ..Quote::default()
                })
            }
            _ => panic!("Curve account should be updated first"),
        }
    }

    fn get_swap_and_account_metas(&self, swap_params: &SwapParams) -> Result<SwapAndAccountMetas> {
        let SwapParams {
            destination_mint,
            source_mint,
            source_token_account,
            destination_token_account,
            token_transfer_authority,
            ..
        } = swap_params;
        let (
            source_vault,
            source_fees_vault,
            mut source_token_program,
            destination_vault,
            mut destination_token_program,
        ) = if *source_mint == self.pool.token_a_mint {
            (
                self.pool.token_a_vault,
                self.pool.token_a_fees_vault,
                self.pool.token_a_program,
                self.pool.token_b_vault,
                self.pool.token_b_program,
            )
        } else {
            (
                self.pool.token_b_vault,
                self.pool.token_b_fees_vault,
                self.pool.token_b_program,
                self.pool.token_a_vault,
                self.pool.token_a_program,
            )
        };
        // If these fields are not set in SwapPool account then they are the original token program.
        if source_token_program == Pubkey::default() {
            source_token_program = anchor_spl::token::spl_token::id();
        }
        if destination_token_program == Pubkey::default() {
            destination_token_program = anchor_spl::token::spl_token::id();
        }

        let account_metas = vec![
            AccountMeta::new(*token_transfer_authority, true),
            AccountMeta::new(self.market_key, false),
            AccountMeta::new_readonly(self.pool_curve(), false),
            AccountMeta::new_readonly(self.pool.pool_authority, false),
            AccountMeta::new_readonly(*source_mint, false),
            AccountMeta::new_readonly(*destination_mint, false),
            AccountMeta::new(source_vault, false),
            AccountMeta::new(destination_vault, false),
            AccountMeta::new(source_fees_vault, false),
            AccountMeta::new(*source_token_account, false),
            AccountMeta::new(*destination_token_account, false),
            AccountMeta::new(self.program_id, false), // This is the source_token_host_fees_account, passing the program_id means None. You should pass your account here if your frontend takes a fee
            AccountMeta::new_readonly(source_token_program, false),
            AccountMeta::new_readonly(destination_token_program, false),
        ];

        Ok(SwapAndAccountMetas {
            swap: Swap::TokenSwapV2, // Maybe this should be different?
            account_metas,
        })
    }

    fn clone_amm(&self) -> Box<dyn Amm + Send + Sync> {
        Box::new(self.clone())
    }
}

#[cfg(test)]
mod tests {
    use anchor_lang::InstructionData;
    use jupiter_core::amm::{Amm, Quote, SwapParams};
    use jupiter_core::amm::{KeyedAccount, QuoteParams, SwapMode};
    use solana_client::rpc_client::RpcClient;
    use solana_sdk::instruction::Instruction;
    use solana_sdk::message::VersionedMessage;
    use solana_sdk::pubkey;
    use solana_sdk::pubkey::Pubkey;
    use solana_sdk::signer::Signer;
    use solana_sdk::transaction::VersionedTransaction;
    use std::collections::HashMap;

    use crate::JupiterRarefish;

    #[test]
    fn test_jupiter_rarefish_integration_quote_sol_usdc() {
        const SOL_USDC_MARKET: Pubkey = pubkey!("H6jDD8QN6McX2QXMeVw6zBs3HKEgVvtnhsH139heFAnF");
        let token_a_decimals = 9.0;
        let token_b_decimals = 6.0;

        let rpc = RpcClient::new("https://api.mainnet-beta.solana.com/");
        let account = rpc.get_account(&SOL_USDC_MARKET).unwrap();

        let market_account = KeyedAccount {
            key: SOL_USDC_MARKET,
            account,
            params: None,
        };

        let mut jupiter_rarefish =
            JupiterRarefish::new_from_keyed_account(&market_account).unwrap();
        let accounts_to_update = jupiter_rarefish.get_accounts_to_update();

        let accounts_map = rpc
            .get_multiple_accounts(&accounts_to_update)
            .unwrap()
            .iter()
            .enumerate()
            .fold(HashMap::new(), |mut m, (index, account)| {
                if let Some(account) = account {
                    m.insert(accounts_to_update[index], account.clone());
                }
                m
            });
        jupiter_rarefish.update(&accounts_map).unwrap();
        let in_amount = 1_000_000_000_000;
        println!(
            "Getting quote for selling {} SOL",
            in_amount as f64 / 10.0_f64.powf(token_a_decimals)
        );
        let quote_in = in_amount as f64 / 10.0_f64.powf(token_a_decimals);
        let quote = jupiter_rarefish
            .quote(&QuoteParams {
                input_mint: jupiter_rarefish.pool.token_a_mint,
                output_mint: jupiter_rarefish.pool.token_b_mint,
                amount: in_amount,
                swap_mode: SwapMode::ExactIn,
            })
            .unwrap();

        let Quote { out_amount, .. } = quote;

        let quote_out = out_amount as f64 / 10.0_f64.powf(token_b_decimals);
        println!("Quote result: {:?} ({})", quote_out, quote_out / quote_in);

        let in_amount = out_amount;

        println!(
            "Getting quote for buying SOL with {} USDC",
            in_amount as f64 / 10.0_f64.powf(token_b_decimals)
        );
        let quote_in = in_amount as f64 / 10.0_f64.powf(token_b_decimals);
        let quote = jupiter_rarefish
            .quote(&QuoteParams {
                input_mint: jupiter_rarefish.pool.token_b_mint,
                output_mint: jupiter_rarefish.pool.token_a_mint,
                amount: out_amount,
                swap_mode: SwapMode::ExactIn,
            })
            .unwrap();

        let Quote { out_amount, .. } = quote;

        let quote_out = out_amount as f64 / 10.0_f64.powf(token_a_decimals);
        println!(
            "Quote result: {:?} ({})",
            out_amount as f64 / 10.0_f64.powf(token_a_decimals),
            quote_in / quote_out
        );
    }

    #[test]
    fn test_jupiter_rarefish_integration_quote_bern_sol() {
        const BERN_SOL_MARKET: Pubkey = pubkey!("4xepyJRLRhMJSrxYHEa1NnujMspUN2hQpcegn2WsoNpZ");
        let token_a_decimals = 5.0;
        let token_b_decimals = 9.0;

        let rpc = RpcClient::new("https://api.mainnet-beta.solana.com/");
        let account = rpc.get_account(&BERN_SOL_MARKET).unwrap();

        let market_account = KeyedAccount {
            key: BERN_SOL_MARKET,
            account,
            params: None,
        };

        let mut jupiter_rarefish =
            JupiterRarefish::new_from_keyed_account(&market_account).unwrap();
        let accounts_to_update = jupiter_rarefish.get_accounts_to_update();

        let accounts_map = rpc
            .get_multiple_accounts(&accounts_to_update)
            .unwrap()
            .iter()
            .enumerate()
            .fold(HashMap::new(), |mut m, (index, account)| {
                if let Some(account) = account {
                    m.insert(accounts_to_update[index], account.clone());
                }
                m
            });
        jupiter_rarefish.update(&accounts_map).unwrap();
        let in_amount = 1_000_000; // 10 BERN
        println!(
            "Getting quote for selling {} BERN",
            in_amount as f64 / 10.0_f64.powf(token_a_decimals)
        );
        let quote_in = in_amount as f64 / 10.0_f64.powf(token_a_decimals);
        let quote = jupiter_rarefish
            .quote(&QuoteParams {
                input_mint: jupiter_rarefish.pool.token_a_mint,
                output_mint: jupiter_rarefish.pool.token_b_mint,
                amount: in_amount,
                swap_mode: SwapMode::ExactIn,
            })
            .unwrap();

        let Quote { out_amount, .. } = quote;
        let quote_out = out_amount as f64 / 10.0_f64.powf(token_b_decimals);
        println!("Quote result: {:?} ({})", quote_out, quote_out / quote_in);

        let in_amount = out_amount;

        println!(
            "Getting quote for buying BERN with {} SOL",
            in_amount as f64 / 10.0_f64.powf(token_b_decimals)
        );
        let quote_in = in_amount as f64 / 10.0_f64.powf(token_b_decimals);
        let quote = jupiter_rarefish
            .quote(&QuoteParams {
                input_mint: jupiter_rarefish.pool.token_b_mint,
                output_mint: jupiter_rarefish.pool.token_a_mint,
                amount: out_amount,
                swap_mode: SwapMode::ExactIn,
            })
            .unwrap();

        let Quote { out_amount, .. } = quote;

        let quote_out = out_amount as f64 / 10.0_f64.powf(token_a_decimals);
        println!(
            "Quote result: {:?} ({})",
            out_amount as f64 / 10.0_f64.powf(token_a_decimals),
            quote_in / quote_out
        );
    }

    #[test]
    fn test_jupiter_rarefish_integration_quote_usdh_hbb() {
        const USDH_HBB_MARKET: Pubkey = pubkey!("EHX2ozvoD4Vd4GajNrY9rXkP5TZmNgcmrw9cs43gKrW3");
        let token_a_decimals = 6.0;
        let token_b_decimals = 6.0;

        let rpc = RpcClient::new("https://api.mainnet-beta.solana.com/");
        let account = rpc.get_account(&USDH_HBB_MARKET).unwrap();

        let market_account = KeyedAccount {
            key: USDH_HBB_MARKET,
            account,
            params: None,
        };

        let mut jupiter_rarefish =
            JupiterRarefish::new_from_keyed_account(&market_account).unwrap();
        let accounts_to_update = jupiter_rarefish.get_accounts_to_update();

        let accounts_map = rpc
            .get_multiple_accounts(&accounts_to_update)
            .unwrap()
            .iter()
            .enumerate()
            .fold(HashMap::new(), |mut m, (index, account)| {
                if let Some(account) = account {
                    m.insert(accounts_to_update[index], account.clone());
                }
                m
            });
        jupiter_rarefish.update(&accounts_map).unwrap();
        let in_amount = 1_000_000;
        println!(
            "Getting quote for selling {} USDH",
            in_amount as f64 / 10.0_f64.powf(token_a_decimals)
        );
        let quote_in = in_amount as f64 / 10.0_f64.powf(token_a_decimals);
        let quote = jupiter_rarefish
            .quote(&QuoteParams {
                input_mint: jupiter_rarefish.pool.token_a_mint,
                output_mint: jupiter_rarefish.pool.token_b_mint,
                amount: in_amount,
                swap_mode: SwapMode::ExactIn,
            })
            .unwrap();

        let Quote { out_amount, .. } = quote;

        let quote_out = out_amount as f64 / 10.0_f64.powf(token_b_decimals);
        println!("Quote result: {:?} ({})", quote_out, quote_out / quote_in);

        let in_amount = out_amount;

        println!(
            "Getting quote for buying USDH with {} HBB",
            in_amount as f64 / 10.0_f64.powf(token_b_decimals)
        );
        let quote_in = in_amount as f64 / 10.0_f64.powf(token_b_decimals);
        let quote = jupiter_rarefish
            .quote(&QuoteParams {
                input_mint: jupiter_rarefish.pool.token_b_mint,
                output_mint: jupiter_rarefish.pool.token_a_mint,
                amount: out_amount,
                swap_mode: SwapMode::ExactIn,
            })
            .unwrap();

        let Quote { out_amount, .. } = quote;

        let quote_out = out_amount as f64 / 10.0_f64.powf(token_a_decimals);
        println!(
            "Quote result: {:?} ({})",
            out_amount as f64 / 10.0_f64.powf(token_a_decimals),
            quote_in / quote_out
        );
    }

    #[test]
    fn test_jupiter_rarefish_integration_sim() {
        const SOL_USDC_MARKET: Pubkey = pubkey!("H6jDD8QN6McX2QXMeVw6zBs3HKEgVvtnhsH139heFAnF");
        let rpc = RpcClient::new("https://api.mainnet-beta.solana.com/");
        let account = rpc.get_account(&SOL_USDC_MARKET).unwrap();

        let market_account = KeyedAccount {
            key: SOL_USDC_MARKET,
            account,
            params: None,
        };
        let jupiter_rarefish = JupiterRarefish::new_from_keyed_account(&market_account).unwrap();
        let signer = solana_sdk::signature::read_keypair_file("keypair.json").unwrap();
        let signer_ata_a = anchor_spl::associated_token::get_associated_token_address(
            &signer.pubkey(),
            &jupiter_rarefish.pool.token_a_mint,
        );
        let signer_ata_b = anchor_spl::associated_token::get_associated_token_address(
            &signer.pubkey(),
            &jupiter_rarefish.pool.token_b_mint,
        );

        let accounts = jupiter_rarefish
            .get_swap_and_account_metas(&SwapParams {
                in_amount: 10_000_000,
                out_amount: 0,
                source_mint: jupiter_rarefish.pool.token_a_mint,
                destination_mint: jupiter_rarefish.pool.token_b_mint,
                source_token_account: signer_ata_a,
                destination_token_account: signer_ata_b,
                token_transfer_authority: signer.pubkey(),
                open_order_address: None,
                quote_mint_to_referrer: None,
                jupiter_program_id: &Pubkey::default(),
            })
            .unwrap();
        let ixn = Instruction {
            program_id: hyperplane::id(),
            accounts: accounts.account_metas,
            data: hyperplane::instruction::Swap {
                amount_in: 10_000_000,
                minimum_amount_out: 0,
            }
            .data(),
        };
        let txn = VersionedTransaction::try_new(
            VersionedMessage::V0(
                solana_sdk::message::v0::Message::try_compile(
                    &signer.pubkey(),
                    &[ixn],
                    &[],
                    rpc.get_latest_blockhash().unwrap(),
                )
                .unwrap(),
            ),
            &[&signer],
        )
        .unwrap();
        let res = rpc.simulate_transaction(&txn).unwrap();
        println!(
            "Simulating swap on SOL/USDC market {} with response {:?} and eventual errors {:?}",
            SOL_USDC_MARKET, res.value.logs, res.value.err
        );
    }
}
