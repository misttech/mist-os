// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::{marker::PhantomData, process::ExitStatus};

use async_trait::async_trait;
use ffx_command::{return_user_error, MetricsSession, Result, ToolRunner};
use ffx_validation::{schema::output::machine, simplify};
use ffx_writer::{MachineWriter, ToolIO};

struct SchemaRunner<T> {
    output_format: Option<ffx_writer::Format>,
    _pd: PhantomData<T>,
}

pub(crate) fn runner<T: ToolIO>(output_format: Option<ffx_writer::Format>) -> impl ToolRunner {
    SchemaRunner::<T> { output_format, _pd: PhantomData }
}

pub fn print_schema<T: ToolIO>(output_format: Option<ffx_writer::Format>) -> Result<()> {
    let Some(mut schema) = T::OUTPUT_SCHEMA else {
        return_user_error!("This subcommand does not have a machine output schema");
    };

    let Some(format) = output_format else {
        return_user_error!("Human-readable schema output not yet implemented");
    };

    let mut writer = MachineWriter::<_, machine::MachineSchema>::new(Some(format));

    let bump = bumpalo::Bump::new();
    let mut ctx = simplify::Ctx::new();
    ctx.process_type(&bump, &mut schema);

    writer.machine(&machine::serialize(&bump, &ctx, schema))?;

    Ok(())
}

#[async_trait(?Send)]
impl<T: ToolIO> ToolRunner for SchemaRunner<T> {
    fn forces_stdout_log(&self) -> bool {
        false
    }

    async fn run(self: Box<Self>, _metrics: MetricsSession) -> Result<ExitStatus> {
        print_schema::<T>(self.output_format).map(|_| ExitStatus::default())
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_print_fails_when_schema_absent() {
        let err = print_schema::<ffx_writer::SimpleWriter>(None).expect_err("Should not succeed");

        assert!(
            format!("{err}").contains("not have a machine output schema"),
            "Unexpected error output: {err}"
        );

        let err =
            print_schema::<ffx_writer::MachineWriter<()>>(None).expect_err("Should not succeed");

        assert!(
            format!("{err}").contains("not have a machine output schema"),
            "Unexpected error output: {err}"
        );
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_print_fails_human_output_not_yet_impl() {
        let err = print_schema::<ffx_writer::VerifiedMachineWriter<()>>(None)
            .expect_err("Should not succeed");

        assert!(format!("{err}").contains("not yet implemented"), "Unexpected error output: {err}");
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_print_machine_output() {
        print_schema::<ffx_writer::VerifiedMachineWriter<()>>(Some(ffx_writer::Format::Json))
            .expect("Should succeed");
    }
}
