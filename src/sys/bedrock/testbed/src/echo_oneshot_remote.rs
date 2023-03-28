// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Integration test that launches a process that serves the Echo protocol and verifies that
//! the the test can call methods on it.
//!
//! The test creates a `Dict` capability and puts the server end of the Echo protocol inside,
//! as a `Handle` capability.
//!
//! The process receives the `Dict` capability, as a handle to the `fuchsia.component.bedrock.Dict`
//! protocol, as a startup handle via processargs. It takes the server end out of the `Dict`
//! and starts serving the `Echo` protocol.
//!
//! The test calls a method on the client end to verify that the protocol is being served.
//!
//! The connection is one-shot, so the client cannot reconnect.

use {
    crate::elf::create_packaged_elf,
    anyhow::{Context, Error},
    cap::Remote,
    exec::{Lifecycle, Start, Stop},
    fidl::endpoints::create_proxy,
    fidl_fuchsia_examples as fexamples, fuchsia_async as fasync,
    fuchsia_runtime::{HandleInfo, HandleType},
    fuchsia_zircon as zx,
    lazy_static::lazy_static,
    process_builder::StartupHandle,
    processargs::ProcessArgs,
    std::iter::once,
    std::path::PathBuf,
};

lazy_static! {
    /// Path to the `bedrock_testbed_echo_server_cap` executable, relative to the test's namespace.
    static ref ECHO_SERVER_CAP_BIN_PATH: PathBuf = PathBuf::from("/pkg/bin/bedrock_testbed_echo_server_cap");
}

/// The name of the echo capability in dicts.
const ECHO_CAP_NAME: &str = "echo";

/// Creates a program that runs the `bedrock_cap_echo_server` executable.
async fn create_echo_server_cap(processargs: ProcessArgs) -> Result<exec::elf::Elf, Error> {
    create_packaged_elf(ECHO_SERVER_CAP_BIN_PATH.as_path(), processargs).await
}

#[fuchsia::test]
async fn test_echo_oneshot_remote() -> Result<(), Error> {
    // Create the "incoming" interface for the echo_server program.
    // This is a Dict capability that contains the server end of the Echo protocol.
    let (echo_proxy, echo_server_end) = create_proxy::<fexamples::EchoMarker>().unwrap();

    let echo_server_cap = cap::Handle::from(zx::Handle::from(echo_server_end));

    let server_dict = Box::new(cap::Dict::new());
    server_dict.entries.lock().await.insert(ECHO_CAP_NAME.into(), Box::new(echo_server_cap));

    // Convert the `Dict` to a Zircon handle that we can pass to the `echo_server_cap` process.
    let (server_dict_handle, server_dict_fut) = server_dict.to_zx_handle();

    // Serve the `Dict` protocol in the background.
    fasync::Task::spawn(server_dict_fut.unwrap()).detach();

    let mut processargs = ProcessArgs::new();
    processargs.add_default_loader()?.add_handles(once(StartupHandle {
        handle: server_dict_handle,
        info: HandleInfo::new(HandleType::User0, 0),
    }));

    // Create the elf.
    let elf = create_echo_server_cap(processargs).await.context("failed to create Elf")?;

    // Start the program.
    let mut program = elf.start().await.context("failed to start")?;

    // Use the Echo protocol served by the program.
    let message = "Hello, bedrock!";
    let response = echo_proxy.echo_string(message).await.context("failed to call EchoString")?;
    assert_eq!(response, message.to_string());

    // Stop the program.
    program.stop().await.context("failed to stop")?;

    // Wait for the program to exit.
    let _return_code = program.on_exit().await?;

    Ok(())
}
