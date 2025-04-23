// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::logging::LogDestination;
use crate::nested::nested_set;
use crate::{ConfigMap, Environment, EnvironmentContext};
use anyhow::{Context, Result};
use serde_json::Value;
use std::cell::Cell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tempfile::{NamedTempFile, TempDir};
use tracing::level_filters::LevelFilter;

use super::{EnvVars, EnvironmentKind, ExecutableKind};

/// A structure that holds information about the test config environment for the duration
/// of a test. This object must continue to exist for the duration of the test, or the test
/// may fail.
#[must_use = "This object must be held for the duration of a test (ie. `let _env = ffx_config::test_init()`) for it to operate correctly."]
pub struct TestEnv {
    pub env_file: NamedTempFile,
    pub context: EnvironmentContext,
    pub isolate_root: TempDir,
    pub user_file: NamedTempFile,
    pub build_file: Option<NamedTempFile>,
    pub global_file: NamedTempFile,
    pub log_subscriber: Arc<dyn tracing::Subscriber + Send + Sync>,
    _guard: async_lock::MutexGuardArc<()>,
}

impl TestEnv {
    async fn new_isolated(
        guard: async_lock::MutexGuardArc<()>,
        env_vars: EnvVars,
        runtime_args: ConfigMap,
    ) -> Result<Self> {
        let env_file = NamedTempFile::new().context("tmp access failed")?;
        let isolate_root = tempfile::tempdir()?;

        let context = EnvironmentContext::isolated(
            ExecutableKind::Test,
            isolate_root.path().to_owned(),
            env_vars,
            runtime_args,
            Some(env_file.path().to_owned()),
            None,
            false,
        )?;
        Self::build_test_env(context, env_file, isolate_root, guard).await
    }

    async fn new_intree(
        build_dir: &Path,
        guard: async_lock::MutexGuardArc<()>,
        env_vars: EnvVars,
        runtime_args: ConfigMap,
    ) -> Result<Self> {
        let env_file = NamedTempFile::new().context("tmp access failed")?;
        let isolate_root = tempfile::tempdir()?;

        let context = EnvironmentContext::new(
            EnvironmentKind::InTree {
                tree_root: isolate_root.path().to_owned(),
                build_dir: Some(PathBuf::from(build_dir)),
            },
            ExecutableKind::Test,
            Some(env_vars),
            runtime_args,
            Some(env_file.path().to_owned()),
            false,
        );
        Self::build_test_env(context, env_file, isolate_root, guard).await
    }

    async fn build_test_env(
        context: EnvironmentContext,
        env_file: NamedTempFile,
        isolate_root: TempDir,
        guard: async_lock::MutexGuardArc<()>,
    ) -> Result<Self> {
        let global_file = NamedTempFile::new().context("tmp access failed")?;
        let global_file_path = global_file.path().to_owned();
        let user_file = NamedTempFile::new().context("tmp access failed")?;
        let user_file_path = user_file.path().to_owned();
        let build_file =
            context.build_dir().and(Some(NamedTempFile::new().context("tmp access failed")?));

        let log_subscriber: Arc<dyn tracing::Subscriber + Send + Sync> =
            Arc::new(crate::logging::configure_subscribers(
                &context,
                vec![LogDestination::TestWriter],
                LevelFilter::DEBUG,
            ));

        // Dropping the subscriber guard causes test flakes as the tracing library panics when
        // closing an instrumentation span on a different subscriber.
        // To mitigate this, only drop the guards at thread exit.
        // See https://github.com/tokio-rs/tracing/issues/1656 for more details.
        let log_guard = tracing::subscriber::set_default(Arc::clone(&log_subscriber));

        thread_local! {
            static GUARD_STASH: Cell<Option<tracing::subscriber::DefaultGuard>> =
                const { Cell::new(None) };
        }

        GUARD_STASH.with(move |guard| guard.set(Some(log_guard)));

        let test_env = TestEnv {
            env_file,
            context,
            user_file,
            build_file,
            global_file,
            isolate_root,
            log_subscriber,
            _guard: guard,
        };

        let mut env = Environment::new_empty(test_env.context.clone());

        env.set_user(Some(&user_file_path));
        if let Some(ref build_file) = test_env.build_file {
            let build_file_path = build_file.path().to_owned();
            env.set_build(&build_file_path)?;
        }
        env.set_global(Some(&global_file_path));
        env.save().await.context("saving env file")?;

        Ok(test_env)
    }

    pub fn load(&self) -> Environment {
        self.context.load().expect("opening test env file")
    }
}

impl Drop for TestEnv {
    fn drop(&mut self) {
        // after the test, wipe out all the test configuration we set up. Explode if things aren't as we
        // expect them.
        let mut env = crate::ENV.lock().expect("Poisoned lock");
        let env_prev = env.clone();
        *env = None;
        drop(env);

        if let Some(env_prev) = env_prev {
            assert_eq!(
                env_prev,
                self.context,
                "environment context changed from isolated environment to {other:?} during test run somehow.",
                other = env_prev
            );
        }

        // since we're not running in async context during drop, we can't clear the cache unfortunately.
    }
}

lazy_static::lazy_static! {
    static ref TEST_LOCK: Arc<async_lock::Mutex<()>> = Arc::default();
}

#[derive(Debug, Default)]
pub struct TestEnvBuilder {
    build_dir: Option<PathBuf>,
    env_vars: EnvVars,
    runtime_config: ConfigMap,
}

/// Creates a TestEnvBuilder with the following defaults:
///  - Backed by `EnvironmentKind::Isolated`,
///  - Does not inherit any environment variables, and
///  - is initialized with an empty runtime configuration.
pub fn test_env() -> TestEnvBuilder {
    TestEnvBuilder { ..Default::default() }
}

impl TestEnvBuilder {
    /// Switches the final built TestEnv to be backed by
    /// `EnvironmentKind::in_tree`.
    /// This also allows ConfigLevel::Build to be used in tests.
    pub fn in_tree(mut self, build_dir: &Path) -> Self {
        self.build_dir = Some(build_dir.into());
        self
    }

    /// Sets a single environment variable on the resulting
    /// `TestEnv.context.env_vars`.
    pub fn env_var(mut self, key: &str, value: &str) -> Self {
        self.env_vars.insert(key.into(), value.into());
        self
    }

    /// Sets a key to a value in the runtime config.
    /// Keys are allowed to be nested, meaning that keys like "target.default"
    /// or "repository.server.mode" are valid.
    pub fn runtime_config<T>(mut self, key: &str, value: T) -> Self
    where
        T: Into<Value>,
    {
        let key_vec: Vec<&str> = key.split('.').collect();
        nested_set(&mut self.runtime_config, key_vec[0], &key_vec[1..], value.into());
        self
    }

    /// Builds a TestEnv backed by EnvironmentKind::Isolated by default, else
    /// EnvironmentKind::InTree if a `.in_tree()` is specified.
    ///
    /// You must hold the returned object object for the duration of the test.
    /// Not doing so will result in strange behaviour.
    pub async fn build(self) -> Result<TestEnv> {
        let env = match self.build_dir {
            Some(build_dir) => {
                TestEnv::new_intree(
                    build_dir.as_path(),
                    TEST_LOCK.lock_arc().await,
                    self.env_vars,
                    self.runtime_config,
                )
                .await
            }
            None => {
                TestEnv::new_isolated(
                    TEST_LOCK.lock_arc().await,
                    self.env_vars,
                    self.runtime_config,
                )
                .await
            }
        }?;

        // Force an overwrite of the configuration setup.
        crate::init(&env.context)?;

        Ok(env)
    }
}

/// When running tests we typically want to initialize a blank slate
/// configuration, so use this for tests.
///
/// For more complex use-cases (eg: in-tree, environment variables, runtime
/// configuration), use `test_env()` instead.
///
/// You must hold the returned object object for the duration of the test.
/// Not doing so will result in strange behaviour.
///
/// FIXME(https://fxbug.dev/411199300): This function inherits environment
/// variables from the real test environment.
pub async fn test_init() -> Result<TestEnv> {
    // TODO(https://fxbug.dev/411199300): Use `test_env()` when we don't need to
    // implicitly inherit environment variables from the real test environment
    // environment anymore.
    (TestEnvBuilder { env_vars: HashMap::from_iter(std::env::vars()), ..Default::default() })
        .build()
        .await
}
