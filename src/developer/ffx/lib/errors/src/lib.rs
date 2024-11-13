// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#[cfg(not(target_os = "fuchsia"))]
use anyhow::anyhow;

use std::process::ExitStatus;

#[cfg(not(target_os = "fuchsia"))]
use fidl_fuchsia_developer_ffx::{
    DaemonError, OpenTargetError, TargetConnectionError, TunnelError,
};

/// Re-exported libraries for macros
#[doc(hidden)]
pub mod macro_deps {
    pub use anyhow;
}

pub const BUG_REPORT_URL: &str =
    "https://issues.fuchsia.dev/issues/new?component=1378294&template=1838957";

/// The ffx main function expects a anyhow::Result from ffx plugins. If the Result is an Err it be
/// downcast to FfxError, and if successful this error is presented as a user-readable error. All
/// other error types are printed with full context and a BUG prefix, guiding the user to file bugs
/// to improve the error condition that they have experienced, with a goal to maximize actionable
/// errors over time.
// TODO(https://fxbug.dev/42135455): consider extending this to allow custom types from plugins.
#[derive(thiserror::Error, Debug)]
pub enum FfxError {
    #[error("{}", .0)]
    Error(#[source] anyhow::Error, i32 /* Error status code */),

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
    #[error("Testing Error")]
    TestingError, // this is here to be used in tests for verifying errors are translated properly.
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
#[cfg(not(target_os = "fuchsia"))]
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

// Utility macro for constructing a FfxError::Error with a simple error string.
#[macro_export]
macro_rules! ffx_error {
    ($error_message: expr) => {{
        $crate::FfxError::Error($crate::macro_deps::anyhow::anyhow!($error_message), 1)
    }};
    ($fmt:expr, $($arg:tt)*) => {
        $crate::ffx_error!(format!($fmt, $($arg)*));
    };
}

#[macro_export]
macro_rules! ffx_error_with_code {
    ($error_code:expr, $error_message:expr $(,)?) => {{
        $crate::FfxError::Error($crate::macro_deps::anyhow::anyhow!($error_message), $error_code)
    }};
    ($error_code:expr, $fmt:expr, $($arg:tt)*) => {
        $crate::ffx_error_with_code!($error_code, format!($fmt, $($arg)*));
    };
}

#[macro_export]
macro_rules! ffx_bail {
    ($msg:literal $(,)?) => {
        return Err($crate::ffx_error!($msg).into())
    };
    ($fmt:expr, $($arg:tt)*) => {
        return Err($crate::ffx_error!($fmt, $($arg)*).into());
    };
}

#[macro_export]
macro_rules! ffx_bail_with_code {
    ($code:literal, $msg:literal $(,)?) => {
        return Err($crate::ffx_error_with_code!($code, $msg).into())
    };
    ($code:expr, $fmt:expr, $($arg:tt)*) => {
        return Err($crate::ffx_error_with_code!($code, $fmt, $($arg)*).into());
    };
}

pub trait IntoExitCode {
    fn exit_code(&self) -> i32;
}

pub trait ResultExt: IntoExitCode {
    fn ffx_error<'a>(&'a self) -> Option<&'a FfxError>;
}

impl ResultExt for anyhow::Error {
    fn ffx_error<'a>(&'a self) -> Option<&'a FfxError> {
        self.downcast_ref()
    }
}

impl IntoExitCode for anyhow::Error {
    fn exit_code(&self) -> i32 {
        match self.downcast_ref() {
            Some(FfxError::Error(_, code)) => *code,
            _ => 1,
        }
    }
}

impl IntoExitCode for FfxError {
    fn exit_code(&self) -> i32 {
        match self {
            FfxError::Error(_, code) => *code,
            FfxError::TestingError => 254,
            #[cfg(not(target_os = "fuchsia"))]
            FfxError::DaemonError { err, target: _ } => err.exit_code(),
            #[cfg(not(target_os = "fuchsia"))]
            FfxError::OpenTargetError { err, target: _ } => err.exit_code(),
            #[cfg(not(target_os = "fuchsia"))]
            FfxError::TunnelError { err, target: _ } => err.exit_code(),
            #[cfg(not(target_os = "fuchsia"))]
            FfxError::TargetConnectionError { err, target: _, logs: _ } => err.exit_code(),
        }
    }
}

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

// so that Result<(), E>::Ok is treated as exit code 0.
impl IntoExitCode for () {
    fn exit_code(&self) -> i32 {
        0
    }
}

impl IntoExitCode for ExitStatus {
    fn exit_code(&self) -> i32 {
        self.code().unwrap_or(0)
    }
}

impl<T, E> ResultExt for Result<T, E>
where
    T: IntoExitCode,
    E: ResultExt,
{
    fn ffx_error<'a>(&'a self) -> Option<&'a FfxError> {
        match self {
            Ok(_) => None,
            Err(ref err) => err.ffx_error(),
        }
    }
}

impl<T, E> IntoExitCode for Result<T, E>
where
    T: IntoExitCode,
    E: ResultExt,
{
    fn exit_code(&self) -> i32 {
        match self {
            Ok(code) => code.exit_code(),
            Err(err) => err.exit_code(),
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use anyhow::anyhow;
    use assert_matches::assert_matches;

    const FFX_STR: &str = "I am an ffx error";
    const ERR_STR: &str = "I am not an ffx error";

    #[test]
    fn test_ffx_result_extension() {
        let err = anyhow::Result::<()>::Err(anyhow!(ERR_STR));
        assert!(err.ffx_error().is_none());

        let err = anyhow::Result::<()>::Err(anyhow::Error::new(ffx_error!(FFX_STR)));
        assert_matches!(err.ffx_error(), Some(FfxError::Error(_, _)));
    }

    #[test]
    fn test_result_ext_exit_code_arbitrary_error() {
        let err = Result::<(), _>::Err(anyhow!(ERR_STR));
        assert_eq!(err.exit_code(), 1);
    }

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
        assert_eq!(target_string(&None), "\"unspecified\"");
        assert_eq!(target_string(&Some("".to_string())), "\"unspecified\"");
        assert_eq!(target_string(&Some("kittens".to_string())), "\"kittens\"");
    }
}
