// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use home::home_dir;
use once_cell::sync::Lazy;
use regex::Regex;
use std::env::VarError;
use std::path::PathBuf;

pub fn get_os() -> String {
    convert_macos_to_darwin(std::env::consts::OS)
}

pub fn get_arch() -> String {
    std::env::consts::ARCH.to_string()
}

fn convert_macos_to_darwin(arch: &str) -> String {
    arch.replace("macos", "Darwin")
}

pub fn analytics_folder() -> String {
    let mut metrics_path = get_home_dir();
    metrics_path.push(".fuchsia/metrics/");
    let path_str = metrics_path.to_str();
    match path_str {
        Some(v) => String::from(v),
        None => String::from("/tmp/.fuchsia/metrics/"),
    }
}

fn get_home_dir() -> PathBuf {
    match home_dir() {
        Some(dir) => dir,
        None => PathBuf::from("/tmp"),
    }
}

const TEST_ENV_VAR: &'static str = "FUCHSIA_TEST_OUTDIR";
const ANALYTICS_DISABLED_ENV_VAR: &'static str = "FUCHSIA_ANALYTICS_DISABLED";

// To detect certain bot environmments that run as USER "builder"
#[cfg(not(test))]
const USER: &'static str = "USER";
const BUILDER: &'static str = "builder";

const BOT_ENV_VARS: &'static [&'static str] = &[
    "TF_BUILD",           // Azure
    "bamboo.buildKey",    // Bamboo
    "BUILDKITE",          // BUILDKITE
    "CIRCLECI",           // Circle
    "CIRRUS_CI",          // Cirrus
    "CODEBUILD_BUILD_ID", // Codebuild
    "UNITTEST_ON_BORG",   // Borg
    "UNITTEST_ON_FORGE",  // Forge
    "SWARMING_BOT_ID",    // Fuchsia
    "GITHUB_ACTIONS",     // GitHub Actions
    "GITLAB_CI",          // GitLab
    "HEROKU_TEST_RUN_ID", // Heroku
    "BUILD_ID",           // Hudson & Jenkins
    "TEAMCITY_VERSION",   // Teamcity
    "TRAVIS",             // Travis
    "BUILD_NUMBER",       // android-ci
];

#[allow(dead_code)]
const C_GOOGLERS_DOMAIN_NAME: &str = r"c.googlers.com$";
#[allow(dead_code)]
const CORP_GOOGLERS_DOMAIN_NAME: &str = r"corp.google.com$";
#[allow(dead_code)]
const USER_REDACTED: &str = "$USER";

pub fn is_test_env() -> bool {
    std::env::var(TEST_ENV_VAR).is_ok()
}

pub fn is_fuchsia_analytics_disabled_set() -> bool {
    std::env::var(ANALYTICS_DISABLED_ENV_VAR).is_ok()
}

pub fn is_running_in_ci_bot_env() -> bool {
    BOT_ENV_VARS.iter().any(|env_var| std::env::var(env_var).is_ok())
}

pub fn is_user_a_bot() -> bool {
    get_username_from_env().is_ok_and(|v| v == BUILDER)
}

pub fn is_analytics_disabled_by_env() -> bool {
    is_test_env()
        || is_fuchsia_analytics_disabled_set()
        || is_running_in_ci_bot_env()
        || is_user_a_bot()
}

#[allow(dead_code)]
pub fn is_googler_host_domain(domain: &str) -> bool {
    static C_GOOGLERS_DOMAIN_RE: Lazy<Regex> =
        Lazy::new(|| Regex::new(C_GOOGLERS_DOMAIN_NAME).unwrap());
    static CORP_GOOGLERS_DOMAIN_RE: Lazy<Regex> =
        Lazy::new(|| Regex::new(CORP_GOOGLERS_DOMAIN_NAME).unwrap());
    C_GOOGLERS_DOMAIN_RE.is_match(domain) || CORP_GOOGLERS_DOMAIN_RE.is_match(domain)
}

#[allow(dead_code)]
pub fn redact_user_name_from(parameter: &str) -> String {
    match get_username_from_env() {
        Ok(user_value) => parameter.replace(user_value.as_str(), USER_REDACTED),
        _ => parameter.to_string(),
    }
}

#[cfg(not(test))]
pub fn get_username_from_env() -> Result<String, VarError> {
    std::env::var(USER)
}

#[cfg(test)]
pub fn get_username_from_env() -> Result<String, VarError> {
    Ok("player1".to_string())
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    // TODO(https://fxbug.dev/42148443): isolate the env test from CI env
    #[ignore]
    // Rust tests are run in parallel in threads, which means that this test is
    // disruptive to other tests. There's little ROI to doing some kind of fork
    // dance here, so the test is included, but not run by default.
    pub fn test_is_test_env() {
        std::env::set_var(TEST_ENV_VAR, "somepath");
        assert_eq!(true, is_test_env());
        std::env::remove_var(TEST_ENV_VAR);
        assert_eq!(false, is_test_env());
    }

    #[test]
    #[ignore]
    // Rust tests are run in parallel in threads, which means that this test is
    // disruptive to other tests. There's little ROI to doing some kind of fork
    // dance here, so the test is included, but not run by default.
    pub fn test_is_analytics_disabled_env() {
        std::env::set_var(ANALYTICS_DISABLED_ENV_VAR, "1");
        assert_eq!(true, is_fuchsia_analytics_disabled_set());
        std::env::remove_var(ANALYTICS_DISABLED_ENV_VAR);
        assert_eq!(false, is_fuchsia_analytics_disabled_set());
    }

    #[test]
    // TODO(https://fxbug.dev/42148443): isolate the env test from CI env
    #[ignore]
    pub fn test_is_bot_env() {
        std::env::set_var(&"BUILD_ID", "1");
        assert_eq!(true, is_running_in_ci_bot_env());
        std::env::remove_var(&"BUILD_ID");
        assert_eq!(false, is_fuchsia_analytics_disabled_set());
    }

    #[test]
    pub fn test_arch_macos_conversion() {
        assert_eq!("Darwin", convert_macos_to_darwin("macos"))
    }

    #[test]
    pub fn test_is_googler_host_domain() {
        assert_eq!(true, is_googler_host_domain("c.googlers.com"));
        assert_eq!(true, is_googler_host_domain("corp.google.com"));
        assert_eq!(true, is_googler_host_domain("ldpa1.c.googlers.com"));
        assert_eq!(true, is_googler_host_domain("ldap2.mtv.corp.google.com"));
    }

    #[test]
    pub fn test_not_is_googler_host_domain() {
        assert_eq!(false, is_googler_host_domain(""));
        assert_eq!(false, is_googler_host_domain("c"));
        assert_eq!(false, is_googler_host_domain("c.googlers"));
        assert_eq!(false, is_googler_host_domain("google.com"));
        assert_eq!(false, is_googler_host_domain("c.googlers.com.edu"));
        assert_eq!(false, is_googler_host_domain("corp.google.com.edu"));
        assert_eq!(false, is_googler_host_domain("ldpa1.c.googlers.com.edu"));
    }

    #[test]
    pub fn test_redact_user_value() {
        let parameter = "hello player1";
        let result = redact_user_name_from(parameter);
        assert_eq!("hello $USER", result);
    }

    #[test]
    pub fn test_redact_user_value_no_match() {
        let parameter = "hello world";
        let result = redact_user_name_from(parameter);
        assert_eq!("hello world", result);
    }
}
