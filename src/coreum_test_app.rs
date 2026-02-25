use anyhow::Error;
use cosmrs::proto::tendermint::v0_37::abci::ResponseDeliverTx;
use cosmrs::Any;
use cosmwasm_std::{Coin, Timestamp};
use prost::Message;
use serde::de::DeserializeOwned;
use test_tube::runner::result::{RunnerExecuteResult, RunnerResult};
use test_tube::runner::Runner;
use test_tube::BaseApp;
use test_tube::{Module, SigningAccount, Wasm};

use crate::{traits::CwItRunner, ContractType};

#[cfg(feature = "multi-test")]
use anyhow::bail;

pub const FEE_DENOM: &str = "ucore";
const ADDRESS_PREFIX: &str = "core";
const CHAIN_ID: &str = "coreum-mainnet-1";
const DEFAULT_GAS_ADJUSTMENT: f64 = 1.2;

#[derive(Debug, PartialEq)]
pub struct CoreumTestApp {
    inner: BaseApp,
}

impl Default for CoreumTestApp {
    fn default() -> Self {
        CoreumTestApp::new()
    }
}

impl CoreumTestApp {
    pub fn new() -> Self {
        Self {
            inner: BaseApp::new(FEE_DENOM, CHAIN_ID, ADDRESS_PREFIX, DEFAULT_GAS_ADJUSTMENT),
        }
    }

    /// Get the current block time as a timestamp
    pub fn get_block_timestamp(&self) -> Timestamp {
        self.inner.get_block_timestamp()
    }

    /// Get the current block time in nanoseconds
    pub fn get_block_time_nanos(&self) -> i64 {
        self.inner.get_block_time_nanos()
    }

    /// Get the current block time in seconds
    pub fn get_block_time_seconds(&self) -> i64 {
        self.inner.get_block_time_nanos() / 1_000_000_000i64
    }

    /// Get the current block height
    pub fn get_block_height(&self) -> i64 {
        self.inner.get_block_height()
    }

    /// Get the first validator address
    pub fn get_first_validator_address(&self) -> RunnerResult<String> {
        self.inner.get_first_validator_address()
    }

    /// Get the first validator signing account
    pub fn get_first_validator_signing_account(&self) -> RunnerResult<SigningAccount> {
        self.inner.get_first_validator_signing_account()
    }

    /// Increase the time of the blockchain by the given number of seconds.
    pub fn increase_time(&self, seconds: u64) {
        self.inner.increase_time(seconds)
    }

    /// Initialize account with initial balance of any coins.
    /// This function mints new coins and send to newly created account
    pub fn init_account(&self, coins: &[Coin]) -> RunnerResult<SigningAccount> {
        self.inner.init_account(coins)
    }
    /// Convinience function to create multiple accounts with the same
    /// Initial coins balance
    pub fn init_accounts(&self, coins: &[Coin], count: u64) -> RunnerResult<Vec<SigningAccount>> {
        self.inner.init_accounts(coins, count)
    }

    /// Simulate transaction execution and return gas info
    pub fn simulate_tx<I>(&self, msgs: I, signer: &SigningAccount) -> RunnerResult<cosmrs::proto::cosmos::base::abci::v1beta1::GasInfo>
    where
        I: IntoIterator<Item = cosmrs::Any>,
    {
        self.inner.simulate_tx(msgs, signer)
    }

    /// Set parameter set for a given subspace.
    pub fn set_param_set(&self, subspace: &str, pset: impl Into<Any>) -> RunnerResult<()> {
        self.inner.set_param_set(subspace, pset)
    }

    /// Get parameter set for a given subspace.
    pub fn get_param_set<P: Message + Default>(&self, subspace: &str, type_url: &str) -> RunnerResult<P> {
        self.inner.get_param_set(subspace, type_url)
    }
}

impl<'a> Runner<'a> for CoreumTestApp {
    fn execute_multiple<M, R>(&self, msgs: &[(M, &str)], signer: &SigningAccount) -> RunnerExecuteResult<R>
    where
        M: ::prost::Message,
        R: ::prost::Message + Default,
    {
        self.inner.execute_multiple(msgs, signer)
    }

    fn query<Q, R>(&self, path: &str, q: &Q) -> RunnerResult<R>
    where
        Q: ::prost::Message,
        R: ::prost::Message + DeserializeOwned + Default,
    {
        self.inner.query(path, q)
    }

    fn execute_tx(&self, tx_bytes: &[u8]) -> RunnerResult<ResponseDeliverTx> {
        self.inner.execute_tx(tx_bytes)
    }

    fn execute_multiple_raw<R>(&self, msgs: Vec<cosmrs::Any>, signer: &SigningAccount) -> RunnerExecuteResult<R>
    where
        R: prost::Message + Default,
    {
        self.inner.execute_multiple_raw(msgs, signer)
    }
}

impl CwItRunner<'_> for CoreumTestApp {
    fn store_code(&self, code: ContractType, signer: &SigningAccount) -> Result<u64, Error> {
        match code {
            #[cfg(feature = "multi-test")]
            ContractType::MultiTestContract(_) => {
                bail!("MultiTestContract not supported for CoreumTestApp")
            }
            ContractType::Artifact(artifact) => {
                let bytes = artifact.get_wasm_byte_code()?;
                let wasm = Wasm::new(self);
                let code_id = wasm.store_code(&bytes, None, signer)?.data.code_id;
                Ok(code_id)
            }
        }
    }

    fn init_account(&self, initial_balance: &[Coin]) -> Result<SigningAccount, Error> {
        Ok(self.init_account(initial_balance)?)
    }

    fn init_accounts(&self, initial_balance: &[Coin], num_accounts: usize) -> Result<Vec<SigningAccount>, Error> {
        Ok(self.init_accounts(initial_balance, num_accounts as u64)?)
    }

    fn increase_time(&self, seconds: u64) -> Result<(), Error> {
        CoreumTestApp::increase_time(self, seconds);
        Ok(())
    }

    fn query_block_time_nanos(&self) -> u64 {
        self.get_block_time_nanos() as u64
    }
}

#[cfg(test)]
mod tests {
    use crate::artifact::Artifact;
    use cosmwasm_std::Coin;

    use super::*;

    const TEST_ARTIFACT: &str = "artifacts/counter.wasm";

    #[test]
    fn coreum_test_app_store_code() {
        let app = CoreumTestApp::new();
        let admin = app.init_account(&[Coin::new(1000000000000, "ucore")]).unwrap();
        let code_id = app
            .store_code(ContractType::Artifact(Artifact::Local(TEST_ARTIFACT.to_string())), &admin)
            .unwrap();

        assert_eq!(code_id, 1);
    }

    #[test]
    #[should_panic]
    #[cfg(feature = "multi-test")]
    fn coreum_test_app_store_code_multi_test_contract() {
        use crate::test_helpers::test_contract;
        use coreum_wasm_sdk::core::{CoreumMsg, CoreumQueries};

        let app = CoreumTestApp::new();
        let admin = app.init_account(&[Coin::new(1000000000000, "ucore")]).unwrap();
        app.store_code(
            ContractType::MultiTestContract(test_contract::contract::<CoreumMsg, CoreumQueries>()),
            &admin,
        )
        .unwrap();
    }

    #[test]
    fn test_increase_time() {
        let app = CoreumTestApp::new();

        let time = app.get_block_time_nanos();
        CwItRunner::increase_time(&app, 69).unwrap();
        assert_eq!(app.get_block_time_nanos(), time + 69000000000);
    }
}
