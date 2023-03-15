// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use {
    crate::elf::resolve::{get_binary_from_pkg_dir, get_loader_from_pkg_dir},
    crate::process::{self as process},
    crate::Start,
    anyhow::{Context as _, Error},
    async_trait::async_trait,
    fidl_fuchsia_io as fio,
    fuchsia_runtime::{HandleInfo, HandleType},
    fuchsia_zircon::{self as zx, HandleBased},
    lazy_static::lazy_static,
    process_builder::{BuiltProcess, ProcessBuilder, StartupHandle},
    processargs::ProcessArgs,
    std::iter::once,
};

mod resolve;

/// Returns a VMO of the VDSO provided to the current process.
fn get_system_vdso_vmo() -> Result<zx::Vmo, zx::Status> {
    lazy_static! {
        static ref VDSO_VMO: zx::Vmo = {
            zx::Vmo::from(
                fuchsia_runtime::take_startup_handle(HandleInfo::new(HandleType::VdsoVmo, 0))
                    .expect("Failed to take VDSO VMO startup handle"),
            )
        };
    }

    VDSO_VMO.duplicate_handle(zx::Rights::SAME_RIGHTS)
}

pub struct Elf {
    built_process: process::BuiltProcess,
}

impl Elf {
    pub async fn create(
        executable: zx::Vmo,
        process_create_params: process::CreateParams,
        processargs: ProcessArgs,
    ) -> Result<Self, Error> {
        let built_process = build_process(executable, process_create_params, processargs).await?;

        Ok(Self { built_process: process::BuiltProcess(built_process) })
    }

    pub async fn create_from_package(
        executable_path: &str,
        package: fio::DirectoryProxy,
        process_create_params: process::CreateParams,
        mut processargs: ProcessArgs,
    ) -> Result<Self, Error> {
        let exec_vmo = get_binary_from_pkg_dir(&package, executable_path).await?;
        let ldsvc = get_loader_from_pkg_dir(&package).await?;

        // TODO(fxbug.dev/122024): Loader handle might already exist in processargs
        processargs.add_handles(once(StartupHandle {
            handle: ldsvc.into_handle(),
            info: HandleInfo::new(HandleType::LdsvcLoader, 0),
        }));

        Elf::create(exec_vmo, process_create_params, processargs).await
    }
}

#[async_trait]
impl Start for Elf {
    type Error = zx::Status;
    type Stop = process::Process;

    async fn start(self) -> Result<Self::Stop, Self::Error> {
        self.built_process.start().await
    }
}

/// Builds a `BuiltProcess` from an executable, parameters, and processargs.
///
/// This creates a Zircon process, but does not start it.
async fn build_process(
    executable: zx::Vmo,
    create_params: process::CreateParams,
    processargs: ProcessArgs,
) -> Result<BuiltProcess, Error> {
    let mut process_builder = ProcessBuilder::new(
        &create_params.name,
        &create_params.job,
        create_params.options,
        executable,
        get_system_vdso_vmo().unwrap(),
    )
    .context("failed to create ProcessBuilder")?;

    process_builder.add_arguments(processargs.args);
    process_builder.add_handles(processargs.handles)?;
    process_builder.add_environment_variables(processargs.environment_variables);
    process_builder.add_namespace_entries(processargs.namespace_entries)?;

    process_builder.build().await.context("failed to build process")
}
