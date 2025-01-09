// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::anyhow;
use errors::{FfxError, IntoExitCode, SourceError};
use fidl_fuchsia_developer_ffx::{
    DaemonError, OpenTargetError, TargetConnectionError, TunnelError,
};
/// The default target name if no target spec is given (for debugging, reporting to the user, etc).
/// TODO(b/371222096): Use this everywhere (will require a bit of digging).
pub const UNSPECIFIED_TARGET_NAME: &str = "[unspecified]";
pub const BUG_REPORT_URL: &str =
    "https://issues.fuchsia.dev/issues/new?component=1378294&template=1838957";

/// FfxTargetError is an error type that maps FIDL errors onto to an error type
/// that can derive |thiserror::Error| for better error messages. These errors should
/// be use for target based (include daemon) libraries.
/// To expose these errors at a higher level with ffx subtools that are not interested
/// in accessing the FIDL error, FfxTargetError should be transformed into FfxError using the
/// Into trait.
#[derive(thiserror::Error, Clone, Debug)]
pub enum FfxTargetError {
    //#[error("{}", .0)]
    // Error(#[source] anyhow::Error, i32 /* Error status code */),
    #[cfg(not(target_os = "fuchsia"))]
    #[error("{}", match .err {
            DaemonError::Timeout => format!("Timeout attempting to reach target {}", target_string(.target)),
            DaemonError::TargetCacheEmpty => format!("No devices found."),
            DaemonError::TargetAmbiguous => format!("Target specification {} matched multiple targets. Use `ffx target list` to list known targets, and use a more specific matcher.", target_string(.target)),
            DaemonError::TargetNotFound => format!("Target {} was not found.", target_string(.target)),
            DaemonError::ProtocolNotFound => "The requested ffx service was not found. Run `ffx doctor --restart-daemon`.".to_string(),
            DaemonError::ProtocolOpenError => "The requested ffx service failed to open. Run `ffx doctor --restart-daemon`.".to_string(),
            DaemonError::BadProtocolRegisterState => "The requested service could not be registered. Run `ffx doctor --restart-daemon`.".to_string(),
        })]
    DaemonError { err: DaemonError, target: Option<String> },

    #[cfg(not(target_os = "fuchsia"))]
    #[error("{}", match .err {
            OpenTargetError::FailedDiscovery => format!("Could not resolve specification {} due to discovery failure", target_string(.target)),
            OpenTargetError::QueryAmbiguous => {
                match target_string(.target) {
                    target if target == "\"unspecified\"" => format!("More than one device/emulator found. Use `ffx target list` to list known targets and choose a target with `ffx -t`."),
                    target => format!("Target specification {} matched multiple targets. Use `ffx target list` to list known targets, and use a more specific matcher.", target),
                }
            },
            OpenTargetError::TargetNotFound => format!("Target specification {} was not found. Use `ffx target list` to list known targets, and use a different matcher.", target_string(.target))
        })]
    OpenTargetError { err: OpenTargetError, target: Option<String> },

    #[cfg(not(target_os = "fuchsia"))]
    #[error("{}", match .err {
            TunnelError::CouldNotListen => "Could not establish a host-side TCP listen socket".to_string(),
            TunnelError::TargetConnectFailed => "Couldn not connect to target to establish a tunnel".to_string(),
        })]
    TunnelError { err: TunnelError, target: Option<String> },

    #[cfg(not(target_os = "fuchsia"))]
    #[error("{}", match .err {
            TargetConnectionError::PermissionDenied => format!("Could not establish SSH connection to the target {}: Permission denied.", target_string(.target)),
            TargetConnectionError::ConnectionRefused => format!("Could not establish SSH connection to the target {}: Connection refused.", target_string(.target)),
            TargetConnectionError::ConnectionClosedByRemoteHost => format!("Could not establish SSH connection to the target {}: Connection closed by remote host.", target_string(.target)),
            TargetConnectionError::UnknownNameOrService => format!("Could not establish SSH connection to the target {}: Unknown name or service.", target_string(.target)),
            TargetConnectionError::Timeout => format!("Could not establish SSH connection to the target {}: Timed out awaiting connection.", target_string(.target)),
            TargetConnectionError::KeyVerificationFailure => format!("Could not establish SSH connection to the target {}: Key verification failed.", target_string(.target)),
            TargetConnectionError::NoRouteToHost => format!("Could not establish SSH connection to the target {}: No route to host.", target_string(.target)),
            TargetConnectionError::NetworkUnreachable => format!("Could not establish SSH connection to the target {}: Network unreachable.", target_string(.target)),
            TargetConnectionError::InvalidArgument => format!("Could not establish SSH connection to the target {}: Invalid argument. Please check the address of the target you are attempting to add.", target_string(.target)),
            TargetConnectionError::UnknownError => format!("Could not establish SSH connection to the target {}. {}. Report the error to the FFX team at {BUG_REPORT_URL}", target_string(.target), .logs.as_ref().map(|s| s.as_str()).unwrap_or("As-yet unknown error. Please refer to the logs at `ffx config get log.dir` and look for 'Unknown host-pipe error received'")),
            TargetConnectionError::FidlCommunicationError => format!("Connection was established to {}, but FIDL communication to the Remote Control Service failed. It may help to try running the command again. If this problem persists, please open a bug at {BUG_REPORT_URL}", target_string(.target)),
            TargetConnectionError::RcsConnectionError => format!("Connection was established to {}, but the Remote Control Service failed initiating a test connection. It may help to try running the command again. If this problem persists, please open a bug at {BUG_REPORT_URL}", target_string(.target)),
            TargetConnectionError::FailedToKnockService => format!("Connection was established to {}, but the Remote Control Service test connection was dropped prematurely. It may help to try running the command again. If this problem persists, please open a bug at {BUG_REPORT_URL}", target_string(.target)),
            TargetConnectionError::TargetIncompatible => {
                match .logs.as_ref() {
                    Some(l) => format!("{l}."),
                    None => format!(
                        "ffx revision {:#X} is not compatible with the target. Unable to determine target ABI revision.",
                        version_history_data::HISTORY.get_misleading_version_for_ffx().abi_revision.as_u64(),
                    ),
                }
            },
        })]
    TargetConnectionError {
        err: TargetConnectionError,
        target: Option<String>,
        logs: Option<String>,
    },
}

pub fn target_string(matcher: &Option<String>) -> String {
    match matcher {
        &None => "\"unspecified\"".to_string(),
        &Some(ref s) if s.is_empty() => "\"unspecified\"".to_string(),
        &Some(ref s) => format!("\"{s}\""),
    }
}

/// Convenience function for converting protocol connection requests into more
/// diagnosable/actionable errors for the user.
pub fn map_daemon_error(svc_name: &str, err: DaemonError) -> anyhow::Error {
    match err {
        DaemonError::ProtocolNotFound => anyhow!(
            "The daemon protocol '{svc_name}' did not match any protocols on the daemon
If you are not developing this plugin or the protocol it connects to, then this is a bug
Please report it at https://fxbug.dev/new/ffx+User+Bug."
        ),
        DaemonError::ProtocolOpenError => anyhow!(
            "The daemon protocol '{svc_name}' failed to open on the daemon.
If you are developing the protocol, there may be an internal failure when invoking the start
function. See the ffx.daemon.log for details at `ffx config get log.dir -p sub`.
If you are NOT developing this plugin or the protocol it connects to, then this is a bug.
Please report it at https://fxbug.dev/new/ffx+User+Bug."
        ),
        unexpected => anyhow!(
"While attempting to open the daemon protocol '{svc_name}', received an unexpected error:
{unexpected:?}
This is not intended behavior and is a bug.
Please report it at https://fxbug.dev/new/ffx+User+Bug."
        ),
    }
    .into()
}

impl SourceError for FfxTargetError {}

impl IntoExitCode for FfxTargetError {
    fn exit_code(&self) -> i32 {
        match self {
            FfxTargetError::DaemonError { err, .. } => {
                i32::try_from(err.into_primitive()).unwrap_or(1)
            }
            FfxTargetError::OpenTargetError { err, .. } => {
                i32::try_from(err.into_primitive()).unwrap_or(1)
            }
            FfxTargetError::TunnelError { err, .. } => {
                i32::try_from(err.into_primitive()).unwrap_or(1)
            }
            FfxTargetError::TargetConnectionError { err, .. } => {
                i32::try_from(err.into_primitive()).unwrap_or(1)
            }
        }
    }
}
impl Into<FfxError> for FfxTargetError {
    fn into(self) -> FfxError {
        match self {
            FfxTargetError::DaemonError { ref target, .. } => {
                FfxError::DaemonError { err: Box::new(self.clone()), target: target.clone() }
            }
            FfxTargetError::OpenTargetError { ref target, .. } => {
                FfxError::OpenTargetError { err: Box::new(self.clone()), target: target.clone() }
            }
            FfxTargetError::TunnelError { ref target, .. } => {
                FfxError::TunnelError { err: Box::new(self.clone()), target: target.clone() }
            }
            FfxTargetError::TargetConnectionError { ref logs, ref target, .. } => {
                FfxError::TargetConnectionError {
                    err: Box::new(self.clone()),
                    target: target.clone(),
                    logs: logs.clone(),
                }
            }
        }
    }
}

#[cfg(cw)]
mod cw {
    #[cfg(not(target_os = "fuchsia"))]
    impl IntoExitCode for DaemonError {
        fn exit_code(&self) -> i32 {
            match self {
                DaemonError::Timeout => 14,
                DaemonError::TargetCacheEmpty => 15,
                DaemonError::TargetAmbiguous => 16,
                DaemonError::TargetNotFound => 17,
                DaemonError::ProtocolNotFound => 20,
                DaemonError::ProtocolOpenError => 21,
                DaemonError::BadProtocolRegisterState => 22,
            }
        }
    }

    #[cfg(not(target_os = "fuchsia"))]
    impl IntoExitCode for OpenTargetError {
        fn exit_code(&self) -> i32 {
            match self {
                OpenTargetError::TargetNotFound => 26,
                OpenTargetError::QueryAmbiguous => 27,
            }
        }
    }

    #[cfg(not(target_os = "fuchsia"))]
    impl IntoExitCode for TunnelError {
        fn exit_code(&self) -> i32 {
            match self {
                TunnelError::CouldNotListen => 31,
                TunnelError::TargetConnectFailed => 32,
            }
        }
    }

    #[cfg(not(target_os = "fuchsia"))]
    impl IntoExitCode for TargetConnectionError {
        fn exit_code(&self) -> i32 {
            match self {
                TargetConnectionError::PermissionDenied => 41,
                TargetConnectionError::ConnectionRefused => 42,
                TargetConnectionError::UnknownNameOrService => 43,
                TargetConnectionError::Timeout => 44,
                TargetConnectionError::KeyVerificationFailure => 45,
                TargetConnectionError::NoRouteToHost => 46,
                TargetConnectionError::NetworkUnreachable => 47,
                TargetConnectionError::InvalidArgument => 48,
                TargetConnectionError::UnknownError => 49,
                TargetConnectionError::FidlCommunicationError => 50,
                TargetConnectionError::RcsConnectionError => 51,
                TargetConnectionError::FailedToKnockService => 52,
                TargetConnectionError::TargetIncompatible => 53,
                TargetConnectionError::ConnectionClosedByRemoteHost => 54,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_daemon_error_strings_containing_target_name() {
        fn assert_contains_target_name(err: DaemonError) {
            let name: Option<String> = Some("fuchsia-f00d".to_string());
            assert!(format!("{}", FfxError::DaemonError { err, target: name.clone() })
                .contains(name.as_ref().unwrap()));
        }
        assert_contains_target_name(DaemonError::Timeout);
        assert_contains_target_name(DaemonError::TargetAmbiguous);
        assert_contains_target_name(DaemonError::TargetNotFound);
    }
    #[test]
    fn test_target_string() {
        assert_eq!(target_string(&None), UNSPECIFIED_TARGET_NAME);
        assert_eq!(target_string(&Some("".to_string())), UNSPECIFIED_TARGET_NAME);
        assert_eq!(target_string(&Some("kittens".to_string())), "\"kittens\"");
    }
}
