use std::str::FromStr;

use anyhow::{anyhow, bail, Result as AnyResult};
use coreum_wasm_sdk::types::coreum::asset::ft::v1::{
    MsgBurn, MsgIssue, MsgMint, QueryTokenRequest, QueryTokenResponse, QueryTokensRequest, QueryTokensResponse, Token,
};
use coreum_wasm_sdk::types::coreum::asset::nft::v1::{
    Class, ClassFeature, MsgBurn as MsgNftBurn, MsgIssueClass, MsgMint as MsgNftMint, QueryClassRequest, QueryClassResponse,
    QueryClassesRequest, QueryClassesResponse,
};
use coreum_wasm_sdk::types::cosmos::nft::v1beta1::{
    MsgSend as MsgNftSend, Nft, QueryNfTsRequest, QueryNfTsResponse, QueryNftRequest, QueryNftResponse, QueryOwnerRequest,
    QueryOwnerResponse,
};
use cosmwasm_schema::cw_serde;
use cosmwasm_std::{
    from_json, to_json_binary, Addr, Api, BankMsg, BankQuery, Binary, BlockInfo, Coin, CustomMsg, CustomQuery, Empty, Event, Querier,
    QueryRequest, Storage, SupplyResponse, Uint128,
};
use cw_multi_test::{AppResponse, BankSudo, CosmosRouter, Module, Stargate, StargateMsg, StargateQuery, SudoMsg};
use cw_storage_plus::{Item, Map};
use prost::Message;
use regex::Regex;
use serde::de::DeserializeOwned;
use coreum_wasm_sdk::{
  core::{CoreumMsg, CoreumQueries},
  nft,
};
use coreum_wasm_sdk::nft::{NFTResponse, NFTsResponse, OwnerResponse};
use coreum_wasm_sdk::pagination::PageRequest;

use crate::traits::{CREATE_TOKEN_FEE, DEFAULT_COIN_DENOM};

const DEFAULT_INIT: &str = constcat::concat!(CREATE_TOKEN_FEE, DEFAULT_COIN_DENOM);

/// Map of **denom -> MsgIssue definition**.
///
/// On Coreum the denom is typically `"{subunit}-{issuer}"`,
/// e.g. `ashare-core1xyz...`.
pub const ISSUED_TOKENS: Map<&str, MsgIssue> = Map::new("coreum_assetft/issued");

/// Map of **class_id -> MsgIssueClass definition**
pub const ISSUED_NFT_CLASSES: Map<&str, MsgIssueClass> = Map::new("coreum_assetnft/issued_classes");

/// Minimal stored NFT representation for cw-multi-test querying.
#[cw_serde]
pub struct StoredNft {
    pub class_id: String,
    pub id: String,
    pub owner: String,
    pub uri: String,
    pub data: Option<coreum_wasm_sdk::shim::Any>,
}

/// (class_id, nft_id) -> StoredNft
pub const MINTED_NFTS: Map<(&str, &str), StoredNft> = Map::new("coreum_assetnft/minted");

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

#[derive(Clone, Default)]
pub struct CoreumQueryModule;

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
        Self::new("", 32, 16, 59 + 16, DEFAULT_INIT)
    }
}

impl TokenFactory<'_> {
    /// Utility: build the Coreum FT denom from MsgIssue.
    ///
    /// This mirrors the chain behaviour: denom = `{subunit}-{issuer}`.
    fn issue_to_denom(msg: &MsgIssue) -> String {
        format!("{}-{}", msg.subunit, msg.issuer)
    }

    fn decode_issue(value: Binary) -> AnyResult<MsgIssue> {
        MsgIssue::try_from(value).map_err(|e| anyhow::anyhow!("failed to decode MsgIssue: {e}"))
    }

    fn decode_mint(value: Binary) -> AnyResult<MsgMint> {
        MsgMint::try_from(value).map_err(|e| anyhow::anyhow!("failed to decode MsgMint: {e}"))
    }

    fn decode_burn(value: Binary) -> AnyResult<MsgBurn> {
        MsgBurn::try_from(value).map_err(|e| anyhow::anyhow!("failed to decode MsgBurn: {e}"))
    }

    fn decode_query_token_req(data: &[u8]) -> AnyResult<QueryTokenRequest> {
        QueryTokenRequest::decode(data).map_err(|e| anyhow::anyhow!("failed to decode QueryTokenRequest: {e}"))
    }

    fn decode_query_tokens_req(data: &[u8]) -> AnyResult<QueryTokensRequest> {
        QueryTokensRequest::decode(data).map_err(|e| anyhow::anyhow!("failed to decode QueryTokensRequest: {e}"))
    }

    /// Convert an issued MsgIssue + denom into a Query `Token` struct.
    fn issue_to_token(denom: &str, issue: &MsgIssue) -> Token {
        Token {
            denom: denom.to_string(),
            issuer: issue.issuer.clone(),
            symbol: issue.symbol.clone(),
            subunit: issue.subunit.clone(),
            precision: issue.precision,
            description: issue.description.clone(),
            // we might add feature flags / booleans later:
            // burn_rate, send_commission_rate, etc...
            ..Token::default()
        }
    }

    /// Helper for minting via BankSudo into the cw-multi-test Bank module.
    fn bank_mint<ExecC, QueryC>(
        &self,
        api: &dyn Api,
        storage: &mut dyn Storage,
        router: &dyn CosmosRouter<ExecC = ExecC, QueryC = QueryC>,
        block: &BlockInfo,
        to: &str,
        coins: Vec<Coin>,
    ) -> AnyResult<AppResponse>
    where
        ExecC: CustomMsg + DeserializeOwned + 'static,
        QueryC: CustomQuery + DeserializeOwned + 'static,
    {
        let sudo = SudoMsg::Bank(BankSudo::Mint {
            to_address: to.to_string(),
            amount: coins,
        });
        let res = router.sudo(api, storage, block, sudo)?;
        Ok(res)
    }

    fn decode_issue_class(value: Binary) -> AnyResult<MsgIssueClass> {
        MsgIssueClass::try_from(value).map_err(|e| anyhow::anyhow!("failed to decode MsgIssueClass: {e}"))
    }

    fn decode_nft_mint(value: Binary) -> AnyResult<MsgNftMint> {
        MsgNftMint::try_from(value).map_err(|e| anyhow::anyhow!("failed to decode MsgMint (NFT): {e}"))
    }

    fn decode_nft_burn(value: Binary) -> AnyResult<MsgNftBurn> {
        MsgNftBurn::try_from(value).map_err(|e| anyhow::anyhow!("failed to decode MsgBurn (NFT): {e}"))
    }

    fn decode_query_class_req(data: &[u8]) -> AnyResult<QueryClassRequest> {
        QueryClassRequest::decode(data).map_err(|e| anyhow::anyhow!("failed to decode QueryClassRequest: {e}"))
    }

    fn decode_query_classes_req(data: &[u8]) -> AnyResult<QueryClassesRequest> {
        QueryClassesRequest::decode(data).map_err(|e| anyhow::anyhow!("failed to decode QueryClassesRequest: {e}"))
    }

    fn decode_query_nft_req(data: &[u8]) -> AnyResult<QueryNftRequest> {
        QueryNftRequest::decode(data).map_err(|e| anyhow::anyhow!("failed to decode QueryNftRequest: {e}"))
    }

    fn decode_query_nfts_req(data: &[u8]) -> AnyResult<QueryNfTsRequest> {
        QueryNfTsRequest::decode(data).map_err(|e| anyhow::anyhow!("failed to decode QueryNfTsRequest: {e}"))
    }

    fn decode_nft_send(value: Binary) -> AnyResult<MsgNftSend> {
        MsgNftSend::try_from(value).map_err(|e| anyhow::anyhow!("failed to decode MsgSend (NFT): {e}"))
    }

    fn decode_query_owner_req(data: &[u8]) -> AnyResult<QueryOwnerRequest> {
        QueryOwnerRequest::decode(data).map_err(|e| anyhow::anyhow!("failed to decode QueryOwnerRequest: {e}"))
    }

    /// Convert stored class definition into a Query `Class`.
    fn issue_class_to_class(class_id: &str, issue: &MsgIssueClass) -> Class {
        Class {
            id: class_id.to_string(),
            issuer: issue.issuer.clone(),
            name: issue.name.clone(),
            symbol: issue.symbol.clone(),
            description: issue.description.clone(),
            uri: issue.uri.clone(),
            uri_hash: issue.uri_hash.clone(),
            data: issue.data.clone(),
            features: issue.features.clone(),
            ..Class::default()
        }
    }

    /// Convert stored NFT into Query `Nft`.
    fn stored_to_nft(stored: &StoredNft) -> Nft {
        Nft {
            class_id: stored.class_id.clone(),
            id: stored.id.clone(),
            uri: stored.uri.clone(),
            uri_hash: "".to_string(),
            data: stored.data.clone(),
            ..Nft::default()
        }
    }

    fn issue<ExecC, QueryC>(
        &self,
        msg: &MsgIssue,
        api: &dyn Api,
        storage: &mut dyn Storage,
        router: &dyn CosmosRouter<ExecC = ExecC, QueryC = QueryC>,
        block: &BlockInfo,
        sender: Addr,
    ) -> AnyResult<AppResponse>
    where
        ExecC: cosmwasm_std::CustomMsg + serde::de::DeserializeOwned + 'static,
        QueryC: cosmwasm_std::CustomQuery + serde::de::DeserializeOwned + 'static,
    {
        // Validate subdenom length
        if msg.subunit.len() > self.max_subdenom_len {
            bail!("Subdenom length is too long, max length is {}", self.max_subdenom_len);
        }
        // Validate creator length
        if msg.issuer.len() > self.max_creator_len {
            bail!("Creator length is too long, max length is {}", self.max_creator_len);
        }
        // Validate creator address not contains '/'
        if msg.issuer.contains('/') {
            bail!("Invalid creator address, creator address cannot contains '/'");
        }
        // Validate sender is the creator
        if msg.issuer != sender.to_string() {
            bail!("Invalid creator address, creator address must be the same as the sender");
        }
        // Validate subdenom and symbol format
        let denom_re = Regex::new("^[a-zA-Z][a-zA-Z0-9/:._-]{2,127}$")?;
        if !denom_re.is_match(&msg.subunit) {
            bail!("subunit must match regex format '^[a-zA-Z][a-zA-Z0-9/:._-]{{2,127}}$': invalid input");
        }
        if !denom_re.is_match(&msg.symbol) {
            bail!("symbol must match regex format '^[a-zA-Z][a-zA-Z0-9/:._-]{{2,127}}$': invalid input");
        }

        let denom = Self::issue_to_denom(&msg);

        ISSUED_TOKENS.save(storage, denom.as_str(), &msg)?;

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

        let mut res = AppResponse::default();
        if !msg.initial_amount.is_empty() {
            let amount_u128: u128 = msg
                .initial_amount
                .parse()
                .map_err(|e| anyhow::anyhow!("invalid initial_amount `{}`: {e}", msg.initial_amount))?;

            if amount_u128 > 0 {
                let coin = Coin {
                    denom: denom.clone(),
                    amount: amount_u128.into(),
                };
                res = self.bank_mint::<ExecC, QueryC>(api, storage, router, block, &msg.issuer, vec![coin])?;
            }
        }

        res.events.push(
            Event::new("/coreum.asset.ft.v1.EventIssued")
                .add_attribute("denom", denom)
                .add_attribute("issuer", msg.issuer.clone()),
            // etc.
        );
        Ok(res)
    }

    pub fn mint<ExecC, QueryC>(
        &self,
        msg: &MsgMint,
        api: &dyn Api,
        storage: &mut dyn Storage,
        router: &dyn CosmosRouter<ExecC = ExecC, QueryC = QueryC>,
        block: &BlockInfo,
        sender: Addr,
    ) -> AnyResult<AppResponse>
    where
        ExecC: cosmwasm_std::CustomMsg + serde::de::DeserializeOwned + 'static,
        QueryC: cosmwasm_std::CustomQuery + serde::de::DeserializeOwned + 'static,
    {
        let Some(coin) = &msg.coin else {
            bail!("MsgMint.coin is None");
        };

        let denom = coin.denom.as_str();

        // Validate sender
        let parts = denom.split('-').collect::<Vec<_>>();
        if parts.len() != 2 {
            bail!("Invalid denom");
        }

        if parts[1] != sender.to_string() {
            bail!("Unauthorized mint. Not the issuer of the denom.");
        }
        if sender.to_string() != msg.sender {
            bail!("Invalid sender. Sender in msg must be same as sender of transaction.");
        }

        if ISSUED_TOKENS.may_load(storage, denom)?.is_none() {
            bail!("MsgMint for unknown Coreum FT denom `{}`", denom);
        }

        let amount_str = coin.amount.clone();
        let amount = Uint128::from_str(&amount_str)?;
        if amount.is_zero() {
            bail!("Invalid zero amount");
        }

        // Determine recipient
        let recipient = if msg.recipient.is_empty() {
            msg.sender.clone()
        } else {
            msg.recipient.clone()
        };

        // Mint through BankKeeper sudo method
        let mut res = self.bank_mint::<ExecC, QueryC>(
            api,
            storage,
            router,
            block,
            &msg.recipient,
            vec![Coin {
                denom: coin.denom.clone(),
                amount,
            }],
        )?;

        res.events.push(
            Event::new("tf_mint")
                .add_attribute("sender", msg.sender.clone())
                .add_attribute("recipient", recipient)
                .add_attribute("denom", denom)
                .add_attribute("amount", amount.to_string()),
        );
        Ok(res)
    }

    pub fn burn<ExecC, QueryC>(
        &self,
        msg: &MsgBurn,
        api: &dyn Api,
        storage: &mut dyn Storage,
        router: &dyn CosmosRouter<ExecC = ExecC, QueryC = QueryC>,
        block: &BlockInfo,
        sender: Addr,
    ) -> AnyResult<AppResponse>
    where
        ExecC: cosmwasm_std::CustomMsg + serde::de::DeserializeOwned + 'static,
        QueryC: cosmwasm_std::CustomQuery + serde::de::DeserializeOwned + 'static,
    {
        let Some(coin) = &msg.coin else {
            bail!("MsgBurn.coin is None");
        };

        let denom = coin.denom.as_str();
        let parts = denom.split('-').collect::<Vec<_>>();
        if parts.len() != 2 {
            bail!("Invalid denom");
        }

        if parts[1] != sender.to_string() {
            bail!("Unauthorized burn. Not the issuer of the denom.");
        }
        if sender.to_string() != msg.sender {
            bail!("Invalid sender. Sender in msg must be same as sender of transaction.");
        }

        if ISSUED_TOKENS.may_load(storage, denom)?.is_none() {
            bail!("MsgBurn for unknown Coreum FT denom `{}`", denom);
        }

        let amount_str = coin.amount.clone();
        let amount = Uint128::from_str(&amount_str)?;
        if amount.is_zero() {
            bail!("Invalid zero amount");
        }

        // Burn through BankKeeper
        let burn_msg = BankMsg::Burn {
            amount: vec![Coin {
                denom: denom.to_string(),
                amount,
            }],
        };
        let mut res = router.execute(api, storage, block, sender.clone(), burn_msg.into())?;
        res.events.push(
            Event::new("tf_burn")
                .add_attribute("burn_from_address", sender.to_string())
                .add_attribute("amount", amount.to_string()),
        );

        Ok(res)
    }

    fn issue_class<ExecC, QueryC>(
        &self,
        msg: &MsgIssueClass,
        _api: &dyn Api,
        storage: &mut dyn Storage,
        _router: &dyn CosmosRouter<ExecC = ExecC, QueryC = QueryC>,
        _block: &BlockInfo,
        sender: Addr,
    ) -> AnyResult<AppResponse>
    where
        ExecC: CustomMsg + DeserializeOwned + 'static,
        QueryC: CustomQuery + DeserializeOwned + 'static,
    {
        // Basic authorization: issuer must be tx sender
        if msg.issuer != sender.to_string() {
            bail!("Invalid issuer. issuer in msg must match sender.");
        }

        // Class id: Coreum uses `{symbol}-{issuer}`
        let class_id = format!("{}-{}", msg.symbol.to_lowercase(), msg.issuer);

        if ISSUED_NFT_CLASSES.may_load(storage, class_id.as_str())?.is_some() {
            bail!("NFT class already exists: {}", class_id);
        }

        ISSUED_NFT_CLASSES.save(storage, class_id.as_str(), msg)?;

        let mut res = AppResponse::default();
        res.events.push(
            Event::new("/coreum.asset.nft.v1.EventClassIssued")
                .add_attribute("class_id", class_id)
                .add_attribute("issuer", msg.issuer.clone()),
        );

        Ok(res)
    }

    pub fn nft_mint<ExecC, QueryC>(
        &self,
        msg: &MsgNftMint,
        _api: &dyn Api,
        storage: &mut dyn Storage,
        _router: &dyn CosmosRouter<ExecC = ExecC, QueryC = QueryC>,
        _block: &BlockInfo,
        sender: Addr,
    ) -> AnyResult<AppResponse>
    where
        ExecC: CustomMsg + DeserializeOwned + 'static,
        QueryC: CustomQuery + DeserializeOwned + 'static,
    {
        if msg.sender != sender.to_string() {
            bail!("Invalid sender. sender in msg must match tx sender.");
        }

        let class_id = msg.class_id.as_str();
        let nft_id = msg.id.as_str();

        let Some(class_issue) = ISSUED_NFT_CLASSES.may_load(storage, class_id)? else {
            bail!("MsgMint for unknown Coreum NFT class `{}`", class_id);
        };

        // Only issuer/admin can mint (simple model).
        if class_issue.issuer != sender.to_string() {
            bail!("Unauthorized mint. Not the issuer of class `{}`", class_id);
        }

        if MINTED_NFTS.may_load(storage, (class_id, nft_id))?.is_some() {
            bail!("NFT already minted: {}/{}", class_id, nft_id);
        }

        let owner = if msg.recipient.is_empty() {
            msg.sender.clone()
        } else {
            msg.recipient.clone()
        };

        println!("Minting NFT {}/{} to owner {}", class_id, nft_id, owner);

        let stored = StoredNft {
            class_id: msg.class_id.clone(),
            id: msg.id.clone(),
            owner: owner.clone(),
            uri: msg.uri.clone(),
            data: msg.data.clone(),
        };

        MINTED_NFTS.save(storage, (class_id, nft_id), &stored)?;

        let mut res = AppResponse::default();
        res.events.push(
            Event::new("/coreum.asset.nft.v1.EventMinted")
                .add_attribute("class_id", class_id.to_string())
                .add_attribute("id", nft_id.to_string())
                .add_attribute("owner", owner),
        );

        Ok(res)
    }

    pub fn nft_burn<ExecC, QueryC>(
        &self,
        msg: &MsgNftBurn,
        _api: &dyn Api,
        storage: &mut dyn Storage,
        _router: &dyn CosmosRouter<ExecC = ExecC, QueryC = QueryC>,
        _block: &BlockInfo,
        sender: Addr,
    ) -> AnyResult<AppResponse>
    where
        ExecC: CustomMsg + DeserializeOwned + 'static,
        QueryC: CustomQuery + DeserializeOwned + 'static,
    {
        if msg.sender != sender.to_string() {
            bail!("Invalid sender. sender in msg must match tx sender.");
        }

        let class_id = msg.class_id.as_str();
        let nft_id = msg.id.as_str();

        let Some(class) = ISSUED_NFT_CLASSES.may_load(storage, class_id)? else {
            bail!("Class id not found: {}", class_id);
        };

        let Some(stored) = MINTED_NFTS.may_load(storage, (class_id, nft_id))? else {
            bail!("NFT not found: {}/{}", class_id, nft_id);
        };

        // Simple burn policy: owner or issuer can burn
        let issuer_may_burn = class.features.contains(&(ClassFeature::Burning as i32));
        println!(
            "Burning NFT {}/{} owned by {}, sender {},  {}",
            class_id, nft_id, stored.owner, sender, issuer_may_burn
        );
        if stored.owner != sender.to_string() && !(issuer_may_burn && class.issuer == sender.to_string()) {
            bail!("Unauthorized burn. Only owner or issuer can burn {}/{}", class_id, nft_id);
        }

        MINTED_NFTS.remove(storage, (class_id, nft_id));

        let mut res = AppResponse::default();
        res.events.push(
            Event::new("/coreum.asset.nft.v1.EventBurned")
                .add_attribute("class_id", class_id.to_string())
                .add_attribute("id", nft_id.to_string())
                .add_attribute("owner", sender.to_string()),
        );

        Ok(res)
    }

    pub fn nft_send<ExecC, QueryC>(
        &self,
        msg: &MsgNftSend,
        _api: &dyn Api,
        storage: &mut dyn Storage,
        _router: &dyn CosmosRouter<ExecC = ExecC, QueryC = QueryC>,
        _block: &BlockInfo,
        sender: Addr,
    ) -> AnyResult<AppResponse>
    where
        ExecC: CustomMsg + DeserializeOwned + 'static,
        QueryC: CustomQuery + DeserializeOwned + 'static,
    {
        let class_id = msg.class_id.as_str();
        let nft_id = msg.id.as_str();

        let Some(class) = ISSUED_NFT_CLASSES.may_load(storage, class_id)? else {
            bail!("Class id not found: {}", class_id);
        };

        let Some(mut stored) = MINTED_NFTS.may_load(storage, (class_id, nft_id))? else {
            bail!("NFT not found: {}/{}", class_id, nft_id);
        };

        let is_soulbound = class.features.contains(&(ClassFeature::Soulbound as i32));

        if msg.sender != sender.to_string() && !(is_soulbound && class.issuer == sender.to_string()) {
            bail!("Invalid sender. sender in msg must match tx sender.");
        }

        // Transfer policy: only current owner can send
        if stored.owner != sender.to_string() && !(is_soulbound && class.issuer == sender.to_string()) {
            bail!("Unauthorized send. Only owner can send {}/{}", class_id, nft_id);
        }

        let to = msg.receiver.clone();
        if to.is_empty() {
            bail!("MsgSend.receiver is empty");
        }

        stored.owner = to.clone();
        MINTED_NFTS.save(storage, (class_id, nft_id), &stored)?;

        let mut res = AppResponse::default();
        res.events.push(
            Event::new("/coreum.asset.nft.v1.EventSent")
                .add_attribute("class_id", class_id.to_string())
                .add_attribute("id", nft_id.to_string())
                .add_attribute("sender", msg.sender.clone())
                .add_attribute("receiver", to),
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
            // --- FT ---
            MsgIssue::TYPE_URL => {
                let msg = Self::decode_issue(value)?;
                self.issue(&msg, api, storage, router, block, sender)
            }
            MsgMint::TYPE_URL => {
                let msg = Self::decode_mint(value)?;
                self.mint(&msg, api, storage, router, block, sender)
            }
            MsgBurn::TYPE_URL => {
                let msg = Self::decode_burn(value)?;
                self.burn(&msg, api, storage, router, block, sender)
            }
            // --- NFT ---
            MsgIssueClass::TYPE_URL => {
                let msg = Self::decode_issue_class(value)?;
                self.issue_class(&msg, api, storage, router, block, sender)
            }
            MsgNftMint::TYPE_URL => {
                let msg = Self::decode_nft_mint(value)?;
                self.nft_mint(&msg, api, storage, router, block, sender)
            }
            MsgNftBurn::TYPE_URL => {
                let msg = Self::decode_nft_burn(value)?;
                self.nft_burn(&msg, api, storage, router, block, sender)
            }
            MsgNftSend::TYPE_URL => {
                let msg = Self::decode_nft_send(value)?;
                self.nft_send(&msg, api, storage, router, block, sender)
            }
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
        _request: Self::QueryT,
    ) -> AnyResult<Binary> {
        bail!("Unsupported query type: Stargate queries are disabled");
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

impl Module for CoreumQueryModule {
    type ExecT = CoreumMsg;        // not used (you can pick Empty too)
    type QueryT = CoreumQueries;   // <-- THIS is what your contract uses
    type SudoT = cosmwasm_std::Empty;

    fn execute<ExecC, QueryC>(
        &self,
        _api: &dyn Api,
        _storage: &mut dyn Storage,
        _router: &dyn CosmosRouter<ExecC = ExecC, QueryC = QueryC>,
        _block: &BlockInfo,
        _sender: cosmwasm_std::Addr,
        _msg: Self::ExecT,
    ) -> AnyResult<AppResponse>
    where
        ExecC: CustomMsg + serde::de::DeserializeOwned + 'static,
        QueryC: CustomQuery + serde::de::DeserializeOwned + 'static,
    {
        bail!("CoreumQueryModule execute is not implemented")
    }

    fn query(
        &self,
        _api: &dyn Api,
        storage: &dyn Storage,
        _querier: &dyn Querier,
        _block: &BlockInfo,
        request: Self::QueryT,
    ) -> AnyResult<Binary> {
        match request {
            CoreumQueries::NFT(q) => match q {
                // This matches your contract query: CoreumQueries::NFT(nft::Query::NFT{..})
                nft::Query::NFT { class_id, id } => {
                    let Some(stored) = MINTED_NFTS.may_load(storage, (&class_id, &id))? else {
                        bail!("NFT not found for {}/{}", class_id, id);
                    };

                    // Map your StoredNft -> your wasm-side nft::NFTResponse
                    let resp = NFTResponse {
                        nft: nft::NFT {
                            class_id: stored.class_id,
                            id: stored.id,
                            uri: if stored.uri.is_empty() { None } else { Some(stored.uri) },
                            uri_hash: None,
                            data: stored.data.map(|any| {
                                cosmwasm_std::Binary::from(any.value)
                            }),
                        },
                    };

                    Ok(to_json_binary(&resp)?)
                }

                nft::Query::NFTs { class_id, owner, pagination } => {
                    let mut nfts: Vec<nft::NFT> = vec![];

                    // Scan all minted; filter locally
                    MINTED_NFTS
                        .range(storage, None, None, cosmwasm_std::Order::Ascending)
                        .for_each(|item| {
                            if let Ok(((cid, nid), stored)) = item {
                                if let Some(ref filter_class) = class_id {
                                    if cid != filter_class.as_str() {
                                        return;
                                    }
                                }
                                if let Some(ref filter_owner) = owner {
                                    if stored.owner != *filter_owner {
                                        return;
                                    }
                                }
                                let _ = nid; // keep for clarity
                                nfts.push(nft::NFT {
                                    class_id: stored.class_id,
                                    id: stored.id,
                                    uri: if stored.uri.is_empty() { None } else { Some(stored.uri) },
                                    uri_hash: None,
                                    data: stored.data.map(|any| cosmwasm_std::Binary::from(any.value)),
                                });
                            }
                        });

                    let resp = NFTsResponse {
                        nfts,
                        pagination: coreum_wasm_sdk::pagination::PageResponse {
                            next_key: None,
                            total: Some(0),
                        },
                    };

                    Ok(to_json_binary(&resp)?)
                }

                nft::Query::Owner { class_id, id } => {
                    let Some(stored) = MINTED_NFTS.may_load(storage, (&class_id, &id))? else {
                        bail!("NFT not found for {}/{}", class_id, id);
                    };

                    let resp = OwnerResponse { owner: stored.owner };

                    Ok(to_json_binary(&resp)?)
                }

                _ => bail!("Coreum NFT query not implemented: {:?}", q),
            },

            CoreumQueries::AssetNFT(q) => match q {
                coreum_wasm_sdk::assetnft::Query::Class { id } => {
                    let Some(issue) = ISSUED_NFT_CLASSES.may_load(storage, &id)? else {
                        bail!("NFT class not found for id `{}`", id);
                    };

                    let class = coreum_wasm_sdk::assetnft::Class {
                        id: id.clone(),
                        issuer: issue.issuer.clone(),
                        name: issue.name.clone(),
                        symbol: issue.symbol.clone(),
                        description: Some(issue.description.clone()),
                        uri: Some(issue.uri.clone()),
                        uri_hash: Some(issue.uri_hash.clone()),
                        features: Some(issue.features.iter().map(|&f| f as u32).collect()),
                        data: issue.data.clone().map(|d| Binary::from(d.value)),
                        royalty_rate: Some("0".to_string()),
                    };

                    let resp = coreum_wasm_sdk::assetnft::ClassResponse { class };

                    Ok(to_json_binary(&resp)?)
                }

                coreum_wasm_sdk::assetnft::Query::Classes { issuer, pagination } => {
                    let mut classes: Vec<coreum_wasm_sdk::assetnft::Class> = Vec::new();
                    ISSUED_NFT_CLASSES
                        .range(storage, None, None, cosmwasm_std::Order::Ascending)
                        .for_each(|item| {
                            if let Ok((class_id, issue)) = item {
                                // issuer is a String, not Option<String>
                                if !issuer.is_empty() && issue.issuer != issuer {
                                    return;
                                }
                                classes.push(coreum_wasm_sdk::assetnft::Class {
                                    id: class_id.clone(),
                                    issuer: issue.issuer.clone(),
                                    name: issue.name.clone(),
                                    symbol: issue.symbol.clone(),
                                    description: Some(issue.description.clone()),
                                    uri: Some(issue.uri.clone()),
                                    uri_hash: Some(issue.uri_hash.clone()),
                                    features: Some(issue.features.iter().map(|&f| f as u32).collect()),
                                    data: issue.data.clone().map(|d| Binary::from(d.value)),
                                    royalty_rate: Some("0".to_string()),
                                });
                            }
                        });

                    let resp = coreum_wasm_sdk::assetnft::ClassesResponse {
                        classes,
                        pagination: coreum_wasm_sdk::pagination::PageResponse {
                            next_key: None,
                            total: Some(0),
                        },
                    };

                    Ok(to_json_binary(&resp)?)
                }

                _ => bail!("Coreum AssetNFT query not implemented: {:?}", q),
            },

            CoreumQueries::AssetFT(q) => match q {
                coreum_wasm_sdk::assetft::Query::Token { denom } => {
                    // If the token was issued via TokenFactory, return that info.
                    // Otherwise, only return a default token for "ucore" (the native Coreum token).
                    let token = if let Some(issue) = ISSUED_TOKENS.may_load(storage, &denom)? {
                        coreum_wasm_sdk::assetft::Token {
                            denom: denom.clone(),
                            issuer: issue.issuer.clone(),
                            symbol: issue.symbol.clone(),
                            subunit: issue.subunit.clone(),
                            precision: issue.precision,
                            description: Some(issue.description.clone()),
                            globally_frozen: Some(false),
                            features: Some(vec![]),
                            burn_rate: "0".to_string(),
                            send_commission_rate: "0".to_string(),
                            version: 0,
                            uri: Some("".to_string()),
                            uri_hash: Some("".to_string()),
                            extension_cw_address: None,
                            admin: None,
                        }
                    } else if denom == DEFAULT_COIN_DENOM {
                        // Return a default token for the native chain token (ucore)
                        coreum_wasm_sdk::assetft::Token {
                            denom: denom.clone(),
                            issuer: "".to_string(),
                            symbol: "CORE".to_string(),
                            subunit: denom.clone(),
                            precision: 6,
                            description: Some("Native Coreum token".to_string()),
                            globally_frozen: Some(false),
                            features: Some(vec![]),
                            burn_rate: "0".to_string(),
                            send_commission_rate: "0".to_string(),
                            version: 0,
                            uri: Some("".to_string()),
                            uri_hash: Some("".to_string()),
                            extension_cw_address: None,
                            admin: None,
                        }
                    } else {
                        bail!("FT not found for denom `{}`", denom);
                    };

                    let resp = coreum_wasm_sdk::assetft::TokenResponse { token };

                    Ok(to_json_binary(&resp)?)
                }

                coreum_wasm_sdk::assetft::Query::Tokens { issuer, pagination } => {
                    let mut tokens: Vec<coreum_wasm_sdk::assetft::Token> = Vec::new();
                    ISSUED_TOKENS
                        .range(storage, None, None, cosmwasm_std::Order::Ascending)
                        .for_each(|item| {
                            if let Ok((denom, issue)) = item {
                                // issuer is a String, not Option<String>
                                if !issuer.is_empty() && issue.issuer != issuer {
                                    return;
                                }
                                tokens.push(coreum_wasm_sdk::assetft::Token {
                                    denom: denom.clone(),
                                    issuer: issue.issuer.clone(),
                                    symbol: issue.symbol.clone(),
                                    subunit: issue.subunit.clone(),
                                    precision: issue.precision,
                                    description: Some(issue.description.clone()),
                                    globally_frozen: Some(false),
                                    features: Some(vec![]),
                                    burn_rate: "0".to_string(),
                                    send_commission_rate: "0".to_string(),
                                    version: 0,
                                    uri: Some("".to_string()),
                                    uri_hash: Some("".to_string()),
                                    extension_cw_address: None,
                                    admin: None,
                                });
                            }
                        });

                    let resp = coreum_wasm_sdk::assetft::TokensResponse {
                        tokens,
                        pagination: coreum_wasm_sdk::pagination::PageResponse {
                            next_key: None,
                            total: Some(0),
                        },
                    };

                    Ok(to_json_binary(&resp)?)
                }

                _ => bail!("Coreum AssetFT query not implemented: {:?}", q),
            },

            _ => bail!("Coreum query not implemented: {:?}", request),
        }
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
        ExecC: CustomMsg + serde::de::DeserializeOwned + 'static,
        QueryC: CustomQuery + serde::de::DeserializeOwned + 'static,
    {
        Ok(AppResponse::default())
    }
}

fn coin_from_sdk_string(sdk_string: &str) -> AnyResult<Coin> {
    let denom_re = Regex::new(r"^[0-9]+[a-z]+$")?;
    let denom_re2 = Regex::new(r"^([0-9]+)([a-z0-9]+)-([A-Za-z0-9]+)$")?;
    let ibc_re = Regex::new(r"^[0-9]+(ibc|IBC)/[0-9A-F]{64}$")?;
    let factory_re = Regex::new(r"^[0-9]+factory/[0-9a-z]+/[0-9a-zA-Z]+$")?;

    if !(denom_re.is_match(sdk_string) || denom_re2.is_match(sdk_string) || ibc_re.is_match(sdk_string) || factory_re.is_match(sdk_string))
    {
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
    use cosmwasm_std::{BalanceResponse, CosmosMsg};
    use cw_multi_test::{BasicAppBuilder, Executor};
    use test_case::test_case;

    const TOKEN_FACTORY: TokenFactory<'static> = TokenFactory::new("factory", 32, 16, 59 + 16, DEFAULT_INIT);

    #[test_case(Addr::unchecked("sender"), "subdenom", &[DEFAULT_INIT]; "valid denom")]
    #[test_case(Addr::unchecked("sen/der"), "subdenom", &[DEFAULT_INIT] => panics "creator address cannot contains" ; "invalid creator address")]
    #[test_case(Addr::unchecked("asdasdasdasdasdasdasdasdasdasdasdasdasdasdasd"), "subdenom", &[DEFAULT_INIT] => panics ; "creator address too long")]
    #[test_case(Addr::unchecked("sender"), "subdenom", &[DEFAULT_INIT, "100subdenom-sender"] => panics "Subdenom already exists" ; "denom exists")]
    fn create_denom(sender: Addr, subdenom: &str, initial_coins: &[&str]) {
        let initial_coins = initial_coins.iter().map(|s| coin_from_sdk_string(s).unwrap()).collect::<Vec<_>>();

        let stargate = TOKEN_FACTORY.clone();

        let mut app = BasicAppBuilder::<Empty, Empty>::new()
            .with_stargate(stargate)
            .build(|router, _, storage| {
                router.bank.init_balance(storage, &sender, initial_coins).unwrap();
            });

        let msg = CosmosMsg::<Empty>::Stargate {
            type_url: MsgIssue::TYPE_URL.to_string(),
            value: MsgIssue {
                issuer: sender.to_string(),
                subunit: subdenom.to_string(),
                symbol: subdenom.to_uppercase(),
                ..MsgIssue::default()
            }
            .into(),
        };

        let res = app.execute(sender.clone(), msg).unwrap();
        res.assert_event(
            &Event::new("/coreum.asset.ft.v1.EventIssued")
                .add_attribute("issuer", sender.to_string())
                .add_attribute("denom", format!("{}-{}", subdenom, sender)),
        );
    }

    #[test_case(false, Addr::unchecked("sender"), Addr::unchecked("sender"), 1000u128 => panics "MsgMint for unknown Coreum FT denom `subdenom-sender`" ; "mint without issue")]
    #[test_case(true, Addr::unchecked("sender"), Addr::unchecked("sender"), 1000u128 ; "valid mint")]
    #[test_case(true, Addr::unchecked("sender"), Addr::unchecked("sender"), 0u128 => panics "Invalid zero amount" ; "zero amount")]
    #[test_case(true, Addr::unchecked("sender"), Addr::unchecked("creator"), 1000u128 => panics "Unauthorized mint. Not the issuer of the denom." ; "sender is not creator")]
    fn mint(issue: bool, sender: Addr, creator: Addr, mint_amount: u128) {
        let stargate = TOKEN_FACTORY.clone();

        let mut app = BasicAppBuilder::<Empty, Empty>::new()
            .with_stargate(stargate)
            .build(|router, _, storage| {
                router
                    .bank
                    .init_balance(storage, &sender, [coin_from_sdk_string(DEFAULT_INIT).unwrap()].to_vec())
                    .unwrap();
            });

        if issue {
            let msg = CosmosMsg::<Empty>::Stargate {
                type_url: MsgIssue::TYPE_URL.to_string(),
                value: MsgIssue {
                    issuer: sender.to_string(),
                    subunit: "subdenom".to_string(),
                    symbol: "SUBDENOM".to_string(),
                    ..MsgIssue::default()
                }
                .into(),
            };
            let res = app.execute(sender.clone(), msg).unwrap();
        }

        let msg = CosmosMsg::<Empty>::Stargate {
            type_url: MsgMint::TYPE_URL.to_string(),
            value: MsgMint {
                sender: sender.to_string(),
                coin: Some(
                    Coin {
                        denom: format!("{}-{}", "subdenom", creator),
                        amount: Uint128::from(mint_amount),
                    }
                    .into(),
                ),
                recipient: sender.to_string(),
            }
            .into(),
        };

        let res = app.execute(sender.clone(), msg).unwrap();

        // Assert event
        res.assert_event(
            &Event::new("tf_mint")
                .add_attribute("recipient", sender.to_string())
                .add_attribute("amount", mint_amount.to_string()),
        );

        // Query bank balance
        let balance_query = BankQuery::Balance {
            address: sender.to_string(),
            denom: format!("{}-{}", "subdenom", creator),
        };
        let balance = app.wrap().query::<BalanceResponse>(&balance_query.into()).unwrap().amount.amount;
        assert_eq!(balance, Uint128::from(mint_amount));
    }

    #[test_case(Addr::unchecked("sender"), Addr::unchecked("sender"), 1000u128, 1000u128 ; "valid burn")]
    #[test_case(Addr::unchecked("sender"), Addr::unchecked("sender"), 1000u128, 2000u128 ; "valid burn 2")]
    #[test_case(Addr::unchecked("sender"), Addr::unchecked("creator"), 1000u128, 1000u128 => panics "Unauthorized burn. Not the issuer of the denom." ; "sender is not creator")]
    #[test_case(Addr::unchecked("sender"), Addr::unchecked("sender"), 0u128, 1000u128 => panics "Invalid zero amount" ; "zero amount")]
    #[test_case(Addr::unchecked("sender"), Addr::unchecked("sender"), 2000u128, 1000u128 => panics "Cannot Sub" ; "insufficient funds")]
    fn burn(sender: Addr, creator: Addr, burn_amount: u128, initial_balance: u128) {
        let stargate = TOKEN_FACTORY.clone();

        let tf_denom = format!("{}-{}", "subdenom", creator);

        let mut app = BasicAppBuilder::<Empty, Empty>::new()
            .with_stargate(stargate)
            .build(|router, _, storage| {
                router
                    .bank
                    .init_balance(storage, &sender, [coin_from_sdk_string(DEFAULT_INIT).unwrap()].to_vec())
                    .unwrap();
            });

        let msg = CosmosMsg::<Empty>::Stargate {
            type_url: MsgIssue::TYPE_URL.to_string(),
            value: MsgIssue {
                issuer: sender.to_string(),
                subunit: "subdenom".to_string(),
                symbol: "SUBDENOM".to_string(),
                initial_amount: initial_balance.to_string(),
                ..MsgIssue::default()
            }
            .into(),
        };
        let res = app.execute(sender.clone(), msg).unwrap();

        let msg = CosmosMsg::<Empty>::Stargate {
            type_url: MsgBurn::TYPE_URL.to_string(),
            value: MsgBurn {
                sender: sender.to_string(),
                coin: Some(
                    Coin {
                        denom: tf_denom.clone(),
                        amount: Uint128::from(burn_amount),
                    }
                    .into(),
                ),
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

    #[test]
    #[cfg(not(feature = "coreum"))]
    fn nft_flow_issue_mint_send_burn() {
        use cosmwasm_std::{CosmosMsg, Empty};
        use cw_multi_test::{BasicAppBuilder, Executor};

        let stargate = TOKEN_FACTORY.clone();
        let sender = Addr::unchecked("sender");
        let receiver = Addr::unchecked("receiver");

        let mut app = BasicAppBuilder::<Empty, Empty>::new()
            .with_stargate(stargate)
            .build(|router, _, storage| {
                router
                    .bank
                    .init_balance(storage, &sender, vec![coin_from_sdk_string(DEFAULT_INIT).unwrap()])
                    .unwrap();
            });

        // 1) Issue class
        let issue_class = CosmosMsg::<Empty>::Stargate {
            type_url: MsgIssueClass::TYPE_URL.to_string(),
            value: MsgIssueClass {
                issuer: sender.to_string(),
                name: "My NFT Class".to_string(),
                symbol: "NFTCLASS".to_string(),
                description: "test".to_string(),
                uri: "ipfs://class".to_string(),
                ..MsgIssueClass::default()
            }
            .into(),
        };

        let res = app.execute(sender.clone(), issue_class).unwrap();
        res.assert_event(
            &Event::new("/coreum.asset.nft.v1.EventClassIssued")
                .add_attribute("issuer", sender.to_string())
                .add_attribute("class_id", "nftclass-sender".to_string()),
        );

        // 2) Mint NFT
        let mint = CosmosMsg::<Empty>::Stargate {
            type_url: MsgNftMint::TYPE_URL.to_string(),
            value: MsgNftMint {
                sender: sender.to_string(),
                class_id: "nftclass-sender".to_string(),
                id: "nft1".to_string(),
                uri: "ipfs://nft1".to_string(),
                recipient: sender.to_string(), // if your struct uses `recipient`
                ..MsgNftMint::default()
            }
            .into(),
        };

        let res = app.execute(sender.clone(), mint).unwrap();
        res.assert_event(
            &Event::new("/coreum.asset.nft.v1.EventMinted")
                .add_attribute("class_id", "nftclass-sender".to_string())
                .add_attribute("id", "nft1".to_string())
                .add_attribute("owner", sender.to_string()),
        );

        // 3) Send NFT to receiver (transfer)
        let send = CosmosMsg::<Empty>::Stargate {
            type_url: MsgNftSend::TYPE_URL.to_string(),
            value: MsgNftSend {
                sender: sender.to_string(),
                class_id: "nftclass-sender".to_string(),
                id: "nft1".to_string(),
                receiver: receiver.to_string(), // if your struct uses `receiver`
                ..MsgNftSend::default()
            }
            .into(),
        };

        let res = app.execute(sender.clone(), send).unwrap();
        res.assert_event(
            &Event::new("/coreum.asset.nft.v1.EventSent")
                .add_attribute("class_id", "nftclass-sender".to_string())
                .add_attribute("id", "nft1".to_string())
                .add_attribute("sender", sender.to_string())
                .add_attribute("receiver", receiver.to_string()),
        );

        // 4) Query NFT, owner and confirm it still exists
        let q = QueryRequest::Stargate {
            path: "/coreum.asset.nft.v1.Query/NFTs".to_string(),
            data: {
                // NOTE: adjust fields if your QueryNfTsRequest differs
                let req = QueryNfTsRequest {
                    class_id: "nftclass-sender".to_string(),
                    owner: receiver.to_string(),
                    pagination: None,
                };
                let mut buf = Vec::new();
                req.encode(&mut buf).unwrap();
                Binary::from(buf)
            },
        };

        let resp = app.wrap().query::<QueryNfTsResponse>(&q).unwrap();
        assert_eq!(resp.nfts.len(), 1);
        assert_eq!(resp.nfts[0].class_id, "nftclass-sender");
        assert_eq!(resp.nfts[0].id, "nft1");

        let q = QueryRequest::Stargate {
            path: "/coreum.asset.nft.v1.Query/Owner".to_string(),
            data: {
                let req = QueryOwnerRequest {
                    class_id: "nftclass-sender".to_string(),
                    id: "nft1".to_string(),
                };
                let mut buf = Vec::new();
                req.encode(&mut buf).unwrap();
                Binary::from(buf)
            },
        };

        let resp = app.wrap().query::<QueryOwnerResponse>(&q).unwrap();
        assert_eq!(resp.owner, receiver.to_string());

        // 5) Burn: must be done by current owner in our model
        let burn = CosmosMsg::<Empty>::Stargate {
            type_url: MsgNftBurn::TYPE_URL.to_string(),
            value: MsgNftBurn {
                sender: receiver.to_string(),
                class_id: "nftclass-sender".to_string(),
                id: "nft1".to_string(),
                ..MsgNftBurn::default()
            }
            .into(),
        };

        let res = app.execute(receiver.clone(), burn).unwrap();
        res.assert_event(
            &Event::new("/coreum.asset.nft.v1.EventBurned")
                .add_attribute("class_id", "nftclass-sender".to_string())
                .add_attribute("id", "nft1".to_string())
                .add_attribute("owner", receiver.to_string()),
        );

        // Query again -> none
        let q = QueryRequest::Stargate {
            path: "/coreum.asset.nft.v1.Query/NFTs".to_string(),
            data: {
                let req = QueryNfTsRequest {
                    class_id: "nftclass-sender".to_string(),
                    owner: receiver.to_string(),
                    pagination: None,
                };
                let mut buf = Vec::new();
                req.encode(&mut buf).unwrap();
                Binary::from(buf)
            },
        };

        let resp = app.wrap().query::<QueryNfTsResponse>(&q).unwrap();
        assert_eq!(resp.nfts.len(), 0);
    }

    #[test]
    #[cfg(feature = "coreum")]
    fn nft_flow_issue_mint_send_burn_coreum() {
        use cw_multi_test::{BasicAppBuilder, Executor};

        let stargate = TOKEN_FACTORY.clone();
        let sender = Addr::unchecked("sender");
        let receiver = Addr::unchecked("receiver");

        let mut app = BasicAppBuilder::<CoreumMsg, CoreumQueries>::new_custom()
            .with_stargate(stargate)
            .with_custom(CoreumQueryModule::default())
            .build(|router, _, storage| {
                router
                    .bank
                    .init_balance(storage, &sender, vec![coin_from_sdk_string(DEFAULT_INIT).unwrap()])
                    .unwrap();
            });

        // 1) Issue class
        let issue_class = CosmosMsg::<CoreumMsg>::Stargate {
            type_url: MsgIssueClass::TYPE_URL.to_string(),
            value: MsgIssueClass {
                issuer: sender.to_string(),
                name: "My NFT Class".to_string(),
                symbol: "NFTCLASS".to_string(),
                description: "test".to_string(),
                uri: "ipfs://class".to_string(),
                ..MsgIssueClass::default()
            }
            .into(),
        };

        let res = app.execute(sender.clone(), issue_class).unwrap();
        res.assert_event(
            &Event::new("/coreum.asset.nft.v1.EventClassIssued")
                .add_attribute("issuer", sender.to_string())
                .add_attribute("class_id", "nftclass-sender".to_string()),
        );

        // 2) Mint NFT
        let mint = CosmosMsg::<CoreumMsg>::Stargate {
            type_url: MsgNftMint::TYPE_URL.to_string(),
            value: MsgNftMint {
                sender: sender.to_string(),
                class_id: "nftclass-sender".to_string(),
                id: "nft1".to_string(),
                uri: "ipfs://nft1".to_string(),
                recipient: sender.to_string(),
                ..MsgNftMint::default()
            }
            .into(),
        };

        let res = app.execute(sender.clone(), mint).unwrap();
        res.assert_event(
            &Event::new("/coreum.asset.nft.v1.EventMinted")
                .add_attribute("class_id", "nftclass-sender".to_string())
                .add_attribute("id", "nft1".to_string())
                .add_attribute("owner", sender.to_string()),
        );

        // 3) Send NFT to receiver (transfer)
        let send = CosmosMsg::<CoreumMsg>::Stargate {
            type_url: MsgNftSend::TYPE_URL.to_string(),
            value: MsgNftSend {
                sender: sender.to_string(),
                class_id: "nftclass-sender".to_string(),
                id: "nft1".to_string(),
                receiver: receiver.to_string(),
                ..MsgNftSend::default()
            }
            .into(),
        };

        let res = app.execute(sender.clone(), send).unwrap();
        res.assert_event(
            &Event::new("/coreum.asset.nft.v1.EventSent")
                .add_attribute("class_id", "nftclass-sender".to_string())
                .add_attribute("id", "nft1".to_string())
                .add_attribute("sender", sender.to_string())
                .add_attribute("receiver", receiver.to_string()),
        );

        // 4) Query NFT using CoreumQueries
        let resp = app
            .wrap()
            .query::<NFTsResponse>(&QueryRequest::Custom(CoreumQueries::NFT(nft::Query::NFTs {
                class_id: Some("nftclass-sender".to_string()),
                owner: Some(receiver.to_string()),
                pagination: None,
            })))
            .unwrap();
        assert_eq!(resp.nfts.len(), 1);
        assert_eq!(resp.nfts[0].class_id, "nftclass-sender");
        assert_eq!(resp.nfts[0].id, "nft1");

        // Query owner
        let resp = app
            .wrap()
            .query::<OwnerResponse>(&QueryRequest::Custom(CoreumQueries::NFT(nft::Query::Owner {
                class_id: "nftclass-sender".to_string(),
                id: "nft1".to_string(),
            })))
            .unwrap();
        assert_eq!(resp.owner, receiver.to_string());

        // 5) Burn: must be done by current owner in our model
        let burn = CosmosMsg::<CoreumMsg>::Stargate {
            type_url: MsgNftBurn::TYPE_URL.to_string(),
            value: MsgNftBurn {
                sender: receiver.to_string(),
                class_id: "nftclass-sender".to_string(),
                id: "nft1".to_string(),
                ..MsgNftBurn::default()
            }
            .into(),
        };

        let res = app.execute(receiver.clone(), burn).unwrap();
        res.assert_event(
            &Event::new("/coreum.asset.nft.v1.EventBurned")
                .add_attribute("class_id", "nftclass-sender".to_string())
                .add_attribute("id", "nft1".to_string())
                .add_attribute("owner", receiver.to_string()),
        );

        // Query again -> none
        let resp = app
            .wrap()
            .query::<NFTsResponse>(&QueryRequest::Custom(CoreumQueries::NFT(nft::Query::NFTs {
                class_id: Some("nftclass-sender".to_string()),
                owner: Some(receiver.to_string()),
                pagination: None,
            })))
            .unwrap();
        assert_eq!(resp.nfts.len(), 0);
    }
}
