// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use async_trait::async_trait;
use ffx_command_error::{return_user_error, Result};
use ffx_target::TargetInfoQuery;
use fho::{DeviceLookupDefaultImpl, FhoEnvironment, TryFromEnv};
use fidl_fuchsia_developer_ffx as ffx_fidl;
use std::ops::Deref;

#[derive(Clone, Debug)]
pub struct TargetInfoHolder(ffx_fidl::TargetInfo);

impl Deref for TargetInfoHolder {
    type Target = ffx_fidl::TargetInfo;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl From<ffx_fidl::TargetInfo> for TargetInfoHolder {
    fn from(value: ffx_fidl::TargetInfo) -> Self {
        TargetInfoHolder(value)
    }
}

#[async_trait(?Send)]
impl TryFromEnv for TargetInfoHolder {
    /// Retrieve `TargetInfo` for a target matching a specifier. Fails if more than one target
    /// matches.
    ///
    /// Note that if no target is specified in configuration or on the command-line that this will
    /// end up attempting to connect to all discoverable targets which may be problematic in test or
    /// lab environments.
    async fn try_from_env(env: &FhoEnvironment) -> Result<Self> {
        // Lazy initialization of DeviceLookup implementation.
        let looker = env.lookup().await;

        let lookup = if let Some(ref lookup) = *looker {
            lookup
        } else {
            let l = Box::new(DeviceLookupDefaultImpl);
            env.set_lookup(l.clone()).await;
            return Self::try_from_env(env).await;
        };

        let target_spec = lookup.target_spec(env.environment_context().clone()).await?;
        let targets = lookup
            .resolve_target_query_to_info(
                TargetInfoQuery::from(target_spec.clone()),
                env.environment_context().clone(),
            )
            .await?;

        if targets.is_empty() {
            match target_spec.as_ref() {
                Some(t) => {
                    return_user_error!("Could not discover any targets for specifier '{}'.", t)
                }
                None => return_user_error!("Could not discover any targets."),
            }
        }
        if targets.len() > 1 {
            return_user_error!("Found more than one target: {targets:#?}.");
        } else {
            Ok(targets[0].clone().into())
        }
    }
}

#[cfg(test)]
mod tests {
    use ffx_config::EnvironmentContext;
    use futures::future::LocalBoxFuture;
    use mockall::mock;

    use super::*;

    mock! {
        DeviceLookup{}
        impl fho::DeviceLookup for DeviceLookup{
            fn target_spec(&self, env: EnvironmentContext) -> LocalBoxFuture<'_, Result<Option<String>>>;

            fn resolve_target_query_to_info(
                &self,
                query: TargetInfoQuery,
                env: EnvironmentContext,
            ) -> LocalBoxFuture<'_, Result<Vec<ffx_fidl::TargetInfo>>>;
        }
    }

    #[fuchsia::test]
    async fn test_target_info_try_from_env_too_many_targets_is_error() {
        let config_env = ffx_config::test_init().await.unwrap();
        let mut mock = MockDeviceLookup::new();
        mock.expect_target_spec()
            .times(1)
            .returning(|_e| Box::pin(async { Ok(Some("frobinator".to_string())) }));
        mock.expect_resolve_target_query_to_info().times(1).returning(|_q, _e| {
            Box::pin(async {
                Ok(vec![ffx_fidl::TargetInfo::default(), ffx_fidl::TargetInfo::default()])
            })
        });
        let tool_env = fho::testing::ToolEnv::new()
            .make_environment_with_lookup(config_env.context.clone(), mock);
        let result = TargetInfoHolder::try_from_env(&tool_env).await;
        assert!(result.is_err());
    }

    #[fuchsia::test]
    async fn test_target_info_try_from_env_specifier_with_no_targets_is_error() {
        let config_env = ffx_config::test_init().await.unwrap();
        let mut mock = MockDeviceLookup::new();
        mock.expect_target_spec()
            .times(1)
            .returning(|_e| Box::pin(async { Ok(Some("frobinator".to_string())) }));
        mock.expect_resolve_target_query_to_info()
            .times(1)
            .returning(|_q, _e| Box::pin(async { Ok(vec![]) }));
        let tool_env = fho::testing::ToolEnv::new()
            .make_environment_with_lookup(config_env.context.clone(), mock);
        let result = TargetInfoHolder::try_from_env(&tool_env).await;
        assert!(result.is_err());
    }

    #[fuchsia::test]
    async fn test_target_info_try_from_env_none_is_okay() {
        let config_env = ffx_config::test_init().await.unwrap();
        let mut mock = MockDeviceLookup::new();
        mock.expect_target_spec().times(1).returning(|_e| Box::pin(async { Ok(None) }));
        mock.expect_resolve_target_query_to_info()
            .times(1)
            .returning(|_q, _e| Box::pin(async { Ok(vec![ffx_fidl::TargetInfo::default()]) }));
        let tool_env = fho::testing::ToolEnv::new()
            .make_environment_with_lookup(config_env.context.clone(), mock);
        let info = TargetInfoHolder::try_from_env(&tool_env).await.unwrap();
        assert_eq!(*info, ffx_fidl::TargetInfo::default());
    }

    #[fuchsia::test]
    async fn test_target_info_try_from_env_no_targets_is_error() {
        let config_env = ffx_config::test_init().await.unwrap();
        let mut mock = MockDeviceLookup::new();
        mock.expect_target_spec().times(1).returning(|_e| Box::pin(async { Ok(None) }));
        mock.expect_resolve_target_query_to_info()
            .times(1)
            .returning(|_q, _e| Box::pin(async { Ok(vec![]) }));
        let tool_env = fho::testing::ToolEnv::new()
            .make_environment_with_lookup(config_env.context.clone(), mock);

        let result = TargetInfoHolder::try_from_env(&tool_env).await;
        assert!(result.is_err());
    }
}
