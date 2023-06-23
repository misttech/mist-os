// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::vdso_vmo::VdsoError,
    ::routing::policy::PolicyError,
    clonable_error::ClonableError,
    fuchsia_zircon as zx,
    runner::{
        component::{ComponentNamespaceError, LaunchError},
        StartInfoProgramError,
    },
    thiserror::Error,
    tracing::error,
};

/// Errors produced by `ElfRunner`.
#[derive(Debug, Clone, Error)]
pub enum ElfRunnerError {
    #[error(
        "failed to register as exception handler for component with url \"{}\": {}",
        url,
        status
    )]
    ExceptionRegistrationFailed { url: String, status: zx::Status },
    #[error("failed to retrieve process koid for component with url \"{}\": {}", url, status)]
    ProcessIdRetrieveFailed { url: String, status: zx::Status },
    #[error(
        "failed to mark main process as critical for component with url \"{}\": {}",
        url,
        status
    )]
    ProcessCriticalMarkFailed { url: String, status: zx::Status },
    #[error("could not create job for component with url \"{}\": {}", url, err)]
    JobError {
        url: String,
        #[source]
        err: JobError,
    },
    #[error("failed to duplicate job for component with url \"{}\": {}", url, status)]
    JobDuplicationFailed { url: String, status: zx::Status },
    #[error("failed to get job koid for component with url \"{}\": {}", url, status)]
    JobGetKoidFailed { url: String, status: zx::Status },
    #[error("failed to use next vDSO for component with url \"{}\": {}", url, err)]
    NextVDSOError {
        url: String,
        #[source]
        err: VdsoError,
    },
    #[error("failed to use direct vDSO for component with url \"{}\": {}", url, err)]
    DirectVDSOError {
        url: String,
        #[source]
        err: VdsoError,
    },
    #[error("program key \"{}\" invalid for component with url \"{}\"", key, url)]
    ProgramKeyInvalid { key: String, url: String },
    #[error("{err}")]
    SecurityPolicyError {
        #[from]
        err: PolicyError,
    },
    #[error("error connecting to fuchsia.process.Launcher protocol for {}: {}", url, err)]
    ProcessLauncherConnectError {
        url: String,
        #[source]
        err: ClonableError,
    },
    #[error("fidl error in fuchsia.process.Launcher protocol for {}: {}", url, err)]
    ProcessLauncherFidlError {
        url: String,
        #[source]
        err: fidl::Error,
    },
    #[error("fuchsia.process.Launcher failed to create process for {}: {}", url, status)]
    CreateProcessFailed { url: String, status: zx::Status },
    #[error("fuchsia.process.Launcher did not return a process handle after launching {}", url)]
    NoProcess { url: String },
    #[error("failed to duplicate UTC clock for component with url \"{}\": {}", url, status)]
    UtcClockDuplicateFailed { url: String, status: zx::Status },
    #[error("failed to populate component's structured config vmo: {_0}")]
    ComponentConfigVmoError(
        #[from]
        #[source]
        ConfigError,
    ),
    #[error("attempting to mark process as shared without also enabling `job_policy_create_raw_processes` for component with url \"{}\".", url)]
    SharedProcessMarkFailed { url: String },

    #[error("component start info does not have resolved URL")]
    MissingResolvedUrl,

    #[error("component resolved URL is malformed")]
    BadResolvedUrl(String),

    #[error("could not get program binary for {}: {}", url, err)]
    ProgramBinaryError {
        url: String,
        #[source]
        err: StartInfoProgramError,
    },

    #[error("could not create component namespace for {}, {}", url, err)]
    ComponentNamespaceError {
        url: String,
        #[source]
        err: ComponentNamespaceError,
    },

    #[error("error configuring process launcher for {}: {}", url, err)]
    ConfigureLauncherError {
        url: String,
        #[source]
        err: LaunchError,
    },
}

impl ElfRunnerError {
    pub fn exception_registration_failed(
        url: impl Into<String>,
        status: zx::Status,
    ) -> ElfRunnerError {
        ElfRunnerError::ExceptionRegistrationFailed { url: url.into(), status }
    }

    pub fn process_id_retrieve_failed(
        url: impl Into<String>,
        status: zx::Status,
    ) -> ElfRunnerError {
        ElfRunnerError::ProcessIdRetrieveFailed { url: url.into(), status }
    }

    pub fn job_duplication_failed(url: impl Into<String>, status: zx::Status) -> ElfRunnerError {
        ElfRunnerError::JobDuplicationFailed { url: url.into(), status }
    }

    pub fn process_critical_mark_failed(
        url: impl Into<String>,
        status: zx::Status,
    ) -> ElfRunnerError {
        ElfRunnerError::ProcessCriticalMarkFailed { url: url.into(), status }
    }

    pub fn job_error(url: impl Into<String>, err: JobError) -> ElfRunnerError {
        ElfRunnerError::JobError { url: url.into(), err }
    }

    pub fn next_vdso_error(url: impl Into<String>, err: VdsoError) -> ElfRunnerError {
        ElfRunnerError::NextVDSOError { url: url.into(), err }
    }

    pub fn direct_vdso_error(url: impl Into<String>, err: VdsoError) -> ElfRunnerError {
        ElfRunnerError::DirectVDSOError { url: url.into(), err }
    }

    pub fn program_key_invalid(key: impl Into<String>, url: impl Into<String>) -> ElfRunnerError {
        ElfRunnerError::ProgramKeyInvalid { key: key.into(), url: url.into() }
    }

    pub fn shared_process_mark_failed(url: impl Into<String>) -> ElfRunnerError {
        ElfRunnerError::SharedProcessMarkFailed { url: url.into() }
    }

    /// Convert this error into its approximate `zx::Status` equivalent.
    pub fn as_zx_status(&self) -> zx::Status {
        match self {
            ElfRunnerError::MissingResolvedUrl { .. } => zx::Status::INVALID_ARGS,
            ElfRunnerError::BadResolvedUrl { .. } => zx::Status::INVALID_ARGS,
            ElfRunnerError::ProgramBinaryError { .. } => zx::Status::INVALID_ARGS,
            ElfRunnerError::ComponentNamespaceError { .. } => zx::Status::INVALID_ARGS,
            ElfRunnerError::ConfigureLauncherError { .. } => zx::Status::UNAVAILABLE,
            ElfRunnerError::SecurityPolicyError { .. } => zx::Status::ACCESS_DENIED,
            _ => zx::Status::INTERNAL,
        }
    }
}

/// Errors from populating a component's structured configuration.
#[derive(Debug, Clone, Error)]
pub enum ConfigError {
    #[error("failed to create a vmo: {_0}")]
    VmoCreate(zx::Status),
    #[error("failed to write to vmo: {_0}")]
    VmoWrite(zx::Status),
    #[error("encountered an unrecognized variant of fuchsia.mem.Data")]
    UnrecognizedDataVariant,
}

/// Errors from creating and initializing a component's job.
#[derive(Debug, Clone, Error)]
pub enum JobError {
    #[error("failed to set job policy: {}", status)]
    SetPolicy { status: zx::Status },
    #[error("failed to create child job: {}", status)]
    CreateChild { status: zx::Status },
}
