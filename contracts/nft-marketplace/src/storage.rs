use soroban_sdk::{contracttype, Address, Env};

use shared::storage::{persistent_get, persistent_set};

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DataKey {
    PendingRefund(Address, Address),
    CollectionInfo(Address),
}

pub struct PendingRefunds;

impl PendingRefunds {
    pub fn get(env: &Env, bidder: &Address, token: &Address) -> i128 {
        persistent_get(env, &DataKey::PendingRefund(bidder.clone(), token.clone())).unwrap_or(0)
    }

    pub fn add(env: &Env, bidder: &Address, token: &Address, amount: i128) -> Result<i128, crate::MarketError> {
        let current = Self::get(env, bidder, token);
        let new_total = current.checked_add(amount).ok_or(crate::MarketError::Overflow)?;
        persistent_set(env, &DataKey::PendingRefund(bidder.clone(), token.clone()), &new_total);
        Ok(new_total)
    }

    pub fn clear(env: &Env, bidder: &Address, token: &Address) {
        persistent_set(env, &DataKey::PendingRefund(bidder.clone(), token.clone()), &0i128);
    }
}
