// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::{marker::PhantomData, process::ExitStatus};

use async_trait::async_trait;
use ffx_command::{return_user_error, MetricsSession, Result, ToolRunner};
use ffx_writer::ToolIO;

struct SchemaRunner<T> {
    _pd: PhantomData<T>,
}

pub(crate) fn runner<T: ToolIO>() -> impl ToolRunner {
    SchemaRunner::<T> { _pd: PhantomData }
}

pub fn print_schema<T: ToolIO>() -> Result<()> {
    let Some(_schema) = T::OUTPUT_SCHEMA else {
        return_user_error!("This subcommand does not have a machine output schema");
    };
    return_user_error!("Schema output not yet implemented")
}

#[async_trait(?Send)]
impl<T: ToolIO> ToolRunner for SchemaRunner<T> {
    fn forces_stdout_log(&self) -> bool {
        false
    }

    async fn run(self: Box<Self>, _metrics: MetricsSession) -> Result<ExitStatus> {
        print_schema::<T>().map(|_| ExitStatus::default())
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_print_fails_when_schema_absent() {
        let err = print_schema::<ffx_writer::SimpleWriter>().expect_err("Should not succeed");

        assert!(
            format!("{err}").contains("not have a machine output schema"),
            "Unexpected error output: {err}"
        );

        let err = print_schema::<ffx_writer::MachineWriter<()>>().expect_err("Should not succeed");

        assert!(
            format!("{err}").contains("not have a machine output schema"),
            "Unexpected error output: {err}"
        );
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_print_fails_not_yet_impl() {
        let err = print_schema::<ffx_writer::VerifiedMachineWriter<()>>()
            .expect_err("Should not succeed");

        assert!(format!("{err}").contains("not yet implemented"), "Unexpected error output: {err}");
    }
}
