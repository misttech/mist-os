// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::log::*,
    anyhow::{anyhow, Error},
    async_utils::event::Event,
    fxfs_crypto::{Crypt, UnwrappedKeys, WrappedKeys, XtsCipherSet},
    once_cell::sync::OnceCell,
    std::{future::Future, sync::Arc},
};

/// Unwraps keys in a background task.
pub struct KeyUnwrapper {
    inner: Arc<KeyUnwrapperInner>,
}

struct KeyUnwrapperInner {
    // Both `event` and `keys` have 2 possible states for a total of 4 possible states:
    //   - `event` is not signalled and `keys` is set: constructed from `new_from_unwrapped`.
    //   - `event` is not signalled and `keys` is not set: keys are still being unwrapped.
    //   - `event` is signalled and `keys` is set: keys are unwrapped.
    //   - `event` is signalled and `keys` is not set: unwrapping the keys failed.
    event: Event,
    keys: OnceCell<XtsCipherSet>,
}

impl KeyUnwrapper {
    /// Creates an instance with the keys already unwrapped. No background task is spawned.
    pub fn new_from_unwrapped(keys: UnwrappedKeys) -> Self {
        let once = OnceCell::new();
        once.get_or_init(|| XtsCipherSet::new(&keys));
        Self { inner: Arc::new(KeyUnwrapperInner { event: Event::new(), keys: once }) }
    }

    /// Creates an instance of `KeyUnwrapper` and also returns a future that when polled will unwrap
    /// the keys and place them into the `KeyUnwrapper` when they are available.
    pub fn new_from_wrapped(
        object_id: u64,
        crypt: Arc<dyn Crypt>,
        keys: WrappedKeys,
    ) -> (Self, impl Future<Output = ()>) {
        let inner = Arc::new(KeyUnwrapperInner { event: Event::new(), keys: OnceCell::new() });
        let inner2 = inner.clone();
        let future = async move {
            match crypt.unwrap_keys(&keys, object_id).await {
                Ok(keys) => {
                    inner2.keys.get_or_init(|| XtsCipherSet::new(&keys));
                }
                Err(e) => {
                    error!(error=?e, oid=object_id, "Failed to unwrap keys");
                }
            }
            inner2.event.signal();
        };
        (Self { inner }, future)
    }

    pub async fn keys(&self) -> Result<&XtsCipherSet, Error> {
        match self.inner.keys.get() {
            Some(keys) => Ok(keys),
            None => {
                // If the keys are not already unwrapped then wait for the event to be signalled. If
                // the event is signalled and the keys still aren't present then the unwrapping
                // failed.
                self.inner.event.wait().await;
                self.inner.keys.get().ok_or_else(|| anyhow!("Failed to unwrap keys"))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use {
        crate::object_store::KeyUnwrapper,
        anyhow::{anyhow, Error},
        async_trait::async_trait,
        fxfs_crypto::{
            Crypt, KeyPurpose, UnwrappedKey, WrappedKey, WrappedKeyBytes, WrappedKeys, KEY_SIZE,
            WRAPPED_KEY_SIZE,
        },
        std::sync::{Arc, Mutex},
    };

    #[fuchsia::test]
    async fn test_key_unwrapper_new_from_unwrapped() {
        let keys = KeyUnwrapper::new_from_unwrapped(vec![(0, UnwrappedKey::new([0; KEY_SIZE]))]);
        keys.keys().await.expect("keys should be unwrapped");
    }

    struct TestCrypt(Mutex<Option<Result<UnwrappedKey, Error>>>);

    impl TestCrypt {
        fn new(result: Result<UnwrappedKey, Error>) -> Arc<Self> {
            Arc::new(Self(Mutex::new(Some(result))))
        }
    }

    #[async_trait]
    impl Crypt for TestCrypt {
        async fn create_key(
            &self,
            _owner: u64,
            _purpose: KeyPurpose,
        ) -> Result<(WrappedKey, UnwrappedKey), Error> {
            unimplemented!("Not used in tests");
        }

        async fn unwrap_key(
            &self,
            _wrapped_key: &WrappedKey,
            _owner: u64,
        ) -> Result<UnwrappedKey, Error> {
            self.0.lock().unwrap().take().expect("Only 1 key can be unwrapped")
        }
    }

    #[fuchsia::test]
    async fn test_key_unwrapper_new_from_wrapped_wait_for_unwrap() {
        let wrapped_keys = WrappedKeys(vec![(
            0,
            WrappedKey {
                wrapping_key_id: 0x1234567812345678,
                key: WrappedKeyBytes([0xff; WRAPPED_KEY_SIZE]),
            },
        )]);
        let crypt = TestCrypt::new(Ok(UnwrappedKey::new([0; KEY_SIZE])));
        let (keys, future) = KeyUnwrapper::new_from_wrapped(0, crypt, wrapped_keys);

        let key_fut = keys.keys();
        futures::pin_mut!(key_fut);
        assert!(futures::poll!(&mut key_fut).is_pending());

        // Unwrap the keys.
        future.await;

        // The keys should now be available.
        assert!(futures::poll!(&mut key_fut).is_ready());
    }

    #[fuchsia::test]
    async fn test_key_unwrapper_new_from_wrapped_keys_already_unwrapped() {
        let wrapped_keys = WrappedKeys(vec![(
            0,
            WrappedKey {
                wrapping_key_id: 0x1234567812345678,
                key: WrappedKeyBytes([0xff; WRAPPED_KEY_SIZE]),
            },
        )]);
        let crypt = TestCrypt::new(Ok(UnwrappedKey::new([0; KEY_SIZE])));
        let (keys, future) = KeyUnwrapper::new_from_wrapped(0, crypt, wrapped_keys);
        future.await;

        keys.keys().await.unwrap();
    }

    #[fuchsia::test]
    async fn test_key_unwrapper_new_from_wrapped_unwrap_failed() {
        let wrapped_keys = WrappedKeys(vec![(
            0,
            WrappedKey {
                wrapping_key_id: 0x1234567812345678,
                key: WrappedKeyBytes([0xff; WRAPPED_KEY_SIZE]),
            },
        )]);
        let crypt = TestCrypt::new(Err(anyhow!("Unwrap failed")));
        let (keys, future) = KeyUnwrapper::new_from_wrapped(0, crypt, wrapped_keys);
        future.await;

        keys.keys().await.map(|_| ()).expect_err("Unwrapping should have failed");
    }
}
