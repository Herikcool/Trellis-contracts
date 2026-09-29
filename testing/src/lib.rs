//! Testing & Simulation Module for Soroban contracts
//!
//! Provides comprehensive test harnesses, mocks, fuzzing helpers, and simulation tools
//! for all contracts in the trellis-contracts repository.

#![no_std]

pub mod examples;
pub mod fault_injection;
pub mod fuzzing;
pub mod helpers;
pub mod lifecycle_events;
pub mod migration;
pub mod mocks;
pub mod sandbox;
pub mod simulation;
pub mod snapshot;
pub mod upgrade;
#[cfg(test)]
mod amount_properties;

pub use fuzzing::*;
pub use helpers::*;
pub use lifecycle_events::*;
pub use migration::*;
pub use mocks::*;
pub use sandbox::*;
pub use simulation::*;
pub use snapshot::*;
pub use upgrade::*;
