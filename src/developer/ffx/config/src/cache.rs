// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{environment::Environment, storage::Config, BuildOverride};
use anyhow::Result;
use async_lock::{RwLock, RwLockUpgradableReadGuard};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

// Timeout for the config cache.
pub const CONFIG_CACHE_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug)]
struct CacheItem {
    created: Instant,
    config: Arc<RwLock<Config>>,
}

#[derive(Debug, Default)]
pub(crate) struct Cache(RwLock<HashMap<Option<PathBuf>, CacheItem>>);

/// Invalidate the cache. Call this if you do anything that might make a cached config go stale
/// in a critical way, like changing the environment.
pub(crate) async fn invalidate(cache: &Cache) {
    cache.0.write().await.clear();
}

fn is_cache_item_expired(item: &CacheItem, now: Instant) -> bool {
    now.checked_duration_since(item.created).map_or(false, |t| t > CONFIG_CACHE_TIMEOUT)
}

async fn read_cache(
    guard: &impl std::ops::Deref<Target = HashMap<Option<PathBuf>, CacheItem>>,
    build_dir: Option<&Path>,
    now: Instant,
) -> Option<Arc<RwLock<Config>>> {
    guard
        .get(&build_dir.map(Path::to_owned)) // TODO(mgnb): get rid of this allocation when we can
        .filter(|item| !is_cache_item_expired(item, now))
        .map(|item| item.config.clone())
}

pub(crate) async fn load_config(
    environment: Environment,
    build_override: Option<BuildOverride<'_>>,
) -> Result<Arc<RwLock<Config>>> {
    load_config_with_instant(
        &environment,
        build_override,
        Instant::now(),
        &environment.context().cache,
    )
    .await
}

async fn load_config_with_instant(
    env: &Environment,
    build_override: Option<BuildOverride<'_>>,
    now: Instant,
    cache: &Cache,
) -> Result<Arc<RwLock<Config>>> {
    let guard = cache.0.upgradable_read().await;
    let build_dir = env.override_build_dir(build_override);
    let cache_hit = read_cache(&guard, build_dir, now).await;
    match cache_hit {
        Some(h) => Ok(h),
        None => {
            let mut guard = RwLockUpgradableReadGuard::upgrade(guard).await;
            let config = Arc::new(RwLock::new(Config::from_env(&env)?));

            guard.insert(
                build_dir.map(Path::to_owned), // TODO(mgnb): get rid of this allocation when we can,
                CacheItem { created: now, config: config.clone() },
            );
            Ok(config)
        }
    }
}

////////////////////////////////////////////////////////////////////////////////
// tests
#[cfg(test)]
mod test {
    use super::*;
    use crate::EnvironmentContext;
    use futures::future::join_all;
    use std::time::Duration;

    async fn load(now: Instant, key: &Option<PathBuf>, cache: &Cache) {
        let env = Environment::new_empty(EnvironmentContext::default())
            .await
            .expect("Empty and anonymous environment shouldn't fail to load");
        let tests = 25;
        let mut futures = Vec::new();
        for _x in 0..tests {
            futures.push(load_config_with_instant(
                &env,
                key.as_deref().map(BuildOverride::Path),
                now,
                cache,
            ));
        }
        let result = join_all(futures).await;
        assert_eq!(tests, result.len());
        result.iter().for_each(|x| {
            assert!(x.is_ok());
        });
    }

    async fn load_and_test(
        now: Instant,
        expected_len_before: usize,
        expected_len_after: usize,
        key: &Option<PathBuf>,
        cache: &Cache,
    ) {
        {
            let read_guard = cache.0.read().await;
            assert_eq!(expected_len_before, (*read_guard).len());
        }
        load(now, key, cache).await;
        {
            let read_guard = cache.0.read().await;
            assert_eq!(expected_len_after, (*read_guard).len());
        }
    }

    fn setup_build_dirs(tests: usize) -> Vec<Option<PathBuf>> {
        let mut build_dirs = Vec::new();
        build_dirs.push(None);
        for x in 0..tests - 1 {
            build_dirs.push(Some(PathBuf::from(format!("test {}", x))));
        }
        build_dirs
    }

    fn setup(tests: usize) -> (Instant, Vec<Option<PathBuf>>, Cache) {
        (Instant::now(), setup_build_dirs(tests), Cache::default())
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_config_one_at_a_time() {
        let _test_env = crate::test_init().await.unwrap();
        let tests = 10;
        let (now, build_dirs, cache) = setup(tests);
        for x in 0..tests {
            load_and_test(now, x, x + 1, &build_dirs[x], &cache).await;
        }
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_config_many_at_a_time() {
        let _test_env = crate::test_init().await.unwrap();
        let tests = 25;
        let (now, build_dirs, cache) = setup(tests);
        let futures = build_dirs.iter().map(|x| load(now, x, &cache));
        let result = join_all(futures).await;
        assert_eq!(tests, result.len());
        let read_guard = cache.0.read().await;
        assert_eq!(tests, (*read_guard).len());
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_config_timeout() {
        let _test_env = crate::test_init().await.unwrap();
        let tests = 1;
        let (now, build_dirs, cache) = setup(tests);
        load_and_test(now, 0, 1, &build_dirs[0], &cache).await;
        let timeout = now.checked_add(CONFIG_CACHE_TIMEOUT).expect("timeout should not overflow");
        let after_timeout = timeout
            .checked_add(Duration::from_millis(1))
            .expect("after timeout should not overflow");
        load_and_test(timeout, 1, 1, &build_dirs[0], &cache).await;
        load_and_test(after_timeout, 1, 1, &build_dirs[0], &cache).await;
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_expiration_check_does_not_panic() -> Result<()> {
        let now = Instant::now();
        let later = now.checked_add(Duration::from_millis(1)).expect("timeout should not overflow");
        let cfg = Config::from_env(&Environment::new_empty(EnvironmentContext::default()).await?)?;
        let item = CacheItem { created: later, config: Arc::new(RwLock::new(cfg)) };
        assert!(!is_cache_item_expired(&item, now));
        Ok(())
    }
}
