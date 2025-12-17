use anyhow::{anyhow, Result as AnyResult};
use osmosis_std::types::cosmos::base::v1beta1::Coin as ProtoCoin;
use osmosis_std::types::cosmos::bank::v1beta1::{
    QueryAllBalancesRequest, QueryAllBalancesResponse, QueryBalanceRequest, QueryBalanceResponse, QuerySupplyOfRequest,
    QuerySupplyOfResponse,
};

use cosmwasm_std::{
    from_json, to_json_binary, Addr, Api, BankQuery, Binary, BlockInfo, ContractResult, Empty, Querier, QuerierWrapper, QueryRequest,
    Storage, SystemResult, WasmQuery,
};
use cw_multi_test::{AppResponse, CosmosRouter, Module, Stargate, StargateFailingModule, StargateMsg, StargateQuery};
use osmosis_std::types::cosmwasm::wasm::v1::{
    ContractInfo, QueryContractInfoRequest, QueryContractInfoResponse, QuerySmartContractStateRequest, QuerySmartContractStateResponse,
};
use prost::Message;
use serde::de::DeserializeOwned;

use crate::multi_test::modules::{
    QUERY_ALL_BALANCES_PATH, QUERY_BALANCE_PATH, QUERY_SUPPLY_PATH, QUERY_WASM_CONTRACT_INFO_PATH, QUERY_WASM_CONTRACT_SMART_PATH,
};

pub struct UnifiedStargate<Stargate = StargateFailingModule> {
    pub extra: Option<Stargate>,
}

impl<StargateT> UnifiedStargate<StargateT>
where
    StargateT: Stargate,
{
    pub fn new_without_extra() -> Self {
        Self { extra: None }
    }

    pub fn new_with_extra(extra: StargateT) -> Self {
        Self { extra: Some(extra) }
    }
}

impl<StargateT> Module for UnifiedStargate<StargateT>
where
    StargateT: Stargate,
{
    type ExecT = StargateMsg;
    type QueryT = StargateQuery;
    type SudoT = Empty;

    fn execute<ExecC, QueryC>(
        &self,
        api: &dyn Api,
        storage: &mut dyn Storage,
        router: &dyn CosmosRouter<ExecC = ExecC, QueryC = QueryC>,
        block: &BlockInfo,
        sender: Addr,
        msg: Self::ExecT,
    ) -> AnyResult<AppResponse>
    where
        ExecC: cosmwasm_std::CustomMsg + DeserializeOwned + 'static,
        QueryC: cosmwasm_std::CustomQuery + DeserializeOwned + 'static,
    {
        if let Some(extra) = &self.extra {
            extra.execute(api, storage, router, block, sender, msg)
        } else {
            // or: Ok(AppResponse::default())
            Err(anyhow::anyhow!(format!("No stargate exec handler for {}", msg.type_url)))
        }
    }

    fn sudo<ExecC, QueryC>(
        &self,
        api: &dyn Api,
        storage: &mut dyn Storage,
        router: &dyn CosmosRouter<ExecC = ExecC, QueryC = QueryC>,
        block: &BlockInfo,
        msg: Self::SudoT,
    ) -> AnyResult<AppResponse>
    where
        ExecC: cosmwasm_std::CustomMsg + DeserializeOwned + 'static,
        QueryC: cosmwasm_std::CustomQuery + DeserializeOwned + 'static,
    {
        if let Some(extra) = &self.extra {
            extra.sudo(api, storage, router, block, msg)
        } else {
            Ok(AppResponse::default())
        }
    }

    fn query(
        &self,
        api: &dyn Api,
        storage: &dyn Storage,
        querier: &dyn Querier,
        block: &BlockInfo,
        request: Self::QueryT,
    ) -> AnyResult<Binary> {
        let path = request.path.as_str();
        let data = request.data.as_slice();
        let wrapper: QuerierWrapper<Empty> = QuerierWrapper::new(querier);

        match path {
            // Bank queries
            QUERY_ALL_BALANCES_PATH => {
                let req = QueryAllBalancesRequest::decode(data).map_err(|e| cosmwasm_std::StdError::generic_err(e.to_string()))?;
                let cw_resp: cosmwasm_std::AllBalanceResponse = wrapper.query(&QueryRequest::Bank(BankQuery::AllBalances {
                    address: req.address.clone(),
                }))?;

                let proto_resp = QueryAllBalancesResponse {
                    balances: cw_resp
                        .amount
                        .into_iter()
                        .map(|c| ProtoCoin {
                            denom: c.denom,
                            amount: c.amount.to_string(),
                        })
                        .collect(),
                    pagination: None,
                };

                Ok(to_json_binary(&proto_resp)?)
            }
            QUERY_BALANCE_PATH => {
                let req = QueryBalanceRequest::decode(data).map_err(|e| cosmwasm_std::StdError::generic_err(e.to_string()))?;
                let cw_resp: cosmwasm_std::BalanceResponse = wrapper.query(&QueryRequest::Bank(BankQuery::Balance {
                    address: req.address,
                    denom: req.denom,
                }))?;

                let proto_resp = QueryBalanceResponse {
                    balance: Some(ProtoCoin {
                        denom: cw_resp.amount.denom,
                        amount: cw_resp.amount.amount.to_string(),
                    }),
                };

                Ok(to_json_binary(&proto_resp)?)
            }
            QUERY_SUPPLY_PATH => {
                let req = QuerySupplyOfRequest::decode(data).map_err(|e| cosmwasm_std::StdError::generic_err(e.to_string()))?;
                let cw_resp: cosmwasm_std::SupplyResponse = wrapper.query(&QueryRequest::Bank(BankQuery::Supply { denom: req.denom }))?;

                let proto_resp = QuerySupplyOfResponse {
                    amount: Some(ProtoCoin {
                        denom: cw_resp.amount.denom,
                        amount: cw_resp.amount.amount.to_string(),
                    }),
                };

                Ok(to_json_binary(&proto_resp)?)
            }
            QUERY_WASM_CONTRACT_SMART_PATH => {
                let req = QuerySmartContractStateRequest::decode(data).map_err(|e| cosmwasm_std::StdError::generic_err(e.to_string()))?;

                let cw_request: QueryRequest<Empty> = QueryRequest::Wasm(WasmQuery::Smart {
                    contract_addr: req.address.clone(),
                    msg: req.query_data.clone().into(),
                });

                let raw_res = querier.raw_query(&to_json_binary(&cw_request)?);

                let cw_bin: Binary = match raw_res {
                    SystemResult::Ok(ContractResult::Ok(bin)) => bin,
                    SystemResult::Ok(ContractResult::Err(err)) => {
                        return Err(anyhow!(err.to_string()));
                    }
                    SystemResult::Err(sys_err) => {
                        return Err(anyhow!(sys_err.to_string()));
                    }
                };

                let proto_resp = QuerySmartContractStateResponse { data: cw_bin.to_vec() };

                Ok(to_json_binary(&proto_resp)?)
            }
            QUERY_WASM_CONTRACT_INFO_PATH => {
                let req = QueryContractInfoRequest::decode(data).map_err(|e| cosmwasm_std::StdError::generic_err(e.to_string()))?;

                let cw_resp: cosmwasm_std::ContractInfoResponse = wrapper.query(&QueryRequest::Wasm(WasmQuery::ContractInfo {
                    contract_addr: req.address.clone(),
                }))?;

                let proto_info = ContractInfo {
                    code_id: cw_resp.code_id,
                    creator: cw_resp.creator,
                    admin: cw_resp.admin.unwrap_or_default(),
                    label: "".to_string(),
                    created: None,
                    ibc_port_id: cw_resp.ibc_port.unwrap_or_default(),
                    extension: None,
                };

                let proto_resp = QueryContractInfoResponse {
                    address: req.address.clone(),
                    contract_info: Some(proto_info),
                };

                Ok(to_json_binary(&proto_resp)?)
            }
            _ => {
                if let Some(extra) = &self.extra {
                    extra.query(api, storage, querier, block, request)
                } else {
                    Err(anyhow!("Unexpected stargate query: path={}, data={:?}", path, request.data))
                }
            }
        }
    }
}

impl<StargateT> Stargate for UnifiedStargate<StargateT> where StargateT: Stargate {}
