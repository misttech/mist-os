// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::sync::Arc;
use std::time::Duration;

use ffx_command_error::Result;
use ffx_config::EnvironmentContext;
use ffx_daemon_proxy::{DaemonVersionCheck, Injection};
use fho::{FhoConnectionBehavior, FhoEnvironment, TryFromEnv as _};
use fidl::encoding::DefaultFuchsiaResourceDialect;
use fidl::endpoints::Proxy;
use target_network_connector::NetworkConnector;

mod daemon_proxy;
mod device_lookup;
mod fake_injector;
pub mod fdomain;
mod from_toolbox;
mod remote_control_proxy;
mod target_info;
mod target_proxy;
mod with_moniker;

pub use daemon_proxy::{daemon_protocol, DaemonProxyHolder};
pub use device_lookup::DeviceLookupDefaultImpl;
pub use fake_injector::FakeInjector;
use from_toolbox::WithToolbox;
pub use from_toolbox::{toolbox, toolbox_or};
pub use remote_control_proxy::{fake_proxy, RemoteControlProxyHolder};
pub use target_info::TargetInfoHolder;
pub use target_proxy::TargetProxyHolder;

const DEFAULT_PROXY_TIMEOUT: Duration = Duration::from_secs(15);

pub async fn init_connection_behavior(
    context: &EnvironmentContext,
) -> Result<FhoConnectionBehavior> {
    if context.is_strict() {
        let connector =
            NetworkConnector::<ffx_target::ssh_connector::SshConnector>::new(context).await?;
        Ok(FhoConnectionBehavior::DirectConnector(Arc::new(connector)))
    } else {
        let build_info = context.build_info();
        let overnet_injector = Injection::initialize_overnet(
            context.clone(),
            None,
            DaemonVersionCheck::SameVersionInfo(build_info),
        )
        .await?;
        Ok(FhoConnectionBehavior::DaemonConnector(Arc::new(overnet_injector)))
    }
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
}
