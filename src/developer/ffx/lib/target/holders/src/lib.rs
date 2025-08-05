// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::sync::Arc;
use std::time::Duration;

use daemon_proxy::Injection;
use ffx_command_error::Result;
use ffx_config::EnvironmentContext;
use ffx_target::fho::FhoConnectionBehavior;
use fho::{FhoEnvironment, TryFromEnv as _};
use fidl::encoding::DefaultFuchsiaResourceDialect;
use fidl::endpoints::Proxy;
use target_network_connector::NetworkConnector;

mod daemon_proxy;
mod fake_injector;
pub mod fdomain;
mod from_toolbox;
mod remote_control_proxy;
mod target_info;
mod target_proxy;
mod with_moniker;

pub use daemon_proxy::{daemon_protocol, DaemonProxyHolder};
pub use fake_injector::FakeInjector;
use from_toolbox::WithToolbox;
pub use from_toolbox::{toolbox, toolbox_or};
pub use remote_control_proxy::{fake_async_proxy, fake_proxy, RemoteControlProxyHolder};
pub use target_info::TargetInfoHolder;
pub use target_proxy::TargetProxyHolder;

const DEFAULT_PROXY_TIMEOUT: Duration = Duration::from_secs(15);

/// Explicitly create direct connection behavior
pub async fn init_direct_connection_behavior(
    context: &EnvironmentContext,
) -> Result<FhoConnectionBehavior> {
    log::info!("Initializing FhoConnectionBehavior::DirectConnector");
    let connector =
        NetworkConnector::<ffx_target::ssh_connector::SshConnector>::new(context).await?;
    Ok(FhoConnectionBehavior::DirectConnector(Arc::new(connector)))
}

pub async fn init_connection_behavior(
    context: &EnvironmentContext,
) -> Result<FhoConnectionBehavior> {
    if context.is_strict() || context.get_direct_connection_mode() {
        log::info!("Initializing FhoConnectionBehavior::DirectConnector");
        let connector =
            NetworkConnector::<ffx_target::ssh_connector::SshConnector>::new(context).await?;
        Ok(FhoConnectionBehavior::DirectConnector(Arc::new(connector)))
    } else {
        let build_info = context.build_info();
        let overnet_injector =
            Injection::initialize_overnet(context.clone(), None, build_info).await?;
        log::info!("Initializing FhoConnectionBehavior::DaemonConnector");
        Ok(FhoConnectionBehavior::DaemonConnector(Arc::new(overnet_injector)))
    }
}

/// Explicitly create daemon connection behavior, for subtools such as `ffx daemon echo`
/// which we guarantee will use the daemon, irrespective of the configured connection type.
/// Returns an error when in strict mode.
pub async fn init_daemon_connection_behavior(
    context: &EnvironmentContext,
) -> Result<FhoConnectionBehavior> {
    if context.is_strict() {
        return Err(ffx_command_error::Error::User(anyhow::anyhow!(
            "Daemon connections are not supported in strict mode"
        )));
    }
    let build_info = context.build_info();
    let overnet_injector = Injection::initialize_overnet(context.clone(), None, build_info).await?;
    log::info!("Initializing FhoConnectionBehavior::DaemonConnector");
    Ok(FhoConnectionBehavior::DaemonConnector(Arc::new(overnet_injector)))
}

/// A decorator for proxy types in [`crate::FfxTool`] implementations so you can
/// specify the moniker for the component exposing the proxy you're loading.
///
/// This is actually an alias to [`toolbox_or`], so it will also try
/// your tool's default toolbox first.
///
/// Example:
///
/// ```rust
/// #[derive(FfxTool)]
/// struct Tool {
///     #[with(fho::moniker("core/foo/thing"))]
///     foo_proxy: FooProxy,
/// }
/// ```
pub fn moniker<P: Proxy>(
    moniker: impl AsRef<str>,
) -> WithToolbox<P, DefaultFuchsiaResourceDialect> {
    toolbox_or(moniker)
}

pub(crate) async fn connect_to_rcs(env: &FhoEnvironment) -> Result<RemoteControlProxyHolder> {
    let retry_count = 1;
    let mut tries = 0;
    // TODO(b/287693891): Remove explicit retries/timeouts here so they can be
    // configurable instead.
    loop {
        tries += 1;
        let res = RemoteControlProxyHolder::try_from_env(env).await;
        if res.is_ok() || tries > retry_count {
            // Using `TryFromEnv` on `RemoteControlProxy` already contains user error information,
            // which will be propagated after exiting the loop.
            break Ok(res?);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ffx_config::environment::ExecutableKind;
    use ffx_config::ConfigMap;

    #[fuchsia::test]
    async fn test_connection_behavior_correct_in_strict() {
        let ctx = EnvironmentContext::strict(ExecutableKind::Test, ConfigMap::new()).unwrap();
        let behavior = init_connection_behavior(&ctx).await.unwrap();
        assert!(matches!(behavior, FhoConnectionBehavior::DirectConnector(_)));
    }

    #[fuchsia::test]
    async fn test_connection_behavior_correct_in_non_strict() {
        let ctx =
            EnvironmentContext::no_context(ExecutableKind::Test, ConfigMap::new(), None, true);
        let behavior = init_connection_behavior(&ctx).await.unwrap();
        assert!(matches!(behavior, FhoConnectionBehavior::DaemonConnector(_)));
    }

    #[fuchsia::test]
    async fn test_daemon_connection_behavior() {
        let ctx =
            EnvironmentContext::no_context(ExecutableKind::Test, ConfigMap::new(), None, true);
        let behavior = init_daemon_connection_behavior(&ctx).await.unwrap();
        assert!(matches!(behavior, FhoConnectionBehavior::DaemonConnector(_)));
    }

    #[fuchsia::test]
    async fn test_daemon_connection_behavior_fails_in_strict() {
        let ctx =
            EnvironmentContext::strict(ExecutableKind::Test, ConfigMap::new()).expect("strict env");
        assert!(matches!(init_daemon_connection_behavior(&ctx).await, Err(_)));
    }
    #[fuchsia::test]
    async fn test_direct_connection_behavior() {
        let runtime_args =
            serde_json::json!({"connectivity": { "direct": true}}).as_object().unwrap().clone();
        let ctx = EnvironmentContext::no_context(ExecutableKind::Test, runtime_args, None, true);
        let behavior = init_connection_behavior(&ctx).await.unwrap();
        assert!(matches!(behavior, FhoConnectionBehavior::DirectConnector(_)));
    }
}
