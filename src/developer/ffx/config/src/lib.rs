// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::api::validate_type;
use crate::api::value::{ConfigValue, ValueStrategy};
use ::errors::ffx_bail;
use analytics::metrics_state::MetricsStatus;
use analytics::{set_new_opt_in_status, show_status_message};
use anyhow::{anyhow, Context, Result};
use api::value::TryConvert;
use core::fmt;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

pub mod api;
pub mod environment;
pub mod keys;
pub mod logging;
pub mod runtime;

mod aliases;
mod cache;
mod mapping;
mod nested;
mod paths;
mod storage;

pub use aliases::{
    is_analytics_disabled, is_mdns_autoconnect_disabled, is_mdns_discovery_disabled,
    is_usb_discovery_disabled,
};
pub use api::query::{ConfigQuery, SelectMode};
pub use api::ConfigError;
pub use config_macros::FfxConfigBacked;

pub use environment::{test_init, test_init_in_tree, Environment, EnvironmentContext, TestEnv};
pub use sdk::{self, Sdk, SdkRoot};
pub use storage::{AssertNoEnv, AssertNoEnvError, ConfigMap};

lazy_static::lazy_static! {
    static ref ENV: Mutex<Option<EnvironmentContext>> = Mutex::default();
}

#[doc(hidden)]
pub mod macro_deps {
    pub use {anyhow, serde_json};
}

/// The levels of configuration possible
// If you edit this enum, make sure to also change the enum counter below to match.
#[derive(Debug, Eq, PartialEq, Copy, Clone, Hash)]
pub enum ConfigLevel {
    /// Default configurations are provided through GN build rules across all subcommands and are
    ///  hard-coded and immutable.
    Default,
    /// Global configuration is intended to be a system-wide configuration level. It is intended to
    /// be used when installing ffx on a host system to set organizational
    /// properties that would be the same for a collection of users.
    Global,
    /// Build configuration is associated with a build directory. It is intended to be used to set
    /// properties describing the output of the build. It should be generated as part of the build
    /// process, and is considered read-only by ffx.
    Build,
    /// User configuration is configuration set in the user's home directory and applies to all
    /// invocations of ffx by that user. User configuration can be overridden only at runtime.
    User,
    /// Runtime configuration is set by the user when invoking ffx, and can't be 'set' by any other means.
    Runtime,
}
impl ConfigLevel {
    /// The number of elements in the above enum, used for tests.
    const _COUNT: usize = 5;

    /// Iterates over the config levels in priority order, starting from the most narrow scope if given None.
    /// Note this is not conformant to Iterator::next(), it's just meant to be a simple source of truth about ordering.
    pub(crate) fn next(current: Option<Self>) -> Option<Self> {
        use ConfigLevel::*;
        match current {
            Some(Default) => None,
            Some(Global) => Some(Default),
            Some(Build) => Some(Global),
            Some(User) => Some(Build),
            Some(Runtime) => Some(User),
            None => Some(Runtime),
        }
    }
}
impl fmt::Display for ConfigLevel {
    // Required method
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let val = match self {
            ConfigLevel::Default => "default",
            ConfigLevel::Global => "global",
            ConfigLevel::User => "user",
            ConfigLevel::Build => "build",
            ConfigLevel::Runtime => "runtime",
        };
        write!(f, "{}", val)
    }
}

impl argh::FromArgValue for ConfigLevel {
    fn from_arg_value(val: &str) -> Result<Self, String> {
        match val {
            "u" | "user" => Ok(ConfigLevel::User),
            "b" | "build" => Ok(ConfigLevel::Build),
            "g" | "global" => Ok(ConfigLevel::Global),
            _ => Err(String::from(
                "Unrecognized value. Possible values are \"user\",\"build\",\"global\".",
            )),
        }
    }
}

pub async fn invalidate_global_cache() {
    if let Some(env_context) = global_env_context() {
        crate::cache::invalidate(&env_context.cache).await;
    }
}

pub fn global_env_context() -> Option<EnvironmentContext> {
    ENV.lock().unwrap().clone()
}

pub fn global_env() -> Result<Environment> {
    let context =
        global_env_context().context("Tried to load global environment before configuration")?;

    match context.load() {
        Err(err) => {
            tracing::error!("failed to load environment, reverting to default: {}", err);
            Ok(Environment::new_empty(context))
        }
        Ok(ctx) => Ok(ctx),
    }
}

/// Initialize the configuration. Only the first call in a process runtime takes effect, so users must
/// call this early with the required values, such as in main() in the ffx binary.
pub fn init(context: &EnvironmentContext) -> Result<()> {
    let mut env_lock = ENV.lock().unwrap();
    if env_lock.is_some() {
        anyhow::bail!("Attempted to set the global environment more than once in a process invocation, outside of a test");
    }
    env_lock.replace(context.clone());
    Ok(())
}

/// Creates a [`ConfigQuery`] against the global config cache and environment.
///
/// Example:
///
/// ```no_run
/// use ffx_config::ConfigLevel;
/// use ffx_config::BuildSelect;
/// use ffx_config::SelectMode;
///
/// let query = ffx_config::build()
///     .name("testing")
///     .level(Some(ConfigLevel::Build))
///     .build(Some(BuildSelect::Path("/tmp/build.json")))
///     .select(SelectMode::All);
/// let value = query.get().await?;
/// ```
pub fn build<'a>() -> ConfigQuery<'a> {
    ConfigQuery::default()
}

/// Creates a [`ConfigQuery`] against the global config cache and environment,
/// using the provided value converted in to a base query.
///
/// Example:
///
/// ```no_run
/// ffx_config::query("a_key").get();
/// ffx_config::query(ffx_config::ConfigLevel::User).get();
/// ```
pub fn query<'a>(with: impl Into<ConfigQuery<'a>>) -> ConfigQuery<'a> {
    with.into()
}

/// A shorthand for the very common case of querying a value from the global config
/// cache and environment, using the provided value converted into a query.
pub fn get<'a, T, U>(with: U) -> std::result::Result<T, ConfigError>
where
    T: TryConvert + ValueStrategy,
    U: Into<ConfigQuery<'a>>,
{
    query(with).get()
}

pub fn get_optional<'a, T, U>(with: U) -> std::result::Result<T, ConfigError>
where
    T: TryConvert + ValueStrategy,
    U: Into<ConfigQuery<'a>>,
{
    query(with).get_optional()
}

pub const SDK_OVERRIDE_KEY_PREFIX: &str = "sdk.overrides";

/// Returns the path to the tool with the given name by first
/// checking for configured override with the key of `sdk.override.{name}`,
/// and no override is found, sdk.get_host_tool() is called.
#[allow(clippy::unused_async)] // TODO(https://fxbug.dev/386387845)
pub async fn get_host_tool(sdk: &Sdk, name: &str) -> Result<PathBuf> {
    // Check for configured override for the host tool.
    let override_key = format!("{SDK_OVERRIDE_KEY_PREFIX}.{name}");
    let override_result: Result<PathBuf, ConfigError> = query(&override_key).get();

    if let Ok(tool_path) = override_result {
        if tool_path.exists() {
            tracing::info!("Using configured override for {name}: {tool_path:?}");
            return Ok(tool_path);
        } else {
            return Err(anyhow!(
                "Override path for {name} set to {tool_path:?}, but does not exist"
            ));
        }
    }
    sdk.get_host_tool(name)
}

#[allow(clippy::unused_async)] // TODO(https://fxbug.dev/386387845)
pub async fn print_config<W: Write>(ctx: &EnvironmentContext, mut writer: W) -> Result<()> {
    let config = ctx.load()?.config_from_cache()?;
    let read_guard = config.read().map_err(|_| anyhow!("config read guard"))?;
    writeln!(writer, "{}", *read_guard).context("displaying config")
}

#[allow(clippy::unused_async)] // TODO(https://fxbug.dev/386387845)
pub async fn get_log_dirs() -> Result<Vec<String>> {
    match query("log.dir").get() {
        Ok(log_dirs) => Ok(log_dirs),
        Err(e) => ffx_bail!("Failed to load host log directories from ffx config: {:?}", e),
    }
}

/// Print out useful hints about where important log information might be found after an error.
pub async fn print_log_hint<W: std::io::Write>(writer: &mut W) {
    let msg = match get_log_dirs().await {
        Ok(log_dirs) if log_dirs.len() == 1 => format!(
                "More information may be available in ffx host logs in directory:\n    {}",
                log_dirs[0]
            ),
        Ok(log_dirs) => format!(
                "More information may be available in ffx host logs in directories:\n    {}",
                log_dirs.join("\n    ")
            ),
        Err(err) => format!(
                "More information may be available in ffx host logs, but ffx failed to retrieve configured log file locations. Error:\n    {}",
                err,
            ),
    };
    if writeln!(writer, "{}", msg).is_err() {
        println!("{}", msg);
    }
}

pub async fn set_metrics_status(value: MetricsStatus) -> Result<()> {
    set_new_opt_in_status(value).await
}

pub async fn enable_basic_metrics() -> Result<()> {
    set_new_opt_in_status(MetricsStatus::OptedIn).await
}

pub async fn enable_enhanced_metrics() -> Result<()> {
    set_new_opt_in_status(MetricsStatus::OptedInEnhanced).await
}

pub async fn disable_metrics() -> Result<()> {
    set_new_opt_in_status(MetricsStatus::OptedOut).await
}

pub async fn show_metrics_status<W: Write>(mut writer: W) -> Result<()> {
    let status_message = show_status_message().await;
    writeln!(&mut writer, "{status_message}")?;
    Ok(())
}

////////////////////////////////////////////////////////////////////////////////
// tests
#[cfg(test)]
mod test {
    use super::*;
    // This is to get the FfxConfigBacked derive to compile, as it
    // creates a token stream referencing `ffx_config` on the inside.
    use crate::{self as ffx_config};
    use serde_json::{json, Value};
    use std::collections::HashSet;
    use std::fs;

    #[test]
    fn test_config_levels_make_sense_from_first() {
        let mut found_set = HashSet::new();
        let mut from_first = None;
        for _ in 0..ConfigLevel::_COUNT + 1 {
            if let Some(next) = ConfigLevel::next(from_first) {
                let entry = found_set.get(&next);
                assert!(entry.is_none(), "Found duplicate config level while iterating: {next:?}");
                found_set.insert(next);
                from_first = Some(next);
            } else {
                break;
            }
        }

        assert_eq!(
            ConfigLevel::_COUNT,
            found_set.len(),
            "A config level was missing from the forward iteration of levels: {found_set:?}"
        );
    }

    #[test]
    fn test_validating_types() {
        assert!(validate_type::<String>(json!("test")).is_some());
        assert!(validate_type::<String>(json!(1)).is_none());
        assert!(validate_type::<String>(json!(false)).is_none());
        assert!(validate_type::<String>(json!(true)).is_none());
        assert!(validate_type::<String>(json!({"test": "whatever"})).is_none());
        assert!(validate_type::<String>(json!(["test", "test2"])).is_none());
        assert!(validate_type::<bool>(json!(true)).is_some());
        assert!(validate_type::<bool>(json!(false)).is_some());
        assert!(validate_type::<bool>(json!("true")).is_some());
        assert!(validate_type::<bool>(json!("false")).is_some());
        assert!(validate_type::<bool>(json!(1)).is_none());
        assert!(validate_type::<bool>(json!("test")).is_none());
        assert!(validate_type::<bool>(json!({"test": "whatever"})).is_none());
        assert!(validate_type::<bool>(json!(["test", "test2"])).is_none());
        assert!(validate_type::<u64>(json!(2)).is_some());
        assert!(validate_type::<u64>(json!(100)).is_some());
        assert!(validate_type::<u64>(json!("100")).is_some());
        assert!(validate_type::<u64>(json!("0")).is_some());
        assert!(validate_type::<u64>(json!(true)).is_none());
        assert!(validate_type::<u64>(json!("test")).is_none());
        assert!(validate_type::<u64>(json!({"test": "whatever"})).is_none());
        assert!(validate_type::<u64>(json!(["test", "test2"])).is_none());
        assert!(validate_type::<PathBuf>(json!("/")).is_some());
        assert!(validate_type::<PathBuf>(json!("test")).is_some());
        assert!(validate_type::<PathBuf>(json!(true)).is_none());
        assert!(validate_type::<PathBuf>(json!({"test": "whatever"})).is_none());
        assert!(validate_type::<PathBuf>(json!(["test", "test2"])).is_none());
    }

    #[test]
    fn test_converting_array() -> Result<()> {
        let c = |val: Value| -> ConfigValue { ConfigValue(Some(val)) };
        let conv_elem: Vec<String> = <_>::try_convert(c(json!("test")))?;
        assert_eq!(1, conv_elem.len());
        let conv_string: Vec<String> = <_>::try_convert(c(json!(["test", "test2"])))?;
        assert_eq!(2, conv_string.len());
        let conv_bool: Vec<bool> = <_>::try_convert(c(json!([true, "false", false])))?;
        assert_eq!(3, conv_bool.len());
        let conv_bool_2: Vec<bool> = <_>::try_convert(c(json!([36, "false", false])))?;
        assert_eq!(2, conv_bool_2.len());
        let conv_num: Vec<u64> = <_>::try_convert(c(json!([3, "36", 1000])))?;
        assert_eq!(3, conv_num.len());
        let conv_num_2: Vec<u64> = <_>::try_convert(c(json!([3, "false", 1000])))?;
        assert_eq!(2, conv_num_2.len());
        let bad_elem: std::result::Result<Vec<u64>, ConfigError> =
            <_>::try_convert(c(json!("test")));
        assert!(bad_elem.is_err());
        let bad_elem_2: std::result::Result<Vec<u64>, ConfigError> =
            <_>::try_convert(c(json!(["test"])));
        assert!(bad_elem_2.is_err());
        Ok(())
    }

    #[derive(FfxConfigBacked, Default)]
    struct TestConfigBackedStruct {
        #[ffx_config_default(key = "test.test.thing", default = "thing")]
        value: Option<String>,

        #[ffx_config_default(default = "what", key = "oops")]
        reverse_value: Option<String>,

        #[ffx_config_default(key = "other.test.thing")]
        other_value: Option<f64>,
    }

    #[derive(FfxConfigBacked, Default)] // This should just compile despite having no config.
    struct TestEmptyBackedStruct {}

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_config_backed_attribute() {
        let env = ffx_config::test_init().await.expect("create test config");
        let mut empty_config_struct = TestConfigBackedStruct::default();
        assert!(empty_config_struct.value.is_none());
        assert_eq!(empty_config_struct.value().unwrap(), "thing");
        assert!(empty_config_struct.reverse_value.is_none());
        assert_eq!(empty_config_struct.reverse_value().unwrap(), "what");

        env.context
            .query("test.test.thing")
            .level(Some(ConfigLevel::User))
            .set(Value::String("config_value_thingy".to_owned()))
            .await
            .unwrap();
        env.context
            .query("other.test.thing")
            .level(Some(ConfigLevel::User))
            .set(Value::Number(serde_json::Number::from_f64(2f64).unwrap()))
            .await
            .unwrap();

        // If this is set, this should pop up before the config values.
        empty_config_struct.value = Some("wat".to_owned());
        assert_eq!(empty_config_struct.value().unwrap(), "wat");
        empty_config_struct.value = None;
        assert_eq!(empty_config_struct.value().unwrap(), "config_value_thingy");
        assert_eq!(empty_config_struct.other_value().unwrap().unwrap(), 2f64);
        env.context
            .query("other.test.thing")
            .level(Some(ConfigLevel::User))
            .set(Value::String("oaiwhfoiwh".to_owned()))
            .await
            .unwrap();

        // This should just compile and drop without panicking is all.
        let _ignore = TestEmptyBackedStruct {};
    }

    /// Writes the file to $root, with the path $path, from the source tree prefix $prefix
    /// (relative to this source file)
    macro_rules! put_file {
        ($root:expr, $prefix:literal, $name:literal) => {{
            fs::create_dir_all($root.join($name).parent().unwrap()).unwrap();
            fs::File::create($root.join($name))
                .unwrap()
                .write_all(include_bytes!(concat!($prefix, "/", $name)))
                .unwrap();
        }};
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_get_host_tool() {
        let env = ffx_config::test_init().await.expect("create test config");
        let sdk_root = env.isolate_root.path().join("sdk");
        env.context
            .query("sdk.root")
            .level(Some(ConfigLevel::User))
            .set(sdk_root.to_string_lossy().into())
            .await
            .expect("creating temp sdk root");

        put_file!(sdk_root, "../test_data/sdk", "meta/manifest.json");
        put_file!(sdk_root, "../test_data/sdk", "tools/x64/a_host_tool-meta.json");

        let sdk = env.context.get_sdk().expect("test sdk");

        let result = get_host_tool(&sdk, "a_host_tool").await.expect("a_host_tool");
        assert_eq!(result, sdk_root.join("tools/x64/a-host-tool"));
    }
    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_get_host_tool_override() {
        let env = ffx_config::test_init().await.expect("create test config");
        let sdk_root = env.isolate_root.path().join("sdk");
        env.context
            .query("sdk.root")
            .level(Some(ConfigLevel::User))
            .set(sdk_root.to_string_lossy().into())
            .await
            .expect("creating temp sdk root");

        put_file!(sdk_root, "../test_data/sdk", "meta/manifest.json");
        put_file!(sdk_root, "../test_data/sdk", "tools/x64/a_host_tool-meta.json");

        // Override the path via config
        let override_path = env.isolate_root.path().join("a_override_host_tool");
        fs::write(&override_path, "a_override_tool_contents").expect("override file written");
        env.context
            .query(&format!("{SDK_OVERRIDE_KEY_PREFIX}.a_host_tool"))
            .level(Some(ConfigLevel::User))
            .set(override_path.to_string_lossy().into())
            .await
            .expect("setting override");

        let sdk = env.context.get_sdk().expect("test sdk");

        let result = get_host_tool(&sdk, "a_host_tool").await.expect("a_host_tool");
        assert_eq!(result, override_path);
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_get_host_tool_override_no_exists() {
        let env = ffx_config::test_init().await.expect("create test config");
        let sdk_root = env.isolate_root.path().join("sdk");
        env.context
            .query("sdk.root")
            .level(Some(ConfigLevel::User))
            .set(sdk_root.to_string_lossy().into())
            .await
            .expect("creating temp sdk root");

        put_file!(sdk_root, "../test_data/sdk", "meta/manifest.json");
        put_file!(sdk_root, "../test_data/sdk", "tools/x64/a_host_tool-meta.json");

        // Override the path via config
        let override_path = env.isolate_root.path().join("a_override_host_tool");

        // do not create file, this should report an error.

        env.context
            .query(&format!("{SDK_OVERRIDE_KEY_PREFIX}.a_host_tool"))
            .level(Some(ConfigLevel::User))
            .set(override_path.to_string_lossy().into())
            .await
            .expect("setting override");

        let sdk = env.context.get_sdk().expect("test sdk");

        let result = get_host_tool(&sdk, "a_host_tool").await;
        assert_eq!(
            result.err().unwrap().to_string(),
            format!("Override path for a_host_tool set to {override_path:?}, but does not exist")
        );
    }
}
