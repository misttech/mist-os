// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Helper class to implement a counter that can be shared across threads.

/// Macro to define an atomic counter for a given base type. This is necessary because rust atomic
/// types are not parametrized on their base type.
macro_rules! atomic_counter_definition {
    ($ty:ty) => {
        paste::paste! {
        #[derive(Debug, Default)]
        pub struct [< Atomic $ty:camel Counter >](std::sync::atomic::[< Atomic $ty:camel >]);

        #[allow(dead_code)]
        impl [< Atomic $ty:camel Counter >] {
            pub const fn new(value: $ty) -> Self {
                Self(std::sync::atomic::[< Atomic $ty:camel >]::new(value))
            }

            pub fn next(&self) -> $ty {
                self.add(1)
            }

            pub fn add(&self, amount: $ty) -> $ty {
                self.0.fetch_add(amount, std::sync::atomic::Ordering::Relaxed)
            }

            pub fn get(&self) -> $ty {
                self.0.load(std::sync::atomic::Ordering::Relaxed)
            }
            pub fn reset(&mut self, value: $ty) {
                *self.0.get_mut() = value;
            }
        }

        impl From<$ty> for [< Atomic $ty:camel Counter >] {
            fn from(value: $ty) -> Self {
                Self::new(value)
            }
        }
        }
    };
}

atomic_counter_definition!(u64);
atomic_counter_definition!(u32);
atomic_counter_definition!(usize);

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[::fuchsia::test]
    fn test_new() {
        const COUNTER: AtomicU64Counter = AtomicU64Counter::new(0);
        assert_eq!(COUNTER.get(), 0);
    }

    #[::fuchsia::test]
    fn test_one_thread() {
        let mut counter = AtomicU64Counter::default();
        assert_eq!(counter.get(), 0);
        assert_eq!(counter.add(5), 0);
        assert_eq!(counter.get(), 5);
        assert_eq!(counter.next(), 5);
        assert_eq!(counter.get(), 6);
        counter.reset(2);
        assert_eq!(counter.get(), 2);
        assert_eq!(counter.next(), 2);
        assert_eq!(counter.get(), 3);
    }

    #[::fuchsia::test]
    fn test_multiple_thread() {
        const THREADS_COUNT: u64 = 10;
        const INC_ITERATIONS: u64 = 1000;
        let mut thread_handles = Vec::new();
        let counter = Arc::new(AtomicU64Counter::default());

        for _ in 0..THREADS_COUNT {
            thread_handles.push(std::thread::spawn({
                let counter = Arc::clone(&counter);
                move || {
                    for _ in 0..INC_ITERATIONS {
                        counter.next();
                    }
                }
            }));
        }
        for handle in thread_handles {
            handle.join().expect("join");
        }
        assert_eq!(THREADS_COUNT * INC_ITERATIONS, counter.get());
    }
}
