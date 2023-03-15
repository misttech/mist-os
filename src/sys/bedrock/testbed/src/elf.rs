// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::{Context as _, Error},
    exec::{elf::Elf, Program, Start, Stop},
    fidl::HandleBased,
    fidl_fuchsia_examples as fexamples, fidl_fuchsia_io as fio, fuchsia_async as fasync,
    fuchsia_component::client::connect_to_protocol_at_dir_svc,
    fuchsia_runtime::{HandleInfo, HandleType},
    fuchsia_zircon as zx,
    futures::io::AsyncReadExt,
    lazy_static::lazy_static,
    process_builder::StartupHandle,
    processargs::ProcessArgs,
    std::ffi::CString,
    std::iter::once,
    std::path::{Path, PathBuf},
};

const STDOUT_FD: u16 = libc::STDOUT_FILENO as u16;

lazy_static! {
    /// Path to the `bedrock_testbed_hello` executable, relative to the test's namespace.
    static ref HELLO_BIN_PATH: PathBuf = PathBuf::from("/pkg/bin/bedrock_testbed_hello");

    /// Path to the `bedrock_testbed_echo_server` executable, relative to the test's namespace.
    static ref ECHO_SERVER_BIN_PATH: PathBuf = PathBuf::from("/pkg/bin/bedrock_testbed_echo_server");
}

/// Creates an `Elf` that runs the `bedrock_testbed_hello` executable.
async fn create_hello_elf(processargs: ProcessArgs) -> Result<Elf, Error> {
    let hello_vmo = {
        let file = fdio::open_fd(
            HELLO_BIN_PATH.to_str().unwrap(),
            fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_EXECUTABLE,
        )?;
        fdio::get_vmo_exec_from_file(&file)?
    };

    let elf = Elf::create(
        hello_vmo,
        exec::process::CreateParams {
            name: CString::new("hello").unwrap(),
            job: fuchsia_runtime::job_default().duplicate(zx::Rights::SAME_RIGHTS)?,
            options: zx::ProcessOptions::empty(),
        },
        processargs,
    )
    .await?;

    Ok(elf)
}

/// Creates an `Elf` that runs an executable from the testbed package.
async fn create_packaged_elf(executable: &Path, processargs: ProcessArgs) -> Result<Elf, Error> {
    // Open the package directory.
    let pkg_dir = fuchsia_fs::directory::open_in_namespace(
        "/pkg",
        fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_EXECUTABLE,
    )
    .context("failed to open /pkg directory")?;

    let process_name =
        CString::new(executable.file_name().unwrap().to_str().context("filename is not unicode")?)
            .context("filename contains nul bytes")?;
    let executable = executable.strip_prefix("/pkg")?.to_str().unwrap();

    let elf = Elf::create_from_package(
        executable,
        pkg_dir,
        exec::process::CreateParams {
            name: process_name,
            job: fuchsia_runtime::job_default().duplicate(zx::Rights::SAME_RIGHTS)?,
            options: zx::ProcessOptions::empty(),
        },
        processargs,
    )
    .await?;

    Ok(elf)
}

#[fuchsia::test]
async fn elf_program_on_exit() -> Result<(), Error> {
    let mut processargs = ProcessArgs::new();
    processargs.add_default_loader()?;

    // Create the elf.
    let elf = create_hello_elf(processargs).await.context("failed to create Elf")?;

    // Start the program.
    let program = elf.start().await.context("failed to start")?;

    // Wait for the program to exit.
    let return_code = program.on_exit().await?;
    assert_eq!(return_code, 0);

    Ok(())
}

// Tests that `Elf` can run an ELF binary that accepts some arguments and outputs a message
// that contains the arguments to stdout.
#[fuchsia::test]
async fn elf_args_stdout() -> Result<(), Error> {
    // Set up processargs.
    let (stdout_rx, stdout_tx) = zx::Socket::create_stream();

    let mut processargs = ProcessArgs::new();
    processargs
        .add_default_loader()?
        .add_args(vec!["hello", "Alice", "Bob"].into_iter().map(|arg| CString::new(arg).unwrap()))
        .add_handles(once(StartupHandle {
            handle: stdout_tx.into_handle(),
            info: HandleInfo::new(HandleType::FileDescriptor, STDOUT_FD),
        }));

    // Create the elf.
    let elf = create_hello_elf(processargs).await.context("failed to create Elf")?;

    // Start the program.
    let program = elf.start().await.context("failed to start")?;

    // Wait for the program to exit.
    let return_code = program.on_exit().await?;
    assert_eq!(return_code, 0);

    // Read from stdout.
    let mut stdout_rx = fasync::Socket::from_socket(stdout_rx)?;
    let mut buf = vec![];
    stdout_rx.read_to_end(&mut buf).await.context("failed to read stdout socket")?;

    assert_eq!(&*buf, b"Hello, Alice and Bob!\n");

    Ok(())
}

#[fuchsia::test]
async fn packaged_elf_args_stdout() -> Result<(), Error> {
    // Set up stdout.
    let (stdout_rx, stdout_tx) = zx::Socket::create_stream();

    let mut processargs = ProcessArgs::new();
    processargs
        .add_default_loader()?
        .add_args(vec!["hello", "Alice", "Bob"].into_iter().map(|arg| CString::new(arg).unwrap()))
        .add_handles(once(StartupHandle {
            handle: stdout_tx.into_handle(),
            info: HandleInfo::new(HandleType::FileDescriptor, STDOUT_FD),
        }));

    // Create the elf.
    let elf =
        create_packaged_elf(&*HELLO_BIN_PATH, processargs).await.context("failed to create Elf")?;

    // Start the program.
    let program = elf.start().await.context("failed to start")?;

    // Wait for the program to exit.
    let return_code = program.on_exit().await?;
    assert_eq!(return_code, 0);

    // Read from stdout.
    let mut stdout_rx = fasync::Socket::from_socket(stdout_rx)?;
    let mut buf = vec![];
    stdout_rx.read_to_end(&mut buf).await.context("failed to read stdout socket")?;

    assert_eq!(&*buf, b"Hello, Alice and Bob!\n");

    Ok(())
}

#[fuchsia::test]
async fn echo_server_use_protocol() -> Result<(), Error> {
    let (outgoing, outgoing_server_end) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>()?;

    let mut processargs = ProcessArgs::new();
    processargs.add_default_loader()?.add_handles(once(StartupHandle {
        handle: outgoing_server_end.into_handle(),
        info: HandleInfo::new(HandleType::DirectoryRequest, 0),
    }));

    // Create the elf.
    let elf = create_packaged_elf(&*ECHO_SERVER_BIN_PATH, processargs)
        .await
        .context("failed to create Elf")?;

    // Start the program.
    let program = elf.start().await.context("failed to start")?;

    // Use the Echo protocol served by the program.
    let echo = connect_to_protocol_at_dir_svc::<fexamples::EchoMarker>(&outgoing)
        .context("failed to connect to Echo")?;

    let message = "Hello, bedrock!";
    let response = echo.echo_string(message).await.context("failed to call EchoString")?;
    assert_eq!(response, message.to_string());

    // Stop the program.
    program.stop().await.context("failed to stop")?;

    // Wait for the program to exit.
    let _return_code = program.on_exit().await?;

    Ok(())
}

#[fuchsia::test]
async fn echo_server_stop_return_code() -> Result<(), Error> {
    // echo_server needs an outgoing directory server end, though we don't use client end.
    let (_outgoing, outgoing_server_end) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>()?;

    let mut processargs = ProcessArgs::new();
    processargs.add_default_loader()?.add_handles(once(StartupHandle {
        handle: outgoing_server_end.into_handle(),
        info: HandleInfo::new(HandleType::DirectoryRequest, 0),
    }));

    // Create the elf.
    let elf = create_packaged_elf(&*ECHO_SERVER_BIN_PATH, processargs)
        .await
        .context("failed to create Elf")?;

    // Start the program.
    let program = elf.start().await.context("failed to start")?;

    // Stop the program.
    program.stop().await.context("failed to stop")?;

    // Wait for the program to exit.
    let return_code = program.on_exit().await?;
    // The program was explicitly stopped, which results in the process being killed.
    assert_eq!(return_code, zx::sys::ZX_TASK_RETCODE_SYSCALL_KILL);

    Ok(())
}
