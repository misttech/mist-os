// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{anyhow, Context, Result};
use errors::ffx_bail;
use ffx_config::EnvironmentContext;
use ffx_target::get_target_specifier;
use ffx_trace_args::{TraceCommand, TraceSubCommand};
use fho::{daemon_protocol, deferred, moniker, FfxMain, FfxTool, MachineWriter, ToolIO};
use fidl_fuchsia_developer_ffx::{self as ffx, RecordingError, TracingProxy};
use fidl_fuchsia_tracing::{BufferingMode, KnownCategory};
use fidl_fuchsia_tracing_controller::{
    ProviderInfo, ProviderSpec, ProviderStats, ProvisionerProxy, TraceConfig,
};
use flyweights::FlyStr;
use futures::future::{BoxFuture, FutureExt};
use fxt::{Arg, ArgValue, RawArg, RawArgValue, RawEventRecord, SessionParser, StringRef};
use lazy_static::lazy_static;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeSet, HashMap};
use std::future::Future;
use std::io::{stdin, LineWriter, Stdin, Write};
use std::path::{Component, PathBuf};
use std::time::Duration;
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
            name: info.name.as_ref().cloned().unwrap_or_else(|| "unknown".to_string()),
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

fn more_than_init_record(
    non_durable_bytes_written: u64,
    durable_buffer_used: f32,
    buffering_mode: BufferingMode,
) -> bool {
    let init_record_size_in_bytes = 16;
    match buffering_mode {
        BufferingMode::Oneshot => non_durable_bytes_written > init_record_size_in_bytes,
        _ => durable_buffer_used > 0.0,
    }
}

fn stats_to_print(trace_stat: ProviderStats, verbose: bool) -> Vec<String> {
    let mut stats_output = Vec::new();
    let (
        Some(provider_name),
        Some(pid),
        Some(buffering_mode),
        Some(wrapped_count),
        Some(records_dropped),
        Some(durable_buffer_used),
        Some(non_durable_bytes_written),
    ) = (
        trace_stat.name,
        trace_stat.pid,
        trace_stat.buffering_mode,
        trace_stat.buffer_wrapped_count,
        trace_stat.records_dropped,
        trace_stat.percentage_durable_buffer_used,
        trace_stat.non_durable_bytes_written,
    )
    else {
        if verbose {
            stats_output.push(String::from("A provider returned stats with missing values"));
        }
        return stats_output;
    };
    if (verbose
        && more_than_init_record(non_durable_bytes_written, durable_buffer_used, buffering_mode))
        || (records_dropped != 0)
    {
        if records_dropped != 0 {
            stats_output
                .push(format!("WARNING: {provider_name:?} dropped {records_dropped:?} records!"));
        }
        if verbose {
            stats_output.extend([
                format!("{provider_name:?} (pid: {pid:?}) trace stats"),
                format!("Buffer wrapped count: {wrapped_count:?}"),
                format!("# records dropped: {records_dropped:?}"),
                format!("Durable buffer used: {durable_buffer_used:.2}%"),
                format!("Bytes written to non-durable buffer: {non_durable_bytes_written:#X}\n"),
            ]);
        }
    }
    return stats_output;
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

async fn get_category_group_names(ctx: &EnvironmentContext) -> Result<Vec<String>> {
    let all_groups = ctx
        .query("trace.category_groups")
        .select(ffx_config::SelectMode::All)
        .get::<Value>()
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

async fn get_category_group(
    ctx: &EnvironmentContext,
    category_group_name: &str,
) -> Result<Vec<String>> {
    let category_group = ctx
        .get::<Vec<String>, _>(&format!("trace.category_groups.{}", category_group_name))
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

async fn expand_categories(
    context: &EnvironmentContext,
    categories: Vec<String>,
) -> Result<Vec<String>> {
    let mut expanded_categories = BTreeSet::new();
    for category in categories {
        match category.strip_prefix('#') {
            Some(category_group_name) => {
                let category_group = get_category_group(context, category_group_name).await?;
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
                .or_insert_with(|| vec![category.to_string()]);
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

fn ir_files_list(env_ctx: &EnvironmentContext) -> Option<Vec<String>> {
    let mut ir_files = Vec::new();
    #[allow(clippy::or_fun_call)] // TODO(https://fxbug.dev/379717780)
    let build_dir = env_ctx.build_dir().unwrap_or(&std::path::Path::new(""));
    let all_fidl_json_path = build_dir.join("all_fidl_json.txt");
    match std::fs::read_to_string(all_fidl_json_path) {
        Ok(file_list) => {
            for line in file_list.lines() {
                if let Some(ir_file_path) = build_dir.join(line).to_str() {
                    ir_files.push(ir_file_path.to_string());
                }
            }
        }
        Err(_) => return None,
    };
    Some(ir_files)
}

fn generate_symbolization_map(ir_files: Vec<String>) -> (HashMap<u64, String>, Vec<String>) {
    let mut ord_fn_map = HashMap::new();
    let mut warnings = vec![];
    // Scan through the list of ir files and look for the provided ordinal in the json contents.
    for ir_file in ir_files {
        let json_string = match std::fs::read_to_string(ir_file.clone()) {
            Ok(content) => content,
            Err(e) => {
                warnings.push(format!("WARNING: Failed to read {ir_file}. Reason: {e}"));
                continue;
            }
        };
        let fidl_json: serde_json::Value = match serde_json::from_str(&json_string) {
            Ok(serialized_json) => serialized_json,
            Err(_) => {
                warnings.push(format!("WARNING: Failed to parse json in IR file {ir_file}"));
                continue;
            }
        };
        let Some(protocols) = fidl_json["protocol_declarations"].as_array() else {
            continue;
        };
        for protocol in protocols {
            // Protocol should have a name, but it is missing for some reason.
            let protocol_name = protocol["name"].as_str().unwrap_or("-");
            let Some(methods) = protocol["methods"].as_array() else {
                continue;
            };
            for method in methods {
                let Some(method_ordinal) = method["ordinal"].as_u64() else {
                    continue;
                };
                let method_name = method["name"].as_str().unwrap_or("-");
                ord_fn_map.insert(method_ordinal, format!("{protocol_name}.{method_name}"));
            }
        }
    }
    (ord_fn_map, warnings)
}

fn symbolize_ordinal(ordinal: u64, ir_files: Vec<String>, mut writer: Writer) -> Result<()> {
    let (fidl_ordinal_map, warnings) = generate_symbolization_map(ir_files);
    for warning in warnings {
        writer.line(warning)?;
    }
    if fidl_ordinal_map.contains_key(&ordinal) {
        writer.line(format!("{} -> {}", ordinal, fidl_ordinal_map[&ordinal]))?;
    } else {
        writer.line(format!(
            "Unable to symbolize ordinal {}. This could be because either:",
            ordinal
        ))?;
        writer.line("1. The ordinal is incorrect")?;
        writer.line("2. The ordinal is not found in IR files in $FUCHSIA_BUILD_DIR/all_fidl_json.txt or the input IR files")?;
    }
    Ok(())
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

pub fn symbolize_fidl_call<'a>(bytes: &[u8], ordinal: u64, method: &'a str) -> Result<Vec<u8>> {
    let (_, mut raw_event_record) =
        RawEventRecord::parse(bytes).expect("Unable to parse event record");
    let mut new_args = vec![];
    for arg in &raw_event_record.args {
        if let &RawArgValue::Unsigned64(arg_value) = &arg.value {
            if arg_value == ordinal {
                let symbolized_arg = RawArg {
                    name: StringRef::Inline("method"),
                    value: RawArgValue::String(StringRef::Inline(method)),
                };
                new_args.push(symbolized_arg);
            }
        }
        new_args.push(arg.clone());
    }

    raw_event_record.args = new_args;
    raw_event_record.serialize().map_err(|e| anyhow!(e))
}

fn symbolize_trace_file(
    trace_file: String,
    outfile: String,
    ctx: &EnvironmentContext,
) -> Result<()> {
    let content = std::fs::read(trace_file)?;
    let mut parser = SessionParser::new(std::io::Cursor::new(content));
    let output = std::fs::File::create(outfile)?;
    let mut output = LineWriter::new(output);

    let (ord_map, _) = generate_symbolization_map(ir_files_list(ctx).unwrap_or_default());
    let ordinal_arg_name = FlyStr::from("ordinal");
    while let Some(record) = parser.next() {
        let mut parsed_bytes = parser.parsed_bytes().to_owned();
        output.write_all(match record {
            Ok(fxt::TraceRecord::Event(fxt::EventRecord { category, args, .. }))
                if category.as_str() == "kernel:ipc" =>
            {
                for arg in args {
                    match arg {
                        Arg { name, value: ArgValue::Unsigned64(ord) }
                            if name == ordinal_arg_name && ord_map.contains_key(&ord) =>
                        {
                            parsed_bytes =
                                symbolize_fidl_call(&parsed_bytes, ord, ord_map[&ord].as_str())
                                    .unwrap_or(parsed_bytes)
                        }
                        _ => continue,
                    }
                }
                &parsed_bytes
            }
            _ => &parsed_bytes,
        })?;
    }
    Ok(output.flush()?)
}

type Writer = MachineWriter<TraceOutput>;
#[derive(FfxTool)]
pub struct TraceTool {
    #[with(daemon_protocol())]
    proxy: TracingProxy,
    #[with(deferred(moniker("/core/trace_manager")))]
    provisioner: fho::Deferred<ProvisionerProxy>,
    #[command]
    cmd: TraceCommand,
    context: EnvironmentContext,
}

fho::embedded_plugin!(TraceTool);

#[async_trait::async_trait(?Send)]
impl FfxMain for TraceTool {
    type Writer = Writer;

    async fn main(self, writer: Self::Writer) -> fho::Result<()> {
        trace(self.context, self.proxy, self.provisioner, writer, self.cmd)
            .await
            .map_err(Into::into)
    }
}

pub async fn trace(
    context: EnvironmentContext,
    proxy: TracingProxy,
    provisioner: fho::Deferred<ProvisionerProxy>,
    mut writer: Writer,
    cmd: TraceCommand,
) -> Result<()> {
    let target_spec: Option<String> = get_target_specifier(&context).await?;
    match cmd.sub_cmd {
        TraceSubCommand::ListCategories(_) => {
            let controller = provisioner.await?;
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
            let provisioner = provisioner.await?;
            let mut providers = handle_fidl_error(provisioner.get_providers().await)?
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
            let group_names = get_category_group_names(&context).await?;
            writer.line("Category groups:")?;
            for group_name in group_names {
                writer.line(format!("  #{}", group_name))?;
            }
        }
        TraceSubCommand::Start(opts) => {
            let default = ffx::TargetQuery { string_matcher: target_spec, ..Default::default() };
            let triggers = if opts.trigger.is_empty() { None } else { Some(opts.trigger) };
            if triggers.is_some() && !opts.background {
                ffx_bail!(
                    "Triggers can only be set on a background trace. \
                     Trace should be run with the --background flag."
                );
            }
            let expanded_categories = expand_categories(&context, opts.categories).await?;
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
            let Ok(target) = res else {
                ffx_bail!("{}", handle_recording_error(&context, res.unwrap_err(), &output).await);
            };
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
            stop_tracing(&context, &proxy, output, writer, opts.verbose, opts.no_symbolize).await?;
        }
        TraceSubCommand::Stop(opts) => {
            let output = match opts.output {
                Some(o) => canonical_path(o)?,
                None => target_spec.unwrap_or_else(|| "".to_owned()),
            };
            stop_tracing(&context, &proxy, output, writer, opts.verbose, opts.no_symbolize).await?;
        }
        TraceSubCommand::Status(_opts) => status(&proxy, writer).await?,
        TraceSubCommand::Symbolize(opts) => {
            if let Some(trace_file) = opts.fxt {
                let outfile = opts.outfile.unwrap_or_else(|| trace_file.clone());
                symbolize_trace_file(trace_file, outfile.clone(), &context)?;
                writer.line(format!("Symbolized traces written to {outfile}"))?;
            } else if let Some(ordinal) = opts.ordinal {
                let mut all_ir_files = opts.ir_path.clone();
                let build_ir_files = match ir_files_list(&context) {
                    None => {
                        writer.line("Unable to read list of FIDL IR files from $FUCHSIA_BUILD_DIR/all_fidl_json.txt.")?;
                        writer.line("Only input IR files will be searched.")?;
                        vec![]
                    }
                    Some(ir_files) => ir_files,
                };
                all_ir_files.extend(build_ir_files);
                symbolize_ordinal(ordinal, all_ir_files, writer)?;
            } else {
                ffx_bail!("Either ordinal or trace file must be provided to symbolize");
            }
        }
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
                    .ok_or_else(|| anyhow!("Trace status response contained no output file"))?,
            ))?;
            if let Some(duration) = trace.duration {
                writer.line(format!("  - Duration:  {} seconds", duration))?;
                writer.line(format!(
                    "  - Remaining: {} seconds",
                    trace.remaining_runtime.ok_or_else(|| anyhow!(
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

async fn stop_tracing(
    context: &EnvironmentContext,
    proxy: &TracingProxy,
    output: String,
    mut writer: Writer,
    verbose: bool,
    skip_symbolization: bool,
) -> Result<()> {
    let res = proxy.stop_recording(&output).await?;
    let (target, output_file) = match res {
        Ok((target, output_file, categories, stop_result)) => {
            for stat in stop_result.provider_stats.unwrap_or_default() {
                for stat_output in stats_to_print(stat, verbose) {
                    writer.line(stat_output)?;
                }
            }

            if !skip_symbolization && categories.contains(&"kernel:ipc".to_string()) {
                symbolize_trace_file(output_file.clone(), output_file.clone(), &context)?;
            }

            (target, output_file)
        }
        Err(e) => ffx_bail!("{}", handle_recording_error(context, e, &output).await),
    };
    // TODO(awdavies): Make a clickable link that auto-uploads the trace file if possible.
    writer.line(format!(
        "Tracing stopped successfully on \"{}\".\nResults written to {}",
        target.nodename.or(target.serial_number).as_deref().unwrap_or("<UNKNOWN>"),
        output_file
    ))?;
    writer.line("Upload to https://ui.perfetto.dev/#!/ to view.")?;
    Ok(())
}

async fn handle_recording_error(
    context: &EnvironmentContext,
    err: RecordingError,
    output: &String,
) -> String {
    let target_spec = get_target_specifier(context).await.unwrap_or(None);
    match err {
        RecordingError::TargetProxyOpen => {
            "Error: ffx trace was unable to connect to trace_manager on the device.

Note that tracing is available for eng and core products, but not user or userdebug.
To fix general connection issues, you could also try:

$ ffx doctor

For a tutorial on getting started with tracing, visit:
https://fuchsia.dev/fuchsia-src/development/sdk/ffx/record-traces"
                .to_owned()
        }
        RecordingError::RecordingAlreadyStarted => {
            // TODO(85098): Also return file info (which output file is being written to).
            format!(
                "Trace already started for target {}",
                target_spec.unwrap_or_else(|| "".to_owned())
            )
        }
        RecordingError::DuplicateTraceFile => {
            // TODO(85098): Also return target info.
            format!("Trace already running for file {}", output)
        }
        RecordingError::RecordingStart => {
            let log_file: String = context.get("log.dir").unwrap();
            format!(
                "Error starting Fuchsia trace. See {}/ffx.daemon.log\n
Search for lines tagged with `ffx_daemon_service_tracing`. A common issue is a
peer closed error from `fuchsia.tracing.controller.Controller`. If this is the
case either tracing is not supported in the product configuration or the tracing
package is missing from the device's system image.",
                log_file
            )
        }
        RecordingError::RecordingStop => {
            let log_file: String = context.get("log.dir").unwrap();
            format!(
                "Error stopping Fuchsia trace. See {}/ffx.daemon.log\n
Search for lines tagged with `ffx_daemon_service_tracing`. A common issue is a
peer closed error from `fuchsia.tracing.controller.Controller`. If this is the
case either tracing is not supported in the product configuration or the tracing
package is missing from the device's system image.",
                log_file
            )
        }
        RecordingError::NoSuchTraceFile => {
            format!("Could not stop trace. No active traces for {}.", output)
        }
        RecordingError::NoSuchTarget => {
            format!(
                "The string '{}' didn't match a trace output file, or any valid targets.",
                target_spec.as_deref().unwrap_or("")
            )
        }
        RecordingError::DisconnectedTarget => {
            format!(
                "The string '{}' didn't match a valid target connected to the ffx daemon.",
                target_spec.as_deref().unwrap_or("")
            )
        }
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
    use ffx_trace_args::{ListCategories, ListProviders, Start, Status, Stop, Symbolize};
    use ffx_writer::{Format, TestBuffers};
    use fidl::endpoints::{ControlHandle, Responder};
    use futures::TryStreamExt;
    use pretty_assertions::assert_eq;
    use serde_json::json;
    use std::matches;
    use tempfile::NamedTempFile;
    use {
        fidl_fuchsia_developer_ffx as ffx, fidl_fuchsia_tracing as tracing,
        fidl_fuchsia_tracing_controller as tracing_controller,
    };

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

    fn generate_stop_result() -> tracing_controller::StopResult {
        let mut stats = tracing_controller::ProviderStats::default();
        stats.name = Some("provider_bar".to_string());
        stats.pid = Some(1234);
        stats.buffering_mode = Some(BufferingMode::Oneshot);
        stats.buffer_wrapped_count = Some(10);
        stats.records_dropped = Some(0);
        stats.percentage_durable_buffer_used = Some(30.0);
        stats.non_durable_bytes_written = Some(40);
        let mut result = tracing_controller::StopResult::default();
        result.provider_stats = Some(vec![stats]);
        return result;
    }

    fn setup_fake_service() -> TracingProxy {
        fho::testing::fake_proxy(|req| match req {
            ffx::TracingRequest::StartRecording { responder, .. } => responder
                .send(Ok(&ffx::TargetInfo {
                    nodename: Some("foo".to_owned()),
                    ..Default::default()
                }))
                .expect("responder err"),
            ffx::TracingRequest::StopRecording { responder, name, .. } => responder
                .send(Ok((
                    &ffx::TargetInfo { nodename: Some("foo".to_owned()), ..Default::default() },
                    &if name.is_empty() { "foo".to_owned() } else { name },
                    &vec!["platypus".to_string(), "beaver".to_string()],
                    &generate_stop_result(),
                )))
                .expect("responder err"),
            ffx::TracingRequest::Status { responder, iterator } => {
                let mut stream = iterator.into_stream();
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

    fn setup_fake_controller_proxy() -> fho::Deferred<ProvisionerProxy> {
        fho::Deferred::from_output(Ok(fho::testing::fake_proxy(|req| match req {
            tracing_controller::ProvisionerRequest::GetKnownCategories { responder, .. } => {
                responder.send(&fake_known_categories()).expect("should respond");
            }
            tracing_controller::ProvisionerRequest::GetProviders { responder, .. } => {
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

    fn setup_closed_fake_controller_proxy() -> fho::Deferred<ProvisionerProxy> {
        fho::Deferred::from_output(Ok(fho::testing::fake_proxy(|req| match req {
            tracing_controller::ProvisionerRequest::GetKnownCategories { responder, .. } => {
                responder.control_handle().shutdown();
            }
            tracing_controller::ProvisionerRequest::GetProviders { responder, .. } => {
                responder.control_handle().shutdown();
            }
            r => panic!("unsupported req: {:?}", r),
        })))
    }

    async fn run_trace_test(ctx: EnvironmentContext, cmd: TraceCommand, writer: Writer) {
        let proxy = setup_fake_service();
        let controller = setup_fake_controller_proxy();
        trace(ctx, proxy, controller, writer, cmd).await.unwrap();
    }

    #[fuchsia::test]
    async fn test_list_categories() {
        let env = ffx_config::test_init().await.unwrap();
        let test_buffers = TestBuffers::default();
        let writer = Writer::new_test(None, &test_buffers);
        run_trace_test(
            env.context.clone(),
            TraceCommand { sub_cmd: TraceSubCommand::ListCategories(ListCategories {}) },
            writer,
        )
        .await;
        let output = test_buffers.into_stdout_str();
        let want = "input  kernel  kernel:arch  kernel:ipc\n\n";
        assert_eq!(want, output);
    }

    #[fuchsia::test]
    async fn test_symbolize_success() {
        let env = ffx_config::test_init().await.unwrap();
        let fake_ir_json = json!({
           "unrelated_key": "unrelated_value",
           "protocol_declarations": [
                {
                    "name": "fake_protocol_name",
                    "methods": [
                        {
                            "ordinal": 12345678,
                            "name": "fake_method_name",
                        },
                    ],
                },
            ],
        });
        let mut temp_file = NamedTempFile::new().expect("Failed to create temp IR file");
        temp_file
            .write_all(fake_ir_json.to_string().as_bytes())
            .expect("Failed to write IR string to file");
        let test_buffers = TestBuffers::default();
        let writer = Writer::new_test(None, &test_buffers);
        let fake_ir_path =
            temp_file.path().to_str().expect("Unable to convert fake IR path to string");
        run_trace_test(
            env.context.clone(),
            TraceCommand {
                sub_cmd: TraceSubCommand::Symbolize(Symbolize {
                    ordinal: Some(12345678),
                    ir_path: vec![fake_ir_path.to_string()],
                    fxt: None,
                    outfile: None,
                }),
            },
            writer,
        )
        .await;
        let output = test_buffers.into_stdout_str();
        let want = "12345678 -> fake_protocol_name.fake_method_name\n";
        assert!(output.contains(want));
    }

    #[fuchsia::test]
    async fn test_symbolize_fail() {
        let env = ffx_config::test_init().await.unwrap();
        let test_buffers = TestBuffers::default();
        let writer = Writer::new_test(None, &test_buffers);
        run_trace_test(
            env.context.clone(),
            TraceCommand {
                sub_cmd: TraceSubCommand::Symbolize(Symbolize {
                    ordinal: Some(12345678),
                    ir_path: vec![],
                    fxt: None,
                    outfile: None,
                }),
            },
            writer,
        )
        .await;
        let output = test_buffers.into_stdout_str();
        let want = "Unable to symbolize ordinal 12345678. This could be because either:\n\
                    1. The ordinal is incorrect\n\
                    2. The ordinal is not found in IR files in $FUCHSIA_BUILD_DIR/all_fidl_json.txt or the input IR files\n";
        assert!(output.contains(want));
    }

    #[fuchsia::test]
    async fn test_list_categories_machine() {
        let env = ffx_config::test_init().await.unwrap();
        let test_buffers = TestBuffers::default();
        let writer = Writer::new_test(Some(Format::Json), &test_buffers);
        run_trace_test(
            env.context.clone(),
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

    #[fuchsia::test]
    async fn test_list_categories_peer_closed() {
        let env = ffx_config::test_init().await.unwrap();
        let test_buffers = TestBuffers::default();
        let writer = Writer::new_test(None, &test_buffers);
        let proxy = setup_fake_service();
        let controller = setup_closed_fake_controller_proxy();
        let cmd = TraceCommand { sub_cmd: TraceSubCommand::ListCategories(ListCategories {}) };
        let res = trace(env.context.clone(), proxy, controller, writer, cmd).await.unwrap_err();
        assert!(res.ffx_error().is_some());
        assert!(res.to_string().contains("This can happen if tracing is not"));
        assert!(test_buffers.into_stdout_str().is_empty());
    }

    #[fuchsia::test]
    async fn test_list_providers() {
        let env = ffx_config::test_init().await.unwrap();
        let test_buffers = TestBuffers::default();
        let writer = Writer::new_test(None, &test_buffers);
        run_trace_test(
            env.context.clone(),
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

    #[fuchsia::test]
    async fn test_list_providers_peer_closed() {
        let env = ffx_config::test_init().await.unwrap();
        let test_buffers = TestBuffers::default();
        let writer = Writer::new_test(None, &test_buffers);
        let proxy = setup_fake_service();
        let controller = setup_closed_fake_controller_proxy();
        let cmd = TraceCommand { sub_cmd: TraceSubCommand::ListProviders(ListProviders {}) };
        let res = trace(env.context.clone(), proxy, controller, writer, cmd).await.unwrap_err();
        assert!(res.ffx_error().is_some());
        assert!(res.to_string().contains("This can happen if tracing is not"));
        assert!(test_buffers.into_stdout_str().is_empty());
    }

    #[fuchsia::test]
    async fn test_list_providers_machine() {
        let env = ffx_config::test_init().await.unwrap();
        let test_buffers = TestBuffers::default();
        let writer = Writer::new_test(Some(Format::Json), &test_buffers);
        run_trace_test(
            env.context.clone(),
            TraceCommand { sub_cmd: TraceSubCommand::ListProviders(ListProviders {}) },
            writer,
        )
        .await;
        let output = test_buffers.into_stdout_str();
        let want = serde_json::to_string(&fake_trace_provider_infos()).unwrap();
        assert_eq!(want, output.trim_end());
    }

    #[fuchsia::test]
    async fn test_start() {
        let env = ffx_config::test_init().await.unwrap();
        let test_buffers = TestBuffers::default();
        let writer = Writer::new_test(None, &test_buffers);
        run_trace_test(
            env.context.clone(),
            TraceCommand {
                sub_cmd: TraceSubCommand::Start(Start {
                    buffer_size: 2,
                    categories: vec!["platypus".to_string(), "beaver".to_string()],
                    duration: None,
                    buffering_mode: tracing::BufferingMode::Oneshot,
                    output: "foo.txt".to_string(),
                    background: true,
                    verbose: false,
                    trigger: vec![],
                    no_symbolize: false,
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

    #[fuchsia::test]
    async fn test_start_with_long_path() {
        let env = ffx_config::test_init().await.unwrap();
        let test_buffers = TestBuffers::default();
        let writer = Writer::new_test(None, &test_buffers);
        run_trace_test(
            env.context.clone(),
            TraceCommand {
                sub_cmd: TraceSubCommand::Start(Start {
                    buffer_size: 2,
                    categories: vec!["platypus".to_string(), "beaver".to_string()],
                    duration: None,
                    buffering_mode: tracing::BufferingMode::Oneshot,
                    output: "long_directory_name_0123456789abcdef_1123456789abcdef_2123456789abcdef_3123456789abcdef_4123456789abcdef_5123456789abcdef_6123456789abcdef_7123456789abcdef_8123456789abcdef_9123456789abcdef_a123456789abcdef_b123456789abcdef_c123456789abcdef_d123456789abcdef_e123456789abcdef_f123456789abcdef/trace.fxt".to_string(),
                    background: true,
                    verbose: false,
                    trigger: vec![],
                    no_symbolize: false,
                }),
            },
            writer,
        )
        .await;
        let output = test_buffers.into_stdout_str();
        // This doesn't find `/.../foo.txt` for the tracing status, since the faked
        // proxy has no state.
        let regex_str = "Tracing started successfully on \"foo\" for categories: \\[ beaver,platypus \\].\nWriting to /([^/]+/)+?trace.fxt
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

    #[fuchsia::test]
    async fn test_status() {
        let env = ffx_config::test_init().await.unwrap();
        let test_buffers = TestBuffers::default();
        let writer = Writer::new_test(None, &test_buffers);
        run_trace_test(
            env.context.clone(),
            TraceCommand { sub_cmd: TraceSubCommand::Status(Status {}) },
            writer,
        )
        .await;
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

    #[fuchsia::test]
    async fn test_stop() {
        let env = ffx_config::test_init().await.unwrap();
        let test_buffers = TestBuffers::default();
        let writer = Writer::new_test(None, &test_buffers);
        run_trace_test(
            env.context.clone(),
            TraceCommand {
                sub_cmd: TraceSubCommand::Stop(Stop {
                    output: Some("foo.txt".to_string()),
                    verbose: false,
                    no_symbolize: false,
                }),
            },
            writer,
        )
        .await;
        let output = test_buffers.into_stdout_str();
        let regex_str = "Tracing stopped successfully on \"foo\".\nResults written to /([^/]+/)+?foo.txt\nUpload to https://ui.perfetto.dev/#!/ to view.";
        let want = Regex::new(regex_str).unwrap();
        assert!(want.is_match(&output), "\"{}\" didn't match regex /{}/", output, regex_str);
    }

    #[fuchsia::test]
    async fn test_stop_with_long_path() {
        let env = ffx_config::test_init().await.unwrap();
        let test_buffers = TestBuffers::default();
        let writer = Writer::new_test(None, &test_buffers);
        run_trace_test(
            env.context.clone(),
            TraceCommand {
                sub_cmd: TraceSubCommand::Stop(Stop {
                    output: Some("long_directory_name_0123456789abcdef_1123456789abcdef_2123456789abcdef_3123456789abcdef_4123456789abcdef_5123456789abcdef_6123456789abcdef_7123456789abcdef_8123456789abcdef_9123456789abcdef_a123456789abcdef_b123456789abcdef_c123456789abcdef_d123456789abcdef_e123456789abcdef_f123456789abcdef/trace.fxt".to_string()),
                    verbose: false,
                    no_symbolize: false,
                }),
            },
            writer,
        )
        .await;
        let output = test_buffers.into_stdout_str();
        let regex_str = "Tracing stopped successfully on \"foo\".\nResults written to /([^/]+/)+?trace.fxt\nUpload to https://ui.perfetto.dev/#!/ to view.";
        let want = Regex::new(regex_str).unwrap();
        assert!(want.is_match(&output), "\"{}\" didn't match regex /{}/", output, regex_str);
    }

    #[fuchsia::test]
    async fn test_start_verbose() {
        let env = ffx_config::test_init().await.unwrap();
        let test_buffers = TestBuffers::default();
        let writer = Writer::new_test(None, &test_buffers);
        run_trace_test(
            env.context.clone(),
            TraceCommand {
                sub_cmd: TraceSubCommand::Start(Start {
                    buffer_size: 2,
                    categories: vec!["platypus".to_string(), "beaver".to_string()],
                    duration: None,
                    buffering_mode: tracing::BufferingMode::Oneshot,
                    output: "foo.txt".to_string(),
                    background: true,
                    verbose: true,
                    trigger: vec![],
                    no_symbolize: false,
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

    #[fuchsia::test]
    async fn test_stop_verbose() {
        let env = ffx_config::test_init().await.unwrap();
        let test_buffers = TestBuffers::default();
        let writer = Writer::new_test(None, &test_buffers);
        run_trace_test(
            env.context.clone(),
            TraceCommand {
                sub_cmd: TraceSubCommand::Stop(Stop {
                    output: Some("foo.txt".to_string()),
                    verbose: true,
                    no_symbolize: false,
                }),
            },
            writer,
        )
        .await;
        let output = test_buffers.into_stdout_str();
        let regex_str = "\"provider_bar\" \\(pid: 1234\\) trace stats\n\
            Buffer wrapped count: 10\n\
            # records dropped: 0\n\
            Durable buffer used: 30.00%\n\
            Bytes written to non-durable buffer: 0x28\n\n\
            Tracing stopped successfully on \"foo\".\n\
            Results written to /([^/]+/)+?foo.txt\n\
            Upload to https://ui.perfetto.dev/#!/ to view.";
        let want = Regex::new(regex_str).unwrap();
        assert!(want.is_match(&output), "\"{}\" didn't match regex /{}/", output, regex_str);
    }

    #[fuchsia::test]
    async fn test_start_with_duration() {
        let env = ffx_config::test_init().await.unwrap();
        let test_buffers = TestBuffers::default();
        let writer = Writer::new_test(None, &test_buffers);
        run_trace_test(
            env.context.clone(),
            TraceCommand {
                sub_cmd: TraceSubCommand::Start(Start {
                    buffer_size: 2,
                    categories: vec![],
                    duration: Some(5.2),
                    buffering_mode: tracing::BufferingMode::Oneshot,
                    output: "foober.fxt".to_owned(),
                    background: true,
                    verbose: false,
                    trigger: vec![],
                    no_symbolize: false,
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

    #[fuchsia::test]
    async fn test_start_with_duration_foreground() {
        let env = ffx_config::test_init().await.unwrap();
        let test_buffers = TestBuffers::default();
        let writer = Writer::new_test(None, &test_buffers);
        run_trace_test(
            env.context.clone(),
            TraceCommand {
                sub_cmd: TraceSubCommand::Start(Start {
                    buffer_size: 2,
                    categories: vec![],
                    duration: Some(0.8),
                    buffering_mode: tracing::BufferingMode::Oneshot,
                    output: "foober.fxt".to_owned(),
                    background: false,
                    verbose: false,
                    trigger: vec![],
                    no_symbolize: false,
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

    #[fuchsia::test]
    async fn test_start_foreground() {
        let env = ffx_config::test_init().await.unwrap();
        let test_buffers = TestBuffers::default();
        let writer = Writer::new_test(None, &test_buffers);
        run_trace_test(
            env.context.clone(),
            TraceCommand {
                sub_cmd: TraceSubCommand::Start(Start {
                    buffer_size: 2,
                    categories: vec![],
                    buffering_mode: tracing::BufferingMode::Oneshot,
                    duration: None,
                    output: "foober.fxt".to_owned(),
                    background: false,
                    verbose: false,
                    trigger: vec![],
                    no_symbolize: false,
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

    #[fuchsia::test]
    async fn test_large_buffer() {
        let env = ffx_config::test_init().await.unwrap();
        let test_buffers = TestBuffers::default();
        let writer = Writer::new_test(None, &test_buffers);
        run_trace_test(
            env.context.clone(),
            TraceCommand {
                sub_cmd: TraceSubCommand::Start(Start {
                    buffer_size: 1024,
                    categories: vec![],
                    buffering_mode: tracing::BufferingMode::Oneshot,
                    duration: None,
                    output: "foober.fxt".to_owned(),
                    background: false,
                    verbose: false,
                    trigger: vec![],
                    no_symbolize: false,
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

    #[fuchsia::test]
    async fn test_get_category_group() {
        let env = ffx_config::test_init().await.unwrap();
        let birds = vec!["chickens", "bald_eagle", "blue-jay", "hawk*", "goose:gosling"];
        env.context
            .query("trace.category_groups.birds")
            .level(Some(ffx_config::ConfigLevel::User))
            .set(json!(birds))
            .await
            .unwrap();
        assert_eq!(birds, get_category_group(&env.context, "birds").await.unwrap());
    }

    #[fuchsia::test]
    async fn test_get_category_group_names() {
        let env = ffx_config::test_init().await.unwrap();
        let birds = vec!["chickens", "ducks"];
        let bees = vec!["honey", "bumble"];
        env.context
            .query("trace.category_groups.birds")
            .level(Some(ffx_config::ConfigLevel::User))
            .set(json!(birds))
            .await
            .unwrap();
        env.context
            .query("trace.category_groups.bees")
            .level(Some(ffx_config::ConfigLevel::User))
            .set(json!(bees))
            .await
            .unwrap();
        env.context
            .query("trace.category_groups.*invalid")
            .level(Some(ffx_config::ConfigLevel::User))
            .set(json!(bees))
            .await
            .unwrap();
        assert!(get_category_group_names(&env.context)
            .await
            .unwrap()
            .contains(&"birds".to_owned()));
        assert!(get_category_group_names(&env.context).await.unwrap().contains(&"bees".to_owned()));
        assert!(get_category_group_names(&env.context)
            .await
            .unwrap()
            .contains(&"*invalid".to_owned()));
    }

    #[fuchsia::test]
    async fn test_get_category_group_not_found() {
        let env = ffx_config::test_init().await.unwrap();
        let err = get_category_group(&env.context, "not_found").await.unwrap_err();
        assert!(
            err.to_string().contains("Error: no category group found for not_found"),
            "the actual value was \"{}\"",
            err.to_string()
        );
    }

    const INVALID_CATEGORIES: &[&str] =
        &["chic*kens", "*turkeys", "golden eagle", "ha,wk*", "goose:gosl\"ing"];

    #[fuchsia::test]
    async fn test_get_category_group_invalid_category() {
        let env = ffx_config::test_init().await.unwrap();
        for invalid_category in INVALID_CATEGORIES {
            env.context
                .query("trace.category_groups.flawed")
                .level(Some(ffx_config::ConfigLevel::User))
                .set(json!(vec![invalid_category]))
                .await
                .unwrap();
            let err = get_category_group(&env.context, "flawed").await.unwrap_err();
            let expected_message = format!("invalid category \"{}\"", invalid_category);
            assert!(
                err.to_string().contains(&expected_message),
                "the actual value was \"{}\"",
                err.to_string()
            );
        }
    }

    #[fuchsia::test]
    async fn test_expand_categories() {
        let env = ffx_config::test_init().await.unwrap();
        let birds = vec!["chickens", "bald_eagle", "hawk*", "goose:gosling", "blue-jay"];
        env.context
            .query("trace.category_groups.birds")
            .level(Some(ffx_config::ConfigLevel::User))
            .set(json!(birds))
            .await
            .unwrap();
        // The result should have all groups expanded, merge duplicate categories, and sort them.
        assert_eq!(
            vec!["*", "bald_eagle", "blue-jay", "chickens", "dove*", "goose:gosling", "hawk*"],
            expand_categories(
                &env.context,
                vec![
                    "dove*".to_string(),
                    "bald_eagle".to_string(),
                    "#birds".to_string(),
                    "*".to_string()
                ]
            )
            .await
            .unwrap()
        );
    }

    #[fuchsia::test]
    async fn test_expand_categories_invalid() {
        let env = ffx_config::test_init().await.unwrap();
        for invalid_category in INVALID_CATEGORIES {
            let err = expand_categories(&env.context, vec![invalid_category.to_string()])
                .await
                .unwrap_err();
            let expected_message = format!("category \"{}\" is invalid", invalid_category);
            assert!(
                err.to_string().contains(&expected_message),
                "the actual value was \"{}\"",
                err.to_string()
            );
        }
    }

    #[fuchsia::test]
    async fn test_curated_category_groups_valid() {
        let env = ffx_config::test_init().await.unwrap();

        // Get all of the category groups found in config.json
        let category_groups_json: serde_json::Value =
            env.context.get("trace.category_groups").unwrap();

        for category_group_name in category_groups_json.as_object().unwrap().keys() {
            let category_group =
                get_category_group(&env.context, category_group_name).await.unwrap();
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

    #[test]
    fn test_stats_to_print() {
        // Verbose output with dropped records
        let mut stats = tracing_controller::ProviderStats::default();
        stats.name = Some("provider_foo".to_string());
        stats.pid = Some(1234);
        stats.buffering_mode = Some(BufferingMode::Oneshot);
        stats.buffer_wrapped_count = Some(10);
        stats.records_dropped = Some(10);
        stats.percentage_durable_buffer_used = Some(30.0);
        stats.non_durable_bytes_written = Some(40);
        let mut expected_output = vec![
            "WARNING: \"provider_foo\" dropped 10 records!",
            "\"provider_foo\" (pid: 1234) trace stats",
            "Buffer wrapped count: 10",
            "# records dropped: 10",
            "Durable buffer used: 30.00%",
            "Bytes written to non-durable buffer: 0x28\n",
        ];

        let mut actual_output = stats_to_print(stats.clone(), true);
        assert_eq!(expected_output, actual_output);

        // Verify that dropped records warning is printed even if not verbose
        expected_output = vec!["WARNING: \"provider_foo\" dropped 10 records!"];
        actual_output = stats_to_print(stats.clone(), false);
        assert_eq!(expected_output, actual_output);

        // Verbose output with missing stats
        stats.buffer_wrapped_count = None;
        expected_output = vec!["A provider returned stats with missing values"];
        actual_output = stats_to_print(stats.clone(), true);
        assert_eq!(expected_output, actual_output);

        // No output on missing stats if not verbose
        expected_output = vec![];
        actual_output = stats_to_print(stats.clone(), false);
        assert_eq!(expected_output, actual_output);
    }

    #[fuchsia::test]
    async fn test_handle_recording_error() {
        let env = ffx_config::test_init().await.unwrap();
        let context = &env.context;
        let output_file = "foo_bar_bazzle_wazzle.fxt";
        let log_dir = "important_log_file.log";
        let target = "fuchsia-device";
        context
            .query("log.dir")
            .level(Some(ffx_config::ConfigLevel::User))
            .set(log_dir.into())
            .await
            .unwrap();
        context
            .query(ffx_config::keys::TARGET_DEFAULT_KEY)
            .level(Some(ffx_config::ConfigLevel::User))
            .set(target.into())
            .await
            .unwrap();

        struct Test {
            error: RecordingError,
            matches: Vec<&'static str>,
        }

        // Avoid being overly prescriptive about the actual contents of the errors. Just make sure
        // the basics are included and the thing we care about is inside.
        use RecordingError::*;
        let tests = vec![
            Test { error: TargetProxyOpen, matches: vec!["unable to connect", "ffx doctor"] },
            Test { error: RecordingAlreadyStarted, matches: vec!["already", target] },
            Test { error: DuplicateTraceFile, matches: vec!["already", output_file] },
            Test { error: RecordingStart, matches: vec![log_dir, "starting"] },
            Test { error: RecordingStop, matches: vec![log_dir, "stopping"] },
            Test { error: NoSuchTraceFile, matches: vec!["stop trace", output_file] },
            Test { error: NoSuchTarget, matches: vec![target] },
            Test { error: DisconnectedTarget, matches: vec![target] },
        ];

        for test in tests.into_iter() {
            let error_string = format!("{:?}", test.error);
            let result =
                handle_recording_error(&context, test.error, &output_file.to_owned()).await;
            for matching_string in test.matches.into_iter() {
                assert!(
                    result.contains(matching_string),
                    "Unable to find string '{}' when handling error '{}'. Error string: \"{}\"",
                    matching_string,
                    error_string,
                    result
                );
            }
        }
    }
}
