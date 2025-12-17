use anyhow::Result as AnyResult;
use cosmwasm_std::{Addr, Api, StdError, Storage};
use cw_multi_test::AddressGenerator;

#[derive(Clone)]
pub struct MockAddressGenerator;

impl AddressGenerator for MockAddressGenerator {
    fn contract_address(&self, api: &dyn Api, _storage: &mut dyn Storage, code_id: u64, instance_id: u64) -> AnyResult<Addr> {
        // Same basic pattern the old generator used:
        let raw = format!("contract_{}_{}", code_id, instance_id);
        api.addr_validate(&raw)
            .map_err(|e| StdError::generic_err(format!("invalid generated addr: {}", e)))?;

        Ok(Addr::unchecked(raw))
    }
}
