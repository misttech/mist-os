// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Result;
use errors::ffx_error;
use ffx_debug_connect::DebugAgentSocket;
use fho::{deferred, moniker, Deferred, FfxContext, FfxMain, FfxTool, SimpleWriter};
use fidl_fuchsia_debugger::DebugAgentProxy;
use fuchsia_async::unblock;
use sdk::SdkVersion;
use std::process::Command;

struct ProcessArguments {
    arguments: Vec<String>,
}

impl ProcessArguments {
    fn new() -> Self {
        Self { arguments: Vec::new() }
    }

    fn add_flag(&mut self, name: &str, value: bool) {
        if value {
            self.arguments.push(name.to_string());
        }
    }

    fn add_value(&mut self, name: &str, value: &str) {
        self.arguments.push(name.to_string());
        self.arguments.push(value.to_string());
    }

    fn add_option(&mut self, name: &str, value: &Option<String>) {
        if let Some(value) = &value {
            self.arguments.push(name.to_string());
            self.arguments.push(value.to_string());
        }
    }

    fn add_values(&mut self, name: &str, value: &Vec<String>) {
        for value in value.iter() {
            self.arguments.push(name.to_string());
            self.arguments.push(value.to_string());
        }
    }
}

#[derive(FfxTool)]
pub struct FidlTool {
    #[command]
    cmd: ffx_debug_fidlcat_args::FidlcatCommand,
    sdk: ffx_config::Sdk,
    #[with(deferred(moniker("/core/debug_agent")))]
    debugger_proxy: Deferred<DebugAgentProxy>,
}

fho::embedded_plugin!(FidlTool);

#[async_trait::async_trait(?Send)]
impl FfxMain for FidlTool {
    type Writer = SimpleWriter;

    async fn main(self, _writer: SimpleWriter) -> fho::Result<()> {
        let Self { cmd, sdk, debugger_proxy } = self;

        if let Err(e) = symbol_index::ensure_symbol_index_registered(&sdk).await {
            tracing::warn!("ensure_symbol_index_registered failed, error was: {:#?}", e);
        }

        let fidlcat_path = sdk.get_host_tool("fidlcat")?;
        let mut arguments = ProcessArguments::new();
        let mut debug_agent_socket: Option<DebugAgentSocket> = None;

        if cmd.from.is_some() && cmd.from.as_ref().unwrap() != "device" {
            arguments.add_value("--from", &cmd.from.unwrap());
        } else {
            let debugger_proxy = debugger_proxy.await?;
            debug_agent_socket = Some(DebugAgentSocket::create(debugger_proxy)?);
        }

        arguments.add_option("--to", &cmd.to);
        arguments.add_option("--format", &cmd.format);
        arguments.add_values("--with", &cmd.with);
        arguments.add_flag("--with-process-info", cmd.with_process_info);
        arguments.add_option("--stack", &cmd.stack);
        arguments.add_values("--syscalls", &cmd.syscalls);
        arguments.add_values("--exclude-syscalls", &cmd.exclude_syscalls);
        arguments.add_values("--messages", &cmd.messages);
        arguments.add_values("--exclude-messages", &cmd.exclude_messages);
        arguments.add_values("--trigger", &cmd.trigger);
        arguments.add_values("--thread", &cmd.thread);
        arguments.add_values("--fidl-ir-path", &cmd.fidl_ir_path);
        arguments.add_flag("--dump-messages", cmd.dump_messages);

        if debug_agent_socket.is_some() {
            // Processes to monitor.
            arguments.add_values("--remote-pid", &cmd.remote_pid);
            arguments.add_values("--remote-name", &cmd.remote_name);
            arguments.add_values("--extra-name", &cmd.extra_name);

            // Components to monitor.
            arguments.add_values("--remote-component", &cmd.remote_component);
            arguments.add_values("--extra-component", &cmd.extra_component);
        }

        if sdk.get_version() == &SdkVersion::InTree {
            // When ffx is used in tree, uses the JSON IR files listed in all_fidl_json.txt.
            let ir_file = format!("@{}/all_fidl_json.txt", sdk.get_path_prefix().to_str().unwrap());
            arguments.add_value("--fidl-ir-path", &ir_file);
        }

        if let Some(ref socket) = debug_agent_socket {
            // Connect to the debug_agent on the device.

            // It's safe to unwrap because the path is created by us.
            let unix_socket_path = socket.unix_socket_path().to_str().unwrap();
            // Connect to the Unix socket.
            arguments.add_value("--unix-connect", unix_socket_path);
        }

        arguments.arguments.extend(cmd.extra_args);

        // Start fidlcat locally.
        let mut fidlcat = Command::new(&fidlcat_path)
            .args(&arguments.arguments)
            .spawn()
            .bug_context("Running fidlcat binary")?;

        // Spawn the task that doing the forwarding in the background.
        let _task = fuchsia_async::Task::local(async move {
            if let Some(socket) = debug_agent_socket {
                let _ = socket.forward_one_connection().await.map_err(|e| {
                    eprintln!("Connection to debug_agent broken: {}", e);
                });
            };
        });

        let status = unblock(move || fidlcat.wait())
            .await
            .bug_context("Error waiting for fidlcat to exit")?;

        if let Some(exit_code) = status.code() {
            if exit_code == 0 {
                Ok(())
            } else {
                Err(ffx_error!("fidlcat exited with code {}", exit_code).into())
            }
        } else {
            Err(ffx_error!("fidlcat terminated by signal").into())
        }
    }
}
