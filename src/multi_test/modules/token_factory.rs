use std::str::FromStr;

use anyhow::{anyhow, bail, Result as AnyResult};
use cosmwasm_std::{
    from_json, Addr, Api, BankMsg, BankQuery, Binary, BlockInfo, Coin, Empty, Event, Querier, QueryRequest, Storage, SupplyResponse,
    Uint128,
};
use osmosis_std::types::osmosis::tokenfactory::v1beta1::{
    MsgBurn, MsgBurnResponse, MsgCreateDenom, MsgCreateDenomResponse, MsgMint, MsgMintResponse,
};
use regex::Regex;

use cw_multi_test::{AppResponse, BankSudo, CosmosRouter, Executor, Module, Stargate, StargateMsg, StargateQuery};

use crate::traits::DEFAULT_COIN_DENOM;

const DEFAULT_INIT: &str = constcat::concat!("10000000", DEFAULT_COIN_DENOM);

/// This is a struct that implements the [`cw_multi_test::Stargate`] trait to
/// mimic the behavior of the Osmosis TokenFactory module.
#[derive(Clone)]
pub struct TokenFactory<'a> {
    pub module_denom_prefix: &'a str,
    pub max_subdenom_len: usize,
    pub max_hrp_len: usize,
    pub max_creator_len: usize,
    pub denom_creation_fee: &'a str,
}

impl<'a> TokenFactory<'a> {
    /// Creates a new TokenFactory instance with the given parameters.
    pub const fn new(
        prefix: &'a str,
        max_subdenom_len: usize,
        max_hrp_len: usize,
        max_creator_len: usize,
        denom_creation_fee: &'a str,
    ) -> Self {
        Self {
            module_denom_prefix: prefix,
            max_subdenom_len,
            max_hrp_len,
            max_creator_len,
            denom_creation_fee,
        }
    }
}

impl Default for TokenFactory<'_> {
    fn default() -> Self {
        Self::new("factory", 32, 16, 59 + 16, DEFAULT_INIT)
    }
}

impl TokenFactory<'_> {
    fn create_denom<ExecC, QueryC>(
        &self,
        api: &dyn Api,
        storage: &mut dyn Storage,
        router: &dyn CosmosRouter<ExecC = ExecC, QueryC = QueryC>,
        block: &BlockInfo,
        sender: Addr,
        value: Binary,
    ) -> AnyResult<AppResponse>
    where
        ExecC: cosmwasm_std::CustomMsg + serde::de::DeserializeOwned + 'static,
        QueryC: cosmwasm_std::CustomQuery + serde::de::DeserializeOwned + 'static,
    {
        let msg: MsgCreateDenom = value.try_into()?;

        // Validate subdenom length
        if msg.subdenom.len() > self.max_subdenom_len {
            bail!("Subdenom length is too long, max length is {}", self.max_subdenom_len);
        }
        // Validate creator length
        if msg.sender.len() > self.max_creator_len {
            bail!("Creator length is too long, max length is {}", self.max_creator_len);
        }
        // Validate creator address not contains '/'
        if msg.sender.contains('/') {
            bail!("Invalid creator address, creator address cannot contains '/'");
        }
        // Validate sender is the creator
        if msg.sender != sender.to_string() {
            bail!("Invalid creator address, creator address must be the same as the sender");
        }

        let denom = format!("{}/{}/{}", self.module_denom_prefix, msg.sender, msg.subdenom);

        // Query supply of denom
        let request = QueryRequest::Bank(BankQuery::Supply { denom: denom.clone() });
        let raw = router.query(api, storage, block, request)?;
        let supply: SupplyResponse = from_json(raw)?;
        if !supply.amount.amount.is_zero() {
            bail!("Subdenom already exists");
        }

        // Charge denom creation fee
        let fee = coin_from_sdk_string(self.denom_creation_fee)?;
        let fee_msg = BankMsg::Burn { amount: vec![fee] };
        router.execute(api, storage, block, sender, fee_msg.into())?;

        let create_denom_response = MsgCreateDenomResponse {
            new_token_denom: denom.clone(),
        };

        let mut res = AppResponse::default();
        res.events.push(
            Event::new("create_denom")
                .add_attribute("creator", msg.sender)
                .add_attribute("new_token_denom", denom),
        );
        res.data = Some(create_denom_response.into());

        Ok(res)
    }

    pub fn mint<ExecC, QueryC>(
        &self,
        api: &dyn Api,
        storage: &mut dyn Storage,
        router: &dyn CosmosRouter<ExecC = ExecC, QueryC = QueryC>,
        block: &BlockInfo,
        sender: Addr,
        value: Binary,
    ) -> AnyResult<AppResponse>
    where
        ExecC: cosmwasm_std::CustomMsg + serde::de::DeserializeOwned + 'static,
        QueryC: cosmwasm_std::CustomQuery + serde::de::DeserializeOwned + 'static,
    {
        let msg: MsgMint = value.try_into()?;

        let denom = msg.amount.clone().ok_or_else(|| anyhow!("missing amount"))?.denom;

        // Validate sender
        let parts = denom.split('/').collect::<Vec<_>>();
        if parts.len() != 3 && parts[0] != self.module_denom_prefix {
            bail!("Invalid denom");
        }

        if parts[1] != sender.to_string() {
            bail!("Unauthorized mint. Not the creator of the denom.");
        }
        if sender.to_string() != msg.sender {
            bail!("Invalid sender. Sender in msg must be same as sender of transaction.");
        }

        let amount_str = msg.amount.as_ref().ok_or_else(|| anyhow!("missing amount"))?.amount.clone();
        let amount = Uint128::from_str(&amount_str)?;
        if amount.is_zero() {
            bail!("Invalid zero amount");
        }

        // Determine recipient
        let recipient = if msg.mint_to_address.is_empty() {
            msg.sender.clone()
        } else {
            msg.mint_to_address.clone()
        };

        // Mint through BankKeeper sudo method
        let mint_msg = BankSudo::Mint {
            to_address: recipient.clone(),
            amount: vec![Coin {
                denom: denom.clone(),
                amount,
            }],
        };
        router.sudo(api, storage, block, mint_msg.into())?;

        let mut res = AppResponse::default();
        let data = MsgMintResponse {};
        res.data = Some(data.into());
        res.events.push(
            Event::new("tf_mint")
                .add_attribute("sender", msg.sender)
                .add_attribute("mint_to_address", msg.mint_to_address)
                .add_attribute("recipient", recipient)
                .add_attribute("denom", denom)
                .add_attribute("amount", amount.to_string()),
        );
        Ok(res)
    }

    pub fn burn<ExecC, QueryC>(
        &self,
        api: &dyn Api,
        storage: &mut dyn Storage,
        router: &dyn CosmosRouter<ExecC = ExecC, QueryC = QueryC>,
        block: &BlockInfo,
        sender: Addr,
        value: Binary,
    ) -> AnyResult<AppResponse>
    where
        ExecC: cosmwasm_std::CustomMsg + serde::de::DeserializeOwned + 'static,
        QueryC: cosmwasm_std::CustomQuery + serde::de::DeserializeOwned + 'static,
    {
        let msg: MsgBurn = value.try_into()?;

        let denom = msg.amount.clone().ok_or_else(|| anyhow!("missing amount"))?.denom;

        let parts = denom.split('/').collect::<Vec<_>>();
        if parts.len() != 3 && parts[0] != self.module_denom_prefix {
            bail!("Invalid denom");
        }

        if parts[1] != sender.to_string() {
            bail!("Unauthorized burn. Not the creator of the denom.");
        }
        if sender.to_string() != msg.sender {
            bail!("Invalid sender. Sender in msg must be same as sender of transaction.");
        }

        let amount_str = msg.amount.as_ref().ok_or_else(|| anyhow!("missing amount"))?.amount.clone();
        let amount = Uint128::from_str(&amount_str)?;
        if amount.is_zero() {
            bail!("Invalid zero amount");
        }

        // Burn through BankKeeper
        let burn_msg = BankMsg::Burn {
            amount: vec![Coin {
                denom: denom.clone(),
                amount,
            }],
        };
        router.execute(api, storage, block, sender.clone(), burn_msg.into())?;

        let mut res = AppResponse::default();
        let data = MsgBurnResponse {};
        res.data = Some(data.into());

        res.events.push(
            Event::new("tf_burn")
                .add_attribute("burn_from_address", sender.to_string())
                .add_attribute("amount", amount.to_string()),
        );

        Ok(res)
    }

    /// Shared internal handler for `CosmosMsg::Stargate`.
    fn handle_any<ExecC, QueryC>(
        &self,
        api: &dyn Api,
        storage: &mut dyn Storage,
        router: &dyn CosmosRouter<ExecC = ExecC, QueryC = QueryC>,
        block: &BlockInfo,
        sender: Addr,
        type_url: String,
        value: Binary,
    ) -> AnyResult<AppResponse>
    where
        ExecC: cosmwasm_std::CustomMsg + serde::de::DeserializeOwned + 'static,
        QueryC: cosmwasm_std::CustomQuery + serde::de::DeserializeOwned + 'static,
    {
        match type_url.as_str() {
            MsgCreateDenom::TYPE_URL => self.create_denom(api, storage, router, block, sender, value),
            MsgMint::TYPE_URL => self.mint(api, storage, router, block, sender, value),
            MsgBurn::TYPE_URL => self.burn(api, storage, router, block, sender, value),
            _ => bail!("Unknown message type {}", type_url),
        }
    }
}

// Implement the generic Module interface using StargateMsg/StargateQuery.
impl<'a> Module for TokenFactory<'a> {
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
        ExecC: cosmwasm_std::CustomMsg + serde::de::DeserializeOwned + 'static,
        QueryC: cosmwasm_std::CustomQuery + serde::de::DeserializeOwned + 'static,
    {
        let StargateMsg { type_url, value, .. } = msg;

        self.handle_any(api, storage, router, block, sender, type_url, value)
            .map_err(|e| anyhow!(e.to_string()))
    }

    fn query(
        &self,
        _api: &dyn Api,
        _storage: &dyn Storage,
        _querier: &dyn Querier,
        _block: &BlockInfo,
        request: Self::QueryT,
    ) -> AnyResult<Binary> {
        Err(anyhow!("Unexpected stargate query: path={}, data={:?}", request.path, request.data))
    }

    fn sudo<ExecC, QueryC>(
        &self,
        _api: &dyn Api,
        _storage: &mut dyn Storage,
        _router: &dyn CosmosRouter<ExecC = ExecC, QueryC = QueryC>,
        _block: &BlockInfo,
        _msg: Self::SudoT,
    ) -> AnyResult<AppResponse>
    where
        ExecC: cosmwasm_std::CustomMsg + serde::de::DeserializeOwned + 'static,
        QueryC: cosmwasm_std::CustomQuery + serde::de::DeserializeOwned + 'static,
    {
        // TokenFactory doesn't use sudo.
        Ok(AppResponse::default())
    }
}

// Mark it as a Stargate module
impl<'a> Stargate for TokenFactory<'a> {}

fn coin_from_sdk_string(sdk_string: &str) -> AnyResult<Coin> {
    let denom_re = Regex::new(r"^[0-9]+[a-z]+$")?;
    let ibc_re = Regex::new(r"^[0-9]+(ibc|IBC)/[0-9A-F]{64}$")?;
    let factory_re = Regex::new(r"^[0-9]+factory/[0-9a-z]+/[0-9a-zA-Z]+$")?;

    if !(denom_re.is_match(sdk_string) || ibc_re.is_match(sdk_string) || factory_re.is_match(sdk_string)) {
        bail!("Invalid sdk string");
    }

    // Parse amount
    let re = Regex::new(r"[0-9]+")?;
    let amount = re.find(sdk_string).unwrap().as_str();
    let amount = Uint128::from_str(amount)?;

    // The denom is the rest of the string
    let denom = sdk_string[amount.to_string().len()..].to_string();

    Ok(Coin { denom, amount })
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosmwasm_std::{BalanceResponse, Binary as StdBinary, CosmosMsg};
    use cw_multi_test::{BasicAppBuilder, Executor};
    use test_case::test_case;

    const TOKEN_FACTORY: TokenFactory<'static> = TokenFactory::new("factory", 32, 16, 59 + 16, DEFAULT_INIT);

    #[test_case(Addr::unchecked("sender"), "subdenom", &[DEFAULT_INIT]; "valid denom")]
    #[test_case(Addr::unchecked("sen/der"), "subdenom", &[DEFAULT_INIT] => panics "creator address cannot contains" ; "invalid creator address")]
    #[test_case(Addr::unchecked("asdasdasdasdasdasdasdasdasdasdasdasdasdasdasd"), "subdenom", &[DEFAULT_INIT] => panics ; "creator address too long")]
    #[test_case(Addr::unchecked("sender"), "subdenom", &[DEFAULT_INIT, "100factory/sender/subdenom"] => panics "Subdenom already exists" ; "denom exists")]
    #[test_case(Addr::unchecked("sender"), "subdenom", &[constcat::concat!("100000", DEFAULT_COIN_DENOM)] => panics "Cannot Sub" ; "insufficient funds for fee")]
    fn create_denom(sender: Addr, subdenom: &str, initial_coins: &[&str]) {
        let initial_coins = initial_coins.iter().map(|s| coin_from_sdk_string(s).unwrap()).collect::<Vec<_>>();

        let stargate = TOKEN_FACTORY.clone();

        let mut app = BasicAppBuilder::<Empty, Empty>::new()
            .with_stargate(stargate)
            .build(|router, _, storage| {
                router.bank.init_balance(storage, &sender, initial_coins).unwrap();
            });

        let msg = CosmosMsg::<Empty>::Stargate {
            type_url: MsgCreateDenom::TYPE_URL.to_string(),
            value: MsgCreateDenom {
                sender: sender.to_string(),
                subdenom: subdenom.to_string(),
            }
            .into(),
        };

        let res = app.execute(sender.clone(), msg).unwrap();

        res.assert_event(
            &Event::new("create_denom")
                .add_attribute("creator", sender.to_string())
                .add_attribute(
                    "new_token_denom",
                    format!("{}/{}/{}", TOKEN_FACTORY.module_denom_prefix, sender, subdenom),
                ),
        );

        assert_eq!(
            res.data.unwrap(),
            StdBinary::from(MsgCreateDenomResponse {
                new_token_denom: format!("{}/{}/{}", TOKEN_FACTORY.module_denom_prefix, sender, subdenom)
            })
        );
    }

    #[test_case(Addr::unchecked("sender"), Addr::unchecked("sender"), 1000u128 ; "valid mint")]
    #[test_case(Addr::unchecked("sender"), Addr::unchecked("sender"), 0u128 => panics "Invalid zero amount" ; "zero amount")]
    #[test_case(Addr::unchecked("sender"), Addr::unchecked("creator"), 1000u128 => panics "Unauthorized mint. Not the creator of the denom." ; "sender is not creator")]
    fn mint(sender: Addr, creator: Addr, mint_amount: u128) {
        let stargate = TOKEN_FACTORY.clone();

        let mut app = BasicAppBuilder::<Empty, Empty>::new().with_stargate(stargate).build(|_, _, _| {});

        let msg = CosmosMsg::<Empty>::Stargate {
            type_url: MsgMint::TYPE_URL.to_string(),
            value: MsgMint {
                sender: sender.to_string(),
                amount: Some(
                    osmosis_std::types::cosmos::base::v1beta1::Coin {
                        denom: format!("{}/{}/{}", TOKEN_FACTORY.module_denom_prefix, creator, "subdenom"),
                        amount: Uint128::from(mint_amount).to_string(),
                    }
                    .into(),
                ),
                mint_to_address: sender.to_string(),
            }
            .into(),
        };

        let res = app.execute(sender.clone(), msg).unwrap();

        // Assert event
        res.assert_event(
            &Event::new("tf_mint")
                .add_attribute("mint_to_address", sender.to_string())
                .add_attribute("amount", mint_amount.to_string()),
        );

        // Query bank balance
        let balance_query = BankQuery::Balance {
            address: sender.to_string(),
            denom: format!("{}/{}/{}", TOKEN_FACTORY.module_denom_prefix, creator, "subdenom"),
        };
        let balance = app.wrap().query::<BalanceResponse>(&balance_query.into()).unwrap().amount.amount;
        assert_eq!(balance, Uint128::from(mint_amount));
    }

    #[test_case(Addr::unchecked("sender"), Addr::unchecked("sender"), 1000u128, 1000u128 ; "valid burn")]
    #[test_case(Addr::unchecked("sender"), Addr::unchecked("sender"), 1000u128, 2000u128 ; "valid burn 2")]
    #[test_case(Addr::unchecked("sender"), Addr::unchecked("creator"), 1000u128, 1000u128 => panics "Unauthorized burn. Not the creator of the denom." ; "sender is not creator")]
    #[test_case(Addr::unchecked("sender"), Addr::unchecked("sender"), 0u128, 1000u128 => panics "Invalid zero amount" ; "zero amount")]
    #[test_case(Addr::unchecked("sender"), Addr::unchecked("sender"), 2000u128, 1000u128 => panics "Cannot Sub" ; "insufficient funds")]
    fn burn(sender: Addr, creator: Addr, burn_amount: u128, initial_balance: u128) {
        let stargate = TOKEN_FACTORY.clone();

        let tf_denom = format!("{}/{}/{}", TOKEN_FACTORY.module_denom_prefix, creator, "subdenom");

        let mut app = BasicAppBuilder::<Empty, Empty>::new()
            .with_stargate(stargate)
            .build(|router, _, storage| {
                router
                    .bank
                    .init_balance(
                        storage,
                        &sender,
                        vec![Coin {
                            denom: tf_denom.clone(),
                            amount: Uint128::from(initial_balance),
                        }],
                    )
                    .unwrap();
            });

        let msg = CosmosMsg::<Empty>::Stargate {
            type_url: MsgBurn::TYPE_URL.to_string(),
            value: MsgBurn {
                sender: sender.to_string(),
                amount: Some(
                    osmosis_std::types::cosmos::base::v1beta1::Coin {
                        denom: tf_denom.clone(),
                        amount: Uint128::from(burn_amount).to_string(),
                    }
                    .into(),
                ),
                burn_from_address: sender.to_string(),
            }
            .into(),
        };

        let res = app.execute(sender.clone(), msg).unwrap();

        // Assert event
        res.assert_event(
            &Event::new("tf_burn")
                .add_attribute("burn_from_address", sender.to_string())
                .add_attribute("amount", burn_amount.to_string()),
        );

        // Query bank balance
        let balance_query = BankQuery::Balance {
            address: sender.to_string(),
            denom: tf_denom,
        };
        let balance = app.wrap().query::<BalanceResponse>(&balance_query.into()).unwrap().amount.amount;
        assert_eq!(balance.u128(), initial_balance - burn_amount);
    }

    #[test_case(DEFAULT_COIN_DENOM ; "native denom")]
    #[test_case("IBC/27394FB092D2ECCD56123C74F36E4C1F926001CEADA9CA97EA622B25F41E5EB2" ; "ibc denom")]
    #[test_case("IBC/27394FB092D2ECCD56123CA622B25F41E5EB2" => panics "Invalid sdk string" ; "invalid ibc denom")]
    #[test_case("IB/27394FB092D2ECCD56123C74F36E4C1F926001CEADA9CA97EA622B25F41E5EB2" => panics "Invalid sdk string" ; "invalid ibc denom 2")]
    #[test_case("factory/sender/subdenom" ; "token factory denom")]
    #[test_case("factory/se1298der/subde192MAnom" ; "token factory denom 2")]
    #[test_case("factor/sender/subdenom" => panics "Invalid sdk string" ; "invalid token factory denom")]
    #[test_case("factory/sender/subdenom/extra" => panics "Invalid sdk string" ; "invalid token factory denom 2")]
    fn test_coin_from_sdk_string(denom: &str) {
        let sdk_string = format!("{}{}", 1000, denom);
        let coin = coin_from_sdk_string(&sdk_string).unwrap();
        assert_eq!(coin.denom, denom);
        assert_eq!(coin.amount, Uint128::from(1000u128));
    }
}
