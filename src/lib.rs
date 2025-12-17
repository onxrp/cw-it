pub mod artifact;
pub mod const_coin;
pub mod error;
pub mod helpers;
pub mod robot;
pub mod traits;

/// This module contains the implementation of the `CwItRunner` trait for the `OsmosisTestApp` struct.
#[cfg(feature = "osmosis-test-tube")]
pub mod osmosis_test_app;
#[cfg(feature = "osmosis-test-tube")]
pub use osmosis_test_app::WhitelistForceUnlock;

/// This module contains the implementation of the `CwItRunner` trait for the `OsmosisTestApp` struct.
#[cfg(feature = "coreum-test-tube")]
pub mod coreum_test_app;

#[cfg(feature = "multi-test")]
#[cfg(test)]
mod test_helpers;

#[cfg(feature = "multi-test")]
pub mod multi_test;

#[cfg(feature = "rpc-runner")]
#[cfg_attr(docsrs, doc(cfg(feature = "rpc-runner")))]
pub mod rpc_runner;

#[cfg(feature = "osmosis")]
#[cfg_attr(docsrs, doc(cfg(feature = "osmosis")))]
pub mod osmosis;

#[cfg(feature = "astroport")]
#[cfg_attr(docsrs, doc(cfg(feature = "astroport")))]
pub mod astroport;

// We apply these attributes to this module since we get warnings when no features have been selected
#[allow(unused_variables)]
#[allow(dead_code)]
pub mod test_runner;

pub use artifact::*;
pub use test_runner::OwnedTestRunner;
pub use test_runner::TestRunner;

// Re-exports for convenience
pub use cosmrs;
pub use osmosis_std;
pub use test_tube;

#[cfg(feature = "osmosis-test-tube")]
pub use osmosis_test_tube;

#[cfg(feature = "coreum-test-tube")]
pub use coreum_test_tube;

#[cfg(feature = "multi-test")]
pub use cw_multi_test as cw_multi_test;

// When multi-test is ON, this trait *includes* Stargate
#[cfg(feature = "multi-test")]
pub trait MultiTestStargateBound: cw_multi_test::Stargate + 'static {}
#[cfg(feature = "multi-test")]
impl<T> MultiTestStargateBound for T where T: cw_multi_test::Stargate + 'static {}
// When multi-test is OFF, it's just a 'static marker with no cw-multi-test dependency
#[cfg(not(feature = "multi-test"))]
pub trait MultiTestStargateBound: 'static {}

#[cfg(not(feature = "multi-test"))]
impl<T> MultiTestStargateBound for T where T: 'static {}
