//! # Staking Atlas Pallet
//!
//! The Staking Atlas pallet implements a modified version of the Aura consensus mechanism
//! with a reputation-based validator selection, called Aura-R DPoS (Randomized Delegated Proof of Stake).
//!
//! ## Overview
//!
//! The Staking Atlas pallet provides functionality for:
//! * Validator registration and delegation
//! * Reputation-based validator selection
//! * Reward distribution for validators and delegators
//! * Slashing for misbehaving validators
//!
//! ### Terminology
//!
//! * **Validator:** A network participant responsible for producing blocks.
//! * **Delegator:** A token holder who backs a validator with their stake.
//! * **Reputation Score:** A metric that measures validator performance.
//! * **Aura-R DPoS:** A consensus mechanism that selects validators based on stake and reputation.
//! * **Era:** A period after which rewards are distributed and validator set may change.
//! * **Session:** A period during which a fixed validator set is active.

#![cfg_attr(not(feature = "std"), no_std)]

use codec::{Decode, Encode, MaxEncodedLen};
use frame_support::{
    decl_error, decl_event, decl_module, decl_storage,
    dispatch::{DispatchError, DispatchResult},
    ensure,
    traits::{Currency, Get, Imbalance, LockIdentifier, LockableCurrency, WithdrawReasons},
    weights::{DispatchClass, Weight},
};
use frame_system::{ensure_signed, pallet_prelude::*};
use scale_info::TypeInfo;
use sp_runtime::{
    traits::{AtLeast32BitUnsigned, CheckedSub, Convert, SaturatedConversion, StaticLookup, Zero},
    Perbill, RuntimeDebug,
};
use sp_staking::SessionIndex;
use sp_std::{collections::btree_map::BTreeMap, prelude::*};

/// The staking atlas pallet's configuration trait.
pub trait Config: frame_system::Config {
    /// The overarching event type.
    type Event: From<Event<Self>> + Into<<Self as frame_system::Config>::Event>;

    /// The currency type used for staking.
    type Currency: LockableCurrency<Self::AccountId>;

    /// The period between eras.
    type EraDuration: Get<Self::BlockNumber>;

    /// The number of validators to select per session.
    type ValidatorsCount: Get<u32>;

    /// The minimum amount required to become a validator.
    type MinValidatorStake: Get<BalanceOf<Self>>;

    /// The minimum amount required to delegate.
    type MinDelegationStake: Get<BalanceOf<Self>>;

    /// The maximum number of delegations per delegator.
    type MaxDelegationsPerDelegator: Get<u32>;

    /// The number of eras that rewards are paid after.
    type RewardPaymentDelay: Get<EraIndex>;

    /// The number of eras that locked staking funds must remain bonded for.
    type BondingDuration: Get<EraIndex>;

    /// The reputation weight in validator selection algorithm (0-100%).
    type ReputationWeight: Get<Perbill>;
}

/// Alias for the balance type from the configuration.
pub type BalanceOf<T> =
    <<T as Config>::Currency as Currency<<T as frame_system::Config>::AccountId>>::Balance;

/// Alias for the negative imbalance type (used for currency creation).
pub type NegativeImbalanceOf<T> =
    <<T as Config>::Currency as Currency<<T as frame_system::Config>::AccountId>>::NegativeImbalance;

/// Era index type.
pub type EraIndex = u32;

/// A value placed in storage that represents a reputation score.
#[derive(Encode, Decode, Clone, PartialEq, Eq, RuntimeDebug, TypeInfo, MaxEncodedLen)]
pub struct ReputationScore<Balance> {
    /// The raw reputation score.
    pub score: Balance,
    /// The last era this score was updated.
    pub last_updated: EraIndex,
}

/// Validator information.
#[derive(Encode, Decode, Clone, PartialEq, Eq, RuntimeDebug, TypeInfo, MaxEncodedLen)]
pub struct Validator<AccountId, Balance> {
    /// The validator's account.
    pub account: AccountId,
    /// The validator's self-stake.
    pub self_stake: Balance,
    /// The total stake backing this validator (self + delegated).
    pub total_stake: Balance,
    /// The validator's reputation score.
    pub reputation: ReputationScore<Balance>,
    /// Whether the validator is currently active.
    pub is_active: bool,
}

/// Delegator information.
#[derive(Encode, Decode, Clone, PartialEq, Eq, RuntimeDebug, TypeInfo, MaxEncodedLen)]
pub struct Delegator<AccountId, Balance> {
    /// The delegator's account.
    pub account: AccountId,
    /// The delegations made by this delegator.
    pub delegations: Vec<(AccountId, Balance)>,
    /// The total staked amount.
    pub total_staked: Balance,
}

/// Exposure of a validator at a particular era.
#[derive(Encode, Decode, Clone, PartialEq, Eq, RuntimeDebug, TypeInfo, MaxEncodedLen)]
pub struct Exposure<AccountId, Balance> {
    /// The validator's own stake.
    pub own: Balance,
    /// The total stake from delegators.
    pub total: Balance,
    /// The delegations to this validator.
    pub delegations: Vec<IndividualExposure<AccountId, Balance>>,
}

/// A delegation from a delegator to a validator.
#[derive(Encode, Decode, Clone, PartialEq, Eq, RuntimeDebug, TypeInfo, MaxEncodedLen)]
pub struct IndividualExposure<AccountId, Balance> {
    /// The delegator account.
    pub who: AccountId,
    /// The amount of stake delegated.
    pub value: Balance,
}

/// The activity status of a validator.
#[derive(Encode, Decode, Clone, PartialEq, Eq, RuntimeDebug, TypeInfo, MaxEncodedLen)]
pub enum ValidatorStatus {
    /// The validator is active and can be selected.
    Active,
    /// The validator has been deregistered and cannot be selected.
    Deregistered,
    /// The validator has been slashed and is temporarily inactive.
    Slashed,
    /// The validator has insufficient stake to be selected.
    InsufficientStake,
}

#[frame_support::pallet]
pub mod pallet {
    use super::*;
    use frame_support::pallet_prelude::*;
    use frame_system::pallet_prelude::*;

    #[pallet::pallet]
    #[pallet::generate_store(pub(super) trait Store)]
    pub struct Pallet<T>(_);

    #[pallet::storage]
    #[pallet::getter(fn current_era)]
    pub type CurrentEra<T> = StorageValue<_, EraIndex, ValueQuery>;

    #[pallet::storage]
    #[pallet::getter(fn active_era)]
    pub type ActiveEra<T> = StorageValue<_, EraIndex, ValueQuery>;

    #[pallet::storage]
    #[pallet::getter(fn era_start_block_number)]
    pub type EraStartBlockNumber<T: Config> = StorageMap<
        _,
        Twox64Concat,
        EraIndex,
        T::BlockNumber,
        ValueQuery,
    >;

    #[pallet::storage]
    #[pallet::getter(fn validators)]
    pub type Validators<T: Config> = StorageMap<
        _,
        Twox64Concat,
        T::AccountId,
        Validator<T::AccountId, BalanceOf<T>>,
        OptionQuery,
    >;

    #[pallet::storage]
    #[pallet::getter(fn delegators)]
    pub type Delegators<T: Config> = StorageMap<
        _,
        Twox64Concat,
        T::AccountId,
        Delegator<T::AccountId, BalanceOf<T>>,
        OptionQuery,
    >;

    #[pallet::storage]
    #[pallet::getter(fn validator_count)]
    pub type ValidatorCount<T> = StorageValue<_, u32, ValueQuery>;

    #[pallet::storage]
    #[pallet::getter(fn minimum_validator_stake)]
    pub type MinimumValidatorStake<T: Config> = StorageValue<_, BalanceOf<T>, ValueQuery>;

    #[pallet::storage]
    #[pallet::getter(fn eras_stakers)]
    pub type ErasStakers<T: Config> = StorageDoubleMap<
        _,
        Twox64Concat,
        EraIndex,
        Twox64Concat,
        T::AccountId,
        Exposure<T::AccountId, BalanceOf<T>>,
        ValueQuery,
    >;

    #[pallet::storage]
    #[pallet::getter(fn eras_validator_list)]
    pub type ErasValidatorList<T: Config> = StorageMap<
        _,
        Twox64Concat,
        EraIndex,
        Vec<T::AccountId>,
        ValueQuery,
    >;

    #[pallet::storage]
    #[pallet::getter(fn eras_total_stake)]
    pub type ErasTotalStake<T: Config> = StorageMap<
        _,
        Twox64Concat,
        EraIndex,
        BalanceOf<T>,
        ValueQuery,
    >;

    #[pallet::storage]
    #[pallet::getter(fn eras_reward)]
    pub type ErasReward<T: Config> = StorageMap<
        _,
        Twox64Concat,
        EraIndex,
        BalanceOf<T>,
        OptionQuery,
    >;

    #[pallet::storage]
    #[pallet::getter(fn validator_status)]
    pub type ValidatorStatuses<T: Config> = StorageMap<
        _,
        Twox64Concat,
        T::AccountId,
        ValidatorStatus,
        ValueQuery,
        fn() -> ValidatorStatus { ValidatorStatus::Deregistered },
    >;

    #[pallet::event]
    #[pallet::generate_deposit(pub(super) fn deposit_event)]
    pub enum Event<T: Config> {
        /// A new era has been started. [era_index]
        NewEra(EraIndex),
        
        /// A validator has been registered. [validator]
        ValidatorRegistered(T::AccountId),
        
        /// A validator has been deregistered. [validator]
        ValidatorDeregistered(T::AccountId),
        
        /// A new delegation has been created. [delegator, validator, amount]
        DelegationCreated(T::AccountId, T::AccountId, BalanceOf<T>),
        
        /// A delegation has been withdrawn. [delegator, validator, amount]
        DelegationWithdrawn(T::AccountId, T::AccountId, BalanceOf<T>),
        
        /// A validator's self-stake has been increased. [validator, amount]
        ValidatorStakeIncreased(T::AccountId, BalanceOf<T>),
        
        /// A validator's self-stake has been decreased. [validator, amount]
        ValidatorStakeDecreased(T::AccountId, BalanceOf<T>),
        
        /// A validator's reputation score has been updated. [validator, new_score]
        ReputationUpdated(T::AccountId, BalanceOf<T>),
        
        /// Rewards have been paid out. [era_index, total_reward]
        RewardsPaid(EraIndex, BalanceOf<T>),
        
        /// A validator has been slashed. [validator, amount]
        ValidatorSlashed(T::AccountId, BalanceOf<T>),
    }

    #[pallet::error]
    pub enum Error<T> {
        /// Not a validator.
        NotValidator,
        
        /// Not a delegator.
        NotDelegator,
        
        /// Already a validator.
        AlreadyValidator,
        
        /// Already a delegator.
        AlreadyDelegator,
        
        /// Validator is not active.
        ValidatorNotActive,
        
        /// Insufficient stake to become a validator.
        InsufficientStake,
        
        /// Insufficient stake to delegate.
        InsufficientDelegationStake,
        
        /// Too many delegations for a single delegator.
        TooManyDelegations,
        
        /// Cannot withdraw stake while active as a validator.
        CannotWithdrawWhileActive,
        
        /// Cannot withdraw stake before the bonding period ends.
        CannotWithdrawBeforeBondingPeriod,
        
        /// No rewards available for this era.
        NoRewardsForEra,
        
        /// Rewards already claimed for this era.
        RewardsAlreadyClaimed,
    }

    #[pallet::call]
    impl<T: Config> Pallet<T> {
        /// Register as a validator.
        ///
        /// The dispatch origin must be Signed.
        ///
        /// # <weight>
        /// - Independent of the arguments. Moderate complexity.
        /// - O(1).
        /// - Three DB entries.
        /// # </weight>
        #[pallet::weight(10_000)]
        pub fn register_validator(
            origin: OriginFor<T>,
            #[pallet::compact] stake: BalanceOf<T>,
        ) -> DispatchResult {
            let who = ensure_signed(origin)?;
            
            // Check if already a validator
            ensure!(!Validators::<T>::contains_key(&who), Error::<T>::AlreadyValidator);
            
            // Check minimum stake
            let min_stake = T::MinValidatorStake::get();
            ensure!(stake >= min_stake, Error::<T>::InsufficientStake);
            
            // Lock the stake
            T::Currency::set_lock(
                LockIdentifier(*b"stakeatls"),
                &who,
                stake,
                WithdrawReasons::all(),
            );
            
            // Create reputation score
            let reputation = ReputationScore {
                score: Zero::zero(),
                last_updated: Self::current_era(),
            };
            
            // Create validator
            let validator = Validator {
                account: who.clone(),
                self_stake: stake,
                total_stake: stake,
                reputation,
                is_active: true,
            };
            
            // Store validator
            Validators::<T>::insert(&who, validator);
            
            // Update validator status
            ValidatorStatuses::<T>::insert(&who, ValidatorStatus::Active);
            
            // Update validator count
            let count = ValidatorCount::<T>::get().saturating_add(1);
            ValidatorCount::<T>::put(count);
            
            Self::deposit_event(Event::ValidatorRegistered(who));
            
            Ok(())
        }
        
        /// Deregister as a validator.
        ///
        /// The dispatch origin must be Signed and the account must be currently registered as a validator.
        ///
        /// # <weight>
        /// - Independent of the arguments. Moderate complexity.
        /// - O(1).
        /// - Three DB entries.
        /// # </weight>
        #[pallet::weight(10_000)]
        pub fn deregister_validator(
            origin: OriginFor<T>,
        ) -> DispatchResult {
            let who = ensure_signed(origin)?;
            
            // Check if is a validator
            ensure!(Validators::<T>::contains_key(&who), Error::<T>::NotValidator);
            
            // Check if validator is active and can be deregistered
            let validator = Validators::<T>::get(&who).ok_or(Error::<T>::NotValidator)?;
            
            // Update validator status
            ValidatorStatuses::<T>::insert(&who, ValidatorStatus::Deregistered);
            
            // Update validator data
            let updated_validator = Validator {
                is_active: false,
                ..validator
            };
            Validators::<T>::insert(&who, updated_validator);
            
            // Update validator count
            let count = ValidatorCount::<T>::get().saturating_sub(1);
            ValidatorCount::<T>::put(count);
            
            // Note: We don't remove the lock on the stake here.
            // The stake will be unlocked after the bonding period.
            
            Self::deposit_event(Event::ValidatorDeregistered(who));
            
            Ok(())
        }
        
        /// Delegate tokens to a validator.
        ///
        /// The dispatch origin must be Signed.
        ///
        /// # <weight>
        /// - Independent of the arguments. Moderate complexity.
        /// - O(1).
        /// - Three DB entries.
        /// # </weight>
        #[pallet::weight(10_000)]
        pub fn delegate(
            origin: OriginFor<T>,
            validator: <T::Lookup as StaticLookup>::Source,
            #[pallet::compact] amount: BalanceOf<T>,
        ) -> DispatchResult {
            let who = ensure_signed(origin)?;
            let validator = T::Lookup::lookup(validator)?;
            
            // Check if validator exists and is active
            ensure!(Validators::<T>::contains_key(&validator), Error::<T>::NotValidator);
            let mut validator_data = Validators::<T>::get(&validator).ok_or(Error::<T>::NotValidator)?;
            ensure!(validator_data.is_active, Error::<T>::ValidatorNotActive);
            
            // Check minimum delegation stake
            ensure!(amount >= T::MinDelegationStake::get(), Error::<T>::InsufficientDelegationStake);
            
            // Update or create delegator
            if Delegators::<T>::contains_key(&who) {
                let mut delegator = Delegators::<T>::get(&who).unwrap();
                
                // Check maximum delegations
                let max_delegations = T::MaxDelegationsPerDelegator::get();
                let delegation_count = delegator.delegations.len() as u32;
                
                let existing_delegation_idx = delegator.delegations.iter().position(|(v, _)| *v == validator);
                
                if let Some(idx) = existing_delegation_idx {
                    // Update existing delegation
                    let current_amount = delegator.delegations[idx].1;
                    delegator.delegations[idx].1 = current_amount.saturating_add(amount);
                    delegator.total_staked = delegator.total_staked.saturating_add(amount);
                } else {
                    // Add new delegation
                    ensure!(delegation_count < max_delegations, Error::<T>::TooManyDelegations);
                    delegator.delegations.push((validator.clone(), amount));
                    delegator.total_staked = delegator.total_staked.saturating_add(amount);
                }
                
                Delegators::<T>::insert(&who, delegator);
            } else {
                // Create new delegator
                let delegator = Delegator {
                    account: who.clone(),
                    delegations: vec![(validator.clone(), amount)],
                    total_staked: amount,
                };
                
                Delegators::<T>::insert(&who, delegator);
            }
            
            // Lock tokens
            T::Currency::set_lock(
                LockIdentifier(*b"delgatls"),
                &who,
                amount,
                WithdrawReasons::all(),
            );
            
            // Update validator's total stake
            validator_data.total_stake = validator_data.total_stake.saturating_add(amount);
            Validators::<T>::insert(&validator, validator_data);
            
            Self::deposit_event(Event::DelegationCreated(who, validator, amount));
            
            Ok(())
        }
        
        /// Undelegate tokens from a validator.
        ///
        /// The dispatch origin must be Signed and the account must have delegated to the validator.
        ///
        /// # <weight>
        /// - Independent of the arguments. Moderate complexity.
        /// - O(1).
        /// - Three DB entries.
        /// # </weight>
        #[pallet::weight(10_000)]
        pub fn undelegate(
            origin: OriginFor<T>,
            validator: <T::Lookup as StaticLookup>::Source,
            #[pallet::compact] amount: BalanceOf<T>,
        ) -> DispatchResult {
            let who = ensure_signed(origin)?;
            let validator = T::Lookup::lookup(validator)?;
            
            // Check if is a delegator
            ensure!(Delegators::<T>::contains_key(&who), Error::<T>::NotDelegator);
            let mut delegator = Delegators::<T>::get(&who).ok_or(Error::<T>::NotDelegator)?;
            
            // Find the delegation
            let delegation_idx = delegator.delegations.iter().position(|(v, _)| *v == validator)
                .ok_or(Error::<T>::NotDelegator)?;
            
            let current_delegation = delegator.delegations[delegation_idx].1;
            ensure!(amount <= current_delegation, Error::<T>::InsufficientDelegationStake);
            
            // Check if validator exists
            ensure!(Validators::<T>::contains_key(&validator), Error::<T>::NotValidator);
            let mut validator_data = Validators::<T>::get(&validator).ok_or(Error::<T>::NotValidator)?;
            
            // Update validator's total stake
            validator_data.total_stake = validator_data.total_stake.saturating_sub(amount);
            Validators::<T>::insert(&validator, validator_data);
            
            // Update delegator data
            if amount == current_delegation {
                // Remove delegation completely
                delegator.delegations.remove(delegation_idx);
            } else {
                // Reduce delegation amount
                delegator.delegations[delegation_idx].1 = current_delegation.saturating_sub(amount);
            }
            
            delegator.total_staked = delegator.total_staked.saturating_sub(amount);
            
            if delegator.delegations.is_empty() {
                // Remove delegator if no delegations left
                Delegators::<T>::remove(&who);
            } else {
                // Update delegator
                Delegators::<T>::insert(&who, delegator);
            }
            
            // Note: We don't remove the lock on the tokens here.
            // The tokens will be unlocked after the bonding period.
            // For now, we'll just emit the event.
            
            Self::deposit_event(Event::DelegationWithdrawn(who, validator, amount));
            
            Ok(())
        }
        
        /// Increase validator's self-stake.
        ///
        /// The dispatch origin must be Signed and the account must be a registered validator.
        ///
        /// # <weight>
        /// - Independent of the arguments. Moderate complexity.
        /// - O(1).
        /// - Two DB entries.
        /// # </weight>
        #[pallet::weight(10_000)]
        pub fn increase_stake(
            origin: OriginFor<T>,
            #[pallet::compact] additional_amount: BalanceOf<T>,
        ) -> DispatchResult {
            let who = ensure_signed(origin)?;
            
            // Check if is a validator
            ensure!(Validators::<T>::contains_key(&who), Error::<T>::NotValidator);
            let mut validator = Validators::<T>::get(&who).ok_or(Error::<T>::NotValidator)?;
            
            // Update validator's stake
            validator.self_stake = validator.self_stake.saturating_add(additional_amount);
            validator.total_stake = validator.total_stake.saturating_add(additional_amount);
            
            Validators::<T>::insert(&who, validator);
            
            // Lock additional tokens
            T::Currency::set_lock(
                LockIdentifier(*b"stakeatls"),
                &who,
                validator.self_stake,
                WithdrawReasons::all(),
            );
            
            Self::deposit_event(Event::ValidatorStakeIncreased(who, additional_amount));
            
            Ok(())
        }
        
        /// Decrease validator's self-stake.
        ///
        /// The dispatch origin must be Signed and the account must be a registered validator.
        /// If the validator is active, they must deregister first.
        ///
        /// # <weight>
        /// - Independent of the arguments. Moderate complexity.
        /// - O(1).
        /// - Two DB entries.
        /// # </weight>
        #[pallet::weight(10_000)]
        pub fn decrease_stake(
            origin: OriginFor<T>,
            #[pallet::compact] amount: BalanceOf<T>,
        ) -> DispatchResult {
            let who = ensure_signed(origin)?;
            
            // Check if is a validator
            ensure!(Validators::<T>::contains_key(&who), Error::<T>::NotValidator);
            let mut validator = Validators::<T>::get(&who).ok_or(Error::<T>::NotValidator)?;
            
            // Check if validator is active
            ensure!(!validator.is_active, Error::<T>::CannotWithdrawWhileActive);
            
            // Check if there's enough stake to withdraw
            ensure!(amount <= validator.self_stake, Error::<T>::InsufficientStake);
            
            let min_stake = T::MinValidatorStake::get();
            ensure!(
                validator.self_stake.saturating_sub(amount) >= min_stake || 
                validator.self_stake.saturating_sub(amount) == Zero::zero(),
                Error::<T>::InsufficientStake
            );
            
            // Update validator's stake
            validator.self_stake = validator.self_stake.saturating_sub(amount);
            validator.total_stake = validator.total_stake.saturating_sub(amount);
            
            // If validator has withdrawn all stake, remove them
            if validator.self_stake.is_zero() {
                Validators::<T>::remove(&who);
                ValidatorStatuses::<T>::remove(&who);
            } else {
                Validators::<T>::insert(&who, validator);
            }
            
            // Update lock
            if !validator.self_stake.is_zero() {
                T::Currency::set_lock(
                    LockIdentifier(*b"stakeatls"),
                    &who,
                    validator.self_stake,
                    WithdrawReasons::all(),
                );
            } else {
                T::Currency::remove_lock(
                    LockIdentifier(*b"stakeatls"),
                    &who,
                );
            }
            
            Self::deposit_event(Event::ValidatorStakeDecreased(who, amount));
            
            Ok(())
        }
        
        /// Calculate and distribute rewards for an era.
        fn distribute_rewards(era: EraIndex) -> DispatchResult {
            // Check if rewards for this era are available
            let era_reward = ErasReward::<T>::get(era).ok_or(Error::<T>::NoRewardsForEra)?;
            
            // Get validators for this era
            let validators = ErasValidatorList::<T>::get(era);
            
            // If no validators, return early
            if validators.is_empty() {
                return Ok(());
            }
            
            // Get total stake for this era
            let total_stake = ErasTotalStake::<T>::get(era);
            
            // If total stake is zero, return early
            if total_stake.is_zero() {
                return Ok(());
            }
            
            let mut reward_remainder = era_reward;
            
            // For each validator
            for validator_id in validators.iter() {
                // Get validator exposure
                let exposure = ErasStakers::<T>::get(era, validator_id);
                
                // Calculate validator's share of rewards based on stake
                let validator_stake_ratio = Perbill::from_rational(exposure.total, total_stake);
                let validator_reward = validator_stake_ratio * era_reward;
                
                // If validator reward is zero, skip
                if validator_reward.is_zero() {
                    continue;
                }
                
                // Get the reputation adjustment for rewards
                // Higher reputation means higher rewards
                let reputation = match Validators::<T>::get(validator_id) {
                    Some(v) => v.reputation.score,
                    None => Zero::zero(),
                };
                
                // We'll use a simple linear reputation adjustment for now
                // A more sophisticated model could be implemented
                let reputation_factor = Perbill::from_rational(reputation, 100u32.into());
                let reputation_bonus = Perbill::from_percent(10) * reputation_factor * validator_reward;
                let adjusted_validator_reward = validator_reward.saturating_add(reputation_bonus);
                
                // Ensure we don't exceed total reward
                let actual_validator_reward = if adjusted_validator_reward > reward_remainder {
                    reward_remainder
                } else {
                    adjusted_validator_reward
                };
                
                reward_remainder = reward_remainder.saturating_sub(actual_validator_reward);
                
                // Calculate validator's commission (percentage of rewards they keep)
                // For simplicity, let's use a fixed 10% commission
                let commission_rate = Perbill::from_percent(10);
                let commission = commission_rate * actual_validator_reward;
                
                // Calculate remaining reward to distribute to delegators
                let delegators_reward = actual_validator_reward.saturating_sub(commission);
                
                // Reward the validator (their own stake + commission)
                let validator_own_stake_ratio = Perbill::from_rational(exposure.own, exposure.total);
                let validator_own_reward = validator_own_stake_ratio * delegators_reward;
                let validator_total_reward = validator_own_reward.saturating_add(commission);
                
                // Send reward to validator
                if !validator_total_reward.is_zero() {
                    let _ = T::Currency::deposit_creating(validator_id, validator_total_reward);
                }
                
                // Distribute remaining reward to delegators
                if !delegators_reward.is_zero() && !exposure.delegations.is_empty() {
                    let remaining_delegators_reward = delegators_reward.saturating_sub(validator_own_reward);
                    
                    for delegation in exposure.delegations.iter() {
                        let delegator_stake_ratio = Perbill::from_rational(delegation.value, exposure.total);
                        let delegator_reward = delegator_stake_ratio * delegators_reward;
                        
                        if !delegator_reward.is_zero() {
                            let _ = T::Currency::deposit_creating(&delegation.who, delegator_reward);
                        }
                    }
                }
            }
            
            // Emit event
            Self::deposit_event(Event::RewardsPaid(era, era_reward.saturating_sub(reward_remainder)));
            
            // Remove era reward after distribution
            ErasReward::<T>::remove(era);
            
            Ok(())
        }
    }

    #[pallet::hooks]
    impl<T: Config> Hooks<BlockNumberFor<T>> for Pallet<T> {
        fn on_initialize(n: T::BlockNumber) -> Weight {
            // Calculate expected era block
            let current_era = Self::current_era();
            let era_start_block = Self::era_start_block_number(current_era);
            let era_duration = T::EraDuration::get();
            let expected_era_end = era_start_block.saturating_add(era_duration);
            
            // Check if we need to start a new era
            if n >= expected_era_end {
                // Start new era
                let new_era = current_era.saturating_add(1);
                CurrentEra::<T>::put(new_era);
                EraStartBlockNumber::<T>::insert(new_era, n);
                
                // Update validator reputation scores
                Self::update_reputation_scores();
                
                // Select validators for the new era
                let _validators = Self::select_validators();
                
                // Update validator set for the next session
                Self::update_validator_set();
                
                // Distribute rewards for the previous era (with a delay)
                if current_era >= T::RewardPaymentDelay::get() {
                    let reward_era = current_era.saturating_sub(T::RewardPaymentDelay::get());
                    let _ = Self::distribute_rewards(reward_era);
                }
                
                Self::deposit_event(Event::NewEra(new_era));
                
                // Return weight indicating moderate computation
                return Weight::from_parts(50_000_000, 0);
            }
            
            // No era change, return minimal weight
            Weight::from_parts(5_000_000, 0)
        }
        
        fn on_finalize(_n: T::BlockNumber) {
            // No finalization logic needed for now
        }
    }

    // Additional implementation for the pallet
    impl<T: Config> Pallet<T> {
        /// Select validators for the next era based on stake and reputation.
        fn select_validators() -> Vec<T::AccountId> {
            // Get all active validators
            let mut validators: Vec<(T::AccountId, BalanceOf<T>, BalanceOf<T>)> = Vec::new();
            
            for (validator_id, validator_data) in Validators::<T>::iter() {
                // Only consider active validators
                if validator_data.is_active {
                    let status = ValidatorStatuses::<T>::get(&validator_id);
                    if status == ValidatorStatus::Active {
                        // Calculate validator score as a combination of stake and reputation
                        // Formula: score = (1 - reputation_weight) * stake + reputation_weight * reputation
                        let reputation_weight = T::ReputationWeight::get();
                        let stake_weight = Perbill::from_percent(100) - reputation_weight;
                        
                        let stake_score = stake_weight * validator_data.total_stake;
                        let reputation_score = reputation_weight * validator_data.reputation.score;
                        
                        let total_score = stake_score + reputation_score;
                        
                        validators.push((validator_id, total_score, validator_data.total_stake));
                    }
                }
            }
            
            // Sort validators by total score (descending)
            validators.sort_by(|a, b| b.1.cmp(&a.1));
            
            // Select top N validators where N is ValidatorsCount
            let count = T::ValidatorsCount::get() as usize;
            let selected = validators.into_iter()
                .take(count)
                .map(|(id, _, _)| id)
                .collect::<Vec<_>>();
            
            // Store selected validators for the current era
            let current_era = Self::current_era();
            ErasValidatorList::<T>::insert(current_era, selected.clone());
            
            // Calculate total stake of selected validators
            let total_stake = validators.iter()
                .filter(|(id, _, _)| selected.contains(id))
                .fold(Zero::zero(), |acc, (_, _, stake)| acc + *stake);
            
            ErasTotalStake::<T>::insert(current_era, total_stake);
            
            selected
        }
        
        /// Calculate and distribute rewards for an era.
        fn distribute_rewards(era: EraIndex) -> DispatchResult {
            // Check if rewards for this era are available
            let era_reward = ErasReward::<T>::get(era).ok_or(Error::<T>::NoRewardsForEra)?;
            
            // Get validators for this era
            let validators = ErasValidatorList::<T>::get(era);
            
            // If no validators, return early
            if validators.is_empty() {
                return Ok(());
            }
            
            // Get total stake for this era
            let total_stake = ErasTotalStake::<T>::get(era);
            
            // If total stake is zero, return early
            if total_stake.is_zero() {
                return Ok(());
            }
            
            let mut reward_remainder = era_reward;
            
            // For each validator
            for validator_id in validators.iter() {
                // Get validator exposure
                let exposure = ErasStakers::<T>::get(era, validator_id);
                
                // Calculate validator's share of rewards based on stake
                let validator_stake_ratio = Perbill::from_rational(exposure.total, total_stake);
                let validator_reward = validator_stake_ratio * era_reward;
                
                // If validator reward is zero, skip
                if validator_reward.is_zero() {
                    continue;
                }
                
                // Get the reputation adjustment for rewards
                // Higher reputation means higher rewards
                let reputation = match Validators::<T>::get(validator_id) {
                    Some(v) => v.reputation.score,
                    None => Zero::zero(),
                };
                
                // We'll use a simple linear reputation adjustment for now
                // A more sophisticated model could be implemented
                let reputation_factor = Perbill::from_rational(reputation, 100u32.into());
                let reputation_bonus = Perbill::from_percent(10) * reputation_factor * validator_reward;
                let adjusted_validator_reward = validator_reward.saturating_add(reputation_bonus);
                
                // Ensure we don't exceed total reward
                let actual_validator_reward = if adjusted_validator_reward > reward_remainder {
                    reward_remainder
                } else {
                    adjusted_validator_reward
                };
                
                reward_remainder = reward_remainder.saturating_sub(actual_validator_reward);
                
                // Calculate validator's commission (percentage of rewards they keep)
                // For simplicity, let's use a fixed 10% commission
                let commission_rate = Perbill::from_percent(10);
                let commission = commission_rate * actual_validator_reward;
                
                // Calculate remaining reward to distribute to delegators
                let delegators_reward = actual_validator_reward.saturating_sub(commission);
                
                // Reward the validator (their own stake + commission)
                let validator_own_stake_ratio = Perbill::from_rational(exposure.own, exposure.total);
                let validator_own_reward = validator_own_stake_ratio * delegators_reward;
                let validator_total_reward = validator_own_reward.saturating_add(commission);
                
                // Send reward to validator
                if !validator_total_reward.is_zero() {
                    let _ = T::Currency::deposit_creating(validator_id, validator_total_reward);
                }
                
                // Distribute remaining reward to delegators
                if !delegators_reward.is_zero() && !exposure.delegations.is_empty() {
                    let remaining_delegators_reward = delegators_reward.saturating_sub(validator_own_reward);
                    
                    for delegation in exposure.delegations.iter() {
                        let delegator_stake_ratio = Perbill::from_rational(delegation.value, exposure.total);
                        let delegator_reward = delegator_stake_ratio * delegators_reward;
                        
                        if !delegator_reward.is_zero() {
                            let _ = T::Currency::deposit_creating(&delegation.who, delegator_reward);
                        }
                    }
                }
            }
            
            // Emit event
            Self::deposit_event(Event::RewardsPaid(era, era_reward.saturating_sub(reward_remainder)));
            
            // Remove era reward after distribution
            ErasReward::<T>::remove(era);
            
            Ok(())
        }
        
        /// Calculate a validator's reputation score based on performance.
        fn calculate_reputation_score(validator: &T::AccountId) -> BalanceOf<T> {
            // TODO: Implement a more sophisticated reputation score calculation
            // For now, just return the current reputation score if it exists
            if let Some(validator_data) = Validators::<T>::get(validator) {
                return validator_data.reputation.score;
            }
            
            Zero::zero()
        }
        
        /// Update validator reputation scores based on their performance.
        fn update_reputation_scores() {
            for (validator_id, mut validator_data) in Validators::<T>::iter() {
                // Calculate new reputation score
                let new_score = Self::calculate_reputation_score(&validator_id);
                
                // Update reputation score
                validator_data.reputation.score = new_score;
                validator_data.reputation.last_updated = Self::current_era();
                
                // Update validator
                Validators::<T>::insert(&validator_id, validator_data);
                
                Self::deposit_event(Event::ReputationUpdated(validator_id, new_score));
            }
        }
        
        /// Update the validator set for the next session.
        fn update_validator_set() {
            // Select validators for the next era
            let selected_validators = Self::select_validators();
            
            // TODO: Integrate with pallet-session to update the validator set
        }
    }
}

// TODO: Implement validator selection algorithm and reward distribution logic 