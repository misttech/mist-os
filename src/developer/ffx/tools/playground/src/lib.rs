// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{anyhow, Result};
use argh::{ArgsInfo, FromArgs};
use async_fs as afs;
use async_trait::async_trait;
use errors::ffx_bail;
use fho::{FfxMain, FfxTool, SimpleWriter};
use fidl::endpoints::Proxy;
use fidl_codec::library as lib;
use fidl_fuchsia_developer_remotecontrol as rc;
use fuchsia_async as fasync;
use futures::channel::oneshot::channel as oneshot;
use futures::future::{select, Either, FutureExt};
use futures::io::AllowStdIo;
use futures::AsyncReadExt;
use playground::interpreter::Interpreter;
use playground::value::Value;
use std::fs::File;
use std::io::{self, BufRead as _, BufReader};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use vfs::directory::helper::DirectlyMutable;

mod cf_fs;
mod presentation;
mod repl;
mod strict_mutex;
mod toolbox_fs;

use presentation::display_result;
use toolbox_fs::toolbox_directory;

#[derive(ArgsInfo, FromArgs, Debug, PartialEq)]
#[argh(subcommand, name = "playground", description = "Directly invoke FIDL services")]
pub struct PlaygroundCommand {
    /// A command to run. If passed, the playground will run that one command,
    /// display the output, and return immediately.
    #[argh(option, short = 'c')]
    pub command: Option<String>,

    /// A file to run.
    #[argh(positional)]
    pub file: Option<String>,
}

#[derive(FfxTool)]
pub struct PlaygroundTool {
    #[command]
    cmd: PlaygroundCommand,
    rcs_proxy: rc::RemoteControlProxy,
}

#[async_trait(?Send)]
impl FfxMain for PlaygroundTool {
    type Writer = SimpleWriter;
    async fn main(self, _writer: Self::Writer) -> fho::Result<()> {
        exec_playground(self.rcs_proxy, self.cmd).await?;
        Ok(())
    }
}

pub async fn exec_playground(
    remote_proxy: rc::RemoteControlProxy,
    command: PlaygroundCommand,
) -> Result<()> {
    let mut lib_namespace = lib::Namespace::new();

    let Ok(fuchsia_dir) = std::env::var("FUCHSIA_DIR") else {
        ffx_bail!("FUCHSIA_DIR environment variable is not set")
    };
    let mut fuchsia_dir = PathBuf::from(fuchsia_dir);
    fuchsia_dir.push(".fx-build-dir");

    let root = match std::fs::read_to_string(&fuchsia_dir) {
        Ok(root) => root,
        Err(e) => ffx_bail!("Could not read {}: {e}", fuchsia_dir.display()),
    };

    let root = PathBuf::from(root.trim());
    let mut path = root.clone();
    path.push("all_fidl_json.txt");

    let file = match File::open(&path) {
        Ok(file) => file,
        Err(e) => ffx_bail!("Could not open {}: {e}", path.display()),
    };

    for line in BufReader::new(file).lines() {
        let line = match line {
            Ok(line) => line,
            Err(e) => ffx_bail!("Error reading {}, {e}", path.display()),
        };
        let line = line.trim();
        let path_piece = PathBuf::from(line);
        let mut path = root.clone();
        path.push(path_piece);
        let json_file = match std::fs::read_to_string(&path) {
            Ok(json_file) => json_file,
            Err(e) => {
                eprintln!("Skipping {line}: read error: {e}");
                continue;
            }
        };
        if let Err(x) = lib_namespace.load(&json_file) {
            eprintln!("Skipping {line}: {x}");
        }
    }

    let remote_proxy = Arc::new(remote_proxy);
    let query = rcs::root_realm_query(&remote_proxy, std::time::Duration::from_secs(5)).await?;
    let toolbox = toolbox_directory(&*remote_proxy, &query).await?;
    let cf_root = cf_fs::CFDirectory::new_root(query);
    let fs_root_simple = vfs::directory::mutable::simple();
    let root_dir_client = vfs::directory::spawn_directory(Arc::clone(&fs_root_simple));
    fs_root_simple.add_entry("toolbox", toolbox)?;
    fs_root_simple.add_entry("cf", cf_root)?;
    let Ok(root_dir_client) = root_dir_client.into_channel() else {
        ffx_bail!("Could not turn proxy back into channel");
    };
    let root_dir_client = root_dir_client.into_zx_channel();
    let (interpreter, runner) = Interpreter::new(lib_namespace, root_dir_client.into()).await;
    fasync::Task::spawn(runner).detach();

    let (quit_sender, mut quit_receiver) = oneshot();
    let quit_sender = Mutex::new(Some(quit_sender));
    interpreter
        .add_command("quit", move |_, _| {
            if let Some(quit_sender) = quit_sender.lock().unwrap().take() {
                quit_sender.send(()).unwrap();
            }

            async move { Ok(Value::Null) }
        })
        .await
        .expect("Failed to install quit command");

    let mut line = String::new();
    if let Some(cmd) = command.command {
        if command.file.is_some() {
            Err(anyhow!("Cannot specify a command and a file at the same time"))
        } else {
            display_result(
                &mut AllowStdIo::new(&io::stdout()),
                interpreter.run(cmd.as_str()).await,
                &interpreter,
            )
            .await?;
            Ok(())
        }
    } else if let Some(file) = command.file {
        afs::File::open(file).await?.read_to_string(&mut line).await?;
        display_result(
            &mut AllowStdIo::new(&io::stdout()),
            interpreter.run(line.as_str()).await,
            &interpreter,
        )
        .await?;
        Ok(())
    } else {
        let node_name = remote_proxy
            .identify_host()
            .await?
            .map_err(|e| anyhow!("Could not identify host: {:?}", e))?
            .nodename
            .unwrap_or_else(|| "<unknown>".to_owned());
        let repl = Arc::new(repl::Repl::new()?);
        let interpreter = Arc::new(interpreter);

        let completer = {
            let interpreter = Arc::clone(&interpreter);
            move |cmd, pos| {
                let interpreter = Arc::clone(&interpreter);
                async move { interpreter.complete(cmd, pos).await }
            }
        };

        while let Either::Left((line, _)) = select(
            repl.get_cmd(&format!("\x1b[1;92m{} \x1b[1;97m➤\x1b[0m", node_name), &completer)
                .boxed(),
            &mut quit_receiver,
        )
        .await
        {
            let repl = Arc::clone(&repl);
            let interpreter = Arc::clone(&interpreter);
            if let Some(line) = line? {
                fasync::Task::local(async move {
                    display_result(
                        &mut repl::ReplWriter::new(&*repl),
                        interpreter.run(line.as_str()).await,
                        &interpreter,
                    )
                    .await
                    .unwrap();
                })
                .detach();
            } else {
                break;
            }
        }
        Ok(())
    }
}
