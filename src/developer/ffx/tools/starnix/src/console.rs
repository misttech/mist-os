// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{anyhow, bail, Result};
use argh::FromArgs;
use blocking::Unblock;
use fho::SimpleWriter;
use fidl_fuchsia_developer_remotecontrol as rc;
use fidl_fuchsia_starnix_container as fstarcontainer;
use futures::{future::FutureExt, pin_mut, select, AsyncReadExt};
use std::fs::File;
use std::os::unix::io::FromRawFd;
use termion::raw::IntoRawMode;

use crate::common::*;

async fn serve_console_connection(local_console: fidl::Socket) -> Result<()> {
    let console = fidl::AsyncSocket::from_socket(local_console)?;
    let (console_source, mut console_sink) = console.split();

    let stdin = unsafe { File::from_raw_fd(0) };
    let stdout = unsafe { File::from_raw_fd(1) };

    let stdin = Unblock::new(stdin);
    let mut stdout = Unblock::new(stdout);

    let copy_futures = futures::future::try_join(
        futures::io::copy(stdin, &mut console_sink),
        futures::io::copy(console_source, &mut stdout),
    );

    copy_futures.await?;

    Ok(())
}

fn get_environ() -> Vec<String> {
    let mut result = vec![];
    for key in vec!["TERM"] {
        if let Ok(value) = std::env::var(key) {
            result.push(format!("{key}={value}").to_string());
        }
    }
    result
}

async fn run_console(
    controller: &fstarcontainer::ControllerProxy,
    argv: Vec<String>,
) -> Result<u8> {
    let (local_console, remote_console) = fidl::Socket::create_stream();
    let binary_path = argv[0].clone();
    let (cols, rows) = termion::terminal_size()?;
    let (x_pixels, y_pixels) = (0, 0); // TODO: Need a newer termion for `terminal_size_pixels()`.
    let exit_future = controller
        .spawn_console(fstarcontainer::ControllerSpawnConsoleRequest {
            console: Some(remote_console),
            binary_path: Some(binary_path),
            argv: Some(argv),
            environ: Some(get_environ()),
            window_size: Some(fstarcontainer::ConsoleWindowSize { rows, cols, x_pixels, y_pixels }),
            ..Default::default()
        })
        .fuse();

    let copy_future = serve_console_connection(local_console).fuse();

    pin_mut!(exit_future, copy_future);

    let raw_mode = std::io::stdout().into_raw_mode().unwrap();
    loop {
        select! {
            exit_code = exit_future => {
                std::mem::drop(raw_mode);
                return exit_code?.map_err(|e| {
                    let status = fidl::Status::from_raw(e);
                    anyhow!("Failed to spawn console: {}", status)
                });
            },
            copy = copy_future => {
                copy?;
                continue;
            },
        };
    }
}

#[derive(FromArgs, Debug, PartialEq)]
#[argh(
    subcommand,
    name = "console",
    example = "ffx starnix console [command [argument ...]]",
    description = "Attach a console to a starnix container"
)]
pub struct StarnixConsoleCommand {
    /// the moniker of the container in which to create the console
    /// (defaults to looking for a container in the current session)
    #[argh(option, short = 'm')]
    pub moniker: Option<String>,

    /// the command and and arguments to run in the console
    #[argh(positional, greedy)]
    argv: Vec<String>,
}

pub async fn starnix_console(
    command: &StarnixConsoleCommand,
    rcs_proxy: &rc::RemoteControlProxy,
    _writer: SimpleWriter,
) -> Result<()> {
    if !termion::is_tty(&std::io::stdout()) {
        bail!("ffx starnix console must be run in a tty.");
    }

    let controller = connect_to_contoller(&rcs_proxy, command.moniker.clone()).await?;
    let argv =
        if command.argv.is_empty() { vec!["/bin/sh".to_string()] } else { command.argv.clone() };
    let exit_code = run_console(&controller, argv).await?;
    println!("(exit code: {})", exit_code);
    Ok(())
}
