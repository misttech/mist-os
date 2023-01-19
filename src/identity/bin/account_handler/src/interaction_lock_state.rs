// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![allow(dead_code)]

use std::time::{Duration, SystemTime};

/// The current state of the account with respect to interaction lock.
//
// TODO(fxb/110859): provide a factory method for InteractionLockState::Unlocked
// which performs the SystemTime::now() construction and can claim -not- to be
// recently authenticated if UTC is unavailable.
pub enum InteractionLockState {
    /// The account is interaction-locked.
    Locked,

    /// The account is interaction-unlocked; interaction with the account is
    /// allowed.
    Unlocked {
        // The time at which this account was last unlocked. Used for comparing
        // against the current time when evaluating whether or not an account
        // was recently authenticated.
        last_authentication_time: SystemTime,
    },
}

impl InteractionLockState {
    // Returns whether or not an account was recently authenticated.
    pub fn is_recently_authenticated(&self, recency_threshold: &Duration) -> bool {
        match self {
            Self::Locked => false,
            Self::Unlocked { last_authentication_time } => {
                (*last_authentication_time + *recency_threshold) >= SystemTime::now()
            }
        }
    }

    // Sets the state as InteractionLocked.
    pub fn lock(&mut self) {
        *self = InteractionLockState::Locked;
    }

    // Sets the state as InteractionUnlocked, and resets
    // 'last_authentication_time' to now, even if the state was previously
    // unlocked.
    pub fn unlock(&mut self) {
        *self = InteractionLockState::Unlocked { last_authentication_time: SystemTime::now() };
    }

    // Returns true if the state is InteractionLocked.
    pub fn is_locked(&self) -> bool {
        matches!(self, Self::Locked)
    }
}

#[cfg(test)]
mod test {
    use super::*;

    const SHORT_TIME: &Duration = &Duration::from_millis(1);
    const LONG_TIME: &Duration = &Duration::from_secs(1);

    #[test]
    fn test_lock_exceeds_recency() {
        let mut state = InteractionLockState::Locked;
        state.unlock();

        std::thread::sleep(*LONG_TIME);

        assert!(!state.is_recently_authenticated(SHORT_TIME));
    }

    #[test]
    fn test_lock_doesnt_immediately_become_unrecent() {
        let mut state = InteractionLockState::Locked;
        state.unlock();

        std::thread::sleep(*SHORT_TIME);

        assert!(state.is_recently_authenticated(LONG_TIME));
    }

    #[test]
    fn test_is_locked_and_is_unlocked() {
        let mut state = InteractionLockState::Locked;
        assert!(state.is_locked());

        state.unlock();
        assert!(!state.is_locked());

        state.lock();
        assert!(state.is_locked());
    }
}
