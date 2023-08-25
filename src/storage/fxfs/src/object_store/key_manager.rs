// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::log::*,
    anyhow::{bail, Error},
    event_listener::Event,
    fuchsia_async as fasync,
    futures::future,
    fxfs_crypto::{Crypt, UnwrappedKeys, WrappedKeys, XtsCipherSet},
    std::{
        cell::UnsafeCell,
        collections::{btree_map::Entry, BTreeMap},
        future::Future,
        pin::pin,
        sync::{Arc, Mutex},
        time::Duration,
    },
};

/// This timeout controls when entries are moved from `hash` to `pending_purge` and then dumped.
/// Entries will remain in the cache until they remain inactive from between PURGE_TIMEOUT and 2 *
/// PURGE_TIMEOUT.  This is deliberately set to 37 seconds (rather than a round number) to reduce
/// the chance that this timer ends up always firing at the same time as other timers.
const PURGE_TIMEOUT: Duration = Duration::from_secs(37);

/// A simple cache that purges entries periodically when `purge` is called.  The API is similar to
/// HashMap's.
struct Cache<V> {
    hash: BTreeMap<u64, V>,
    pending_purge: BTreeMap<u64, V>,
    permanent: BTreeMap<u64, V>,
}

impl<V> Cache<V> {
    fn new() -> Self {
        Self { hash: BTreeMap::new(), pending_purge: BTreeMap::new(), permanent: BTreeMap::new() }
    }

    fn get(&mut self, key: u64) -> Option<&V> {
        match self.hash.entry(key) {
            Entry::Occupied(o) => Some(o.into_mut()),
            Entry::Vacant(v) => {
                // If we find an entry in `pending_purge`, move it into `hash`.
                if let Some(value) = self.pending_purge.remove(&key) {
                    Some(v.insert(value))
                } else {
                    self.permanent.get(&key)
                }
            }
        }
    }

    fn insert(&mut self, key: u64, value: V, permanent: bool) {
        if permanent {
            self.permanent.insert(key, value);
        } else {
            self.hash.insert(key, value);
        }
    }

    /// This purges entries that haven't been accessed since the last call to purge.
    ///
    /// Returns true if the cache no longer has any purgeable entries.
    fn purge(&mut self) -> bool {
        self.pending_purge = std::mem::take(&mut self.hash);
        self.pending_purge.is_empty()
    }

    fn clear(&mut self) {
        self.hash.clear();
        self.pending_purge.clear();
        self.permanent.clear();
    }

    fn remove(&mut self, key: u64) {
        self.hash.remove(&key);
        self.pending_purge.remove(&key);
        self.permanent.remove(&key);
    }
}

pub struct KeyManager {
    inner: Arc<Mutex<Inner>>,
}

struct Inner {
    keys: Cache<Arc<XtsCipherSet>>,
    unwrapping: BTreeMap<u64, Arc<UnwrapResult>>,
    purge_task: Option<fasync::Task<()>>,
}

impl Inner {
    fn start_purge_task(&mut self, inner: &Arc<Mutex<Inner>>) {
        self.purge_task.get_or_insert_with(move || {
            let inner = inner.clone();
            fasync::Task::spawn(async move {
                loop {
                    fasync::Timer::new(PURGE_TIMEOUT).await;
                    let mut inner = inner.lock().unwrap();
                    if inner.keys.purge() {
                        inner.purge_task = None;
                        break;
                    }
                }
            })
        });
    }
}

struct UnwrapResult {
    event: Event,
    error: UnsafeCell<bool>,
}

impl UnwrapResult {
    fn new() -> Arc<Self> {
        Arc::new(UnwrapResult { event: Event::new(), error: UnsafeCell::new(false) })
    }

    fn set(
        &self,
        inner: &Arc<Mutex<Inner>>,
        object_id: u64,
        permanent: bool,
        result: Result<Option<Arc<XtsCipherSet>>, Error>,
    ) {
        if let Err(error) = &result {
            // SAFETY: This is safe because we have exclusive access until we call notify below.
            unsafe {
                *self.error.get() = true;
            }
            error!(?error, oid = object_id, "Failed to unwrap keys");
        }
        let mut guard = inner.lock().unwrap();
        if let Entry::Occupied(o) = guard.unwrapping.entry(object_id) {
            if std::ptr::eq(Arc::as_ptr(o.get()), self) {
                o.remove();
                if let Ok(Some(keys)) = &result {
                    guard.keys.insert(object_id, keys.clone(), permanent);
                    guard.start_purge_task(inner);
                }
            }
        }
        self.event.notify(usize::MAX);
    }
}

unsafe impl Send for UnwrapResult {}
unsafe impl Sync for UnwrapResult {}

impl KeyManager {
    pub fn new() -> Self {
        let inner = Arc::new(Mutex::new(Inner {
            keys: Cache::new(),
            unwrapping: BTreeMap::new(),
            purge_task: None,
        }));

        Self { inner }
    }

    /// Retrieves a key from the cache but won't initiate unwrapping if no key is present.  If the
    /// key is currently in the process of being unwrapped, this will wait until that has finished.
    /// This should be used with permanent keys.
    pub async fn get(&self, object_id: u64) -> Result<Option<Arc<XtsCipherSet>>, Error> {
        loop {
            let (unwrap_result, listener) = {
                let mut inner = self.inner.lock().unwrap();

                if let Some(keys) = inner.keys.get(object_id) {
                    return Ok(Some(keys.clone()));
                }
                let unwrap_result = match inner.unwrapping.entry(object_id) {
                    Entry::Vacant(_) => return Ok(None),
                    Entry::Occupied(o) => o.get().clone(),
                };
                let listener = unwrap_result.event.listen();
                (unwrap_result, listener)
            };
            listener.await;
            // SAFETY: This is safe because there can be no mutations happening at this point.
            if unsafe { *unwrap_result.error.get() } {
                bail!("Failed to unwrap keys");
            }
        }
    }

    /// This retrieves keys from the cache or initiates unwrapping if they are not in the cache.
    pub fn get_or_insert(
        &self,
        object_id: u64,
        crypt: Arc<dyn Crypt>,
        wrapped_keys: impl Future<Output = Result<WrappedKeys, Error>>,
        permanent: bool,
    ) -> impl Future<Output = Result<Arc<XtsCipherSet>, Error>> {
        let inner = self.inner.clone();
        async move {
            let mut wrapped_keys = pin!(future::maybe_done(wrapped_keys));

            loop {
                let (unwrap_result, listener) = {
                    let mut inner = inner.lock().unwrap();

                    if let Some(keys) = inner.keys.get(object_id) {
                        return Ok(keys.clone());
                    }

                    match inner.unwrapping.entry(object_id) {
                        Entry::Vacant(v) => {
                            let unwrap_result = UnwrapResult::new();
                            v.insert(unwrap_result.clone());
                            (unwrap_result, None)
                        }
                        Entry::Occupied(o) => {
                            let unwrap_result = o.get().clone();
                            let listener = unwrap_result.event.listen();
                            (unwrap_result, Some(listener))
                        }
                    }
                };
                if let Some(listener) = listener {
                    listener.await;
                    // SAFETY: This is safe because there can be no mutations happening at this
                    // point.
                    if !unsafe { *unwrap_result.error.get() } {
                        // Loop around and try and get the key.
                        continue;
                    }
                } else {
                    // Use a guard in case we're dropped.
                    let mut result = scopeguard::guard(Ok(None), |result| {
                        unwrap_result.set(&inner, object_id, permanent, result);
                    });

                    wrapped_keys.as_mut().await;
                    let error = match wrapped_keys.as_mut().output_mut().unwrap() {
                        Ok(wrapped_keys) => {
                            match crypt.unwrap_keys(wrapped_keys, object_id).await {
                                Ok(unwrapped_keys) => {
                                    let keys = unwrapped_keys.to_cipher_set();
                                    *result = Ok(Some(keys.clone()));
                                    return Ok(keys);
                                }
                                Err(e) => e,
                            }
                        }
                        Err(_) => wrapped_keys.take_output().unwrap().unwrap_err(),
                    };
                    *result = Err(error);
                }
                bail!("Failed to unwrap keys");
            }
        }
    }

    /// This inserts the keys into the cache.  Any existing keys will be overwritten.  It's
    /// unspecified what happens if keys for the object are currently being unwrapped.
    pub fn insert(&self, object_id: u64, keys: impl ToCipherSet, permanent: bool) {
        let mut inner = self.inner.lock().unwrap();
        inner.keys.insert(object_id, keys.to_cipher_set(), permanent);
        inner.start_purge_task(&self.inner);
    }

    pub fn remove(&self, object_id: u64) {
        self.inner.lock().unwrap().keys.remove(object_id);
    }

    /// This clears the caches of all keys.
    pub fn clear(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.keys.clear();
        inner.unwrapping.clear();
    }
}

pub trait ToCipherSet {
    fn to_cipher_set(self) -> Arc<XtsCipherSet>;
}

impl ToCipherSet for &UnwrappedKeys {
    fn to_cipher_set(self) -> Arc<XtsCipherSet> {
        Arc::new(XtsCipherSet::new(self))
    }
}

impl ToCipherSet for Arc<XtsCipherSet> {
    fn to_cipher_set(self) -> Arc<XtsCipherSet> {
        self
    }
}

#[cfg(target_os = "fuchsia")]
#[cfg(test)]
mod tests {
    use {
        super::{KeyManager, PURGE_TIMEOUT},
        anyhow::{anyhow, Error},
        async_trait::async_trait,
        fuchsia_async::{self as fasync, TestExecutor, Time},
        fuchsia_zircon as zx,
        fxfs_crypto::{
            Crypt, KeyPurpose, UnwrappedKey, WrappedKey, WrappedKeyBytes, WrappedKeys,
            XtsCipherSet, KEY_SIZE, WRAPPED_KEY_SIZE,
        },
        std::sync::{
            atomic::{AtomicU8, Ordering},
            Arc,
        },
    };

    const PLAIN_TEXT: &[u8] = b"The quick brown fox jumps over the lazy dog";
    const ERROR_COUNTER: u8 = 0xff;

    fn unwrapped_key(counter: u8) -> UnwrappedKey {
        UnwrappedKey::new(vec![counter; KEY_SIZE].try_into().unwrap())
    }

    fn cipher_text(counter: u8) -> Vec<u8> {
        let mut text = PLAIN_TEXT.to_vec();
        XtsCipherSet::new(&vec![(0, unwrapped_key(counter))])
            .encrypt(0, 0, &mut text)
            .expect("encrypt failed");
        text
    }

    fn wrapped_keys() -> WrappedKeys {
        WrappedKeys(vec![(
            0,
            WrappedKey {
                wrapping_key_id: 0x1234567812345678,
                key: WrappedKeyBytes([0xff; WRAPPED_KEY_SIZE]),
            },
        )])
    }

    struct TestCrypt(AtomicU8);

    impl TestCrypt {
        fn new(counter: u8) -> Arc<Self> {
            Arc::new(Self(AtomicU8::new(counter)))
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
            fasync::Timer::new(std::time::Duration::from_secs(1)).await;
            let counter = self.0.fetch_add(1, Ordering::Relaxed);
            if counter == ERROR_COUNTER {
                Err(anyhow!("Unwrap failed!"))
            } else {
                Ok(unwrapped_key(counter))
            }
        }
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn test_get_or_insert() {
        TestExecutor::advance_to(Time::from_nanos(0)).await;

        let crypt = TestCrypt::new(0);
        let manager1 = Arc::new(KeyManager::new());
        let manager2 = manager1.clone();
        let manager3 = manager1.clone();
        let crypt1 = crypt.clone();
        let crypt2 = crypt.clone();

        let task1 = fasync::Task::spawn(async move {
            let mut buf = cipher_text(0);
            manager1
                .get_or_insert(1, crypt1, async { Ok(wrapped_keys()) }, false)
                .await
                .expect("get_or_insert failed")
                .decrypt(0, 0, &mut buf)
                .expect("decrypt failed");
            assert_eq!(&buf, PLAIN_TEXT);
        });
        let task2 = fasync::Task::spawn(async move {
            let mut buf = cipher_text(0);
            manager2
                .get_or_insert(1, crypt2, async { Ok(wrapped_keys()) }, false)
                .await
                .expect("get_or_insert failed")
                .decrypt(0, 0, &mut buf)
                .expect("decrypt failed");
            assert_eq!(&buf, PLAIN_TEXT);
        });
        let task3 = fasync::Task::spawn(async move {
            // Make sure this starts after the get_or_inserts.
            fasync::Timer::new(zx::Duration::from_millis(500)).await;
            let mut buf = cipher_text(0);
            manager3
                .get(1)
                .await
                .expect("get failed")
                .expect("missing key")
                .decrypt(0, 0, &mut buf)
                .expect("decrypt failed");
            assert_eq!(&buf, PLAIN_TEXT);
        });

        TestExecutor::advance_to(Time::after(zx::Duration::from_millis(1500))).await;

        task1.await;
        task2.await;
        task3.await;
    }

    #[fuchsia::test]
    async fn test_insert_and_remove() {
        let manager = Arc::new(KeyManager::new());

        manager.insert(1, &vec![(0, unwrapped_key(0))], false);
        let mut buf = cipher_text(0);
        manager
            .get(1)
            .await
            .expect("get failed")
            .expect("missing key")
            .decrypt(0, 0, &mut buf)
            .expect("decrypt failed");
        assert_eq!(&buf, PLAIN_TEXT);
        manager.remove(1);
        assert!(manager.get(1).await.expect("get failed").is_none());
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn test_purge() {
        TestExecutor::advance_to(Time::from_nanos(0)).await;

        let manager = Arc::new(KeyManager::new());
        manager.insert(1, &vec![(0, unwrapped_key(0))], false);

        TestExecutor::advance_to(Time::after(PURGE_TIMEOUT.into())).await;

        // After 1 period, the key should still be present.
        manager.get(1).await.expect("get failed").expect("missing key");

        TestExecutor::advance_to(Time::after(PURGE_TIMEOUT.into())).await;

        // The last access should have reset the timer and it should still be present.
        manager.get(1).await.expect("get failed").expect("missing key");

        TestExecutor::advance_to(Time::after((2 * PURGE_TIMEOUT).into())).await;

        // The key should have been evicted since two periods passed.
        assert!(manager.get(1).await.expect("get failed").is_none());
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn test_permanent() {
        TestExecutor::advance_to(Time::from_nanos(0)).await;

        let manager = Arc::new(KeyManager::new());
        manager.insert(1, &vec![(0, unwrapped_key(0))], true);
        manager.insert(2, &vec![(0, unwrapped_key(0))], false);

        // Skip forward two periods which should cause 2 to be purged but not 1.
        TestExecutor::advance_to(Time::after((2 * PURGE_TIMEOUT).into())).await;

        assert!(manager.get(1).await.expect("get failed").is_some());
        assert!(manager.get(2).await.expect("get failed").is_none());
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn test_clear() {
        TestExecutor::advance_to(Time::from_nanos(0)).await;

        let manager = Arc::new(KeyManager::new());
        manager.insert(1, &vec![(0, unwrapped_key(0))], true);
        manager.insert(2, &vec![(0, unwrapped_key(0))], false);
        manager.insert(3, &vec![(0, unwrapped_key(0))], false);

        // Skip forward 1 period which should make keys 2 and 3 pending deletion.
        TestExecutor::advance_to(Time::after(PURGE_TIMEOUT.into())).await;

        // Touch the the second key which should promote it to the active list.
        assert!(manager.get(2).await.expect("get failed").is_some());

        manager.clear();

        // Clearing should have removed all three keys.
        assert!(manager.get(1).await.expect("get failed").is_none());
        assert!(manager.get(2).await.expect("get failed").is_none());
        assert!(manager.get(3).await.expect("get failed").is_none());
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn test_error() {
        TestExecutor::advance_to(Time::from_nanos(0)).await;

        let crypt = TestCrypt::new(ERROR_COUNTER);
        let manager1 = Arc::new(KeyManager::new());
        let manager2 = manager1.clone();
        let crypt1 = crypt.clone();
        let crypt2 = crypt.clone();

        let task1 = fasync::Task::spawn(async move {
            assert!(manager1
                .get_or_insert(1, crypt1, async { Ok(wrapped_keys()) }, false,)
                .await
                .is_err());
        });
        let task2 = fasync::Task::spawn(async move {
            assert!(manager2
                .get_or_insert(1, crypt2, async { Ok(wrapped_keys()) }, false,)
                .await
                .is_err());
        });

        TestExecutor::advance_to(Time::after(zx::Duration::from_seconds(1))).await;

        task1.await;
        task2.await;
    }
}
