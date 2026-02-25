#[cfg(not(feature = "coreum"))]
#[macro_export]
macro_rules! create_contract_wrappers {
    ( $( $name:expr ),* ) => {{
        use std::collections::HashMap;
        use cw_multi_test::{ContractWrapper, Contract};
        use cosmwasm_std::Empty;
        vec![
            $(
                {

                    paste::paste! {
                      use[<$name>]::contract::{execute, instantiate, query};
                    }
                    ($name.to_string(), Box::new(ContractWrapper::new_with_empty(
                        execute,
                        instantiate,
                        query,
                    )) as Box<dyn Contract<Empty, Empty>>)
                }
            ),*
        ].into_iter().collect::<HashMap<String,Box<dyn Contract<Empty, Empty>>>>()
    }};
}

#[cfg(feature = "coreum")]
#[macro_export]
macro_rules! create_contract_wrappers {
    ( $( $name:expr ),* ) => {{
        use std::collections::HashMap;
        use cw_multi_test::{ContractWrapper, Contract};
        use coreum_wasm_sdk::core::{CoreumMsg, CoreumQueries};
        vec![
            $(
                {

                    paste::paste! {
                      use[<$name>]::contract::{execute, instantiate, query};
                    }
                    ($name.to_string(), Box::new(ContractWrapper::<_, _, _, _, _, _, CoreumMsg, CoreumQueries>::new_with_empty(
                        execute,
                        instantiate,
                        query,
                    )) as Box<dyn Contract<CoreumMsg, CoreumQueries>>)
                }
            ),*
        ].into_iter().collect::<HashMap<String,Box<dyn Contract<CoreumMsg, CoreumQueries>>>>()
    }};
}

#[cfg(not(feature = "coreum"))]
#[macro_export]
macro_rules! create_contract_wrappers_with_reply {
    ( $( $name:expr ),* ) => {{
        use std::collections::HashMap;
        use cw_multi_test::{ContractWrapper, Contract};
        use cosmwasm_std::Empty;
        vec![
            $(
                {

                    paste::paste! {
                      use[<$name>]::contract::{execute, instantiate, query, reply};
                    }
                    ($name.to_string(), Box::new(ContractWrapper::new_with_empty(
                        execute,
                        instantiate,
                        query,
                    ).with_reply(reply)) as Box<dyn Contract<Empty, Empty>>)
                }
            ),*
        ].into_iter().collect::<HashMap<String,Box<dyn Contract<Empty, Empty>>>>()
    }};
}

#[cfg(feature = "coreum")]
#[macro_export]
macro_rules! create_contract_wrappers_with_reply {
    ( $( $name:expr ),* ) => {{
        use std::collections::HashMap;
        use cw_multi_test::{ContractWrapper, Contract};
        use coreum_wasm_sdk::core::{CoreumMsg, CoreumQueries};
        vec![
            $(
                {

                    paste::paste! {
                      use[<$name>]::contract::{execute, instantiate, query, reply};
                    }
                    ($name.to_string(), Box::new(ContractWrapper::<_, _, _, _, _, _, CoreumMsg, CoreumQueries>::new_with_empty(
                        execute,
                        instantiate,
                        query,
                    ).with_reply_empty(reply)) as Box<dyn Contract<CoreumMsg, CoreumQueries>>)
                }
            ),*
        ].into_iter().collect::<HashMap<String,Box<dyn Contract<CoreumMsg, CoreumQueries>>>>()
    }};
}

#[cfg(feature = "astroport")]
#[cfg(test)]
mod tests {
    #[test]
    fn test_create_contract_wrappers_macro() {
        let contract_wrappers =
            create_contract_wrappers!("astroport_factory", "astroport-pair-stable");

        assert_eq!(contract_wrappers.len(), 2);
    }

    #[test]
    fn test_create_contract_wrappers_with_reply_macro() {
        let contract_wrappers =
            create_contract_wrappers_with_reply!("astroport_factory", "astroport-pair-stable");

        assert_eq!(contract_wrappers.len(), 2);
    }
}
