// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::{anyhow, Context as _, Result},
    direct_mode::{get_ld_from_bin, ProcessLoader},
    fidl_fuchsia_io as fio, fidl_fuchsia_kernel as fkernel,
    fuchsia_component::client::connect_to_protocol,
    fuchsia_fs::file::open_in_namespace,
    fuchsia_runtime::{take_startup_handle, HandleInfo, HandleType},
    fuchsia_zircon::{Guest, HandleBased, Rights, Status, Vmo},
    process_builder::StartupHandle,
    std::ffi::CString,
};

// The index of the direct vDSO within the startup handles.
const DIRECT_VDSO_INDEX: u16 = 1;

/// Take the direct vDSO from the startup handles.
///
/// If the vDSO is unavailable, this function will panic.
pub fn take_direct_vdso() -> Vmo {
    let handle = take_startup_handle(HandleInfo::new(HandleType::VdsoVmo, DIRECT_VDSO_INDEX))
        .expect("Failed to take direct vDSO, please check component manifest");
    Vmo::from(handle)
}

/// Start an ELF binary, by loading and executing it.
///
/// This is a helper function that makes it easy to load and execute an ELF
/// binary by providing the direct vDSO (from `take_direct_vdso`), and then the
/// `args`, `vars`, `paths`, and `handles` for the process.
pub async fn start(
    vdso_vmo: Vmo,
    args: Vec<CString>,
    vars: Vec<CString>,
    paths: Vec<CString>,
    mut handles: Vec<StartupHandle>,
) -> Result<()> {
    // Create the guest.
    let hypervisor = connect_to_protocol::<fkernel::HypervisorResourceMarker>()
        .context("connect to hypervisor resource")?;
    let resource = hypervisor.get().await.context("get hypervisor resource")?;
    let (guest, guest_vmar) = Guest::direct(&resource).context("create guest")?;
    if vars.iter().any(|x| x.as_bytes() == b"DIRECT_MODE=use_guest") {
        let handle =
            guest.duplicate_handle(Rights::SAME_RIGHTS).context("duplicate guest handle")?;
        handles.push(StartupHandle {
            handle: handle.into(),
            info: HandleInfo::new(HandleType::User0, 0),
        });
    }

    let name = args.first().ok_or_else(|| anyhow!("first process argument"))?;
    let name = name.to_str().context("first process argument as string")?;

    // Load the binary.
    let mut bin_vmo = open_in_namespace(
        name,
        fio::OpenFlags::NOT_DIRECTORY
            | fio::OpenFlags::RIGHT_READABLE
            | fio::OpenFlags::RIGHT_EXECUTABLE,
    )
    .with_context(|| format!("{}: open", name))?
    .get_backing_memory(fio::VmoFlags::READ | fio::VmoFlags::EXECUTE)
    .await
    .with_context(|| format!("{}: get backing memory call", name))?
    .map_err(Status::from_raw)
    .with_context(|| format!("{}: get backing memory response", name))?;

    // Load the linker
    let (ld_vmo, loader) =
        get_ld_from_bin(&mut bin_vmo).await.with_context(|| format!("{}: get ld", name))?;

    // Load the process.
    let process = ProcessLoader::new(guest, guest_vmar)
        .vdso(vdso_vmo)?
        .bin(bin_vmo)
        .ld(ld_vmo)
        .loader(loader)
        .args(args)
        .vars(vars)
        .paths(paths)
        .handles(handles)
        .load()
        .context("load")?;

    // Run the process.
    process.run().context("run")
}
