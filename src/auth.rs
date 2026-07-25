//! Gateway authentication boundary for seren-router.
//!
//! M2 implements constant-time validation of `SEREN_ROUTER_GATEWAY_KEY` here.
//! The generic template's SerenCore identity dependencies are intentionally not
//! carried into this single-caller service.
