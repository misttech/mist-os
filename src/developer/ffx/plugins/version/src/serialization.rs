// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use chrono::{Offset, TimeZone};
use fho::FfxContext;
use fho::Result;
use fidl_fuchsia_developer_ffx::{self as ffx};
use serde::Serialize;
use std::{fmt::Display, io::Write};

const UNKNOWN_BUILD_HASH: &str = "(unknown)";

/// Since the fidl-derived versioninfo struct doesn't implement serde::Serialize,
/// and it uses a private member to make it non_exhaustive, we can't
/// really use the serde remote serializer mechanism. So this is just a simple
/// serializeable copy of VersionInfo.
#[derive(Serialize, Debug, Default, PartialEq, Eq)]
pub struct VersionInfo {
    pub commit_hash: Option<String>,
    pub commit_timestamp: Option<u64>,
    pub build_version: Option<String>,
    pub abi_revision: Option<u64>,
    pub api_level: Option<u64>,
    pub exec_path: Option<String>,
    pub build_id: Option<String>,
}

impl From<ffx::VersionInfo> for VersionInfo {
    fn from(value: ffx::VersionInfo) -> Self {
        let ffx::VersionInfo {
            commit_hash,
            commit_timestamp,
            build_version,
            abi_revision,
            api_level,
            exec_path,
            build_id,
            ..
        } = value;
        VersionInfo {
            commit_hash,
            commit_timestamp,
            build_version,
            abi_revision,
            api_level,
            exec_path,
            build_id,
        }
    }
}

impl From<VersionInfo> for ffx::VersionInfo {
    fn from(value: VersionInfo) -> Self {
        let VersionInfo {
            commit_hash,
            commit_timestamp,
            build_version,
            abi_revision,
            api_level,
            exec_path,
            build_id,
            ..
        } = value;
        ffx::VersionInfo {
            commit_hash,
            commit_timestamp,
            build_version,
            abi_revision,
            api_level,
            exec_path,
            build_id,
            ..Default::default()
        }
    }
}

#[derive(Serialize)]
pub struct Versions {
    pub tool_version: VersionInfo,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub daemon_version: Option<VersionInfo>,
}

pub fn format_version_info<O: Offset + Display>(
    header: &str,
    info: &VersionInfo,
    verbose: bool,
    tz: &impl TimeZone<Offset = O>,
) -> String {
    let build_version = info.build_version.as_deref().unwrap_or("(unknown build version)");
    if !verbose {
        return build_version.to_owned();
    }

    // Convert the ABI revision back to hex string so that it matches
    // the format in //sdk/version_history.json.
    let abi_revision = match info.abi_revision {
        Some(abi) => format!("{:#X}", abi),
        None => String::from("(unknown ABI revision)"),
    };
    let api_level = match info.api_level {
        Some(api) => format!("{}", api),
        None => String::from("(unknown API level)"),
    };

    let hash = info.commit_hash.as_deref().unwrap_or(UNKNOWN_BUILD_HASH);
    let timestamp_str = match info.commit_timestamp {
        Some(t) => tz.timestamp(t as i64, 0).to_rfc2822(),
        None => String::from("(unknown commit time)"),
    };

    return format!(
        "\
{header}:
  abi-revision: {abi_revision}
  api-level: {api_level}
  build-version: {build_version}
  integration-commit-hash: {hash}
  integration-commit-time: {timestamp_str}",
    );
}

pub fn format_versions<W: Write, O: Offset + Display>(
    version_info: &Versions,
    verbose: bool,
    w: &mut W,
    tz: impl TimeZone<Offset = O>,
) -> Result<()> {
    writeln!(w, "{}", format_version_info("ffx", &version_info.tool_version, verbose, &tz))
        .bug()?;

    if let Some(daemon_version_info) = version_info.daemon_version.as_ref() {
        writeln!(w, "\n{}", format_version_info("daemon", daemon_version_info, verbose, &tz))
            .bug()?;
    }
    Ok(())
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::test::*;
    use chrono::Utc;

    fn run_version_test(
        tool_version: VersionInfo,
        daemon_version: Option<VersionInfo>,
        verbose: bool,
    ) -> String {
        let mut writer = Vec::new();
        let versions = Versions { tool_version, daemon_version };
        let result = format_versions(&versions, verbose, &mut writer, Utc);
        assert!(result.is_ok());
        String::from_utf8(writer).unwrap()
    }

    fn assert_lines(output: String, expected_lines: Vec<String>) {
        let output_lines: Vec<&str> = output.lines().collect();

        if output_lines.len() != expected_lines.len() {
            let mut writer = std::io::stdout();
            writeln!(&mut writer, "FULL OUTPUT: \n{}\n", output).unwrap();
            writer.flush().unwrap();
            assert!(false, "{} lines =/= {} lines", output_lines.len(), expected_lines.len());
        }

        for (out_line, expected_line) in output_lines.iter().zip(expected_lines) {
            if !expected_line.is_empty() {
                if !out_line.contains(&expected_line) {
                    assert!(false, "'{}' does not contain '{}'", out_line, expected_line);
                }
            }
        }
    }

    #[test]
    fn test_success() {
        let output = run_version_test(frontend_info(), None, false);
        assert_eq!(output, format!("{}\n", FAKE_FRONTEND_BUILD_VERSION));
    }

    #[test]
    fn test_empty_version_info_not_verbose() {
        let output = run_version_test(VersionInfo::default(), None, false);
        assert_eq!(output, "(unknown build version)\n");
    }

    #[test]
    fn test_success_verbose() {
        let output = run_version_test(frontend_info(), Some(daemon_info()), true);
        assert_lines(
            output,
            vec![
                "ffx:".to_string(),
                format!("  abi-revision: {}", ABI_REVISION_STR),
                format!("  api-level: {}", FAKE_API_LEVEL),
                format!("  build-version: {}", FAKE_FRONTEND_BUILD_VERSION),
                format!("  integration-commit-hash: {}", FAKE_FRONTEND_HASH),
                format!("  integration-commit-time: {}", TIMESTAMP_STR),
                String::default(),
                "daemon:".to_string(),
                format!("  abi-revision: {}", ABI_REVISION_STR),
                format!("  api-level: {}", FAKE_API_LEVEL),
                format!("  build-version: {}", FAKE_DAEMON_BUILD_VERSION),
                format!("  integration-commit-hash: {}", FAKE_DAEMON_HASH),
                format!("  integration-commit-time: {}", TIMESTAMP_STR),
            ],
        );
    }

    #[test]
    fn test_frontend_empty_and_daemon_returns_none() {
        let output = run_version_test(VersionInfo::default(), Some(VersionInfo::default()), true);

        assert_lines(
            output,
            vec![
                "ffx:".to_string(),
                "  abi-revision: (unknown ABI revision)".to_string(),
                "  api-level: (unknown API level)".to_string(),
                "  build-version: (unknown build version)".to_string(),
                "  integration-commit-hash: (unknown)".to_string(),
                "  integration-commit-time: (unknown commit time)".to_string(),
                String::default(),
                "daemon:".to_string(),
                "  abi-revision: (unknown ABI revision)".to_string(),
                "  api-level: (unknown API level)".to_string(),
                "  build-version: (unknown build version)".to_string(),
                "  integration-commit-hash: (unknown)".to_string(),
                "  integration-commit-time: (unknown commit time)".to_string(),
            ],
        );
    }
}
