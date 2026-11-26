//! Immutable Double-Entry Financial Ledger (Simulated USDT).
//!
//! Accounting rules enforced in code:
//! - Unit: Micro-USDT (`10^-5` USDT). `100_000` units = `1.00000` USDT.
//!   Matches `notional_1e5_usdt(price_ticks, qty_lots)` with zero conversion loss.
//! - Conservation of Value: Every journal entry must have `sum(Debits) == sum(Credits)`.
//! - Non-negative User Accounts: `UserAvailable` and `UserReservedMargin` can never go below 0.
//! - Append-Only History: Committed journal entries are never mutated or deleted.
//! - Idempotency: Duplicate `idempotency_key` submissions are rejected without double-posting.

use std::collections::{HashMap, HashSet};

use crate::types::UserId;

/// Fixed-point currency unit: 1 unit = 10^-5 USDT (100,000 units = 1 USDT).
pub type MicroUsdt = i128;

/// Conversion helper: whole USDT to MicroUsdt (`10^-5` USDT).
pub const MICRO_USDT_PER_USDT: MicroUsdt = 100_000;

/// Unique monotonic journal entry identifier.
pub type EntryId = u64;

/// Ledger account buckets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AccountKey {
    /// Simulated virtual USDT mint/faucet source (allowed to carry negative credit balance).
    SystemTreasury,
    /// Exchange fee collection account.
    ExchangeFeeRevenue,
    /// Trader's free available simulated USDT balance.
    UserAvailable(UserId),
    /// Trader's locked isolated margin for active orders and open positions.
    UserReservedMargin(UserId),
}

/// Double-entry posting direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntrySide {
    Debit,
    Credit,
}

/// Single leg inside a balanced journal entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LedgerLeg {
    pub account: AccountKey,
    pub side: EntrySide,
    pub amount: MicroUsdt,
}

/// Immutable, balanced accounting record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JournalEntry {
    pub entry_id: EntryId,
    pub idempotency_key: String,
    pub reason: String,
    pub legs: Vec<LedgerLeg>,
}

/// Errors returned when a ledger transaction violates accounting invariants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LedgerError {
    ZeroOrNegativeAmount,
    EmptyLegs,
    UnbalancedEntry {
        total_debits: MicroUsdt,
        total_credits: MicroUsdt,
    },
    DuplicateIdempotencyKey,
    InsufficientFunds {
        account: AccountKey,
        available: MicroUsdt,
        required: MicroUsdt,
    },
}

/// In-memory deterministic double-entry ledger engine.
#[derive(Debug, Default, Clone)]
pub struct DoubleEntryLedger {
    /// Current net balance per account (`Credits - Debits`).
    /// User accounts are strictly >= 0; `SystemTreasury` carries the offsetting negative balance.
    balances: HashMap<AccountKey, MicroUsdt>,
    /// Append-only audit log of all committed journal entries.
    journal: Vec<JournalEntry>,
    /// Seen idempotency keys to guarantee at-most-once execution.
    seen_keys: HashSet<String>,
    /// Monotonic entry ID generator.
    next_entry_id: EntryId,
}

impl DoubleEntryLedger {
    /// Creates an empty ledger with zero balances and zero entries.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the current balance of any account in `MicroUsdt` (defaults to 0).
    pub fn balance_of(&self, account: AccountKey) -> MicroUsdt {
        *self.balances.get(&account).unwrap_or(&0)
    }

    /// Returns the full append-only audit log of committed journal entries.
    pub fn journal(&self) -> &[JournalEntry] {
        &self.journal
    }

    /// Audit invariant check: verifies that the sum of ALL account balances across the entire
    /// system equals strictly zero (`0`).
    pub fn verify_global_zero_sum(&self) -> bool {
        let net_sum: MicroUsdt = self.balances.values().copied().sum();
        net_sum == 0
    }

    /// Posts an atomic, balanced multi-leg journal entry.
    pub fn post_entry(
        &mut self,
        idempotency_key: impl Into<String>,
        reason: impl Into<String>,
        legs: Vec<LedgerLeg>,
    ) -> Result<JournalEntry, LedgerError> {
        let key = idempotency_key.into();
        if self.seen_keys.contains(&key) {
            return Err(LedgerError::DuplicateIdempotencyKey);
        }
        if legs.is_empty() {
            return Err(LedgerError::EmptyLegs);
        }

        let mut total_debits: MicroUsdt = 0;
        let mut total_credits: MicroUsdt = 0;

        // Calculate net delta per account so multi-leg entries validate net impact accurately
        let mut net_deltas: HashMap<AccountKey, MicroUsdt> = HashMap::new();

        for leg in &legs {
            if leg.amount <= 0 {
                return Err(LedgerError::ZeroOrNegativeAmount);
            }
            match leg.side {
                EntrySide::Debit => {
                    total_debits += leg.amount;
                    *net_deltas.entry(leg.account).or_insert(0) -= leg.amount;
                }
                EntrySide::Credit => {
                    total_credits += leg.amount;
                    *net_deltas.entry(leg.account).or_insert(0) += leg.amount;
                }
            }
        }

        // Enforce Double-Entry Golden Rule: Debits == Credits
        if total_debits != total_credits {
            return Err(LedgerError::UnbalancedEntry {
                total_debits,
                total_credits,
            });
        }

        // Enforce non-negative balance rule on all user accounts before mutating state
        for (account, delta) in &net_deltas {
            if matches!(
                account,
                AccountKey::UserAvailable(_) | AccountKey::UserReservedMargin(_)
            ) {
                let current = self.balance_of(*account);
                let projected = current + *delta;
                if projected < 0 {
                    return Err(LedgerError::InsufficientFunds {
                        account: *account,
                        available: current,
                        required: -*delta,
                    });
                }
            }
        }

        // Commit state atomically
        for (account, delta) in net_deltas {
            let entry = self.balances.entry(account).or_insert(0);
            *entry += delta;
        }

        self.seen_keys.insert(key.clone());
        self.next_entry_id += 1;

        let committed = JournalEntry {
            entry_id: self.next_entry_id,
            idempotency_key: key,
            reason: reason.into(),
            legs,
        };
        self.journal.push(committed.clone());
        Ok(committed)
    }

    /// Credits initial simulated USDT from `SystemTreasury` into a user's `UserAvailable` balance.
    pub fn grant_simulated_funds(
        &mut self,
        idempotency_key: impl Into<String>,
        user_id: UserId,
        amount: MicroUsdt,
    ) -> Result<JournalEntry, LedgerError> {
        self.post_entry(
            idempotency_key,
            format!("Grant simulated funds to user {}", user_id),
            vec![
                LedgerLeg {
                    account: AccountKey::SystemTreasury,
                    side: EntrySide::Debit,
                    amount,
                },
                LedgerLeg {
                    account: AccountKey::UserAvailable(user_id),
                    side: EntrySide::Credit,
                    amount,
                },
            ],
        )
    }

    /// Moves simulated USDT from `UserAvailable` to `UserReservedMargin` when placing an order.
    pub fn reserve_margin(
        &mut self,
        idempotency_key: impl Into<String>,
        user_id: UserId,
        amount: MicroUsdt,
    ) -> Result<JournalEntry, LedgerError> {
        self.post_entry(
            idempotency_key,
            format!("Reserve isolated margin for user {}", user_id),
            vec![
                LedgerLeg {
                    account: AccountKey::UserAvailable(user_id),
                    side: EntrySide::Debit,
                    amount,
                },
                LedgerLeg {
                    account: AccountKey::UserReservedMargin(user_id),
                    side: EntrySide::Credit,
                    amount,
                },
            ],
        )
    }

    /// Releases unused isolated margin from `UserReservedMargin` back to `UserAvailable`.
    pub fn release_margin(
        &mut self,
        idempotency_key: impl Into<String>,
        user_id: UserId,
        amount: MicroUsdt,
    ) -> Result<JournalEntry, LedgerError> {
        self.post_entry(
            idempotency_key,
            format!("Release isolated margin for user {}", user_id),
            vec![
                LedgerLeg {
                    account: AccountKey::UserReservedMargin(user_id),
                    side: EntrySide::Debit,
                    amount,
                },
                LedgerLeg {
                    account: AccountKey::UserAvailable(user_id),
                    side: EntrySide::Credit,
                    amount,
                },
            ],
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grants_reserves_and_releases_margin_with_zero_sum_invariant() {
        let mut ledger = DoubleEntryLedger::new();
        let user = 101;
        let ten_thousand_usdt = 10_000 * MICRO_USDT_PER_USDT;
        let three_thousand_usdt = 3_000 * MICRO_USDT_PER_USDT;
        let one_thousand_usdt = 1_000 * MICRO_USDT_PER_USDT;

        ledger
            .grant_simulated_funds("grant-101-init", user, ten_thousand_usdt)
            .unwrap();
        assert_eq!(
            ledger.balance_of(AccountKey::UserAvailable(user)),
            ten_thousand_usdt
        );
        assert!(ledger.verify_global_zero_sum());

        ledger
            .reserve_margin("order-1-lock", user, three_thousand_usdt)
            .unwrap();
        assert_eq!(
            ledger.balance_of(AccountKey::UserAvailable(user)),
            7_000 * MICRO_USDT_PER_USDT
        );
        assert_eq!(
            ledger.balance_of(AccountKey::UserReservedMargin(user)),
            three_thousand_usdt
        );
        assert!(ledger.verify_global_zero_sum());

        ledger
            .release_margin("order-1-cancel-unlock", user, one_thousand_usdt)
            .unwrap();
        assert_eq!(
            ledger.balance_of(AccountKey::UserAvailable(user)),
            8_000 * MICRO_USDT_PER_USDT
        );
        assert_eq!(
            ledger.balance_of(AccountKey::UserReservedMargin(user)),
            2_000 * MICRO_USDT_PER_USDT
        );
        assert_eq!(ledger.journal().len(), 3);
        assert!(ledger.verify_global_zero_sum());
    }

    #[test]
    fn blocks_overdraft_and_duplicate_idempotency_keys() {
        let mut ledger = DoubleEntryLedger::new();
        let user = 202;
        let five_hundred_usdt = 500 * MICRO_USDT_PER_USDT;

        ledger
            .grant_simulated_funds("grant-202", user, five_hundred_usdt)
            .unwrap();

        // Duplicate idempotency key must fail and NOT double-credit user
        assert_eq!(
            ledger.grant_simulated_funds("grant-202", user, five_hundred_usdt),
            Err(LedgerError::DuplicateIdempotencyKey)
        );
        assert_eq!(
            ledger.balance_of(AccountKey::UserAvailable(user)),
            five_hundred_usdt
        );

        // Attempting to lock more margin than available must fail atomically
        let too_much = 600 * MICRO_USDT_PER_USDT;
        assert_eq!(
            ledger.reserve_margin("lock-too-much", user, too_much),
            Err(LedgerError::InsufficientFunds {
                account: AccountKey::UserAvailable(user),
                available: five_hundred_usdt,
                required: too_much,
            })
        );
        assert_eq!(
            ledger.balance_of(AccountKey::UserReservedMargin(user)),
            0
        );
        assert!(ledger.verify_global_zero_sum());
    }

    #[test]
    fn rejects_unbalanced_or_non_positive_entries() {
        let mut ledger = DoubleEntryLedger::new();

        let unbalanced = ledger.post_entry(
            "bad-1",
            "Unbalanced test",
            vec![
                LedgerLeg {
                    account: AccountKey::SystemTreasury,
                    side: EntrySide::Debit,
                    amount: 100,
                },
                LedgerLeg {
                    account: AccountKey::UserAvailable(1),
                    side: EntrySide::Credit,
                    amount: 99,
                },
            ],
        );
        assert_eq!(
            unbalanced,
            Err(LedgerError::UnbalancedEntry {
                total_debits: 100,
                total_credits: 99,
            })
        );

        let zero_amt = ledger.grant_simulated_funds("bad-2", 1, 0);
        assert_eq!(zero_amt, Err(LedgerError::ZeroOrNegativeAmount));
        assert_eq!(ledger.journal().len(), 0);
    }
}
