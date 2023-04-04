// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::{Context as _, Result},
    direct_mode_vmm::{start, take_direct_vdso},
    fuchsia_async as fasync,
    fuchsia_runtime::{take_startup_handle, HandleInfo, HandleType},
    process_builder::{NamespaceEntry, StartupHandle},
    std::{
        env,
        ffi::{CString, NulError},
        io,
    },
};

/// A helper binary that will load the ELF binary provided as its first
/// argument into direct mode.
#[fasync::run_singlethreaded]
async fn main() -> Result<()> {
    let vdso_vmo = take_direct_vdso();
    let args = env::args()
        .skip(1)
        .map(CString::new)
        .collect::<Result<Vec<_>, NulError>>()
        .context("args")?;
    let vars = env::vars()
        .map(|(key, val)| CString::new(format!("{}={}", key, val)))
        .collect::<Result<Vec<_>, NulError>>()
        .context("vars")?;
    let namespace = fdio::Namespace::installed().context("root namespace")?;
    let entries = namespace.export().context("namespace export")?;
    let mut namespace_entries = Vec::with_capacity(entries.len());
    for fdio::NamespaceEntry { handle, path } in entries {
        let path = CString::new(path).context("namespace entry path")?;
        let directory = handle.into();
        namespace_entries.push(NamespaceEntry { path, directory })
    }
    let mut handles = vec![
        StartupHandle {
            handle: fdio::clone_fd(&io::stdout()).context("clone stdout")?,
            info: HandleInfo::new(HandleType::FileDescriptor, 1),
        },
        StartupHandle {
            handle: fdio::clone_fd(&io::stderr()).context("clone stderr")?,
            info: HandleInfo::new(HandleType::FileDescriptor, 2),
        },
    ];
    if let Some(handle) = take_startup_handle(HandleType::DirectoryRequest.into()) {
        handles.push(StartupHandle { handle, info: HandleType::DirectoryRequest.into() })
    }
    if let Some(handle) = take_startup_handle(HandleType::ComponentConfigVmo.into()) {
        handles.push(StartupHandle { handle, info: HandleType::ComponentConfigVmo.into() })
    }
    start(vdso_vmo, args, vars, namespace_entries, handles).await
}
