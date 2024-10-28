// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use core::mem::replace;
use core::task::Waker;

/// The locker was not writeable.
#[derive(Debug)]
pub struct NotWriteable;

/// A dual-custody memory location which can be written to and read.
pub enum Locker<T> {
    Free(usize),
    Pending(Option<Waker>),
    Ready(T),
    Canceled,
    Finished,
}

impl<T> Locker<T> {
    /// Writes the given value to the locker.
    ///
    /// On success, returns `true` if the reader canceled and the locker can now be freed.
    pub fn write(&mut self, value: T) -> Result<bool, NotWriteable> {
        match self {
            Self::Free(_) | Self::Ready(_) | Self::Finished => Err(NotWriteable),
            Self::Pending(waker) => {
                if let Some(waker) = waker.take() {
                    waker.wake();
                }
                *self = Locker::Ready(value);
                Ok(false)
            }
            Self::Canceled => Ok(true),
        }
    }

    /// Retrieves the result written to the locker, if any.
    ///
    /// On success, this finishes the locker and allows it to be freed. If the locker was pending,
    /// the given waker is registered.
    pub fn read(&mut self, waker: &Waker) -> Option<T> {
        match replace(self, Self::Pending(Some(waker.clone()))) {
            Self::Free(_) | Self::Canceled | Self::Finished => unreachable!(),
            Self::Pending(_) => None,
            Self::Ready(result) => {
                *self = Self::Finished;
                Some(result)
            }
        }
    }

    /// Cancels the pending read for the locker.
    ///
    /// Returns `true` if the locker was already written and can now be freed.
    pub fn cancel(&mut self) -> bool {
        match self {
            Self::Free(_) | Self::Canceled | Self::Finished => unreachable!(),
            Self::Pending(_) => {
                *self = Self::Canceled;
                false
            }
            Self::Ready(_) => {
                *self = Self::Canceled;
                true
            }
        }
    }
}

/// A free list of [`Locker`]s.
///
/// Allocated lockers are assigned a unique ID which is not reused until the locker is freed.
pub struct Lockers<T> {
    lockers: Vec<Locker<T>>,
    next_free: usize,
}

impl<T> Lockers<T> {
    /// Returns a new `Lockers`.
    pub fn new() -> Self {
        Self { lockers: Vec::new(), next_free: 0 }
    }

    /// Allocates a fresh locker, returning its index.
    pub fn alloc(&mut self) -> u32 {
        if self.next_free < self.lockers.len() {
            let locker = replace(&mut self.lockers[self.next_free], Locker::Pending(None));
            let Locker::Free(next_free) = locker else {
                panic!("unexpected allocation in free list");
            };
            replace(&mut self.next_free, next_free) as u32
        } else {
            let result = self.lockers.len();
            self.lockers.push(Locker::Pending(None));
            self.next_free = self.lockers.len();
            result as u32
        }
    }

    /// Frees the locker with the given index.
    pub fn free(&mut self, index: u32) {
        self.lockers[index as usize] = Locker::Free(self.next_free);
        self.next_free = index as usize;
    }

    /// Wakes up all of the `Pending` lockers.
    pub fn wake_all(&mut self) {
        for locker in self.lockers.iter_mut() {
            if let Locker::Pending(waker) = locker {
                if let Some(waker) = waker.take() {
                    waker.wake();
                }
            }
        }
    }

    /// Gets the locker corresponding to the given index.
    pub fn get(&mut self, index: u32) -> Option<&mut Locker<T>> {
        let locker = self.lockers.get_mut(index as usize)?;
        if matches!(locker, Locker::Free(_)) {
            None
        } else {
            Some(locker)
        }
    }
}
