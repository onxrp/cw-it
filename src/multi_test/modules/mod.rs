pub mod unified_stargate;

#[cfg(not(feature = "coreum"))]
mod token_factory;
#[cfg(feature = "coreum")]
mod token_factory_coreum;

#[cfg(not(feature = "coreum"))]
pub use token_factory::TokenFactory;
#[cfg(feature = "coreum")]
pub use token_factory_coreum::TokenFactory;

pub const QUERY_ALL_BALANCES_PATH: &str = "/cosmos.bank.v1beta1.Query/AllBalances";
pub const QUERY_BALANCE_PATH: &str = "/cosmos.bank.v1beta1.Query/Balance";
pub const QUERY_SUPPLY_PATH: &str = "/cosmos.bank.v1beta1.Query/SupplyOf";
pub const QUERY_WASM_CONTRACT_SMART_PATH: &str = "/cosmwasm.wasm.v1.Query/SmartContractState";
pub const QUERY_WASM_CONTRACT_RAW_PATH: &str = "/cosmwasm.wasm.v1.Query/RawContractState";
pub const QUERY_WASM_CONTRACT_INFO_PATH: &str = "/cosmwasm.wasm.v1.Query/ContractInfo";
pub const QUERY_WASM_CODE_INFO_PATH: &str = "/cosmwasm.wasm.v1.Query/CodeInfo";
