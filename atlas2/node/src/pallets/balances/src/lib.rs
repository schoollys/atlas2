//! # Atlas Balances Pallet
//!
//! The Atlas Balances pallet extends the standard pallet-balances with additional
//! functionality specific to the Atlas2 public ledger.
//!
//! ## Overview
//!
//! The Atlas Balances pallet provides functionality for:
//! * Managing account balances on the public ledger
//! * Supporting transfers between public accounts
//! * Integrating with the staking system
//! * Preparing for interaction with the shielded pool
//!
//! ### Terminology
//!
//! * **Public Account:** An account on the transparent ledger with a publicly visible balance.
//! * **Public Transfer:** A fully transparent transfer between two public accounts.
//! * **Account Type:** Indicates whether an account is normal, contract, or gateway.

#![cfg_attr(not(feature = "std"), no_std)]

use codec::{Decode, Encode, MaxEncodedLen};
use frame_support::{
    decl_error, decl_event, decl_module, decl_storage,
    dispatch::{DispatchError, DispatchResult},
    ensure,
    traits::{
        Currency, ExistenceRequirement, Get, Imbalance, LockIdentifier, LockableCurrency,
        ReservableCurrency, WithdrawReasons,
    },
    weights::{DispatchClass, Weight},
};
use frame_system::{ensure_signed, pallet_prelude::*};
use scale_info::TypeInfo;
use sp_runtime::{
    traits::{AtLeast32BitUnsigned, CheckedSub, StaticLookup, Zero},
    DispatchError as RtDispatchError, RuntimeDebug,
};
use sp_std::prelude::*;

// Re-export pallet items so that they can be accessed from the crate namespace.
pub use pallet::*;

/// The account type, used to differentiate between different kinds of accounts
/// on the public ledger.
#[derive(Encode, Decode, Clone, PartialEq, Eq, RuntimeDebug, TypeInfo, MaxEncodedLen)]
pub enum AccountType {
    /// A normal user account
    Normal,
    /// A smart contract account
    Contract,
    /// A gateway account (used for shielding operations)
    Gateway,
}

impl Default for AccountType {
    fn default() -> Self {
        Self::Normal
    }
}

/// Additional account information beyond the basic balance.
#[derive(Encode, Decode, Clone, PartialEq, Eq, RuntimeDebug, TypeInfo, MaxEncodedLen)]
pub struct AccountInfo<Balance> {
    /// The type of this account
    pub account_type: AccountType,
    /// Number of transactions sent from this account
    pub nonce: u64,
    /// Total amount transferred out of this account
    pub total_sent: Balance,
    /// Total amount received by this account
    pub total_received: Balance,
}

impl<Balance: Default> Default for AccountInfo<Balance> {
    fn default() -> Self {
        Self {
            account_type: AccountType::Normal,
            nonce: 0,
            total_sent: Balance::default(),
            total_received: Balance::default(),
        }
    }
}

#[frame_support::pallet]
pub mod pallet {
    use super::*;
    use frame_support::pallet_prelude::*;
    use frame_system::pallet_prelude::*;

    /// The module configuration trait.
    #[pallet::config]
    pub trait Config: frame_system::Config + pallet_balances::Config {
        /// The overarching event type.
        type Event: From<Event<Self>> + IsType<<Self as frame_system::Config>::Event>;
        
        /// The balance type from the main balances pallet
        type Balance: Parameter
            + Member
            + AtLeast32BitUnsigned
            + Default
            + Copy
            + MaxEncodedLen;
    }

    #[pallet::pallet]
    #[pallet::generate_store(pub(super) trait Store)]
    pub struct Pallet<T>(_);

    // Storage declarations
    #[pallet::storage]
    #[pallet::getter(fn account_info)]
    pub type AccountInfos<T: Config> = StorageMap<
        _,
        Blake2_128Concat,
        T::AccountId,
        AccountInfo<T::Balance>,
        ValueQuery,
    >;

    // Events
    #[pallet::event]
    #[pallet::generate_deposit(pub(super) fn deposit_event)]
    pub enum Event<T: Config> {
        /// Account information was updated
        AccountInfoUpdated(T::AccountId),
        /// Account type was changed
        AccountTypeChanged(T::AccountId, AccountType),
    }

    // Errors
    #[pallet::error]
    pub enum Error<T> {
        /// Account type change is not allowed
        AccountTypeChangeNotAllowed,
        /// Invalid account type for this operation
        InvalidAccountType,
    }

    // Dispatchable functions
    #[pallet::call]
    impl<T: Config> Pallet<T> {
        /// Set the account type for an account
        #[pallet::weight(10_000)]
        pub fn set_account_type(
            origin: OriginFor<T>,
            account: <T::Lookup as StaticLookup>::Source,
            new_type: AccountType,
        ) -> DispatchResult {
            let who = ensure_signed(origin)?;
            let account = T::Lookup::lookup(account)?;
            
            // TODO: Implement proper access control here
            // For now, only the account itself can change its type
            ensure!(who == account, Error::<T>::AccountTypeChangeNotAllowed);
            
            AccountInfos::<T>::mutate(&account, |info| {
                info.account_type = new_type.clone();
            });
            
            Self::deposit_event(Event::AccountTypeChanged(account, new_type));
            Ok(())
        }
    }

    // Hooks
    #[pallet::hooks]
    impl<T: Config> Hooks<BlockNumberFor<T>> for Pallet<T> {
        // TODO: Implement hooks
    }
}

// TODO: Implement functions to integrate with the shielded pool
// This will be developed in later tasks
