// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{anyhow, Context, Result};
use errors::ffx_bail;
use ffx_config::keys::TARGET_DEFAULT_KEY;
use ffx_trace_args::{TraceCommand, TraceSubCommand};
use fho::{daemon_protocol, deferred, selector, FfxMain, FfxTool, MachineWriter, ToolIO};
use fidl_fuchsia_developer_ffx::{self as ffx, RecordingError, TracingProxy};
use fidl_fuchsia_tracing::KnownCategory;
use fidl_fuchsia_tracing_controller::{ControllerProxy, ProviderInfo, ProviderSpec, TraceConfig};
use futures::future::{BoxFuture, FutureExt};
use lazy_static::lazy_static;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeSet, HashMap},
    future::Future,
    io::{stdin, Stdin},
    path::{Component, PathBuf},
    time::Duration,
};
use term_grid::Grid;
use termion::terminal_size;

// This is to make the schema make sense as this plugin can output one of these based on the
// subcommand. An alternative is to break this one plugin into multiple plugins each with their own
// output type. That is probably preferred but either way works.
// TODO(121214): Fix incorrect- or invalid-type writer declarations
#[derive(Debug, Deserialize, Serialize)]
#[serde(untagged)]
pub enum TraceOutput {
    ListCategories(Vec<TraceKnownCategory>),
    ListProviders(Vec<TraceProviderInfo>),
}

// These fields are arranged this way because deriving Ord uses field declaration order.
#[derive(Debug, Deserialize, Serialize, PartialOrd, Ord, PartialEq, Eq)]
pub struct TraceKnownCategory {
    /// The name of the category.
    name: String,
    /// A short, possibly empty description of this category.
    description: String,
}

impl From<KnownCategory> for TraceKnownCategory {
    fn from(category: KnownCategory) -> Self {
        Self { name: category.name, description: category.description }
    }
}

impl From<&'static str> for TraceKnownCategory {
    fn from(name: &'static str) -> Self {
        Self { name: name.to_string(), description: String::new() }
    }
}

// These fields are arranged this way because deriving Ord uses field declaration order.
#[derive(Debug, Deserialize, Serialize, PartialOrd, Ord, PartialEq, Eq)]
pub struct TraceProviderInfo {
    name: String,
    id: Option<u32>,
    pid: Option<u64>,
}

impl From<ProviderInfo> for TraceProviderInfo {
    fn from(info: ProviderInfo) -> Self {
        Self {
            id: info.id,
            pid: info.pid,
            name: info.name.as_ref().cloned().unwrap_or("unknown".to_string()),
        }
    }
}

fn handle_fidl_error<T>(res: Result<T, fidl::Error>) -> Result<T> {
    res.map_err(|e| anyhow!(handle_peer_closed(e)))
}

fn handle_peer_closed(err: fidl::Error) -> errors::FfxError {
    match err {
        fidl::Error::ClientChannelClosed { status, protocol_name, reason } => {
            errors::ffx_error!("An attempt to access {} resulted in a bad status: {} reason: {}.
This can happen if tracing is not supported on the product configuration you are running or if it is missing from the base image.", protocol_name, status, reason.as_ref().map(String::as_str).unwrap_or("not given"))
        }
        _ => {
            errors::ffx_error!("Accessing the tracing controller failed: {:#?}", err)
        }
    }
}

// LineWaiter abstracts waiting for the user to press enter.  It is needed
// to unit test interactive mode.
trait LineWaiter<'a> {
    type LineWaiterFut: 'a + Future<Output = ()>;
    fn wait(&'a mut self) -> Self::LineWaiterFut;
}

impl<'a> LineWaiter<'a> for Stdin {
    type LineWaiterFut = BoxFuture<'a, ()>;

    fn wait(&'a mut self) -> Self::LineWaiterFut {
        if cfg!(not(test)) {
            use std::io::BufRead;
            blocking::unblock(|| {
                let mut line = String::new();
                let stdin = stdin();
                let mut locked = stdin.lock();
                // Ignoring error, though maybe Ack would want to bubble up errors instead?
                let _ = locked.read_line(&mut line);
            })
            .boxed()
        } else {
            async move {}.boxed()
        }
    }
}

fn validate_category_name(category_name: &str) -> Result<()> {
    lazy_static! {
        static ref VALID_CATEGORY_REGEX: Regex = Regex::new(r#"^[^\*",\s]*\*?$"#).unwrap();
    }
    if !VALID_CATEGORY_REGEX.is_match(category_name) {
        return Err(anyhow!("Error: category \"{}\" is invalid", category_name));
    }
    Ok(())
}

async fn get_category_group_names() -> Result<Vec<String>> {
    let all_groups = ffx_config::query(ffx_config::build().select(ffx_config::SelectMode::All))
        .name(Some("trace.category_groups"))
        .get::<Value>()
        .await
        .context("could not query `trace.category_groups` in config.")?;
    let mut group_names: Vec<String> = all_groups
        .as_array()
        .unwrap()
        .into_iter()
        .flat_map(|subgroups| subgroups.as_object().unwrap())
        .map(|(group_name, _)| group_name)
        .cloned()
        .collect();
    group_names.sort_unstable();
    Ok(group_names)
}

async fn get_category_group(category_group_name: &str) -> Result<Vec<String>> {
    let category_group = ffx_config::get::<Vec<String>, _>(&format!(
        "trace.category_groups.{}",
        category_group_name
    ))
    .await
    .context(format!(
        "Error: no category group found for {0}, you can add this category locally by calling \
              `ffx config set trace.category_groups.{0} '[\"list\", \"of\", \"categories\"]'`\
              or globally by adding it to data/config.json in the ffx trace plugin.",
        category_group_name
    ))?;
    for category in &category_group {
        validate_category_name(&category).context(format!(
            "Error: #{} contains an invalid category \"{}\"",
            category_group_name, category
        ))?;
    }
    Ok(category_group)
}

async fn expand_categories(categories: Vec<String>) -> Result<Vec<String>> {
    let mut expanded_categories = BTreeSet::new();
    for category in categories {
        match category.strip_prefix('#') {
            Some(category_group_name) => {
                let category_group = get_category_group(category_group_name).await?;
                expanded_categories.extend(category_group);
            }
            None => {
                validate_category_name(&category)?;
                expanded_categories.insert(category);
            }
        }
    }
    Ok(expanded_categories.into_iter().collect())
}

fn map_categories_to_providers(categories: &Vec<String>) -> TraceConfig {
    let mut provider_specific_categories = HashMap::<&str, Vec<String>>::new();
    let mut umbrella_categories = vec![];
    for category in categories {
        if let Some((provider_name, category)) = category.split_once("/") {
            provider_specific_categories
                .entry(provider_name)
                .and_modify(|categories| categories.push(category.to_string()))
                .or_insert(vec![category.to_string()]);
        } else {
            umbrella_categories.push(category.clone());
        }
    }

    let mut trace_config = TraceConfig::default();
    if !categories.is_empty() {
        trace_config.categories = Some(umbrella_categories.clone());
    }
    if !provider_specific_categories.is_empty() {
        trace_config.provider_specs = Some(
            provider_specific_categories
                .into_iter()
                .map(|(name, categories)| ProviderSpec {
                    name: Some(name.to_string()),
                    categories: Some(categories),
                    ..Default::default()
                })
                .collect(),
        );
    }
    trace_config
}

// Print as a grid that fills the width of the terminal. Falls back to one value
// per line if any value is wider than the terminal.
fn print_grid(writer: &mut Writer, values: Vec<String>) -> Result<()> {
    let mut grid = Grid::new(term_grid::GridOptions {
        direction: term_grid::Direction::TopToBottom,
        filling: term_grid::Filling::Spaces(2),
    });
    for value in &values {
        grid.add(term_grid::Cell::from(value.as_str()));
    }

    let terminal_width = terminal_size().unwrap_or((80, 80)).0;
    let formatted_values = match grid.fit_into_width(terminal_width.into()) {
        Some(grid_display) => grid_display.to_string(),
        None => values.join("\n"),
    };
    writer.line(formatted_values)?;
    Ok(())
}

type Writer = MachineWriter<TraceOutput>;
#[derive(FfxTool)]
pub struct TraceTool {
    #[with(daemon_protocol())]
    proxy: TracingProxy,
    #[with(deferred(selector(
        "core/trace_manager:expose:fuchsia.tracing.controller.Controller"
    )))]
    controller: fho::Deferred<ControllerProxy>,
    #[command]
    cmd: TraceCommand,
}

fho::embedded_plugin!(TraceTool);

#[async_trait::async_trait(?Send)]
impl FfxMain for TraceTool {
    type Writer = Writer;

    async fn main(self, writer: Self::Writer) -> fho::Result<()> {
        trace(self.proxy, self.controller, writer, self.cmd).await.map_err(Into::into)
    }
}

pub async fn trace(
    proxy: TracingProxy,
    controller: fho::Deferred<ControllerProxy>,
    mut writer: Writer,
    cmd: TraceCommand,
) -> Result<()> {
    let default_target: Option<String> = ffx_config::get(TARGET_DEFAULT_KEY).await?;
    match cmd.sub_cmd {
        TraceSubCommand::ListCategories(_) => {
            let controller = controller.await?;
            let mut categories = handle_fidl_error(controller.get_known_categories().await)?;
            categories.sort_unstable();
            if writer.is_machine() {
                let categories = categories
                    .into_iter()
                    .map(TraceKnownCategory::from)
                    .collect::<Vec<TraceKnownCategory>>();

                writer.machine(&TraceOutput::ListCategories(categories))?;
            } else {
                print_grid(
                    &mut writer,
                    categories.into_iter().map(|category| category.name).collect(),
                )?;
            }
        }
        TraceSubCommand::ListProviders(_) => {
            let controller = controller.await?;
            let mut providers = handle_fidl_error(controller.get_providers().await)?
                .into_iter()
                .map(TraceProviderInfo::from)
                .collect::<Vec<TraceProviderInfo>>();
            providers.sort_unstable();
            if writer.is_machine() {
                writer.machine(&TraceOutput::ListProviders(providers))?;
            } else {
                writer.line("Trace providers:")?;
                print_grid(
                    &mut writer,
                    providers.into_iter().map(|provider| provider.name).collect(),
                )?;
            }
        }
        TraceSubCommand::ListCategoryGroups(_) => {
            let group_names = get_category_group_names().await?;
            writer.line("Category groups:")?;
            for group_name in group_names {
                writer.line(format!("  #{}", group_name))?;
            }
        }
        TraceSubCommand::Start(opts) => {
            let string_matcher: Option<String> = ffx_config::get(TARGET_DEFAULT_KEY).await.ok();
            let default = ffx::TargetQuery { string_matcher, ..Default::default() };
            let triggers = if opts.trigger.is_empty() { None } else { Some(opts.trigger) };
            if triggers.is_some() && !opts.background {
                ffx_bail!(
                    "Triggers can only be set on a background trace. \
                     Trace should be run with the --background flag."
                );
            }
            if opts.buffer_size > 64 {
                ffx_bail!(
                    "Error: Requested buffer size of {}MB is larger \
                           than the maximum supported buffer size of 64MB",
                    opts.buffer_size
                );
            }
            let expanded_categories = expand_categories(opts.categories).await?;
            let trace_config = TraceConfig {
                buffer_size_megabytes_hint: Some(opts.buffer_size),
                categories: Some(expanded_categories.clone()),
                buffering_mode: Some(opts.buffering_mode),
                ..map_categories_to_providers(&expanded_categories)
            };
            let output = canonical_path(opts.output)?;
            let res = proxy
                .start_recording(
                    &default,
                    &output,
                    &ffx::TraceOptions { duration: opts.duration, triggers, ..Default::default() },
                    &trace_config,
                )
                .await?;
            let target = handle_recording_result(res, &output).await?;
            writer.print(format!(
                "Tracing started successfully on \"{}\" for categories: [ {} ].\nWriting to {}",
                target.nodename.or(target.serial_number).as_deref().unwrap_or("<UNKNOWN>"),
                expanded_categories.join(","),
                output
            ))?;
            if let Some(duration) = &opts.duration {
                writer.line(format!(" for {} seconds.", duration))?;
            } else {
                writer.line("")?;
            }
            if opts.background {
                writer.line("To manually stop the trace, use `ffx trace stop`")?;
                writer.line("Current tracing status:")?;
                return status(&proxy, writer).await;
            }

            let waiter = &mut stdin();
            if let Some(duration) = &opts.duration {
                writer.line(format!("Waiting for {} seconds.", duration))?;
                fuchsia_async::Timer::new(Duration::from_secs_f64(*duration)).await;
            } else {
                writer.line("Press <enter> to stop trace.")?;
                waiter.wait().await;
            }
            writer.line(format!("Shutting down recording and writing to file."))?;
            stop_tracing(&proxy, output, writer).await?;
        }
        TraceSubCommand::Stop(opts) => {
            let output = match opts.output {
                Some(o) => canonical_path(o)?,
                None => default_target.unwrap_or("".to_owned()),
            };
            stop_tracing(&proxy, output, writer).await?;
        }
        TraceSubCommand::Status(_opts) => status(&proxy, writer).await?,
    }
    Ok(())
}

async fn status(proxy: &TracingProxy, mut writer: Writer) -> Result<()> {
    let (iter_proxy, server) = fidl::endpoints::create_proxy::<ffx::TracingStatusIteratorMarker>()?;
    proxy.status(server).await?;
    let mut res = Vec::new();
    loop {
        let r = iter_proxy.get_next().await?;
        if r.len() > 0 {
            res.extend(r);
        } else {
            break;
        }
    }
    if res.is_empty() {
        writer.line("No active traces running.")?;
    } else {
        let mut unknown_target_counter = 1;
        for trace in res.into_iter() {
            // TODO(awdavies): Fall back to SSH address, or return SSH
            // address from the protocol.
            let target_string =
                trace.target.and_then(|t| t.nodename.or(t.serial_number)).unwrap_or_else(|| {
                    let res = format!("Unknown Target {}", unknown_target_counter);
                    unknown_target_counter += 1;
                    res
                });
            writer.line(format!("- {}:", target_string))?;
            writer.line(format!(
                "  - Output file: {}",
                trace
                    .output_file
                    .ok_or(anyhow!("Trace status response contained no output file"))?,
            ))?;
            if let Some(duration) = trace.duration {
                writer.line(format!("  - Duration:  {} seconds", duration))?;
                writer.line(format!(
                    "  - Remaining: {} seconds",
                    trace.remaining_runtime.ok_or(anyhow!(
                        "Malformed status. Contained duration but not remaining runtime"
                    ))?
                ))?;
            } else {
                writer.line("  - Duration: indefinite")?;
            }
            if let Some(config) = trace.config {
                writer.line("  - Config:")?;
                if let Some(categories) = config.categories {
                    writer.line("    - Categories:")?;
                    writer.line(format!("      - {}", categories.join(",")))?;
                }
            }
            if let Some(triggers) = trace.triggers {
                writer.line("  - Triggers:")?;
                for trigger in triggers.into_iter() {
                    if trigger.alert.is_some() && trigger.action.is_some() {
                        writer.line(format!(
                            "    - {} : {:?}",
                            trigger.alert.unwrap(),
                            trigger.action.unwrap()
                        ))?;
                    }
                }
            }
        }
    }
    Ok(())
}

async fn stop_tracing(proxy: &TracingProxy, output: String, mut writer: Writer) -> Result<()> {
    let res = proxy.stop_recording(&output).await?;
    let target = handle_recording_result(res, &output).await?;
    // TODO(awdavies): Make a clickable link that auto-uploads the trace file if possible.
    writer.line(format!(
        "Tracing stopped successfully on \"{}\".\nResults written to {}",
        target.nodename.or(target.serial_number).as_deref().unwrap_or("<UNKNOWN>"),
        output
    ))?;
    writer.line("Upload to https://ui.perfetto.dev/#!/ to view.")?;
    Ok(())
}

async fn handle_recording_result(
    res: Result<ffx::TargetInfo, RecordingError>,
    output: &String,
) -> Result<ffx::TargetInfo> {
    let default: Option<String> = ffx_config::get(TARGET_DEFAULT_KEY).await.ok();
    match res {
        Ok(t) => Ok(t),
        Err(e) => match e {
            RecordingError::TargetProxyOpen => {
                ffx_bail!("Trace unable to open target proxy.");
            }
            RecordingError::RecordingAlreadyStarted => {
                // TODO(85098): Also return file info (which output file is being written to).
                ffx_bail!("Trace already started for target {}", default.unwrap_or("".to_owned()));
            }
            RecordingError::DuplicateTraceFile => {
                // TODO(85098): Also return target info.
                ffx_bail!("Trace already running for file {}", output);
            }
            RecordingError::RecordingStart => {
                let log_file: String = ffx_config::get("log.dir").await?;
                ffx_bail!(
                    "Error starting Fuchsia trace. See {}/ffx.daemon.log\n
Search for lines tagged with `ffx_daemon_service_tracing`. A common issue is a
peer closed error from `fuchsia.tracing.controller.Controller`. If this is the
case either tracing is not supported in the product configuration or the tracing
package is missing from the device's system image.",
                    log_file
                );
            }
            RecordingError::RecordingStop => {
                let log_file: String = ffx_config::get("log.dir").await?;
                ffx_bail!(
                    "Error stopping Fuchsia trace. See {}/ffx.daemon.log\n
Search for lines tagged with `ffx_daemon_service_tracing`. A common issue is a
peer closed error from `fuchsia.tracing.controller.Controller`. If this is the
case either tracing is not supported in the product configuration or the tracing
package is missing from the device's system image.",
                    log_file
                );
            }
            RecordingError::NoSuchTraceFile => {
                ffx_bail!("Could not stop trace. No active traces for {}.", output);
            }
            RecordingError::NoSuchTarget => {
                ffx_bail!(
                    "The string '{}' didn't match a trace output file, or any valid targets.",
                    default.as_deref().unwrap_or("")
                );
            }
            RecordingError::DisconnectedTarget => {
                ffx_bail!(
                    "The string '{}' didn't match a valid target connected to the ffx daemon.",
                    default.as_deref().unwrap_or("")
                );
            }
        },
    }
}

fn canonical_path(output_path: String) -> Result<String> {
    let output_path = PathBuf::from(output_path);
    let mut path = PathBuf::new();
    if !output_path.has_root() {
        path.push(std::env::current_dir()?);
    }
    path.push(output_path);
    let mut components = path.components().peekable();
    let mut res = if let Some(c @ Component::Prefix(..)) = components.peek().cloned() {
        components.next();
        PathBuf::from(c.as_os_str())
    } else {
        PathBuf::new()
    };
    for component in components {
        match component {
            Component::Prefix(..) => return Err(anyhow!("prefix unreachable")),
            Component::RootDir => {
                res.push(component.as_os_str());
            }
            Component::CurDir => {}
            Component::ParentDir => {
                res.pop();
            }
            Component::Normal(c) => {
                res.push(c);
            }
        }
    }
    res.into_os_string()
        .into_string()
        .map_err(|e| anyhow!("unable to convert OsString to string {:?}", e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use errors::ResultExt as _;
    use ffx_trace_args::{ListCategories, ListProviders, Start, Status, Stop};
    use ffx_writer::{Format, TestBuffers};
    use fidl::endpoints::{ControlHandle, Responder};
    use fidl_fuchsia_developer_ffx as ffx;
    use fidl_fuchsia_tracing as tracing;
    use fidl_fuchsia_tracing_controller as tracing_controller;
    use futures::TryStreamExt;
    use pretty_assertions::assert_eq;
    use regex::Regex;
    use serde_json::json;
    use std::matches;

    #[test]
    fn test_canonical_path_has_root() {
        let p = canonical_path("what".to_string()).unwrap();
        let got = PathBuf::from(p);
        let got = got.components().next().unwrap();
        assert!(matches!(got, Component::RootDir));
    }

    #[test]
    fn test_canonical_path_cleans_dots() {
        let mut path = PathBuf::new();
        path.push(Component::RootDir);
        path.push("this");
        path.push(Component::ParentDir);
        path.push("that");
        path.push("these");
        path.push(Component::CurDir);
        path.push("what.txt");
        let got = canonical_path(path.into_os_string().into_string().unwrap()).unwrap();
        let mut want = PathBuf::new();
        want.push(Component::RootDir);
        want.push("that");
        want.push("these");
        want.push("what.txt");
        let want = want.into_os_string().into_string().unwrap();
        assert_eq!(want, got);
    }

    #[test]
    fn test_print_grid_too_wide() {
        let test_buffers = TestBuffers::default();
        let mut writer = Writer::new_test(None, &test_buffers);
        print_grid(
            &mut writer,
            vec![
                "really_really_really_really\
                _really_really_really_really\
                _really_really_long_category"
                    .to_string(),
                "short_category".to_string(),
                "another_short_category".to_string(),
            ],
        )
        .unwrap();
        let output = test_buffers.into_stdout_str();
        let want = "really_really_really_really\
                          _really_really_really_really\
                          _really_really_long_category\n\
                          short_category\n\
                          another_short_category\n";
        assert_eq!(want, output);
    }

    fn setup_fake_service() -> TracingProxy {
        fho::testing::fake_proxy(|req| match req {
            ffx::TracingRequest::StartRecording { responder, .. } => responder
                .send(&mut Ok(ffx::TargetInfo {
                    nodename: Some("foo".to_owned()),
                    ..Default::default()
                }))
                .expect("responder err"),
            ffx::TracingRequest::StopRecording { responder, .. } => responder
                .send(&mut Ok(ffx::TargetInfo {
                    nodename: Some("foo".to_owned()),
                    ..Default::default()
                }))
                .expect("responder err"),
            ffx::TracingRequest::Status { responder, iterator } => {
                let mut stream = iterator.into_stream().unwrap();
                fuchsia_async::Task::local(async move {
                    let ffx::TracingStatusIteratorRequest::GetNext { responder, .. } =
                        stream.try_next().await.unwrap().unwrap();
                    responder
                        .send(&[
                            ffx::TraceInfo {
                                target: Some(ffx::TargetInfo {
                                    nodename: Some("foo".to_string()),
                                    ..Default::default()
                                }),
                                output_file: Some("/foo/bar.fxt".to_string()),
                                ..Default::default()
                            },
                            ffx::TraceInfo {
                                output_file: Some("/foo/bar/baz.fxt".to_string()),
                                ..Default::default()
                            },
                            ffx::TraceInfo {
                                output_file: Some("/florp/o/matic.txt".to_string()),
                                triggers: Some(vec![
                                    ffx::Trigger {
                                        alert: Some("foo".to_owned()),
                                        action: Some(ffx::Action::Terminate),
                                        ..Default::default()
                                    },
                                    ffx::Trigger {
                                        alert: Some("bar".to_owned()),
                                        action: Some(ffx::Action::Terminate),
                                        ..Default::default()
                                    },
                                ]),
                                ..Default::default()
                            },
                        ])
                        .unwrap();
                    let ffx::TracingStatusIteratorRequest::GetNext { responder, .. } =
                        stream.try_next().await.unwrap().unwrap();
                    responder.send(&[]).unwrap();
                })
                .detach();
                responder.send().expect("responder err")
            }
        })
    }

    fn setup_fake_controller_proxy() -> fho::Deferred<ControllerProxy> {
        fho::Deferred::from_output(Ok(fho::testing::fake_proxy(|req| match req {
            tracing_controller::ControllerRequest::GetKnownCategories { responder, .. } => {
                responder.send(&fake_known_categories()).expect("should respond");
            }
            tracing_controller::ControllerRequest::GetProviders { responder, .. } => {
                responder.send(&fake_provider_infos()).expect("should respond");
            }
            r => panic!("unsupported req: {:?}", r),
        })))
    }

    fn fake_known_categories() -> Vec<tracing::KnownCategory> {
        vec![
            tracing::KnownCategory {
                name: String::from("input"),
                description: String::from("Input system"),
            },
            tracing::KnownCategory {
                name: String::from("kernel"),
                description: String::from("All kernel trace events"),
            },
            tracing::KnownCategory {
                name: String::from("kernel:arch"),
                description: String::from("Kernel arch events"),
            },
            tracing::KnownCategory {
                name: String::from("kernel:ipc"),
                description: String::from("Kernel ipc events"),
            },
        ]
    }

    fn fake_provider_infos() -> Vec<tracing_controller::ProviderInfo> {
        vec![
            tracing_controller::ProviderInfo {
                id: Some(42),
                name: Some("foo".to_string()),
                ..Default::default()
            },
            tracing_controller::ProviderInfo {
                id: Some(99),
                pid: Some(1234567),
                name: Some("bar".to_string()),
                ..Default::default()
            },
            tracing_controller::ProviderInfo { id: Some(2), ..Default::default() },
        ]
    }

    fn fake_trace_provider_infos() -> Vec<TraceProviderInfo> {
        let mut infos: Vec<TraceProviderInfo> =
            fake_provider_infos().into_iter().map(TraceProviderInfo::from).collect();
        infos.sort_unstable();
        infos
    }

    fn setup_closed_fake_controller_proxy() -> fho::Deferred<ControllerProxy> {
        fho::Deferred::from_output(Ok(fho::testing::fake_proxy(|req| match req {
            tracing_controller::ControllerRequest::GetKnownCategories { responder, .. } => {
                responder.control_handle().shutdown();
            }
            tracing_controller::ControllerRequest::GetProviders { responder, .. } => {
                responder.control_handle().shutdown();
            }
            r => panic!("unsupported req: {:?}", r),
        })))
    }

    async fn run_trace_test(cmd: TraceCommand, writer: Writer) {
        let proxy = setup_fake_service();
        let controller = setup_fake_controller_proxy();
        trace(proxy, controller, writer, cmd).await.unwrap();
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_list_categories() {
        let _env = ffx_config::test_init().await.unwrap();
        let test_buffers = TestBuffers::default();
        let writer = Writer::new_test(None, &test_buffers);
        run_trace_test(
            TraceCommand { sub_cmd: TraceSubCommand::ListCategories(ListCategories {}) },
            writer,
        )
        .await;
        let output = test_buffers.into_stdout_str();
        let want = "input  kernel  kernel:arch  kernel:ipc\n\n";
        assert_eq!(want, output);
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_list_categories_machine() {
        let _env = ffx_config::test_init().await.unwrap();
        let test_buffers = TestBuffers::default();
        let writer = Writer::new_test(Some(Format::Json), &test_buffers);
        run_trace_test(
            TraceCommand { sub_cmd: TraceSubCommand::ListCategories(ListCategories {}) },
            writer,
        )
        .await;
        let output = test_buffers.into_stdout_str();
        let want = serde_json::to_string(
            &fake_known_categories()
                .into_iter()
                .map(TraceKnownCategory::from)
                .collect::<Vec<TraceKnownCategory>>(),
        )
        .unwrap();
        assert_eq!(want, output.trim_end());
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_list_categories_peer_closed() {
        let _env = ffx_config::test_init().await.unwrap();
        let test_buffers = TestBuffers::default();
        let writer = Writer::new_test(None, &test_buffers);
        let proxy = setup_fake_service();
        let controller = setup_closed_fake_controller_proxy();
        let cmd = TraceCommand { sub_cmd: TraceSubCommand::ListCategories(ListCategories {}) };
        let res = trace(proxy, controller, writer, cmd).await.unwrap_err();
        assert!(res.ffx_error().is_some());
        assert!(res.to_string().contains("This can happen if tracing is not"));
        assert!(test_buffers.into_stdout_str().is_empty());
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_list_providers() {
        let _env = ffx_config::test_init().await.unwrap();
        let test_buffers = TestBuffers::default();
        let writer = Writer::new_test(None, &test_buffers);
        run_trace_test(
            TraceCommand { sub_cmd: TraceSubCommand::ListProviders(ListProviders {}) },
            writer,
        )
        .await;
        let output = test_buffers.into_stdout_str();
        let want = "Trace providers:\n\
                   bar  foo  unknown\n\n"
            .to_string();
        assert_eq!(want, output);
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_list_providers_peer_closed() {
        let _env = ffx_config::test_init().await.unwrap();
        let test_buffers = TestBuffers::default();
        let writer = Writer::new_test(None, &test_buffers);
        let proxy = setup_fake_service();
        let controller = setup_closed_fake_controller_proxy();
        let cmd = TraceCommand { sub_cmd: TraceSubCommand::ListProviders(ListProviders {}) };
        let res = trace(proxy, controller, writer, cmd).await.unwrap_err();
        assert!(res.ffx_error().is_some());
        assert!(res.to_string().contains("This can happen if tracing is not"));
        assert!(test_buffers.into_stdout_str().is_empty());
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_list_providers_machine() {
        let _env = ffx_config::test_init().await.unwrap();
        let test_buffers = TestBuffers::default();
        let writer = Writer::new_test(Some(Format::Json), &test_buffers);
        run_trace_test(
            TraceCommand { sub_cmd: TraceSubCommand::ListProviders(ListProviders {}) },
            writer,
        )
        .await;
        let output = test_buffers.into_stdout_str();
        let want = serde_json::to_string(&fake_trace_provider_infos()).unwrap();
        assert_eq!(want, output.trim_end());
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_start() {
        let _env = ffx_config::test_init().await.unwrap();
        let test_buffers = TestBuffers::default();
        let writer = Writer::new_test(None, &test_buffers);
        run_trace_test(
            TraceCommand {
                sub_cmd: TraceSubCommand::Start(Start {
                    buffer_size: 2,
                    categories: vec!["platypus".to_string(), "beaver".to_string()],
                    duration: None,
                    buffering_mode: tracing::BufferingMode::Oneshot,
                    output: "foo.txt".to_string(),
                    background: true,
                    trigger: vec![],
                }),
            },
            writer,
        )
        .await;
        let output = test_buffers.into_stdout_str();
        // This doesn't find `/.../foo.txt` for the tracing status, since the faked
        // proxy has no state.
        let regex_str = "Tracing started successfully on \"foo\" for categories: \\[ beaver,platypus \\].\nWriting to /([^/]+/)+?foo.txt
To manually stop the trace, use `ffx trace stop`
Current tracing status:
- foo:
  - Output file: /foo/bar.fxt
  - Duration: indefinite
- Unknown Target 1:
  - Output file: /foo/bar/baz.fxt
  - Duration: indefinite
- Unknown Target 2:
  - Output file: /florp/o/matic.txt
  - Duration: indefinite
  - Triggers:
    - foo : Terminate
    - bar : Terminate\n";
        let want = Regex::new(regex_str).unwrap();
        assert!(want.is_match(&output), "\"{}\" didn't match regex /{}/", output, regex_str);
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_status() {
        let _env = ffx_config::test_init().await.unwrap();
        let test_buffers = TestBuffers::default();
        let writer = Writer::new_test(None, &test_buffers);
        run_trace_test(TraceCommand { sub_cmd: TraceSubCommand::Status(Status {}) }, writer).await;
        let output = test_buffers.into_stdout_str();
        let want = "- foo:
  - Output file: /foo/bar.fxt
  - Duration: indefinite
- Unknown Target 1:
  - Output file: /foo/bar/baz.fxt
  - Duration: indefinite
- Unknown Target 2:
  - Output file: /florp/o/matic.txt
  - Duration: indefinite
  - Triggers:
    - foo : Terminate
    - bar : Terminate\n";
        assert_eq!(want, output);
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_stop() {
        let _env = ffx_config::test_init().await.unwrap();
        let test_buffers = TestBuffers::default();
        let writer = Writer::new_test(None, &test_buffers);
        run_trace_test(
            TraceCommand {
                sub_cmd: TraceSubCommand::Stop(Stop { output: Some("foo.txt".to_string()) }),
            },
            writer,
        )
        .await;
        let output = test_buffers.into_stdout_str();
        let regex_str = "Tracing stopped successfully on \"foo\".\nResults written to /([^/]+/)+?foo.txt\nUpload to https://ui.perfetto.dev/#!/ to view.";
        let want = Regex::new(regex_str).unwrap();
        assert!(want.is_match(&output), "\"{}\" didn't match regex /{}/", output, regex_str);
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_start_with_duration() {
        let _env = ffx_config::test_init().await.unwrap();
        let test_buffers = TestBuffers::default();
        let writer = Writer::new_test(None, &test_buffers);
        run_trace_test(
            TraceCommand {
                sub_cmd: TraceSubCommand::Start(Start {
                    buffer_size: 2,
                    categories: vec![],
                    duration: Some(5.2),
                    buffering_mode: tracing::BufferingMode::Oneshot,
                    output: "foober.fxt".to_owned(),
                    background: true,
                    trigger: vec![],
                }),
            },
            writer,
        )
        .await;
        let output = test_buffers.into_stdout_str();
        let regex_str =
            "Tracing started successfully on \"foo\" for categories: \\[  \\].\nWriting to /([^/]+/)+?foober.fxt for 5.2 seconds.";
        let want = Regex::new(regex_str).unwrap();
        assert!(want.is_match(&output), "\"{}\" didn't match regex /{}/", output, regex_str);
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_start_with_duration_foreground() {
        let _env = ffx_config::test_init().await.unwrap();
        let test_buffers = TestBuffers::default();
        let writer = Writer::new_test(None, &test_buffers);
        run_trace_test(
            TraceCommand {
                sub_cmd: TraceSubCommand::Start(Start {
                    buffer_size: 2,
                    categories: vec![],
                    duration: Some(0.8),
                    buffering_mode: tracing::BufferingMode::Oneshot,
                    output: "foober.fxt".to_owned(),
                    background: false,
                    trigger: vec![],
                }),
            },
            writer,
        )
        .await;
        let output = test_buffers.into_stdout_str();
        let regex_str =
            "Tracing started successfully on \"foo\" for categories: \\[  \\].\nWriting to /([^/]+/)+?foober.fxt for 0.8 seconds.\n\
            Waiting for 0.8 seconds.\n\
            Shutting down recording and writing to file.\n\
            Tracing stopped successfully on \"foo\".\nResults written to /([^/]+/)+?foober.fxt\n\
            Upload to https://ui.perfetto.dev/#!/ to view.";
        let want = Regex::new(regex_str).unwrap();
        assert!(want.is_match(&output), "\"{}\" didn't match regex /{}/", output, regex_str);
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_start_foreground() {
        let _env = ffx_config::test_init().await.unwrap();
        let test_buffers = TestBuffers::default();
        let writer = Writer::new_test(None, &test_buffers);
        run_trace_test(
            TraceCommand {
                sub_cmd: TraceSubCommand::Start(Start {
                    buffer_size: 2,
                    categories: vec![],
                    buffering_mode: tracing::BufferingMode::Oneshot,
                    duration: None,
                    output: "foober.fxt".to_owned(),
                    background: false,
                    trigger: vec![],
                }),
            },
            writer,
        )
        .await;
        let output = test_buffers.into_stdout_str();
        let regex_str =
            "Tracing started successfully on \"foo\" for categories: \\[  \\].\nWriting to /([^/]+/)+?foober.fxt\n\
            Press <enter> to stop trace.\n\
            Shutting down recording and writing to file.\n\
            Tracing stopped successfully on \"foo\".\nResults written to /([^/]+/)+?foober.fxt\n\
            Upload to https://ui.perfetto.dev/#!/ to view.";
        let want = Regex::new(regex_str).unwrap();
        assert!(want.is_match(&output), "\"{}\" didn't match regex /{}/", output, regex_str);
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_large_buffer() {
        let _env = ffx_config::test_init().await.unwrap();
        let test_buffers = TestBuffers::default();
        let writer = Writer::new_test(None, &test_buffers);
        let proxy = setup_fake_service();
        let controller = setup_closed_fake_controller_proxy();
        let cmd = TraceCommand {
            sub_cmd: TraceSubCommand::Start(Start {
                buffering_mode: tracing::BufferingMode::Oneshot,
                categories: vec![],
                duration: Some(1.0),
                background: false,
                buffer_size: 65,
                output: "foober.fxt".to_owned(),
                trigger: vec![],
            }),
        };
        let res = trace(proxy, controller, writer, cmd).await.unwrap_err();
        assert!(res.ffx_error().is_some());
        assert!(res.to_string().contains("Error: Requested buffer size of"));
        assert!(test_buffers.into_stdout_str().is_empty());
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_get_category_group() {
        let _env = ffx_config::test_init().await.unwrap();
        let birds = vec!["chickens", "bald_eagle", "blue-jay", "hawk*", "goose:gosling"];
        ffx_config::query("trace.category_groups.birds")
            .level(Some(ffx_config::ConfigLevel::User))
            .set(json!(birds))
            .await
            .unwrap();
        assert_eq!(birds, get_category_group("birds").await.unwrap());
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_get_category_group_names() {
        let _env = ffx_config::test_init().await.unwrap();
        let birds = vec!["chickens", "ducks"];
        let bees = vec!["honey", "bumble"];
        ffx_config::query("trace.category_groups.birds")
            .level(Some(ffx_config::ConfigLevel::User))
            .set(json!(birds))
            .await
            .unwrap();
        ffx_config::query("trace.category_groups.bees")
            .level(Some(ffx_config::ConfigLevel::User))
            .set(json!(bees))
            .await
            .unwrap();
        ffx_config::query("trace.category_groups.*invalid")
            .level(Some(ffx_config::ConfigLevel::User))
            .set(json!(bees))
            .await
            .unwrap();
        assert!(get_category_group_names().await.unwrap().contains(&"birds".to_owned()));
        assert!(get_category_group_names().await.unwrap().contains(&"bees".to_owned()));
        assert!(get_category_group_names().await.unwrap().contains(&"*invalid".to_owned()));
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_get_category_group_not_found() {
        let _env = ffx_config::test_init().await.unwrap();
        let err = get_category_group("not_found").await.unwrap_err();
        assert!(
            err.to_string().contains("Error: no category group found for not_found"),
            "the actual value was \"{}\"",
            err.to_string()
        );
    }

    const INVALID_CATEGORIES: &[&str] =
        &["chic*kens", "*turkeys", "golden eagle", "ha,wk*", "goose:gosl\"ing"];

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_get_category_group_invalid_category() {
        let _env = ffx_config::test_init().await.unwrap();
        for invalid_category in INVALID_CATEGORIES {
            ffx_config::query("trace.category_groups.flawed")
                .level(Some(ffx_config::ConfigLevel::User))
                .set(json!(vec![invalid_category]))
                .await
                .unwrap();
            let err = get_category_group("flawed").await.unwrap_err();
            let expected_message = format!("invalid category \"{}\"", invalid_category);
            assert!(
                err.to_string().contains(&expected_message),
                "the actual value was \"{}\"",
                err.to_string()
            );
        }
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_expand_categories() {
        let _env = ffx_config::test_init().await.unwrap();
        let birds = vec!["chickens", "bald_eagle", "hawk*", "goose:gosling", "blue-jay"];
        ffx_config::query("trace.category_groups.birds")
            .level(Some(ffx_config::ConfigLevel::User))
            .set(json!(birds))
            .await
            .unwrap();
        // The result should have all groups expanded, merge duplicate categories, and sort them.
        assert_eq!(
            vec!["*", "bald_eagle", "blue-jay", "chickens", "dove*", "goose:gosling", "hawk*"],
            expand_categories(vec![
                "dove*".to_string(),
                "bald_eagle".to_string(),
                "#birds".to_string(),
                "*".to_string()
            ])
            .await
            .unwrap()
        );
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_expand_categories_invalid() {
        let _env = ffx_config::test_init().await.unwrap();
        for invalid_category in INVALID_CATEGORIES {
            let err = expand_categories(vec![invalid_category.to_string()]).await.unwrap_err();
            let expected_message = format!("category \"{}\" is invalid", invalid_category);
            assert!(
                err.to_string().contains(&expected_message),
                "the actual value was \"{}\"",
                err.to_string()
            );
        }
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_curated_category_groups_valid() {
        let _env = ffx_config::test_init().await.unwrap();

        // Get all of the category groups found in config.json
        let category_groups_json: serde_json::Value =
            ffx_config::get("trace.category_groups").await.unwrap();

        for category_group_name in category_groups_json.as_object().unwrap().keys() {
            let category_group = get_category_group(category_group_name).await.unwrap();
            assert_ne!(0, category_group.len());
        }
    }

    #[test]
    fn test_map_categories_to_providers() {
        let expected_trace_config = TraceConfig {
            categories: Some(vec!["talon".to_string(), "beak".to_string()]),
            provider_specs: Some(vec![
                ProviderSpec {
                    name: Some("falcon".to_string()),
                    categories: Some(vec!["prairie".to_string(), "peregrine".to_string()]),
                    ..Default::default()
                },
                ProviderSpec {
                    name: Some("owl".to_string()),
                    categories: Some(vec![
                        "screech".to_string(),
                        "elf".to_string(),
                        "snowy".to_string(),
                    ]),
                    ..Default::default()
                },
            ]),
            ..Default::default()
        };

        let mut actual_trace_config = map_categories_to_providers(&vec![
            "owl/screech".to_string(),
            "owl/elf".to_string(),
            "owl/snowy".to_string(),
            "falcon/prairie".to_string(),
            "talon".to_string(),
            "beak".to_string(),
            "falcon/peregrine".to_string(),
        ]);

        // Lexicographically sort the provider specs on names to ensure a stable test.
        // The order doesn't matter, but it can vary with different platforms and compiler flags.
        actual_trace_config
            .provider_specs
            .as_mut()
            .unwrap()
            .sort_unstable_by_key(|s| s.name.clone().unwrap());
        assert_eq!(expected_trace_config, actual_trace_config);
    }
}
