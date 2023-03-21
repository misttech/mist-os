// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

mod artifacts;
mod cancel;
mod connector;
pub mod diagnostics;
mod outcome;
pub mod output;
mod params;
mod realm;
mod run;
mod running_suite;
mod stream_util;
mod trace;

pub use {
    connector::{RunBuilderConnector, SingleRunConnector},
    outcome::{ConnectionError, Outcome, RunTestSuiteError, UnexpectedEventError},
    params::{RunParams, TestParams, TimeoutBehavior},
    realm::parse_provided_realm,
    run::{create_reporter, run_tests_and_get_outcome, DirectoryReporterOptions},
};
