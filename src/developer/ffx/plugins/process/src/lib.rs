// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Library that obtains and prints information about all processes of a running fuchsia device.

mod fuchsia_map;
mod processes_data;
mod write_human_readable_output;

use anyhow::{Context, Result};

use ffx_config::global_env_context;
use ffx_process_args::{Args, ProcessCommand, Task};
use ffx_writer::{MachineWriter, ToolIO as _};
use fho::{FfxMain, FfxTool};
use fidl_fuchsia_buildinfo::{BuildInfo, ProviderProxy};
use fidl_fuchsia_process_explorer::{
    ProcessExplorerGetStackTraceRequest, ProcessExplorerKillTaskRequest, ProcessExplorerProxy,
    QueryProxy,
};
use fuchsia_map::json;
use futures::AsyncReadExt;
use processes_data::{processed, raw};
use std::collections::HashSet;
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use target_holders::moniker;
use write_human_readable_output::{
    pretty_print_invalid_koids, pretty_print_processes_data, pretty_print_processes_name_and_koid,
};
use zx_status::Status;
use zx_types::zx_koid_t;

const BARRIER: &str = "<ffx symbolizer>\n";

pub(crate) type Writer = MachineWriter<processed::ProcessesData>;

#[derive(FfxTool)]
pub struct ProcessTool {
    #[with(moniker("/core/process_explorer"))]
    query_proxy: QueryProxy,
    #[with(moniker("/core/process_explorer"))]
    explorer_proxy: ProcessExplorerProxy,
    #[with(moniker("/core/build-info"))]
    provider_proxy: ProviderProxy,
    #[command]
    cmd: ProcessCommand,

    task_writer: MachineWriter<raw::TasksData>,
}

fho::embedded_plugin!(ProcessTool);

#[async_trait::async_trait(?Send)]
impl FfxMain for ProcessTool {
    type Writer = Writer;

    async fn main(self, writer: Writer) -> fho::Result<()> {
        self.handle_cmd(writer).await.map_err(Into::into)
    }
}

impl ProcessTool {
    async fn handle_cmd(self, writer: Writer) -> Result<()> {
        // We handle the tree subcommand before the other subcommands because it uses a different
        // JSON blob. There's no sense in transferring the process data JSON blob for the tree
        // subcommand because we'll just throw away that data.
        if let Args::Tree(arg) = &self.cmd.arg {
            return tree_subcommand(self.query_proxy, self.task_writer, arg.threads).await;
        }

        let processes_data = get_processes_data(self.query_proxy).await?;
        let output = processed::ProcessesData::from(processes_data);

        let build_info = self.provider_proxy.get_build_info().await?;

        match self.cmd.arg {
            Args::List(arg) => list_subcommand(writer, output, arg.verbose),
            Args::Filter(arg) => filter_subcommand(writer, output, arg.process_koids),
            Args::GenerateFuchsiaMap(_) => {
                generate_fuchsia_map_subcommand(writer, build_info, output)
            }
            Args::Kill(arg) => kill_subcommand(writer, self.explorer_proxy, arg.task_to_kill).await,
            Args::StackTrace(arg) => {
                stack_trace_subcommand(writer, self.explorer_proxy, arg.task).await
            }
            Args::Tree(_) => unreachable!(), // Handled above.
        }
    }
}

/// Returns a buffer containing the data obtained via the QueryProxyInterface.
async fn get_raw_data(
    query_proxy: impl fidl_fuchsia_process_explorer::QueryProxyInterface,
) -> Result<Vec<u8>> {
    // Create a socket.
    let (rx, tx) = fidl::Socket::create_stream();

    // Send one end of the socket to the remote device.
    query_proxy.write_json_processes_data(tx)?;

    // Read all the bytes sent from the other end of the socket.
    let mut rx_async = fidl::AsyncSocket::from_socket(rx);
    let mut buffer = Vec::new();
    rx_async.read_to_end(&mut buffer).await?;

    Ok(buffer)
}

/// Returns data structured according to ProcessesData obtained via the `QueryProxyInterface`. Performs basic schema validation.
async fn get_processes_data(
    query_proxy: impl fidl_fuchsia_process_explorer::QueryProxyInterface,
) -> Result<raw::ProcessesData> {
    let buffer = get_raw_data(query_proxy).await?;
    Ok(serde_json::from_slice(&buffer)?)
}

/// Returns data that contains information related to the processes contained in `koids`, and a vector containing any invalid koids (if any).
fn filter_by_process_koids(
    processes_data: processed::ProcessesData,
    koids: Vec<zx_koid_t>,
) -> (processed::ProcessesData, Vec<zx_koid_t>) {
    let koids_set: HashSet<zx_koid_t> = HashSet::from_iter(koids);
    let mut filtered_processes = Vec::new();
    let mut filtered_processes_koids: HashSet<zx_koid_t> = HashSet::new();

    for process in processes_data.processes {
        if koids_set.contains(&process.koid) {
            filtered_processes_koids.insert(process.koid);
            filtered_processes.push(process);
        }
    }

    let mut invalid_koids: Vec<zx_koid_t> = Vec::new();
    for koid in koids_set {
        if !filtered_processes_koids.contains(&koid) {
            invalid_koids.push(koid);
        }
    }

    (
        processed::ProcessesData {
            processes_count: filtered_processes.len(),
            processes: filtered_processes,
        },
        invalid_koids,
    )
}

fn list_subcommand(
    mut w: Writer,
    processes_data: processed::ProcessesData,
    verbose: bool,
) -> Result<()> {
    if verbose {
        if w.is_machine() {
            w.machine(&processes_data)?;
            Ok(())
        } else {
            pretty_print_processes_data(w, processes_data)
        }
    } else {
        pretty_print_processes_name_and_koid(w, processes_data)
    }
}

async fn get_tasks_data(
    query_proxy: impl fidl_fuchsia_process_explorer::QueryProxyInterface,
) -> Result<raw::TasksData> {
    let (rx, tx) = fidl::Socket::create_stream();

    // Send one end of the socket to the remote device.
    query_proxy.write_json_task_hierarchy_data(tx)?;

    // Read all the bytes sent from the other end of the socket.
    let mut rx_async = fidl::AsyncSocket::from_socket(rx);
    let mut buffer = Vec::new();
    rx_async.read_to_end(&mut buffer).await?;
    Ok(serde_json::from_slice(&buffer)?)
}

fn get_symbol_for_task_type(task_type: &str) -> &'static str {
    match task_type {
        "job" => "j",
        "process" => "p",
        "thread" => "t",
        _ => "?",
    }
}

async fn tree_subcommand(
    query_proxy: impl fidl_fuchsia_process_explorer::QueryProxyInterface,
    mut w: MachineWriter<raw::TasksData>,
    threads: bool,
) -> Result<()> {
    let tasks_data = get_tasks_data(query_proxy).await?;

    if w.is_machine() {
        w.machine(&tasks_data)?;
    } else {
        for task in tasks_data.tasks {
            if !threads && task.task_type == "thread" {
                continue;
            }
            for _ in 0..task.depth {
                write!(w, "  ")?;
            }
            let symbol = get_symbol_for_task_type(&task.task_type);
            writeln!(w, "{}: {:<6} {}", symbol, task.koid, task.name)?;
        }
    }

    Ok(())
}

fn filter_subcommand(
    w: Writer,
    processes_data: processed::ProcessesData,
    koids: Vec<zx_koid_t>,
) -> Result<()> {
    let (filtered_output, invalid_koids) = filter_by_process_koids(processes_data, koids);
    if invalid_koids.len() > 0 {
        pretty_print_invalid_koids(w, invalid_koids)
    } else {
        pretty_print_processes_data(w, filtered_output)
    }
}

fn generate_fuchsia_map_subcommand(
    mut w: Writer,
    build_info: BuildInfo,
    processes_data: processed::ProcessesData,
) -> Result<()> {
    let json = json::make_fuchsia_map_json(processes_data, build_info);
    serde_json::to_writer(&mut w, &json)?;
    Ok(())
}

async fn kill_subcommand(
    mut w: Writer,
    explorer_proxy: ProcessExplorerProxy,
    task: Task,
) -> Result<()> {
    let arg = match task {
        Task::Koid(koid) => ProcessExplorerKillTaskRequest::Koid(koid),
        Task::ProcessName(name) => ProcessExplorerKillTaskRequest::ProcessName(name),
    };
    match explorer_proxy.kill_task(&arg).await?.map_err(Status::from_raw) {
        Ok(koid) => {
            writeln!(w, "Successfully killed task: {}", koid)?;
            Ok(())
        }
        Err(Status::NOT_FOUND) => {
            writeln!(w, "Failed to find process")?;
            Ok(())
        }
        Err(e) => {
            writeln!(w, "Failed to kill process with error {:?}", e)?;
            Err(e.into())
        }
    }
}

async fn stack_trace_subcommand(
    mut w: Writer,
    explorer_proxy: ProcessExplorerProxy,
    task: Task,
) -> Result<()> {
    let arg = match task {
        Task::Koid(koid) => ProcessExplorerGetStackTraceRequest::Koid(koid),
        Task::ProcessName(name) => ProcessExplorerGetStackTraceRequest::ProcessName(name),
    };
    match explorer_proxy.get_stack_trace(&arg).await?.map_err(Status::from_raw) {
        Ok(stack_trace) => {
            write_symbolized_stack_traces(w, stack_trace).await?;
            Ok(())
        }
        Err(Status::NOT_FOUND) => {
            writeln!(w, "Failed to find process")?;
            Ok(())
        }
        Err(e) => {
            writeln!(w, "Failed to get stack trace for process with error {:?}", e)?;
            Err(e.into())
        }
    }
}

async fn write_symbolized_stack_traces(mut w: Writer, stack_trace: String) -> Result<()> {
    let sdk = global_env_context().context("Loading global environment context")?.get_sdk()?;
    if let Err(e) = symbol_index::ensure_symbol_index_registered(&sdk) {
        tracing::warn!("ensure_symbol_index_registered failed, error was: {:#?}", e);
    }

    let path = ffx_config::get_host_tool(&sdk, "symbolizer")
        .await
        .context("getting symbolizer binary path")?;
    let mut cmd = Command::new(path)
        .args(vec![
            "--symbol-server",
            "gs://fuchsia-artifacts/debug",
            "--symbol-server",
            "gs://fuchsia-artifacts-internal/debug",
            "--symbol-server",
            "gs://fuchsia-artifacts-release/debug",
        ])
        .stdout(Stdio::piped())
        .stdin(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .context("Spawning symbolizer")?;
    let mut stdin = cmd.stdin.take().context("missing stdin")?;
    let mut stdout = BufReader::new(cmd.stdout.take().context("missing stdout")?);
    stdin.write_all(stack_trace.as_bytes())?;
    stdin.write_all(BARRIER.as_bytes())?;

    loop {
        let mut stdout_buf = String::default();
        match stdout.read_line(&mut stdout_buf) {
            Ok(_) => {}
            Err(e) => {
                tracing::warn!("reading from symbolizer stdout failed: {}", e);
                continue;
            }
        }

        if stdout_buf.as_str() == BARRIER {
            break;
        }
        write!(w, "{}", stdout_buf)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::AsyncWriteExt;
    use target_holders::fake_proxy;

    lazy_static::lazy_static! {
    static ref EXPECTED_PROCESSES_DATA: raw::ProcessesData = raw::ProcessesData{
        processes: vec![
            raw::Process {
                koid: 1,
                name: "process1".to_string(),
                objects: vec![
                    raw::KernelObject {
                        object_type: 4,
                        koid: 78,
                        related_koid: 79,
                        peer_owner_koid: 2,
                    },
                    raw::KernelObject {
                        object_type: 4,
                        koid: 52,
                        related_koid: 53,
                        peer_owner_koid: 0,
                    },
                    raw::KernelObject {
                        object_type: 17,
                        koid: 36,
                        related_koid: 0,
                        peer_owner_koid: 0,
                    },
                ],
            },
            raw::Process {
                koid: 2,
                name: "process2".to_string(),
                objects: vec![
                    raw::KernelObject {
                        object_type: 19,
                        koid: 28,
                        related_koid: 0,
                        peer_owner_koid: 0,
                    },
                    raw::KernelObject {
                        object_type: 14,
                        koid: 95,
                        related_koid: 96,
                        peer_owner_koid: 0,
                    },
                    raw::KernelObject {
                        object_type: 4,
                        koid: 79,
                        related_koid: 78,
                        peer_owner_koid: 1,
                    },
                ],
            },
        ],
    };

    static ref DATA_WRITTEN_BY_PROCESS_EXPLORER: Vec<u8> = serde_json::to_vec(&*EXPECTED_PROCESSES_DATA).unwrap();

    }

    use fidl_fuchsia_process_explorer::QueryRequest;

    /// Returns a fake query service that writes `EXPECTED_PROCESSES_DATA` serialized to JSON to the socket when `WriteJsonProcessesData` is called.
    fn setup_fake_query_svc() -> QueryProxy {
        fake_proxy(|request| match request {
            QueryRequest::WriteJsonProcessesData { socket, .. } => {
                let mut s = fidl::AsyncSocket::from_socket(socket);
                fuchsia_async::Task::local(async move {
                    s.write_all(&serde_json::to_vec(&*EXPECTED_PROCESSES_DATA).unwrap())
                        .await
                        .unwrap();
                })
                .detach();
            }
            QueryRequest::WriteJsonTaskHierarchyData { .. } => {
                unreachable!();
            }
            _ => unreachable!(),
        })
    }

    /// Tests that `get_raw_data` properly reads data from the process explorer query service.
    #[fuchsia_async::run_singlethreaded(test)]
    async fn get_raw_data_test() {
        let query_proxy = setup_fake_query_svc();
        let raw_data = get_raw_data(query_proxy).await.expect("failed to get raw data");
        assert_eq!(raw_data, *DATA_WRITTEN_BY_PROCESS_EXPLORER);
    }

    /// Tests that `get_processes_data` properly reads and parses data from the query service.
    #[fuchsia_async::run_singlethreaded(test)]
    async fn get_processes_data_test() {
        let query_proxy = setup_fake_query_svc();
        let processes_data =
            get_processes_data(query_proxy).await.expect("failed to get processes_data");
        assert_eq!(processes_data, *EXPECTED_PROCESSES_DATA);
    }
}
