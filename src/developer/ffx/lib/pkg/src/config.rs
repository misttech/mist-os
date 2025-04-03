// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{anyhow, Context, Result};
use ffx_config::ConfigLevel;
const CONFIG_KEY_DEFAULT_REPOSITORY: &str = "repository.default";
const CONFIG_KEY_SERVER_LISTEN: &str = "repository.server.listen";

/// Default name used for package repositories in ffx. It is expected that there is no need to
/// change this constant. But in case this is changed, ensure that it is consistent with the ffx
/// developer documentation, see
/// https://cs.opensource.google/search?q=devhost&sq=&ss=fuchsia%2Ffuchsia:src%2Fdeveloper%2Fffx%2F
// LINT.IfChange
pub const DEFAULT_REPO_NAME: &str = "devhost";
// LINT.ThenChange(/src/developer/ffx/plugins/repository/add-from-pm/src/args.rs)

// Try to figure out why the server is not running.
pub async fn determine_why_repository_server_is_not_running() -> anyhow::Error {
    macro_rules! check {
        ($e:expr) => {
            match $e {
                Ok(value) => value,
                Err(err) => {
                    return err;
                }
            }
        };
    }

    match check!(repository_listen_addr().await) {
        Some(addr) => {
            return anyhow!(
                "ffx config detects repository.server.listen to be {addr} \
                Another process may be using that address. \
                Try shutting it down \n\
                $ ffx repository server stop --all\n\
                Or alternatively specify a different address on the command line\n\
                $ ffx repository server start --address <addr>",
            );
        }
        None => {
            return anyhow!(
                "Server listening address is unspecified. You can fix this with:\n\
                $ ffx config set repository.server.listen '[::]:8083'\n\
                $ ffx repository server start\n\
                Or alternatively specify at runtime \n\
                $ ffx repository server start --address <addr>",
            );
        }
    }
}

/// Return the repository server address from ffx config.
pub async fn repository_listen_addr() -> Result<Option<std::net::SocketAddr>> {
    if let Some(address) = ffx_config::get::<Option<String>, _>(CONFIG_KEY_SERVER_LISTEN)? {
        if address.is_empty() {
            Ok(None)
        } else {
            Ok(Some(
                address
                    .parse::<std::net::SocketAddr>()
                    .with_context(|| format!("Parsing {}", CONFIG_KEY_SERVER_LISTEN))?,
            ))
        }
    } else {
        Ok(None)
    }
}

/// Return the default repository from the configuration if set.
pub async fn get_default_repository() -> Result<Option<String>> {
    ffx_config::get(CONFIG_KEY_DEFAULT_REPOSITORY).map_err(Into::into)
}

/// Sets the default repository from the config.
pub async fn set_default_repository(repo_name: &str) -> Result<()> {
    ffx_config::invalidate_global_cache().await; // Necessary when the daemon does some writes and the CLI does others
    ffx_config::query(CONFIG_KEY_DEFAULT_REPOSITORY)
        .level(Some(ConfigLevel::User))
        .set(repo_name.into())
        .await
}

/// Unsets the default repository from the config.
pub async fn unset_default_repository() -> Result<()> {
    ffx_config::query(CONFIG_KEY_DEFAULT_REPOSITORY).level(Some(ConfigLevel::User)).remove().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use serde_json::{json, Value};

    const CONFIG_KEY_ROOT: &str = "repository";

    #[fuchsia::test]
    async fn test_get_set_unset_default_repository() {
        let env = ffx_config::test_init().await.expect("test init");
        env.context
            .query(CONFIG_KEY_ROOT)
            .level(Some(ConfigLevel::User))
            .set(json!({}))
            .await
            .unwrap();

        // Initially there's no default.
        assert_eq!(get_default_repository().await.unwrap(), None);

        // Setting the default should write to the config.
        set_default_repository("foo").await.unwrap();
        assert_eq!(
            env.context.get::<Value, _>(CONFIG_KEY_DEFAULT_REPOSITORY).unwrap(),
            json!("foo"),
        );
        assert_eq!(get_default_repository().await.unwrap(), Some("foo".into()));

        // We don't care if the repository has `.` in it.
        set_default_repository("foo.bar").await.unwrap();
        assert_eq!(
            env.context.get::<Value, _>(CONFIG_KEY_DEFAULT_REPOSITORY).unwrap(),
            json!("foo.bar"),
        );
        assert_eq!(get_default_repository().await.unwrap(), Some("foo.bar".into()));

        // Unset removes the default repository from the config.
        unset_default_repository().await.unwrap();
        assert_eq!(
            env.context.get::<Option<Value>, _>(CONFIG_KEY_DEFAULT_REPOSITORY).unwrap(),
            None,
        );
        assert_eq!(get_default_repository().await.unwrap(), None);

        // Unsetting the repo again returns an error.
        assert!(unset_default_repository().await.is_err());
    }
}
