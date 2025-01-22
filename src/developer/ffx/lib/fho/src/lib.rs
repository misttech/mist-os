// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

mod adapters;
mod connector;
mod fho_env;
mod from_env;
mod try_from_env;

pub mod subtool;
pub mod testing;

pub use subtool::{FfxMain, FfxTool};

// Re-export TryFromEnv related symbols
pub use from_env::{
    daemon_protocol, init_daemon_behavior, moniker, moniker_f, AvailabilityFlag, CheckEnv,
    DeviceLookupDefaultImpl,
};

pub use fho_env::{FhoConnectionBehavior, FhoEnvironment};
pub use try_from_env::{deferred, Deferred, TryFromEnv, TryFromEnvWith};

pub use from_env::{toolbox, toolbox_or};

// Used for deriving an FFX tool.
pub use fho_macro::FfxTool;

// Direct connection to a target device
pub use connector::{DirectConnector, MockDirectConnector};

// Re-expose the Error, Result, and FfxContext types from ffx_command
// so you don't have to pull both in all the time.
pub use ffx_command::{
    bug, exit_with_code, return_bug, return_user_error, user_error, Error, FfxContext,
    NonFatalError, Result,
};

// Re-expose the ffx_writer::Writer as the 'simple writer'
pub use ffx_writer::{
    Format, MachineWriter, SimpleWriter, TestBuffer, TestBuffers, ToolIO, VerifiedMachineWriter,
};

#[doc(hidden)]
pub mod macro_deps {
    pub use async_trait::async_trait;
    pub use ffx_command::{
        bug, check_strict_constraints, return_bug, return_user_error, Ffx, FfxCommandLine,
        ToolRunner,
    };
    pub use ffx_config::{global_env_context, EnvironmentContext};
    pub use ffx_core::Injector;
    pub use {crate as fho, anyhow, argh, async_lock, ffx_writer, futures, serde};
}
