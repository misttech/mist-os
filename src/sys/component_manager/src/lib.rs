// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// This is needed for the pseudo_directory nesting in crate::model::tests
#![recursion_limit = "256"]
// Printing to stdout and stderr directly is discouraged for component_manager.
// Instead, the tracing library, e.g. through macros like `info!`, and `error!`,
// should be used.
#![cfg_attr(not(test), deny(clippy::print_stdout, clippy::print_stderr,))]

pub mod bootfs;
pub mod builtin;
pub mod builtin_environment;
pub mod capability;
pub mod elf_runner;
pub mod framework;
pub mod model;
pub mod startup;

pub(crate) mod directory_ready_notifier;
pub(crate) mod root_stop_notifier;

mod constants;
mod diagnostics;
