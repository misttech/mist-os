// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::storage::{AssertNoEnv, AssertNoEnvError};
use anyhow::{anyhow, Result};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

// Timeout for the config cache.
pub const CONFIG_CACHE_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug)]
struct CacheItem<T> {
    created: Instant,
    config: Arc<RwLock<T>>,
}

impl<T> CacheItem<T> {
    fn is_cache_item_expired(&self, now: Instant, timeout: Duration) -> bool {
        now.checked_duration_since(self.created).map_or(false, |t| t > timeout)
    }
}

#[derive(Debug)]
pub(crate) struct Cache<T> {
    locker: RwLock<Option<CacheItem<T>>>,
    cache_timeout: Option<Duration>,
}

impl<T> Cache<T> {
    pub fn new(cache_timeout: Option<Duration>) -> Self {
        Self { locker: RwLock::default(), cache_timeout }
    }
}

impl<T> Default for Cache<T> {
    fn default() -> Self {
        Self { locker: RwLock::default(), cache_timeout: Some(CONFIG_CACHE_TIMEOUT) }
    }
}

impl<T: AssertNoEnv + Default> AssertNoEnv for Cache<T> {
    fn assert_no_env(
        &self,
        preamble: Option<String>,
        ctx: &crate::EnvironmentContext,
    ) -> Result<(), AssertNoEnvError> {
        load_config(self, || Ok(T::default()))
            .map_err(|e| AssertNoEnvError::Unexpected(e.into()))?;
        let config = self.locker.read().expect("cache read mutex poisoned");
        let defaults = config.as_ref().expect("config did not load");
        let default_config = defaults.config.read().expect("config read mutex poisoned");
        default_config.assert_no_env(preamble, ctx)
    }
}

impl Cache<crate::storage::Config> {
    /// Overwrites the default config with a specific `ConfigMap`. This is intended for use-cases
    /// in which the config needs flattening.
    pub(crate) fn overwrite_default(&self, overwrite: &crate::storage::ConfigMap) -> Result<()> {
        load_config(self, || Ok(crate::storage::Config::default()))?;
        let config = self.locker.read().expect("cache read mutex poisoned");
        let defaults = config.as_ref().expect("config did not load");
        let mut defaults_config = defaults.config.write().expect("config write mutex poisoned");
        crate::api::value::merge_map(&mut defaults_config.default, overwrite);

        Ok(())
    }
}

/// Invalidate the cache. Call this if you do anything that might make a cached config go stale
/// in a critical way, like changing the environment.
pub(crate) async fn invalidate<T>(cache: &Cache<T>) {
    *cache.locker.write().expect("config write guard") = None;
}

fn read_cache<T>(
    guard: &impl std::ops::Deref<Target = Option<CacheItem<T>>>,
    now: Instant,
    timeout: Option<Duration>,
) -> Option<Arc<RwLock<T>>> {
    guard
        .as_ref()
        .filter(|item| match timeout {
            None => true,
            Some(t) => !item.is_cache_item_expired(now, t),
        })
        .map(|item| item.config.clone())
}

pub(crate) fn load_config<T>(
    cache: &Cache<T>,
    new_config: impl FnOnce() -> Result<T>,
) -> Result<Arc<RwLock<T>>> {
    load_config_with_instant(Instant::now(), cache, new_config)
}

fn load_config_with_instant<T>(
    now: Instant,
    cache: &Cache<T>,
    new_config: impl FnOnce() -> Result<T>,
) -> Result<Arc<RwLock<T>>> {
    let cache_hit = {
        let guard = cache.locker.read().map_err(|_| anyhow!("config read guard"))?;
        read_cache(&guard, now, cache.cache_timeout)
    };
    match cache_hit {
        Some(h) => Ok(h),
        None => {
            let mut guard = cache.locker.write().map_err(|_| anyhow!("config write guard"))?;
            let config = Arc::new(RwLock::new(new_config()?));

            *guard = Some(CacheItem { created: now, config: config.clone() });
            Ok(config)
        }
    }
}

////////////////////////////////////////////////////////////////////////////////
// tests
#[cfg(test)]
mod test {
    use super::*;

    async fn load(now: Instant, cache: &Cache<usize>) {
        let tests = 25;
        let mut result = Vec::new();
        for x in 0..tests {
            result.push(load_config_with_instant(now, cache, move || Ok(x)));
        }
        assert_eq!(tests, result.len());
        result.iter().for_each(|x| {
            assert!(x.is_ok());
        });
    }

    async fn load_and_test(
        now: Instant,
        expected_before: bool,
        expected_after: bool,
        cache: &Cache<usize>,
    ) {
        {
            let read_guard = cache.locker.read().expect("config read guard");
            assert_eq!(expected_before, (*read_guard).is_some());
        }
        load(now, cache).await;
        {
            let read_guard = cache.locker.read().expect("config read guard");
            assert_eq!(expected_after, (*read_guard).is_some());
        }
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_config_timeout() {
        let now = Instant::now();
        let cache = Cache::default();
        load_and_test(now, false, true, &cache).await;
        let timeout = now.checked_add(CONFIG_CACHE_TIMEOUT).expect("timeout should not overflow");
        let after_timeout = timeout
            .checked_add(Duration::from_millis(1))
            .expect("after timeout should not overflow");
        load_and_test(timeout, true, true, &cache).await;
        load_and_test(after_timeout, true, true, &cache).await;
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_expiration_check_does_not_panic() -> Result<()> {
        let now = Instant::now();
        let later = now.checked_add(Duration::from_millis(1)).expect("timeout should not overflow");
        let item = CacheItem { created: later, config: Arc::new(RwLock::new(1)) };
        assert!(!item.is_cache_item_expired(now, CONFIG_CACHE_TIMEOUT));
        Ok(())
    }
}
