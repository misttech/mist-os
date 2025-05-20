// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Context;
use async_trait::async_trait;
use ffx_assembly_args::*;
use ffx_writer::SimpleWriter;
use fho::{FfxMain, FfxTool, Result};
mod base_package;
mod blobfs;
mod compiled_package;
mod extra_hash_descriptor;
mod fvm;
mod fxfs;
mod operations;
mod subpackage_blobs_package;
mod zbi;
use assembly_components as _;
pub mod vbmeta;
pub mod vfs;

#[derive(FfxTool)]
pub struct AssemblyTool {
    #[command]
    cmd: AssemblyCommand,
}

#[async_trait(?Send)]
impl FfxMain for AssemblyTool {
    type Writer = SimpleWriter;
    async fn main(self, _writer: SimpleWriter) -> Result<()> {
        // Dispatch to the correct operation based on the command.
        // The context() is used to display which operation failed in the event of
        // an error.
        match self.cmd.op_class {
            OperationClass::CreateSystem(args) => {
                operations::create_system::create_system(args).await.context("Create System")
            }
            OperationClass::CreateUpdate(args) => {
                operations::create_update::create_update(args).context("Create Update Package")
            }
            OperationClass::Product(args) => {
                operations::product::assemble(args).context("Product Assembly")
            }
            OperationClass::SizeCheck(args) => match args.op_class {
                SizeCheckOperationClass::Package(args) => {
                    operations::size_check::package::verify_package_budgets(args)
                        .context("Package size checker")
                }
                SizeCheckOperationClass::Product(args) => {
                    // verify_product_budgets() returns a boolean that indicates whether the budget was
                    // exceeded or not. We don't intend to fail the build when budgets are exceeded so the
                    // returned value is dropped.
                    operations::size_check::product::verify_product_budgets(args)
                        .await
                        .context("Product size checker")
                        .map(|_| ())
                }
            },
        }
        .map_err(fho::Error::User)
    }
}
