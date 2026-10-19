//! # NFT Marketplace — Advanced NFT Trading and Auction Platform
//!
//! A comprehensive Soroban contract for P2P NFT trading, auctions, royalty
//! enforcement, and collection management with integrated payment settlement
//! across multiple currencies.
//!
//! ## Features
//!
//! - **Fixed-price listings** with configurable currency support
//! - **English auctions** with bid increments, auto-extension, and hammer-down
//! - **Dutch auctions** with time-based price decay
//! - **Royalty enforcement** at transaction level with configurable splits
//! - **Collection management** with IPFS metadata pinning
//! - **Offer-based trading** with time-limited make/accept offers
//! - **Bulk listing/trading** for efficient collection management
//! - **Escrow** for payments with release on NFT transfer confirmation
//! - **Marketplace fees** with governance-controlled splits

#![no_std]
#![allow(clippy::too_many_arguments)]

extern crate alloc;

#[cfg(test)]
extern crate std;

use soroban_sdk::{
    contract, contractimpl, contracttype, symbol_short, token, Address, Bytes, BytesN, Env, Map, Symbol,
    Vec,
};

pub mod storage;

use shared::auth;
use shared::events;
use shared::storage::{
    instance_get, instance_set, persistent_get, persistent_has, persistent_read, persistent_set,
};
use shared::utils::{is_expired, now};
use storage::{DataKey, PendingRefunds};

// ===========================================================================
// Marketplace-local error codes
// ===========================================================================

/// Marketplace-specific error codes (2000–2023).
#[soroban_sdk::contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum MarketError {
    /// The NFT listing was not found.
    ListingNotFound = 2000,
    /// The NFT listing has already been sold or cancelled.
    ListingAlreadySold = 2001,
    /// The caller is not the owner of this NFT.
    NotOwner = 2002,
    /// The collection is already registered.
    CollectionAlreadyRegistered = 2003,
    /// The collection was not found.
    CollectionNotFound = 2004,
    /// The currency address is not whitelisted.
    CurrencyNotWhitelisted = 2005,
    /// The metadata hash is invalid (wrong length).
    InvalidMetadataHash = 2006,
    /// The royalty basis-point rate is out of range.
    InvalidRoyaltyRate = 2007,
    /// The auto-extension window is invalid.
    InvalidExtensionWindow = 2008,
    /// The auction has already ended.
    AuctionEnded = 2009,
    /// The bid amount is below the current minimum bid.
    BidTooLow = 2010,
    /// The caller is the current highest bidder and cannot be outbid by themselves.
    BidderIsCurrentHighest = 2011,
    /// The auction settlement period has not yet ended.
    AuctionNotYetEnded = 2012,
    /// The offer has expired.
    OfferExpired = 2013,
    /// The offer has already been accepted or cancelled.
    OfferAlreadySettled = 2014,
    /// The caller is not the offer recipient and cannot accept.
    NotOfferRecipient = 2015,
    /// The Dutch auction price has reached zero.
    DutchAuctionPriceZero = 2016,
    /// The Dutch auction floor price must be less than start price.
    InvalidDutchAuctionPrices = 2017,
    /// The supplied monetary amount is invalid.
    InvalidAmount = 2018,
    /// An arithmetic operation would overflow.
    Overflow = 2019,
    /// The contract is paused.
    ContractPaused = 2020,
    /// Caller is not authorised.
    Unauthorized = 2021,
    /// Invalid argument supplied.
    InvalidArgument = 2022,
    /// The contract has already been initialized.
    AlreadyInitialized = 2023,
}

/// Result type alias for marketplace operations.
pub type MktResult<T> = core::result::Result<T, MarketError>;

// ===========================================================================
// Constants
// ===========================================================================

const BPS_DENOM: i128 = 10_000;
const DEFAULT_PLATFORM_FEE_BPS: i128 = 250;
const DEFAULT_BID_INCREMENT_BPS: i128 = 500;
const DEFAULT_AUTO_EXTENSION_SECONDS: u64 = 300;
const MAX_ROYALTY_BPS: i128 = 1_000;
const MAX_FEE_BPS: i128 = 1_000;
const MAX_ROYALTY_RECIPIENTS: u32 = 10;
const MAX_BULK_SIZE: u32 = 50;
const METADATA_HASH_LEN: u32 = 32;

// ===========================================================================
// Storage key symbols
// ===========================================================================

const KEY_PLATFORM_FEE_BPS: Symbol = symbol_short!("plat_fee");
const KEY_FEE_RECIPIENT: Symbol = symbol_short!("fee_rcp");
const KEY_BID_INCREMENT_BPS: Symbol = symbol_short!("bid_inc");
const KEY_AUTO_EXT: Symbol = symbol_short!("auto_ext");
const KEY_CURRENCIES: Symbol = symbol_short!("currenc");
const KEY_LISTING_SEQ: Symbol = symbol_short!("list_seq");
const KEY_AUCTION_SEQ: Symbol = symbol_short!("auc_seq");
const KEY_OFFER_SEQ: Symbol = symbol_short!("off_seq");
const KEY_ORACLE: Symbol = symbol_short!("oracle");

// ===========================================================================
// Types
// ===========================================================================

#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TokenStandard {
    ERC721,
    ERC1155,
}

#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ListingStatus {
    Active,
    Sold,
    Cancelled,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Listing {
    pub id: u64,
    pub seller: Address,
    pub collection: Address,
    pub token_id: u64,
    pub amount: u64,
    pub price: i128,
    pub currency: Address,
    pub status: ListingStatus,
    pub created_at: u64,
    pub metadata_hash: Bytes,
}

#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuctionType {
    English,
    Dutch,
}

#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuctionStatus {
    Active,
    Ended,
    Settled,
    Cancelled,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Auction {
    pub id: u64,
    pub auction_type: AuctionType,
    pub seller: Address,
    pub collection: Address,
    pub token_id: u64,
    pub amount: u64,
    pub currency: Address,
    pub start_price: i128,
    pub floor_price: i128,
    pub current_bid: i128,
    pub current_bidder: Address,
    pub status: AuctionStatus,
    pub start_time: u64,
    pub end_time: u64,
    pub auto_extension_window: u64,
    pub metadata_hash: Bytes,
}

#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OfferStatus {
    Pending,
    Accepted,
    Cancelled,
    Expired,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Offer {
    pub id: u64,
    pub offerer: Address,
    pub recipient: Address,
    pub collection: Address,
    pub token_id: u64,
    pub amount: u64,
    pub offer_amount: i128,
    pub currency: Address,
    pub offer_nft_collection: Address,
    pub offer_nft_token_id: u64,
    pub status: OfferStatus,
    pub created_at: u64,
    pub expires_at: u64,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RoyaltyRecipient {
    pub address: Address,
    pub share_bps: i128,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RoyaltyConfig {
    pub recipients: Vec<RoyaltyRecipient>,
    pub total_bps: i128,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CollectionInfo {
    pub address: Address,
    pub admin: Address,
    pub name: Symbol,
    pub ipfs_uri: Bytes,
    pub metadata_hash: Bytes,
    pub standard: TokenStandard,
    pub royalty_config: RoyaltyConfig,
    pub registered_at: u64,
    pub active_listings: u32,
    pub active_auctions: u32,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BulkListingItem {
    pub collection: Address,
    pub token_id: u64,
    pub amount: u64,
    pub price: i128,
    pub currency: Address,
    pub metadata_hash: Bytes,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BulkAuctionItem {
    pub collection: Address,
    pub token_id: u64,
    pub amount: u64,
    pub auction_type: AuctionType,
    pub start_price: i128,
    pub floor_price: i128,
    pub currency: Address,
    pub duration_seconds: u64,
    pub metadata_hash: Bytes,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BulkResult {
    pub succeeded: u32,
    pub failed: u32,
    pub ids: Vec<u64>,
}

// ===========================================================================
// Internal helpers
// ===========================================================================

fn default_correlation_id(env: &Env) -> BytesN<32> {
    BytesN::from_array(env, &[0u8; 32])
}

fn emit_action(env: &Env, caller: &Address, action: Symbol) {
    let cid = default_correlation_id(env);
    events::emit_action_executed(
        env,
        &cid,
        symbol_short!("nft_mkt"),
        action,
        caller,
        true,
        env.ledger().timestamp(),
    );
}

fn emit_bid_refund_credited(env: &Env, bidder: &Address, token: &Address, amount: i128) {
    env.events().publish(
        (symbol_short!("nft_mkt"), symbol_short!("ref_crd")),
        (bidder.clone(), token.clone(), amount),
    );
}

fn emit_bid_refund_claimed(env: &Env, bidder: &Address, token: &Address, amount: i128) {
    env.events().publish(
        (symbol_short!("nft_mkt"), symbol_short!("ref_clm")),
        (bidder.clone(), token.clone(), amount),
    );
}

fn validate_price(price: i128) -> MktResult<()> {
    if price <= 0 {
        return Err(MarketError::InvalidAmount);
    }
    Ok(())
}

fn validate_currency(env: &Env, currency: &Address) -> MktResult<()> {
    let currencies: Map<Address, bool> =
        instance_get(env, &KEY_CURRENCIES).unwrap_or_else(|| Map::new(env));
    if currencies.get(currency.clone()).unwrap_or(false) {
        Ok(())
    } else {
        Err(MarketError::CurrencyNotWhitelisted)
    }
}

fn validate_metadata_hash(hash: &Bytes) -> MktResult<()> {
    if hash.len() != METADATA_HASH_LEN {
        return Err(MarketError::InvalidMetadataHash);
    }
    Ok(())
}

fn listing_key(id: u64) -> (Symbol, u64) {
    (symbol_short!("listing"), id)
}

fn auction_key(id: u64) -> (Symbol, u64) {
    (symbol_short!("auction"), id)
}

fn offer_key(id: u64) -> (Symbol, u64) {
    (symbol_short!("offer"), id)
}

fn next_listing_id(env: &Env) -> MktResult<u64> {
    let current: u64 = instance_get(env, &KEY_LISTING_SEQ).unwrap_or(0);
    let next = current.checked_add(1).ok_or(MarketError::Overflow)?;
    instance_set(env, &KEY_LISTING_SEQ, &next);
    Ok(next)
}

fn next_auction_id(env: &Env) -> MktResult<u64> {
    let current: u64 = instance_get(env, &KEY_AUCTION_SEQ).unwrap_or(0);
    let next = current.checked_add(1).ok_or(MarketError::Overflow)?;
    instance_set(env, &KEY_AUCTION_SEQ, &next);
    Ok(next)
}

fn next_offer_id(env: &Env) -> MktResult<u64> {
    let current: u64 = instance_get(env, &KEY_OFFER_SEQ).unwrap_or(0);
    let next = current.checked_add(1).ok_or(MarketError::Overflow)?;
    instance_set(env, &KEY_OFFER_SEQ, &next);
    Ok(next)
}

/// Returns the contract's own address as a placeholder for "no bidder yet".
/// Used only as a sentinel; the `current_bid > 0` check gates all bidder logic.
fn sentinel_address(env: &Env) -> Address {
    env.current_contract_address()
}

fn verify_collection(env: &Env, collection: &Address) -> MktResult<()> {
    if persistent_has(env, &DataKey::CollectionInfo(collection.clone())) {
        Ok(())
    } else {
        Err(MarketError::CollectionNotFound)
    }
}

fn collection_exists(env: &Env, collection: &Address) -> bool {
    persistent_has(env, &DataKey::CollectionInfo(collection.clone()))
}

fn update_collection_stats(
    env: &Env,
    collection: &Address,
    listing_delta: i32,
    auction_delta: i32,
) {
    if let Some(mut info) = persistent_get::<DataKey, CollectionInfo>(env, &DataKey::CollectionInfo(collection.clone())) {
        if listing_delta > 0 {
            info.active_listings += listing_delta as u32;
        } else if listing_delta < 0 && info.active_listings > 0 {
            info.active_listings -= listing_delta.unsigned_abs();
        }
        if auction_delta > 0 {
            info.active_auctions += auction_delta as u32;
        } else if auction_delta < 0 && info.active_auctions > 0 {
            info.active_auctions -= auction_delta.unsigned_abs();
        }
        persistent_set(env, &DataKey::CollectionInfo(collection.clone()), &info);
    }
}

fn dutch_auction_price(env: &Env, auction: &Auction) -> MktResult<i128> {
    let current_time = now(env);
    if current_time >= auction.end_time {
        return Ok(auction.floor_price);
    }
    if current_time <= auction.start_time {
        return Ok(auction.start_price);
    }

    let duration = auction
        .end_time
        .checked_sub(auction.start_time)
        .ok_or(MarketError::Overflow)?;
    let elapsed = current_time
        .checked_sub(auction.start_time)
        .ok_or(MarketError::Overflow)?;
    let price_range = auction
        .start_price
        .checked_sub(auction.floor_price)
        .ok_or(MarketError::Overflow)?;

    let decay = price_range
        .checked_mul(elapsed as i128)
        .ok_or(MarketError::Overflow)?
        .checked_div(duration as i128)
        .ok_or(MarketError::Overflow)?;

    let price = auction
        .start_price
        .checked_sub(decay)
        .ok_or(MarketError::Overflow)?;

    Ok(price.max(0))
}

fn compute_platform_fee(price: i128, platform_fee_bps: i128) -> MktResult<i128> {
    price
        .checked_mul(platform_fee_bps)
        .ok_or(MarketError::Overflow)?
        .checked_div(BPS_DENOM)
        .ok_or(MarketError::Overflow)
        .map(|v| v.max(0))
}

fn compute_royalty_amounts(
    price: i128,
    royalty_config: &RoyaltyConfig,
) -> alloc::vec::Vec<(Address, i128)> {
    let mut amounts = alloc::vec::Vec::new();
    if royalty_config.total_bps <= 0 {
        return amounts;
    }
    for r in royalty_config.recipients.iter() {
        let amount = price
            .checked_mul(r.share_bps)
            .unwrap_or(0)
            .checked_div(BPS_DENOM)
            .unwrap_or(0);
        if amount > 0 {
            amounts.push((r.address.clone(), amount));
        }
    }
    amounts
}

fn distribute_royalties(
    env: &Env,
    payment_client: &token::Client,
    from: &Address,
    royalty_amounts: &[(Address, i128)],
    token_id: u64,
) {
    let cid = default_correlation_id(env);
    for (recipient, amount) in royalty_amounts.iter() {
        if *amount > 0 {
            payment_client.transfer(from, recipient, amount);
            events::emit_royalty_paid(env, &cid, token_id, recipient, *amount, now(env));
        }
    }
}

// ===========================================================================
// Contract
// ===========================================================================

#[contract]
pub struct NftMarketplace;

#[contractimpl]
impl NftMarketplace {
    // -----------------------------------------------------------------------
    // Initialization
    // -----------------------------------------------------------------------

    pub fn initialize(
        env: Env,
        admin: Address,
        platform_fee_bps: i128,
        fee_recipient: Address,
        bid_increment_bps: i128,
        auto_extension_seconds: u64,
    ) -> Result<(), MarketError> {
        if !(0..=MAX_FEE_BPS).contains(&platform_fee_bps) {
            return Err(MarketError::InvalidArgument);
        }
        if !(0..=5_000).contains(&bid_increment_bps) {
            return Err(MarketError::InvalidArgument);
        }

        auth::initialize_admin(&env, &admin).map_err(|error| match error {
            shared::Error::AlreadyInitialized => MarketError::AlreadyInitialized,
            _ => MarketError::Unauthorized,
        })?;
        instance_set(&env, &KEY_PLATFORM_FEE_BPS, &platform_fee_bps);
        instance_set(&env, &KEY_FEE_RECIPIENT, &fee_recipient);
        instance_set(&env, &KEY_BID_INCREMENT_BPS, &bid_increment_bps);
        instance_set(&env, &KEY_AUTO_EXT, &auto_extension_seconds);
        instance_set(&env, &KEY_CURRENCIES, &Map::<Address, bool>::new(&env));
        instance_set(&env, &KEY_LISTING_SEQ, &0_u64);
        instance_set(&env, &KEY_AUCTION_SEQ, &0_u64);
        instance_set(&env, &KEY_OFFER_SEQ, &0_u64);

        let cid = default_correlation_id(&env);
        events::emit_module_initialized(
            &env,
            &cid,
            symbol_short!("nft_mkt"),
            1,
            &admin,
            env.ledger().timestamp(),
        );
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Configuration (admin only)
    // -----------------------------------------------------------------------

    pub fn set_platform_fee(
        env: Env,
        caller: Address,
        new_fee_bps: i128,
    ) -> Result<(), MarketError> {
        require_admin(&env, &caller)?;
        if !(0..=MAX_FEE_BPS).contains(&new_fee_bps) {
            return Err(MarketError::InvalidArgument);
        }
        instance_set(&env, &KEY_PLATFORM_FEE_BPS, &new_fee_bps);
        emit_action(&env, &caller, symbol_short!("set_fee"));
        Ok(())
    }

    pub fn set_fee_recipient(
        env: Env,
        caller: Address,
        new_recipient: Address,
    ) -> Result<(), MarketError> {
        require_admin(&env, &caller)?;
        instance_set(&env, &KEY_FEE_RECIPIENT, &new_recipient);
        emit_action(&env, &caller, symbol_short!("set_frcp"));
        Ok(())
    }

    pub fn set_bid_increment(env: Env, caller: Address, bps: i128) -> Result<(), MarketError> {
        require_admin(&env, &caller)?;
        if !(0..=5_000).contains(&bps) {
            return Err(MarketError::InvalidArgument);
        }
        instance_set(&env, &KEY_BID_INCREMENT_BPS, &bps);
        emit_action(&env, &caller, symbol_short!("set_binc"));
        Ok(())
    }

    pub fn set_auto_extension(env: Env, caller: Address, seconds: u64) -> Result<(), MarketError> {
        require_admin(&env, &caller)?;
        if seconds > 3600 {
            return Err(MarketError::InvalidExtensionWindow);
        }
        instance_set(&env, &KEY_AUTO_EXT, &seconds);
        emit_action(&env, &caller, symbol_short!("set_aext"));
        Ok(())
    }

    pub fn set_currency(
        env: Env,
        caller: Address,
        currency: Address,
        whitelisted: bool,
    ) -> Result<(), MarketError> {
        require_admin(&env, &caller)?;
        let mut currencies: Map<Address, bool> =
            instance_get(&env, &KEY_CURRENCIES).unwrap_or_else(|| Map::new(&env));
        currencies.set(currency, whitelisted);
        instance_set(&env, &KEY_CURRENCIES, &currencies);
        emit_action(&env, &caller, symbol_short!("set_curr"));
        Ok(())
    }

    pub fn set_oracle(env: Env, caller: Address, oracle: Address) -> Result<(), MarketError> {
        require_admin(&env, &caller)?;
        instance_set(&env, &KEY_ORACLE, &oracle);
        emit_action(&env, &caller, symbol_short!("set_orcl"));
        Ok(())
    }

    pub fn platform_fee_bps(env: Env) -> i128 {
        instance_get(&env, &KEY_PLATFORM_FEE_BPS).unwrap_or(DEFAULT_PLATFORM_FEE_BPS)
    }

    pub fn fee_recipient_addr(env: Env) -> Address {
        instance_get(&env, &KEY_FEE_RECIPIENT).expect("fee recipient not set")
    }

    pub fn bid_increment_bps(env: Env) -> i128 {
        instance_get(&env, &KEY_BID_INCREMENT_BPS).unwrap_or(DEFAULT_BID_INCREMENT_BPS)
    }

    pub fn auto_extension_seconds(env: Env) -> u64 {
        instance_get(&env, &KEY_AUTO_EXT).unwrap_or(DEFAULT_AUTO_EXTENSION_SECONDS)
    }

    pub fn is_currency_whitelisted(env: Env, currency: Address) -> bool {
        let currencies: Map<Address, bool> =
            instance_get(&env, &KEY_CURRENCIES).unwrap_or_else(|| Map::new(&env));
        currencies.get(currency).unwrap_or(false)
    }

    pub fn oracle_address(env: Env) -> Option<Address> {
        instance_get(&env, &KEY_ORACLE)
    }

    // -----------------------------------------------------------------------
    // Collection Management
    // -----------------------------------------------------------------------

    pub fn register_collection(
        env: Env,
        caller: Address,
        collection: Address,
        name: Symbol,
        ipfs_uri: Bytes,
        metadata_hash: Bytes,
        standard: TokenStandard,
        royalty_recipients: Vec<RoyaltyRecipient>,
    ) -> Result<(), MarketError> {
        require_admin(&env, &caller)?;

        let collection_key = DataKey::CollectionInfo(collection.clone());
        if persistent_has(&env, &collection_key) {
            return Err(MarketError::CollectionAlreadyRegistered);
        }
        if metadata_hash.len() != METADATA_HASH_LEN {
            return Err(MarketError::InvalidMetadataHash);
        }
        shared::sanitize::validate_external_url(&ipfs_uri, shared::sanitize::MAX_URL_LEN, false)
            .map_err(|_| MarketError::InvalidArgument)?;

        if royalty_recipients.len() > MAX_ROYALTY_RECIPIENTS {
            return Err(MarketError::InvalidArgument);
        }

        let mut total_bps: i128 = 0;
        for r in royalty_recipients.iter() {
            if r.share_bps < 0 || r.share_bps > MAX_ROYALTY_BPS {
                return Err(MarketError::InvalidRoyaltyRate);
            }
            total_bps += r.share_bps;
        }
        let info = CollectionInfo {
            address: collection.clone(),
            admin: caller.clone(),
            name,
            ipfs_uri,
            metadata_hash,
            standard,
            royalty_config: RoyaltyConfig {
                recipients: royalty_recipients,
                total_bps,
            },
            registered_at: now(&env),
            active_listings: 0,
            active_auctions: 0,
        };

        persistent_set(&env, &collection_key, &info);

        let cid = default_correlation_id(&env);
        events::emit_collection_registered(&env, &cid, &collection, &caller, now(&env));
        Ok(())
    }

    pub fn set_collection_royalties(
        env: Env,
        caller: Address,
        collection: Address,
        royalty_recipients: Vec<RoyaltyRecipient>,
    ) -> Result<(), MarketError> {
        caller.require_auth();

        let collection_key = DataKey::CollectionInfo(collection.clone());
        let mut info: CollectionInfo =
            persistent_get(&env, &collection_key).ok_or(MarketError::CollectionNotFound)?;

        if info.admin != caller {
            return Err(MarketError::Unauthorized);
        }

        if royalty_recipients.len() > MAX_ROYALTY_RECIPIENTS {
            return Err(MarketError::InvalidArgument);
        }

        let mut total_bps: i128 = 0;
        for r in royalty_recipients.iter() {
            if r.share_bps < 0 || r.share_bps > MAX_ROYALTY_BPS {
                return Err(MarketError::InvalidRoyaltyRate);
            }
            total_bps += r.share_bps;
        }

        info.royalty_config = RoyaltyConfig {
            recipients: royalty_recipients,
            total_bps,
        };
        persistent_set(&env, &collection_key, &info);
        Ok(())
    }

    pub fn get_collection(env: Env, collection: Address) -> Result<CollectionInfo, MarketError> {
        persistent_get(&env, &DataKey::CollectionInfo(collection))
            .ok_or(MarketError::CollectionNotFound)
    }

    // -----------------------------------------------------------------------
    // Fixed-Price Listings
    // -----------------------------------------------------------------------

    pub fn list_nft(
        env: Env,
        seller: Address,
        collection: Address,
        token_id: u64,
        amount: u64,
        price: i128,
        currency: Address,
        metadata_hash: Bytes,
    ) -> Result<u64, MarketError> {
        require_not_paused(&env)?;
        seller.require_auth();

        validate_price(price)?;
        validate_currency(&env, &currency)?;
        validate_metadata_hash(&metadata_hash)?;
        verify_collection(&env, &collection)?;

        let listing_id = next_listing_id(&env)?;

        let nft_client = token::Client::new(&env, &collection);
        nft_client.transfer(&seller, &env.current_contract_address(), &(amount as i128));

        let listing = Listing {
            id: listing_id,
            seller: seller.clone(),
            collection: collection.clone(),
            token_id,
            amount,
            price,
            currency: currency.clone(),
            status: ListingStatus::Active,
            created_at: now(&env),
            metadata_hash,
        };

        persistent_set(&env, &listing_key(listing_id), &listing);
        update_collection_stats(&env, &collection, 1, 0);

        let cid = default_correlation_id(&env);
        events::emit_nft_listed(
            &env,
            &cid,
            listing_id,
            &seller,
            &collection,
            token_id,
            price,
            &currency,
            now(&env),
        );
        Ok(listing_id)
    }

    pub fn buy_nft(env: Env, buyer: Address, listing_id: u64) -> Result<(), MarketError> {
        require_not_paused(&env)?;
        buyer.require_auth();

        let key = listing_key(listing_id);
        let mut listing: Listing =
            persistent_read(&env, &key).ok_or(MarketError::ListingNotFound)?;

        if listing.status != ListingStatus::Active {
            return Err(MarketError::ListingAlreadySold);
        }

        listing.status = ListingStatus::Sold;
        persistent_set(&env, &key, &listing);

        let fee_recipient: Address =
            instance_get(&env, &KEY_FEE_RECIPIENT).expect("fee recipient not set");
        let platform_fee_bps: i128 =
            instance_get(&env, &KEY_PLATFORM_FEE_BPS).unwrap_or(DEFAULT_PLATFORM_FEE_BPS);

        let platform_fee = compute_platform_fee(listing.price, platform_fee_bps)?;

        let royalty_amounts = persistent_get::<DataKey, CollectionInfo>(
            &env,
            &DataKey::CollectionInfo(listing.collection.clone()),
        )
        .map(|info| compute_royalty_amounts(listing.price, &info.royalty_config))
        .unwrap_or_default();

        let total_royalties: i128 = royalty_amounts.iter().map(|(_, a)| *a).sum();
        let seller_proceeds = listing
            .price
            .checked_sub(platform_fee)
            .ok_or(MarketError::Overflow)?
            .checked_sub(total_royalties)
            .ok_or(MarketError::Overflow)?;

        let payment_client = token::Client::new(&env, &listing.currency);
        if platform_fee > 0 {
            payment_client.transfer(&buyer, &fee_recipient, &platform_fee);
        }
        distribute_royalties(
            &env,
            &payment_client,
            &buyer,
            &royalty_amounts,
            listing.token_id,
        );
        if seller_proceeds > 0 {
            payment_client.transfer(&buyer, &listing.seller, &seller_proceeds);
        }

        let nft_client = token::Client::new(&env, &listing.collection);
        nft_client.transfer(
            &env.current_contract_address(),
            &buyer,
            &(listing.amount as i128),
        );

        update_collection_stats(&env, &listing.collection, -1, 0);
        let cid = default_correlation_id(&env);
        events::emit_nft_sold(
            &env,
            &cid,
            listing_id,
            &listing.seller,
            &buyer,
            listing.price,
            now(&env),
        );
        Ok(())
    }

    pub fn cancel_listing(env: Env, caller: Address, listing_id: u64) -> Result<(), MarketError> {
        caller.require_auth();

        let key = listing_key(listing_id);
        let mut listing: Listing =
            persistent_read(&env, &key).ok_or(MarketError::ListingNotFound)?;

        if listing.status != ListingStatus::Active {
            return Err(MarketError::ListingAlreadySold);
        }
        if listing.seller != caller {
            return Err(MarketError::NotOwner);
        }

        listing.status = ListingStatus::Cancelled;
        persistent_set(&env, &key, &listing);

        let nft_client = token::Client::new(&env, &listing.collection);
        nft_client.transfer(
            &env.current_contract_address(),
            &listing.seller,
            &(listing.amount as i128),
        );
        update_collection_stats(&env, &listing.collection, -1, 0);
        Ok(())
    }

    pub fn get_listing(env: Env, listing_id: u64) -> Result<Listing, MarketError> {
        persistent_read(&env, &listing_key(listing_id)).ok_or(MarketError::ListingNotFound)
    }

    // -----------------------------------------------------------------------
    // English Auction
    // -----------------------------------------------------------------------

    pub fn create_english_auction(
        env: Env,
        seller: Address,
        collection: Address,
        token_id: u64,
        amount: u64,
        start_price: i128,
        currency: Address,
        duration_seconds: u64,
        metadata_hash: Bytes,
    ) -> Result<u64, MarketError> {
        require_not_paused(&env)?;
        seller.require_auth();

        validate_price(start_price)?;
        validate_currency(&env, &currency)?;
        validate_metadata_hash(&metadata_hash)?;
        verify_collection(&env, &collection)?;

        if duration_seconds == 0 || duration_seconds > 604_800 {
            return Err(MarketError::InvalidArgument);
        }

        let auction_id = next_auction_id(&env)?;
        let nft_client = token::Client::new(&env, &collection);
        nft_client.transfer(&seller, &env.current_contract_address(), &(amount as i128));

        let start_time = now(&env);
        let end_time = start_time
            .checked_add(duration_seconds)
            .ok_or(MarketError::Overflow)?;
        let auto_ext: u64 =
            instance_get(&env, &KEY_AUTO_EXT).unwrap_or(DEFAULT_AUTO_EXTENSION_SECONDS);

        let auction = Auction {
            id: auction_id,
            auction_type: AuctionType::English,
            seller: seller.clone(),
            collection: collection.clone(),
            token_id,
            amount,
            currency,
            start_price,
            floor_price: 0,
            current_bid: 0,
            current_bidder: sentinel_address(&env),
            status: AuctionStatus::Active,
            start_time,
            end_time,
            auto_extension_window: auto_ext,
            metadata_hash,
        };

        persistent_set(&env, &auction_key(auction_id), &auction);
        update_collection_stats(&env, &collection, 0, 1);
        let cid = default_correlation_id(&env);
        events::emit_nft_auction(
            &env,
            &cid,
            auction_id,
            &seller,
            &collection,
            token_id,
            start_price,
            end_time,
        );
        Ok(auction_id)
    }

    pub fn place_bid(
        env: Env,
        bidder: Address,
        auction_id: u64,
        bid_amount: i128,
    ) -> Result<(), MarketError> {
        require_not_paused(&env)?;
        bidder.require_auth();
        validate_price(bid_amount)?;

        let key = auction_key(auction_id);
        let mut auction: Auction =
            persistent_read(&env, &key).ok_or(MarketError::ListingNotFound)?;

        if auction.status != AuctionStatus::Active {
            return Err(MarketError::AuctionEnded);
        }
        if auction.auction_type != AuctionType::English {
            return Err(MarketError::InvalidArgument);
        }
        if now(&env) >= auction.end_time {
            auction.status = AuctionStatus::Ended;
            persistent_set(&env, &key, &auction);
            return Err(MarketError::AuctionEnded);
        }

        let increment_bps: i128 =
            instance_get(&env, &KEY_BID_INCREMENT_BPS).unwrap_or(DEFAULT_BID_INCREMENT_BPS);
        let min_bid = if auction.current_bid == 0 {
            auction.start_price
        } else {
            let increment = auction
                .current_bid
                .checked_mul(increment_bps)
                .ok_or(MarketError::Overflow)?
                .checked_div(BPS_DENOM)
                .ok_or(MarketError::Overflow)?
                .max(1);
            auction
                .current_bid
                .checked_add(increment)
                .ok_or(MarketError::Overflow)?
        };

        if bid_amount < min_bid {
            return Err(MarketError::BidTooLow);
        }
        if auction.current_bid > 0 && auction.current_bidder == bidder {
            return Err(MarketError::BidderIsCurrentHighest);
        }

        // Refund previous bidder
        if auction.current_bid > 0 {
            let prev = auction.current_bidder.clone();
            PendingRefunds::add(&env, &prev, &auction.currency, auction.current_bid)?;
            emit_bid_refund_credited(&env, &prev, &auction.currency, auction.current_bid);
        }

        // Escrow new bid
        let pc = token::Client::new(&env, &auction.currency);
        pc.transfer(&bidder, &env.current_contract_address(), &bid_amount);

        // Auto-extension
        let mut new_end = auction.end_time;
        let remaining = auction.end_time.saturating_sub(now(&env));
        if remaining <= auction.auto_extension_window {
            new_end = now(&env)
                .checked_add(auction.auto_extension_window)
                .ok_or(MarketError::Overflow)?;
        }

        auction.current_bid = bid_amount;
        auction.current_bidder = bidder.clone();
        auction.end_time = new_end;
        persistent_set(&env, &key, &auction);

        let cid = default_correlation_id(&env);
        events::emit_nft_bid(&env, &cid, auction_id, &bidder, bid_amount, new_end);
        Ok(())
    }

    pub fn settle_english_auction(env: Env, auction_id: u64) -> Result<(), MarketError> {
        require_not_paused(&env)?;

        let key = auction_key(auction_id);
        let mut auction: Auction =
            persistent_read(&env, &key).ok_or(MarketError::ListingNotFound)?;

        if auction.status == AuctionStatus::Settled {
            return Err(MarketError::ListingAlreadySold);
        }
        if auction.status != AuctionStatus::Active && auction.status != AuctionStatus::Ended {
            return Err(MarketError::AuctionEnded);
        }
        if now(&env) < auction.end_time {
            return Err(MarketError::AuctionNotYetEnded);
        }

        auction.status = AuctionStatus::Settled;
        persistent_set(&env, &key, &auction);

        if auction.current_bid == 0 {
            let nft_client = token::Client::new(&env, &auction.collection);
            nft_client.transfer(
                &env.current_contract_address(),
                &auction.seller,
                &(auction.amount as i128),
            );
            update_collection_stats(&env, &auction.collection, 0, -1);
            return Ok(());
        }

        let final_price = auction.current_bid;
        let winner = auction.current_bidder.clone();

        let fee_recipient: Address =
            instance_get(&env, &KEY_FEE_RECIPIENT).expect("fee recipient not set");
        let platform_fee_bps: i128 =
            instance_get(&env, &KEY_PLATFORM_FEE_BPS).unwrap_or(DEFAULT_PLATFORM_FEE_BPS);

        let platform_fee = compute_platform_fee(final_price, platform_fee_bps)?;

        let royalty_amounts = persistent_get::<DataKey, CollectionInfo>(
            &env,
            &DataKey::CollectionInfo(auction.collection.clone()),
        )
        .map(|info| compute_royalty_amounts(final_price, &info.royalty_config))
        .unwrap_or_default();

        let total_royalties: i128 = royalty_amounts.iter().map(|(_, a)| *a).sum();
        let seller_proceeds = final_price
            .checked_sub(platform_fee)
            .ok_or(MarketError::Overflow)?
            .checked_sub(total_royalties)
            .ok_or(MarketError::Overflow)?;

        let pc = token::Client::new(&env, &auction.currency);
        if platform_fee > 0 {
            pc.transfer(
                &env.current_contract_address(),
                &fee_recipient,
                &platform_fee,
            );
        }
        distribute_royalties(
            &env,
            &pc,
            &env.current_contract_address(),
            &royalty_amounts,
            auction.token_id,
        );
        if seller_proceeds > 0 {
            pc.transfer(
                &env.current_contract_address(),
                &auction.seller,
                &seller_proceeds,
            );
        }

        let nft_client = token::Client::new(&env, &auction.collection);
        nft_client.transfer(
            &env.current_contract_address(),
            &winner,
            &(auction.amount as i128),
        );

        update_collection_stats(&env, &auction.collection, 0, -1);
        let cid = default_correlation_id(&env);
        events::emit_nft_settle(&env, &cid, auction_id, &winner, final_price, now(&env));
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Dutch Auction
    // -----------------------------------------------------------------------

    pub fn create_dutch_auction(
        env: Env,
        seller: Address,
        collection: Address,
        token_id: u64,
        amount: u64,
        start_price: i128,
        floor_price: i128,
        currency: Address,
        duration_seconds: u64,
        metadata_hash: Bytes,
    ) -> Result<u64, MarketError> {
        require_not_paused(&env)?;
        seller.require_auth();

        validate_price(start_price)?;
        validate_price(floor_price)?;
        validate_currency(&env, &currency)?;
        validate_metadata_hash(&metadata_hash)?;
        verify_collection(&env, &collection)?;

        if floor_price >= start_price {
            return Err(MarketError::InvalidDutchAuctionPrices);
        }
        if duration_seconds == 0 || duration_seconds > 604_800 {
            return Err(MarketError::InvalidArgument);
        }

        let auction_id = next_auction_id(&env)?;
        let nft_client = token::Client::new(&env, &collection);
        nft_client.transfer(&seller, &env.current_contract_address(), &(amount as i128));

        let start_time = now(&env);
        let end_time = start_time
            .checked_add(duration_seconds)
            .ok_or(MarketError::Overflow)?;

        let auction = Auction {
            id: auction_id,
            auction_type: AuctionType::Dutch,
            seller: seller.clone(),
            collection: collection.clone(),
            token_id,
            amount,
            currency,
            start_price,
            floor_price,
            current_bid: 0,
            current_bidder: sentinel_address(&env),
            status: AuctionStatus::Active,
            start_time,
            end_time,
            auto_extension_window: 0,
            metadata_hash,
        };

        persistent_set(&env, &auction_key(auction_id), &auction);
        update_collection_stats(&env, &collection, 0, 1);
        let cid = default_correlation_id(&env);
        events::emit_nft_auction(
            &env,
            &cid,
            auction_id,
            &seller,
            &collection,
            token_id,
            start_price,
            end_time,
        );
        Ok(auction_id)
    }

    pub fn buy_dutch_auction(env: Env, buyer: Address, auction_id: u64) -> Result<(), MarketError> {
        require_not_paused(&env)?;
        buyer.require_auth();

        let key = auction_key(auction_id);
        let mut auction: Auction =
            persistent_read(&env, &key).ok_or(MarketError::ListingNotFound)?;

        if auction.status != AuctionStatus::Active {
            return Err(MarketError::AuctionEnded);
        }
        if auction.auction_type != AuctionType::Dutch {
            return Err(MarketError::InvalidArgument);
        }

        let current_price = dutch_auction_price(&env, &auction)?;
        if current_price == 0 {
            return Err(MarketError::DutchAuctionPriceZero);
        }

        auction.status = AuctionStatus::Settled;
        persistent_set(&env, &key, &auction);

        let fee_recipient: Address =
            instance_get(&env, &KEY_FEE_RECIPIENT).expect("fee recipient not set");
        let platform_fee_bps: i128 =
            instance_get(&env, &KEY_PLATFORM_FEE_BPS).unwrap_or(DEFAULT_PLATFORM_FEE_BPS);

        let platform_fee = compute_platform_fee(current_price, platform_fee_bps)?;

        let royalty_amounts = persistent_get::<DataKey, CollectionInfo>(
            &env,
            &DataKey::CollectionInfo(auction.collection.clone()),
        )
        .map(|info| compute_royalty_amounts(current_price, &info.royalty_config))
        .unwrap_or_default();

        let total_royalties: i128 = royalty_amounts.iter().map(|(_, a)| *a).sum();
        let seller_proceeds = current_price
            .checked_sub(platform_fee)
            .ok_or(MarketError::Overflow)?
            .checked_sub(total_royalties)
            .ok_or(MarketError::Overflow)?;

        let pc = token::Client::new(&env, &auction.currency);
        if platform_fee > 0 {
            pc.transfer(&buyer, &fee_recipient, &platform_fee);
        }
        distribute_royalties(&env, &pc, &buyer, &royalty_amounts, auction.token_id);
        if seller_proceeds > 0 {
            pc.transfer(&buyer, &auction.seller, &seller_proceeds);
        }

        let nft_client = token::Client::new(&env, &auction.collection);
        nft_client.transfer(
            &env.current_contract_address(),
            &buyer,
            &(auction.amount as i128),
        );

        update_collection_stats(&env, &auction.collection, 0, -1);
        let cid = default_correlation_id(&env);
        events::emit_nft_sold(
            &env,
            &cid,
            auction_id,
            &auction.seller,
            &buyer,
            current_price,
            now(&env),
        );
        Ok(())
    }

    pub fn dutch_auction_current_price(env: Env, auction_id: u64) -> Result<i128, MarketError> {
        let auction: Auction =
            persistent_read(&env, &auction_key(auction_id)).ok_or(MarketError::ListingNotFound)?;
        dutch_auction_price(&env, &auction)
    }

    pub fn cancel_dutch_auction(
        env: Env,
        caller: Address,
        auction_id: u64,
    ) -> Result<(), MarketError> {
        caller.require_auth();

        let key = auction_key(auction_id);
        let mut auction: Auction =
            persistent_read(&env, &key).ok_or(MarketError::ListingNotFound)?;

        if auction.status != AuctionStatus::Active {
            return Err(MarketError::AuctionEnded);
        }
        if auction.auction_type != AuctionType::Dutch {
            return Err(MarketError::InvalidArgument);
        }
        if auction.seller != caller {
            return Err(MarketError::NotOwner);
        }

        auction.status = AuctionStatus::Cancelled;
        persistent_set(&env, &key, &auction);

        let nft_client = token::Client::new(&env, &auction.collection);
        nft_client.transfer(
            &env.current_contract_address(),
            &auction.seller,
            &(auction.amount as i128),
        );
        update_collection_stats(&env, &auction.collection, 0, -1);
        Ok(())
    }

    pub fn get_auction(env: Env, auction_id: u64) -> Result<Auction, MarketError> {
        persistent_read(&env, &auction_key(auction_id)).ok_or(MarketError::ListingNotFound)
    }

    pub fn claim_bid_refund(env: Env, bidder: Address, token: Address) -> Result<(), MarketError> {
        bidder.require_auth();

        let refund_amount = PendingRefunds::get(&env, &bidder, &token);
        if refund_amount <= 0 {
            return Err(MarketError::InvalidAmount);
        }

        PendingRefunds::clear(&env, &bidder, &token);

        let pc = token::Client::new(&env, &token);
        pc.transfer(&env.current_contract_address(), &bidder, &refund_amount);

        emit_bid_refund_claimed(&env, &bidder, &token, refund_amount);
        Ok(())
    }

    pub fn get_pending_refund(env: Env, bidder: Address, token: Address) -> i128 {
        PendingRefunds::get(&env, &bidder, &token)
    }

    // -----------------------------------------------------------------------
    // Offer-Based Trading
    // -----------------------------------------------------------------------

    pub fn make_offer(
        env: Env,
        offerer: Address,
        recipient: Address,
        collection: Address,
        token_id: u64,
        amount: u64,
        offer_amount: i128,
        currency: Address,
        offer_nft_collection: Address,
        offer_nft_token_id: u64,
        duration_seconds: u64,
    ) -> Result<u64, MarketError> {
        require_not_paused(&env)?;
        offerer.require_auth();

        validate_price(offer_amount)?;
        validate_currency(&env, &currency)?;
        if duration_seconds == 0 || duration_seconds > 604_800 {
            return Err(MarketError::InvalidArgument);
        }

        let offer_id = next_offer_id(&env)?;
        let created_at = now(&env);
        let expires_at = created_at
            .checked_add(duration_seconds)
            .ok_or(MarketError::Overflow)?;

        let pc = token::Client::new(&env, &currency);
        pc.transfer(&offerer, &env.current_contract_address(), &offer_amount);

        let offer = Offer {
            id: offer_id,
            offerer: offerer.clone(),
            recipient,
            collection,
            token_id,
            amount,
            offer_amount,
            currency,
            offer_nft_collection,
            offer_nft_token_id,
            status: OfferStatus::Pending,
            created_at,
            expires_at,
        };

        persistent_set(&env, &offer_key(offer_id), &offer);
        let cid = default_correlation_id(&env);
        events::emit_nft_offer(&env, &cid, offer_id, &offerer, token_id, offer_amount, expires_at);
        Ok(offer_id)
    }

    pub fn accept_offer(env: Env, caller: Address, offer_id: u64) -> Result<(), MarketError> {
        require_not_paused(&env)?;
        caller.require_auth();

        let key = offer_key(offer_id);
        let mut offer: Offer = persistent_read(&env, &key).ok_or(MarketError::ListingNotFound)?;

        if offer.status != OfferStatus::Pending {
            return Err(MarketError::OfferAlreadySettled);
        }
        if offer.recipient != caller {
            return Err(MarketError::NotOfferRecipient);
        }
        if is_expired(&env, offer.expires_at) {
            offer.status = OfferStatus::Expired;
            persistent_set(&env, &key, &offer);
            let pc = token::Client::new(&env, &offer.currency);
            pc.transfer(
                &env.current_contract_address(),
                &offer.offerer,
                &offer.offer_amount,
            );
            return Err(MarketError::OfferExpired);
        }

        offer.status = OfferStatus::Accepted;
        persistent_set(&env, &key, &offer);

        let platform_fee_bps: i128 =
            instance_get(&env, &KEY_PLATFORM_FEE_BPS).unwrap_or(DEFAULT_PLATFORM_FEE_BPS);
        let platform_fee = compute_platform_fee(offer.offer_amount, platform_fee_bps)?;
        let seller_proceeds = offer
            .offer_amount
            .checked_sub(platform_fee)
            .ok_or(MarketError::Overflow)?;

        let fee_recipient: Address =
            instance_get(&env, &KEY_FEE_RECIPIENT).expect("fee recipient not set");
        let pc = token::Client::new(&env, &offer.currency);

        if platform_fee > 0 {
            pc.transfer(
                &env.current_contract_address(),
                &fee_recipient,
                &platform_fee,
            );
        }
        if seller_proceeds > 0 {
            pc.transfer(
                &env.current_contract_address(),
                &offer.recipient,
                &seller_proceeds,
            );
        }

        let nft_client = token::Client::new(&env, &offer.collection);
        nft_client.transfer(&caller, &offer.offerer, &(offer.amount as i128));

        let cid = default_correlation_id(&env);
        events::emit_nft_sold(
            &env,
            &cid,
            offer_id,
            &offer.recipient,
            &offer.offerer,
            offer.offer_amount,
            now(&env),
        );
        Ok(())
    }

    pub fn cancel_offer(env: Env, caller: Address, offer_id: u64) -> Result<(), MarketError> {
        caller.require_auth();

        let key = offer_key(offer_id);
        let mut offer: Offer = persistent_read(&env, &key).ok_or(MarketError::ListingNotFound)?;

        if offer.status != OfferStatus::Pending {
            return Err(MarketError::OfferAlreadySettled);
        }
        if offer.offerer != caller {
            return Err(MarketError::NotOwner);
        }

        offer.status = OfferStatus::Cancelled;
        persistent_set(&env, &key, &offer);

        let pc = token::Client::new(&env, &offer.currency);
        pc.transfer(
            &env.current_contract_address(),
            &offer.offerer,
            &offer.offer_amount,
        );
        Ok(())
    }

    pub fn get_offer(env: Env, offer_id: u64) -> Result<Offer, MarketError> {
        persistent_read(&env, &offer_key(offer_id)).ok_or(MarketError::ListingNotFound)
    }

    // -----------------------------------------------------------------------
    // Bulk Operations
    // -----------------------------------------------------------------------

    pub fn bulk_list(
        env: Env,
        seller: Address,
        items: Vec<BulkListingItem>,
    ) -> Result<BulkResult, MarketError> {
        require_not_paused(&env)?;
        seller.require_auth();

        let count = items.len();
        if count == 0 || count > MAX_BULK_SIZE {
            return Err(MarketError::InvalidArgument);
        }

        let mut ids = Vec::new(&env);
        let mut succeeded: u32 = 0;
        let mut failed: u32 = 0;

        for item in items.iter() {
            if validate_price(item.price).is_err()
                || validate_currency(&env, &item.currency).is_err()
                || validate_metadata_hash(&item.metadata_hash).is_err()
                || !collection_exists(&env, &item.collection)
            {
                failed += 1;
                continue;
            }

            let listing_id = next_listing_id(&env)?;

            let nft_client = token::Client::new(&env, &item.collection);
            nft_client.transfer(
                &seller,
                &env.current_contract_address(),
                &(item.amount as i128),
            );

            let listing = Listing {
                id: listing_id,
                seller: seller.clone(),
                collection: item.collection.clone(),
                token_id: item.token_id,
                amount: item.amount,
                price: item.price,
                currency: item.currency.clone(),
                status: ListingStatus::Active,
                created_at: now(&env),
                metadata_hash: item.metadata_hash.clone(),
            };

            persistent_set(&env, &listing_key(listing_id), &listing);
            update_collection_stats(&env, &item.collection, 1, 0);
            ids.push_back(listing_id);
            succeeded += 1;
        }

        Ok(BulkResult {
            succeeded,
            failed,
            ids,
        })
    }

    pub fn bulk_create_auctions(
        env: Env,
        seller: Address,
        items: Vec<BulkAuctionItem>,
    ) -> Result<BulkResult, MarketError> {
        require_not_paused(&env)?;
        seller.require_auth();

        let count = items.len();
        if count == 0 || count > MAX_BULK_SIZE {
            return Err(MarketError::InvalidArgument);
        }

        let mut ids = Vec::new(&env);
        let mut succeeded: u32 = 0;
        let mut failed: u32 = 0;

        for item in items.iter() {
            if item.duration_seconds == 0 || item.duration_seconds > 604_800 {
                failed += 1;
                continue;
            }
            if validate_price(item.start_price).is_err() {
                failed += 1;
                continue;
            }
            if item.auction_type == AuctionType::Dutch && item.floor_price >= item.start_price {
                failed += 1;
                continue;
            }
            if !collection_exists(&env, &item.collection) {
                failed += 1;
                continue;
            }

            let auction_id = next_auction_id(&env)?;

            let nft_client = token::Client::new(&env, &item.collection);
            nft_client.transfer(
                &seller,
                &env.current_contract_address(),
                &(item.amount as i128),
            );

            let start_time = now(&env);
            let end_time = start_time
                .checked_add(item.duration_seconds)
                .unwrap_or(u64::MAX);
            let auto_ext: u64 =
                instance_get(&env, &KEY_AUTO_EXT).unwrap_or(DEFAULT_AUTO_EXTENSION_SECONDS);

            let auction = Auction {
                id: auction_id,
                auction_type: item.auction_type,
                seller: seller.clone(),
                collection: item.collection.clone(),
                token_id: item.token_id,
                amount: item.amount,
                currency: item.currency.clone(),
                start_price: item.start_price,
                floor_price: item.floor_price,
                current_bid: 0,
                current_bidder: sentinel_address(&env),
                status: AuctionStatus::Active,
                start_time,
                end_time,
                auto_extension_window: if item.auction_type == AuctionType::English {
                    auto_ext
                } else {
                    0
                },
                metadata_hash: item.metadata_hash.clone(),
            };

            persistent_set(&env, &auction_key(auction_id), &auction);
            update_collection_stats(&env, &item.collection, 0, 1);
            ids.push_back(auction_id);
            succeeded += 1;
        }

        Ok(BulkResult {
            succeeded,
            failed,
            ids,
        })
    }
}

// ===========================================================================
// Auth helpers (local, since shared::auth::require_admin returns shared::Error)
// ===========================================================================

fn require_admin(env: &Env, caller: &Address) -> MktResult<()> {
    shared::auth::require_permission(env, caller, shared::auth::Permission::ManageConfiguration)
        .map_err(|_| MarketError::Unauthorized)
}

fn require_not_paused(env: &Env) -> MktResult<()> {
    if shared::storage::is_paused(env) {
        return Err(MarketError::ContractPaused);
    }
    Ok(())
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use soroban_sdk::testutils::{Address as _, Ledger};
    use soroban_sdk::{token, Env};

    struct MockEnv {
        env: Env,
        admin: Address,
        fee_recipient: Address,
        nft_collection: Address,
        nft_client: token::Client<'static>,
        nft_asset: token::StellarAssetClient<'static>,
        currency: Address,
        currency_client: token::Client<'static>,
        currency_asset: token::StellarAssetClient<'static>,
        marketplace: Address,
    }

    fn mk_hash(e: &Env) -> Bytes {
        let mut h = Bytes::new(e);
        for i in 0..32 {
            h.push_back(i as u8);
        }
        h
    }

    fn setup() -> MockEnv {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 1_000_000);

        let admin = Address::generate(&env);
        let fee_recipient = Address::generate(&env);

        let nft_collection = env.register_stellar_asset_contract(admin.clone());
        let nft_client = token::Client::new(&env, &nft_collection);
        let nft_asset = token::StellarAssetClient::new(&env, &nft_collection);

        let currency = env.register_stellar_asset_contract(admin.clone());
        let currency_client = token::Client::new(&env, &currency);
        let currency_asset = token::StellarAssetClient::new(&env, &currency);

        let marketplace = env.register_contract(None, NftMarketplace);

        MockEnv {
            env,
            admin,
            fee_recipient,
            nft_collection,
            nft_client,
            nft_asset,
            currency,
            currency_client,
            currency_asset,
            marketplace,
        }
    }

    fn client(m: &MockEnv) -> NftMarketplaceClient {
        NftMarketplaceClient::new(&m.env, &m.marketplace)
    }

    fn init(m: &MockEnv) {
        let c = client(m);
        c.initialize(&m.admin, &250, &m.fee_recipient, &500, &300);
        c.set_currency(&m.admin, &m.currency, &true);
        c.register_collection(
            &m.admin,
            &m.nft_collection,
            &Symbol::new(&m.env, "TestNFT"),
            &Bytes::from_slice(&m.env, b"ipfs://QmTest"),
            &mk_hash(&m.env),
            &TokenStandard::ERC721,
            &Vec::new(&m.env),
        );
    }

    fn fresh_user(m: &MockEnv) -> Address {
        Address::generate(&m.env)
    }

    // ── Initialization ──────────────────────────────────────────────

    #[test]
    fn init_sets_config() {
        let m = setup();
        let c = client(&m);
        c.initialize(&m.admin, &250, &m.fee_recipient, &500, &300);
        assert_eq!(c.platform_fee_bps(), 250);
        assert_eq!(c.fee_recipient_addr(), m.fee_recipient);
        assert_eq!(c.bid_increment_bps(), 500);
        assert_eq!(c.auto_extension_seconds(), 300);
    }

    #[test]
    fn init_rejects_bad_fee() {
        let m = setup();
        let c = client(&m);
        assert_eq!(
            c.try_initialize(&m.admin, &10_001, &m.fee_recipient, &500, &300),
            Err(Ok(MarketError::InvalidArgument))
        );
    }

    #[test]
    fn set_fee_requires_admin() {
        let m = setup();
        let c = client(&m);
        c.initialize(&m.admin, &250, &m.fee_recipient, &500, &300);
        let u = fresh_user(&m);
        assert_eq!(
            c.try_set_platform_fee(&u, &100),
            Err(Ok(MarketError::Unauthorized))
        );
    }

    // ── Currency whitelist ──────────────────────────────────────────

    #[test]
    fn currency_whitelist() {
        let m = setup();
        let c = client(&m);
        c.initialize(&m.admin, &250, &m.fee_recipient, &500, &300);
        assert!(!c.is_currency_whitelisted(&m.currency));
        c.set_currency(&m.admin, &m.currency, &true);
        assert!(c.is_currency_whitelisted(&m.currency));
    }

    // ── Collection ──────────────────────────────────────────────────

    #[test]
    fn register_collection() {
        let m = setup();
        init(&m);
        let c = client(&m);
        let info = c.get_collection(&m.nft_collection);
        assert_eq!(info.admin, m.admin);
        assert_eq!(info.standard, TokenStandard::ERC721);
    }

    #[test]
    fn register_collection_duplicate() {
        let m = setup();
        init(&m);
        let c = client(&m);
        assert_eq!(
            c.try_register_collection(
                &m.admin,
                &m.nft_collection,
                &Symbol::new(&m.env, "Dup"),
                &Bytes::from_slice(&m.env, b"ipfs://x"),
                &mk_hash(&m.env),
                &TokenStandard::ERC721,
                &Vec::new(&m.env),
            ),
            Err(Ok(MarketError::CollectionAlreadyRegistered))
        );
    }

    #[test]
    fn oversized_royalty_update_is_rejected_before_scanning_or_mutating() {
        let m = setup();
        init(&m);
        let c = client(&m);
        let mut recipients = Vec::new(&m.env);
        for _ in 0..=MAX_ROYALTY_RECIPIENTS {
            recipients.push_back(RoyaltyRecipient {
                address: Address::generate(&m.env),
                share_bps: 10,
            });
        }

        assert_eq!(
            c.try_set_collection_royalties(&m.admin, &m.nft_collection, &recipients),
            Err(Ok(MarketError::InvalidArgument))
        );
        assert!(c
            .get_collection(&m.nft_collection)
            .royalty_config
            .recipients
            .is_empty());
    }

    // ── Fixed-price listing ─────────────────────────────────────────

    #[test]
    fn list_and_buy() {
        let m = setup();
        init(&m);
        let c = client(&m);
        let seller = fresh_user(&m);
        let buyer = fresh_user(&m);
        m.nft_asset.mint(&seller, &10);
        m.currency_asset.mint(&buyer, &1_000_000);

        let id = c.list_nft(
            &seller,
            &m.nft_collection,
            &1,
            &1,
            &1_000,
            &m.currency,
            &mk_hash(&m.env),
        );
        assert_eq!(id, 1);

        c.buy_nft(&buyer, &id);
        assert_eq!(c.get_listing(&id).status, ListingStatus::Sold);
        assert_eq!(m.nft_client.balance(&buyer), 1);
        // 2.5% of 1000 = 25
        assert_eq!(m.currency_client.balance(&m.fee_recipient), 25);
        assert_eq!(m.currency_client.balance(&seller), 975);
    }

    #[test]
    fn buy_rejects_already_sold() {
        let m = setup();
        init(&m);
        let c = client(&m);
        let seller = fresh_user(&m);
        let b1 = fresh_user(&m);
        let b2 = fresh_user(&m);
        m.nft_asset.mint(&seller, &10);
        m.currency_asset.mint(&b1, &1_000_000);
        m.currency_asset.mint(&b2, &1_000_000);

        let id = c.list_nft(
            &seller,
            &m.nft_collection,
            &1,
            &1,
            &1_000,
            &m.currency,
            &mk_hash(&m.env),
        );
        c.buy_nft(&b1, &id);
        assert_eq!(
            c.try_buy_nft(&b2, &id),
            Err(Ok(MarketError::ListingAlreadySold))
        );
    }

    #[test]
    fn cancel_listing() {
        let m = setup();
        init(&m);
        let c = client(&m);
        let seller = fresh_user(&m);
        m.nft_asset.mint(&seller, &10);

        let id = c.list_nft(
            &seller,
            &m.nft_collection,
            &1,
            &1,
            &1_000,
            &m.currency,
            &mk_hash(&m.env),
        );
        c.cancel_listing(&seller, &id);
        assert_eq!(c.get_listing(&id).status, ListingStatus::Cancelled);
        assert_eq!(m.nft_client.balance(&seller), 10);
    }

    #[test]
    fn cancel_listing_not_owner() {
        let m = setup();
        init(&m);
        let c = client(&m);
        let seller = fresh_user(&m);
        let other = fresh_user(&m);
        m.nft_asset.mint(&seller, &10);

        let id = c.list_nft(
            &seller,
            &m.nft_collection,
            &1,
            &1,
            &1_000,
            &m.currency,
            &mk_hash(&m.env),
        );
        assert_eq!(
            c.try_cancel_listing(&other, &id),
            Err(Ok(MarketError::NotOwner))
        );
    }

    #[test]
    fn list_rejects_unknown_currency() {
        let m = setup();
        init(&m);
        let c = client(&m);
        let seller = fresh_user(&m);
        m.nft_asset.mint(&seller, &10);
        let bad = fresh_user(&m);

        assert_eq!(
            c.try_list_nft(
                &seller,
                &m.nft_collection,
                &1,
                &1,
                &1_000,
                &bad,
                &mk_hash(&m.env)
            ),
            Err(Ok(MarketError::CurrencyNotWhitelisted))
        );
    }

    #[test]
    fn list_rejects_bad_hash() {
        let m = setup();
        init(&m);
        let c = client(&m);
        let seller = fresh_user(&m);
        m.nft_asset.mint(&seller, &10);

        assert_eq!(
            c.try_list_nft(
                &seller,
                &m.nft_collection,
                &1,
                &1,
                &1_000,
                &m.currency,
                &Bytes::from_slice(&m.env, b"short")
            ),
            Err(Ok(MarketError::InvalidMetadataHash))
        );
    }

    // ── English auction ─────────────────────────────────────────────

    #[test]
    fn english_auction_full_flow() {
        let m = setup();
        init(&m);
        let c = client(&m);
        let seller = fresh_user(&m);
        let b1 = fresh_user(&m);
        let b2 = fresh_user(&m);
        m.nft_asset.mint(&seller, &10);
        m.currency_asset.mint(&b1, &100_000);
        m.currency_asset.mint(&b2, &100_000);

        let aid = c.create_english_auction(
            &seller,
            &m.nft_collection,
            &1,
            &1,
            &1_000,
            &m.currency,
            &3_600,
            &mk_hash(&m.env),
        );

        c.place_bid(&b1, &aid, &1_000);
        c.place_bid(&b2, &aid, &1_100);

        let a = c.get_auction(&aid);
        assert_eq!(a.current_bid, 1_100);
        assert_eq!(a.current_bidder, b2);
        assert_eq!(c.get_pending_refund(&b1, &m.currency), 1_000);
        assert_eq!(m.currency_client.balance(&b1), 99_000); // not yet claimed

        c.claim_bid_refund(&b1, &m.currency);
        assert_eq!(c.get_pending_refund(&b1, &m.currency), 0);
        assert_eq!(m.currency_client.balance(&b1), 100_000); // claimed refund

        m.env.ledger().with_mut(|l| l.timestamp += 4_000);
        c.settle_english_auction(&aid);

        assert_eq!(c.get_auction(&aid).status, AuctionStatus::Settled);
        assert_eq!(m.nft_client.balance(&b2), 1);

        let fee = 1_100 * 250 / 10_000;
        assert_eq!(m.currency_client.balance(&m.fee_recipient), fee);
        assert_eq!(m.currency_client.balance(&seller), 1_100 - fee);
    }

    #[test]
    fn english_auction_no_bids() {
        let m = setup();
        init(&m);
        let c = client(&m);
        let seller = fresh_user(&m);
        m.nft_asset.mint(&seller, &10);

        let aid = c.create_english_auction(
            &seller,
            &m.nft_collection,
            &1,
            &1,
            &1_000,
            &m.currency,
            &3_600,
            &mk_hash(&m.env),
        );
        m.env.ledger().with_mut(|l| l.timestamp += 4_000);
        c.settle_english_auction(&aid);
        assert_eq!(m.nft_client.balance(&seller), 10);
    }

    #[test]
    fn bid_too_low() {
        let m = setup();
        init(&m);
        let c = client(&m);
        let seller = fresh_user(&m);
        let bidder = fresh_user(&m);
        m.nft_asset.mint(&seller, &10);
        m.currency_asset.mint(&bidder, &100_000);

        let aid = c.create_english_auction(
            &seller,
            &m.nft_collection,
            &1,
            &1,
            &1_000,
            &m.currency,
            &3_600,
            &mk_hash(&m.env),
        );
        assert_eq!(
            c.try_place_bid(&bidder, &aid, &500),
            Err(Ok(MarketError::BidTooLow))
        );
    }

    #[test]
    fn bid_below_increment() {
        let m = setup();
        init(&m);
        let c = client(&m);
        let seller = fresh_user(&m);
        let b1 = fresh_user(&m);
        let b2 = fresh_user(&m);
        m.nft_asset.mint(&seller, &10);
        m.currency_asset.mint(&b1, &100_000);
        m.currency_asset.mint(&b2, &100_000);

        let aid = c.create_english_auction(
            &seller,
            &m.nft_collection,
            &1,
            &1,
            &1_000,
            &m.currency,
            &3_600,
            &mk_hash(&m.env),
        );
        c.place_bid(&b1, &aid, &1_000);
        // 5% of 1000 = 50, min next bid = 1050
        assert_eq!(
            c.try_place_bid(&b2, &aid, &1_020),
            Err(Ok(MarketError::BidTooLow))
        );
    }

    #[test]
    fn cannot_outbid_self() {
        let m = setup();
        init(&m);
        let c = client(&m);
        let seller = fresh_user(&m);
        let bidder = fresh_user(&m);
        m.nft_asset.mint(&seller, &10);
        m.currency_asset.mint(&bidder, &100_000);

        let aid = c.create_english_auction(
            &seller,
            &m.nft_collection,
            &1,
            &1,
            &1_000,
            &m.currency,
            &3_600,
            &mk_hash(&m.env),
        );
        c.place_bid(&bidder, &aid, &1_000);
        assert_eq!(
            c.try_place_bid(&bidder, &aid, &2_000),
            Err(Ok(MarketError::BidderIsCurrentHighest))
        );
    }

    #[test]
    fn settle_too_early() {
        let m = setup();
        init(&m);
        let c = client(&m);
        let seller = fresh_user(&m);
        let bidder = fresh_user(&m);
        m.nft_asset.mint(&seller, &10);
        m.currency_asset.mint(&bidder, &100_000);

        let aid = c.create_english_auction(
            &seller,
            &m.nft_collection,
            &1,
            &1,
            &1_000,
            &m.currency,
            &3_600,
            &mk_hash(&m.env),
        );
        c.place_bid(&bidder, &aid, &1_000);
        assert_eq!(
            c.try_settle_english_auction(&aid),
            Err(Ok(MarketError::AuctionNotYetEnded))
        );
    }

    // ── Auto-extension ──────────────────────────────────────────────

    #[test]
    fn auto_extends_on_late_bid() {
        let m = setup();
        init(&m);
        let c = client(&m);
        let seller = fresh_user(&m);
        let b1 = fresh_user(&m);
        let b2 = fresh_user(&m);
        m.nft_asset.mint(&seller, &10);
        m.currency_asset.mint(&b1, &100_000);
        m.currency_asset.mint(&b2, &100_000);

        // Auction starts at t=1_000_000, duration=1000s → end=1_001_000
        let aid = c.create_english_auction(
            &seller,
            &m.nft_collection,
            &1,
            &1,
            &1_000,
            &m.currency,
            &1_000,
            &mk_hash(&m.env),
        );

        // t=1_000_700: 300s remaining = auto_ext window → extend to 1_000_700+300=1_001_000
        m.env.ledger().with_mut(|l| l.timestamp = 1_000_700);
        c.place_bid(&b1, &aid, &1_000);
        assert_eq!(c.get_auction(&aid).end_time, 1_001_000);

        // t=1_000_900: 100s remaining < auto_ext window → extend to 1_000_900+300=1_001_200
        m.env.ledger().with_mut(|l| l.timestamp = 1_000_900);
        c.place_bid(&b2, &aid, &1_100);
        assert_eq!(c.get_auction(&aid).end_time, 1_001_200);
    }

    // ── Dutch auction ───────────────────────────────────────────────

    #[test]
    fn dutch_price_decay() {
        let m = setup();
        init(&m);
        let c = client(&m);
        let seller = fresh_user(&m);
        m.nft_asset.mint(&seller, &10);

        let aid = c.create_dutch_auction(
            &seller,
            &m.nft_collection,
            &1,
            &1,
            &10_000,
            &2_000,
            &m.currency,
            &1_000,
            &mk_hash(&m.env),
        );

        assert_eq!(c.dutch_auction_current_price(&aid), 10_000);

        m.env.ledger().with_mut(|l| l.timestamp += 500);
        let mid = c.dutch_auction_current_price(&aid);
        assert!(mid > 2_000 && mid < 10_000);

        m.env.ledger().with_mut(|l| l.timestamp += 600);
        assert_eq!(c.dutch_auction_current_price(&aid), 2_000);
    }

    #[test]
    fn buy_dutch_auction() {
        let m = setup();
        init(&m);
        let c = client(&m);
        let seller = fresh_user(&m);
        let buyer = fresh_user(&m);
        m.nft_asset.mint(&seller, &10);
        m.currency_asset.mint(&buyer, &1_000_000);

        let aid = c.create_dutch_auction(
            &seller,
            &m.nft_collection,
            &1,
            &1,
            &10_000,
            &2_000,
            &m.currency,
            &1_000,
            &mk_hash(&m.env),
        );
        m.env.ledger().with_mut(|l| l.timestamp += 250);
        c.buy_dutch_auction(&buyer, &aid);

        assert_eq!(m.nft_client.balance(&buyer), 1);
        // price at 25%: 10000 - (8000*250/1000) = 8000
        let fee = 8_000 * 250 / 10_000;
        assert_eq!(m.currency_client.balance(&m.fee_recipient), fee);
        assert_eq!(m.currency_client.balance(&seller), 8_000 - fee);
    }

    #[test]
    fn cancel_dutch_auction() {
        let m = setup();
        init(&m);
        let c = client(&m);
        let seller = fresh_user(&m);
        m.nft_asset.mint(&seller, &10);

        let aid = c.create_dutch_auction(
            &seller,
            &m.nft_collection,
            &1,
            &1,
            &10_000,
            &2_000,
            &m.currency,
            &1_000,
            &mk_hash(&m.env),
        );
        c.cancel_dutch_auction(&seller, &aid);
        assert_eq!(c.get_auction(&aid).status, AuctionStatus::Cancelled);
        assert_eq!(m.nft_client.balance(&seller), 10);
    }

    // ── Offers ──────────────────────────────────────────────────────

    #[test]
    fn make_and_accept_offer() {
        let m = setup();
        init(&m);
        let c = client(&m);
        let offerer = fresh_user(&m);
        let recipient = fresh_user(&m);
        m.currency_asset.mint(&offerer, &100_000);
        m.nft_asset.mint(&recipient, &10); // recipient must hold NFT to transfer

        let oid = c.make_offer(
            &offerer,
            &recipient,
            &m.nft_collection,
            &1,
            &1,
            &5_000,
            &m.currency,
            &fresh_user(&m),
            &0,
            &3_600,
        );
        assert_eq!(c.get_offer(&oid).status, OfferStatus::Pending);

        c.accept_offer(&recipient, &oid);
        assert_eq!(c.get_offer(&oid).status, OfferStatus::Accepted);

        let fee = 5_000 * 250 / 10_000;
        assert_eq!(m.currency_client.balance(&recipient), 5_000 - fee);
        assert_eq!(m.currency_client.balance(&m.fee_recipient), fee);
    }

    #[test]
    fn cancel_offer() {
        let m = setup();
        init(&m);
        let c = client(&m);
        let offerer = fresh_user(&m);
        let recipient = fresh_user(&m);
        m.currency_asset.mint(&offerer, &100_000);

        let oid = c.make_offer(
            &offerer,
            &recipient,
            &m.nft_collection,
            &1,
            &1,
            &5_000,
            &m.currency,
            &fresh_user(&m),
            &0,
            &3_600,
        );
        c.cancel_offer(&offerer, &oid);
        assert_eq!(c.get_offer(&oid).status, OfferStatus::Cancelled);
        assert_eq!(m.currency_client.balance(&offerer), 100_000);
    }

    #[test]
    fn accept_offer_not_recipient() {
        let m = setup();
        init(&m);
        let c = client(&m);
        let offerer = fresh_user(&m);
        let recipient = fresh_user(&m);
        let other = fresh_user(&m);
        m.currency_asset.mint(&offerer, &100_000);

        let oid = c.make_offer(
            &offerer,
            &recipient,
            &m.nft_collection,
            &1,
            &1,
            &5_000,
            &m.currency,
            &fresh_user(&m),
            &0,
            &3_600,
        );
        assert_eq!(
            c.try_accept_offer(&other, &oid),
            Err(Ok(MarketError::NotOfferRecipient))
        );
    }

    // ── Royalties ───────────────────────────────────────────────────

    #[test]
    fn royalties_distributed() {
        let m = setup();
        let c = client(&m);
        c.initialize(&m.admin, &250, &m.fee_recipient, &500, &300);
        c.set_currency(&m.admin, &m.currency, &true);

        let creator1 = fresh_user(&m);
        let creator2 = fresh_user(&m);
        let mut rr = Vec::new(&m.env);
        rr.push_back(RoyaltyRecipient {
            address: creator1.clone(),
            share_bps: 500,
        });
        rr.push_back(RoyaltyRecipient {
            address: creator2.clone(),
            share_bps: 250,
        });

        c.register_collection(
            &m.admin,
            &m.nft_collection,
            &Symbol::new(&m.env, "TestNFT"),
            &Bytes::from_slice(&m.env, b"ipfs://QmTest"),
            &mk_hash(&m.env),
            &TokenStandard::ERC721,
            &rr,
        );

        let seller = fresh_user(&m);
        let buyer = fresh_user(&m);
        m.nft_asset.mint(&seller, &10);
        m.currency_asset.mint(&buyer, &1_000_000);

        let id = c.list_nft(
            &seller,
            &m.nft_collection,
            &1,
            &1,
            &10_000,
            &m.currency,
            &mk_hash(&m.env),
        );
        c.buy_nft(&buyer, &id);

        let fee = 10_000 * 250 / 10_000;
        let r1 = 10_000 * 500 / 10_000;
        let r2 = 10_000 * 250 / 10_000;
        assert_eq!(m.currency_client.balance(&m.fee_recipient), fee);
        assert_eq!(m.currency_client.balance(&creator1), r1);
        assert_eq!(m.currency_client.balance(&creator2), r2);
        assert_eq!(m.currency_client.balance(&seller), 10_000 - fee - r1 - r2);
    }

    // ── Bulk ────────────────────────────────────────────────────────

    #[test]
    fn bulk_list() {
        let m = setup();
        init(&m);
        let c = client(&m);
        let seller = fresh_user(&m);
        m.nft_asset.mint(&seller, &100);

        let mut items = Vec::new(&m.env);
        for i in 1..=5 {
            items.push_back(BulkListingItem {
                collection: m.nft_collection.clone(),
                token_id: i,
                amount: 1,
                price: 1_000 * i as i128,
                currency: m.currency.clone(),
                metadata_hash: mk_hash(&m.env),
            });
        }
        let r = c.bulk_list(&seller, &items);
        assert_eq!(r.succeeded, 5);
        assert_eq!(r.failed, 0);
    }

    #[test]
    fn bulk_create_auctions() {
        let m = setup();
        init(&m);
        let c = client(&m);
        let seller = fresh_user(&m);
        m.nft_asset.mint(&seller, &100);

        let mut items = Vec::new(&m.env);
        for i in 1..=3 {
            items.push_back(BulkAuctionItem {
                collection: m.nft_collection.clone(),
                token_id: i,
                amount: 1,
                auction_type: AuctionType::English,
                start_price: 1_000 * i as i128,
                floor_price: 0,
                currency: m.currency.clone(),
                duration_seconds: 3_600,
                metadata_hash: mk_hash(&m.env),
            });
        }
        let r = c.bulk_create_auctions(&seller, &items);
        assert_eq!(r.succeeded, 3);
        assert_eq!(r.failed, 0);
    }

    // ── Edge cases ──────────────────────────────────────────────────

    #[test]
    fn zero_price_rejected() {
        let m = setup();
        init(&m);
        let c = client(&m);
        let seller = fresh_user(&m);
        m.nft_asset.mint(&seller, &10);
        assert_eq!(
            c.try_list_nft(
                &seller,
                &m.nft_collection,
                &1,
                &1,
                &0,
                &m.currency,
                &mk_hash(&m.env)
            ),
            Err(Ok(MarketError::InvalidAmount))
        );
    }

    #[test]
    fn negative_price_rejected() {
        let m = setup();
        init(&m);
        let c = client(&m);
        let seller = fresh_user(&m);
        m.nft_asset.mint(&seller, &10);
        assert_eq!(
            c.try_list_nft(
                &seller,
                &m.nft_collection,
                &1,
                &1,
                &-100,
                &m.currency,
                &mk_hash(&m.env)
            ),
            Err(Ok(MarketError::InvalidAmount))
        );
    }

    #[test]
    fn dutch_floor_gte_start() {
        let m = setup();
        init(&m);
        let c = client(&m);
        let seller = fresh_user(&m);
        m.nft_asset.mint(&seller, &10);
        assert_eq!(
            c.try_create_dutch_auction(
                &seller,
                &m.nft_collection,
                &1,
                &1,
                &5_000,
                &5_000,
                &m.currency,
                &1_000,
                &mk_hash(&m.env),
            ),
            Err(Ok(MarketError::InvalidDutchAuctionPrices))
        );
    }

    #[test]
    fn pull_based_refund_accumulates_and_can_be_claimed() {
        let m = setup();
        init(&m);
        let c = client(&m);
        let seller = fresh_user(&m);
        let b1 = fresh_user(&m);
        let b2 = fresh_user(&m);
        let b3 = fresh_user(&m);
        m.nft_asset.mint(&seller, &10);
        m.currency_asset.mint(&b1, &100_000);
        m.currency_asset.mint(&b2, &100_000);
        m.currency_asset.mint(&b3, &100_000);

        let aid = c.create_english_auction(
            &seller,
            &m.nft_collection,
            &1,
            &1,
            &1_000,
            &m.currency,
            &3_600,
            &mk_hash(&m.env),
        );

        // b1 places first bid
        c.place_bid(&b1, &aid, &1_000);
        assert_eq!(c.get_pending_refund(&b1, &m.currency), 0);

        // b2 outbids b1
        c.place_bid(&b2, &aid, &1_200);
        assert_eq!(c.get_pending_refund(&b1, &m.currency), 1_000);

        // b1 outbids b2 with another bid
        c.place_bid(&b1, &aid, &1_500);
        assert_eq!(c.get_pending_refund(&b2, &m.currency), 1_200);

        // b3 outbids b1
        c.place_bid(&b3, &aid, &1_800);
        // b1 now has accumulated refunds: 1_000 + 1_500 = 2_500
        assert_eq!(c.get_pending_refund(&b1, &m.currency), 2_500);

        // Claiming with zero balance should fail
        let stranger = fresh_user(&m);
        assert_eq!(
            c.try_claim_bid_refund(&stranger, &m.currency),
            Err(Ok(MarketError::InvalidAmount))
        );

        // b1 claims accumulated refund
        let b1_bal_before = m.currency_client.balance(&b1);
        c.claim_bid_refund(&b1, &m.currency);
        assert_eq!(c.get_pending_refund(&b1, &m.currency), 0);
        assert_eq!(m.currency_client.balance(&b1), b1_bal_before + 2_500);

        // b1 cannot claim again (now zero)
        assert_eq!(
            c.try_claim_bid_refund(&b1, &m.currency),
            Err(Ok(MarketError::InvalidAmount))
        );
    }

    #[test]
    fn auction_cannot_be_locked_by_outbid_bidder_no_inline_transfer() {
        let m = setup();
        init(&m);
        let c = client(&m);
        let seller = fresh_user(&m);
        // b1 simulates an address that cannot receive inline transfers (e.g. frozen/revoked trustline or contract)
        let b1 = fresh_user(&m);
        let b2 = fresh_user(&m);
        m.nft_asset.mint(&seller, &10);
        m.currency_asset.mint(&b1, &100_000);
        m.currency_asset.mint(&b2, &100_000);

        let aid = c.create_english_auction(
            &seller,
            &m.nft_collection,
            &1,
            &1,
            &1_000,
            &m.currency,
            &3_600,
            &mk_hash(&m.env),
        );

        // b1 bids
        c.place_bid(&b1, &aid, &1_000);

        // With pull-based refunds, place_bid NEVER transfers tokens to previous bidders during bid placement.
        // Therefore, subsequent valid bids from b2 succeed immediately without reverting, preventing DoS.
        c.place_bid(&b2, &aid, &1_200);

        let a = c.get_auction(&aid);
        assert_eq!(a.current_bid, 1_200);
        assert_eq!(a.current_bidder, b2);
        assert_eq!(c.get_pending_refund(&b1, &m.currency), 1_000);
    }

    #[test]
    fn test_stress_multiple_collections_no_instance_storage_bloat() {
        let m = setup();
        init(&m);
        let c = client(&m);

        // Register 105 distinct collections into persistent storage
        for _i in 0..105u32 {
            let col_addr = Address::generate(&m.env);
            c.register_collection(
                &m.admin,
                &col_addr,
                &Symbol::new(&m.env, "TEST"),
                &Bytes::from_slice(&m.env, b"ipfs://QmTestCol"),
                &mk_hash(&m.env),
                &TokenStandard::ERC721,
                &Vec::new(&m.env),
            );
            let info = c.get_collection(&col_addr);
            assert_eq!(info.address, col_addr);
        }
    }
}

