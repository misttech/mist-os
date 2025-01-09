// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::target_formatter::{JsonTarget, JsonTargetFormatter, TargetFormatter};
use anyhow::Result;
use async_trait::async_trait;
use errors::{ffx_bail, ffx_bail_with_code};
use ffx_config::EnvironmentContext;
use ffx_list_args::{AddressTypes, ListCommand};
use ffx_target::{KnockError, TargetInfoQuery};
use fho::{daemon_protocol, deferred, Deferred, FfxMain, FfxTool, ToolIO, VerifiedMachineWriter};
use fidl_fuchsia_developer_ffx as ffx;
use fuchsia_async::TimeoutExt;
use futures::{StreamExt, TryStreamExt};
use std::time::Duration;

mod target_formatter;

fn address_types_from_cmd(cmd: &ListCommand) -> AddressTypes {
    if cmd.no_ipv4 && cmd.no_ipv6 {
        AddressTypes::None
    } else if cmd.no_ipv4 {
        AddressTypes::Ipv6Only
    } else if cmd.no_ipv6 {
        AddressTypes::Ipv4Only
    } else {
        AddressTypes::All
    }
}

#[derive(FfxTool)]
#[no_target]
pub struct ListTool {
    #[command]
    cmd: ListCommand,
    #[with(deferred(daemon_protocol()))]
    tc_proxy: Deferred<ffx::TargetCollectionProxy>,
    context: EnvironmentContext,
}

fho::embedded_plugin!(ListTool);

#[async_trait(?Send)]
impl FfxMain for ListTool {
    type Writer = VerifiedMachineWriter<Vec<JsonTarget>>;
    async fn main(self, mut writer: Self::Writer) -> fho::Result<()> {
        // XXX Shouldn't check `is_strict()`. Eventually we'll _always_ do local discovery,
        // at which point this check goes away.
        let infos =
            if !self.context.is_strict() && ffx_target::is_discovery_enabled(&self.context).await {
                list_targets(self.tc_proxy.await?, &self.cmd).await?
            } else {
                local_list_targets(&self.context, &self.cmd).await?
            };
        show_targets(self.cmd, infos, &mut writer, &self.context).await?;
        Ok(())
    }
}

async fn show_targets(
    cmd: ListCommand,
    mut infos: Vec<ffx::TargetInfo>,
    writer: &mut VerifiedMachineWriter<Vec<JsonTarget>>,
    context: &EnvironmentContext,
) -> Result<()> {
    // Provide stable output. Use "unstable" since we don't care about the original ordering.
    infos.sort_unstable_by(|a, b| a.nodename.cmp(&b.nodename));
    match infos.len() {
        0 => {
            // Printed to stderr, so that if a user is parsing output, say from a formatted
            // output, that the message is not consumed. A stronger future strategy would
            // have richer behavior dependent upon whether the user has a controlling
            // terminal, which would require passing in more and richer IO delegates.
            if let Some(n) = cmd.nodename {
                ffx_bail_with_code!(2, "Device {} not found.", n);
            } else {
                if !writer.is_machine() {
                    writeln!(writer.stderr(), "No devices found.")?;
                } else {
                    writer.machine(&Vec::new())?;
                }
            }
        }
        _ => {
            let address_types = address_types_from_cmd(&cmd);
            if let AddressTypes::None = address_types {
                ffx_bail!("Invalid arguments, cannot specify both --no_ipv4 and --no_ipv6")
            }
            if writer.is_machine() {
                let res = target_formatter::filter_targets_by_address_types(infos, address_types);
                let mut formatter = JsonTargetFormatter::try_from(res)?;
                let default: Option<String> = ffx_target::get_target_specifier(&context).await?;
                JsonTargetFormatter::set_default_target(&mut formatter.targets, default.as_deref());
                writer.machine(&formatter.targets)?;
            } else {
                let formatter =
                    Box::<dyn TargetFormatter>::try_from((cmd.format, address_types, infos))?;
                let default: Option<String> = ffx_target::get_target_specifier(&context).await?;
                writer.line(formatter.lines(default.as_deref()).join("\n"))?;
            }
        }
    }
    Ok(())
}

const DEFAULT_SSH_TIMEOUT_MS: u64 = 10000;

async fn try_get_target_info(
    spec: String,
    context: &EnvironmentContext,
) -> Result<(ffx::RemoteControlState, Option<String>, Option<String>), KnockError> {
    let mut resolution = ffx_target::resolve_target_address(&Some(spec), context)
        .await
        .map_err(|e| KnockError::CriticalError(e.into()))?;
    let (rcs_state, pc, bc) = match resolution.identify(context).await {
        Ok(id_result) => (
            ffx::RemoteControlState::Up,
            id_result.product_config.clone(),
            id_result.board_config.clone(),
        ),
        _ => (ffx::RemoteControlState::Down, None, None),
    };
    Ok((rcs_state, pc, bc))
}

#[tracing::instrument]
async fn get_target_info(
    context: &EnvironmentContext,
    addrs: &[addr::TargetAddr],
) -> Result<(ffx::RemoteControlState, Option<String>, Option<String>)> {
    let ssh_timeout: u64 =
        ffx_config::get("target.host_pipe_ssh_timeout").unwrap_or(DEFAULT_SSH_TIMEOUT_MS);
    let ssh_timeout = Duration::from_millis(ssh_timeout);
    for addr in addrs {
        // An address is, conveniently, a valid target spec as well
        let spec =
            if addr.port() == 0 { format!("{addr}") } else { format!("{addr}:{}", addr.port()) };
        tracing::debug!("Trying to make a connection to spec {spec:?}");

        match try_get_target_info(spec, context)
            .on_timeout(ssh_timeout, || {
                Err(KnockError::NonCriticalError(anyhow::anyhow!("knock_rcs() timed out")))
            })
            .await
        {
            Ok(res) => {
                return Ok(res);
            }
            Err(KnockError::NonCriticalError(e)) => {
                tracing::debug!("Could not connect to {addr:?}: {e:?}");
                continue;
            }
            e => {
                tracing::debug!("Got error {e:?} when trying to connect to {addr:?}");
                return Ok((ffx::RemoteControlState::Unknown, None, None));
            }
        }
    }
    Ok((ffx::RemoteControlState::Down, None, None))
}

async fn handle_res_to_info(
    context: &EnvironmentContext,
    handle: Result<discovery::TargetHandle>,
    connect_to_target: bool,
) -> Result<ffx::TargetInfo> {
    match handle {
        Ok(h) => handle_to_info(context, h, connect_to_target).await,
        Err(e) => async { Err(e) }.await,
    }
}

async fn handle_to_info(
    context: &EnvironmentContext,
    handle: discovery::TargetHandle,
    connect_to_target: bool,
) -> Result<ffx::TargetInfo> {
    let (target_state, addresses) = match handle.state {
        discovery::TargetState::Unknown => (ffx::TargetState::Unknown, None),
        discovery::TargetState::Product(target_addrs) => {
            (ffx::TargetState::Product, Some(target_addrs))
        }
        discovery::TargetState::Fastboot(fts) => {
            let addresses = match fts.connection_state {
                discovery::FastbootConnectionState::Usb => Some(vec![]),
                discovery::FastbootConnectionState::Tcp(addresses) => Some(addresses.to_vec()),
                discovery::FastbootConnectionState::Udp(addresses) => Some(addresses.to_vec()),
            };
            (ffx::TargetState::Fastboot, addresses)
        }
        discovery::TargetState::Zedboot => (ffx::TargetState::Zedboot, None),
    };
    let (rcs_state, product_config, board_config) = if connect_to_target {
        if let Some(ref target_addrs) = addresses {
            get_target_info(context, target_addrs).await?
        } else {
            (ffx::RemoteControlState::Unknown, None, None)
        }
    } else {
        (ffx::RemoteControlState::Unknown, None, None)
    };
    let addresses =
        addresses.map(|ta| ta.into_iter().map(|x| x.into()).collect::<Vec<ffx::TargetAddrInfo>>());
    Ok(ffx::TargetInfo {
        nodename: handle.node_name,
        addresses,
        rcs_state: Some(rcs_state),
        target_state: Some(target_state),
        board_config,
        product_config,
        ..Default::default()
    })
}

async fn do_connect_to_target(ctx: &EnvironmentContext, cmd: &ListCommand) -> bool {
    // Should we probe discovered targets by default?
    if cmd.no_probe {
        // It'd be nice to use the FfxConfigBacked functionality, but that only works with Option arguments
        false
    } else {
        // XXX Shouldn't check `is_strict()`. Eventually we'll fix b/340330010,
        // at which point this check goes away.
        // TODO(b/340330010) Change the default to "true" when we are ready to roll this out to everyone
        ctx.is_strict() || ctx.get("ffx.target-list.local-connect").unwrap_or(false)
    }
}

async fn local_list_targets(
    ctx: &EnvironmentContext,
    cmd: &ListCommand,
) -> Result<Vec<ffx::TargetInfo>> {
    let connect = do_connect_to_target(ctx, cmd).await;
    let stream = get_handle_stream(cmd, ctx).await?;
    let targets = handles_to_infos(stream, ctx, connect).await?;
    Ok(targets)
}

async fn handles_to_infos(
    stream: impl futures::Stream<Item = Result<discovery::TargetHandle>>,
    ctx: &EnvironmentContext,
    connect: bool,
) -> Result<Vec<fidl_fuchsia_developer_ffx::TargetInfo>> {
    let info_futures = stream.then(|t| handle_res_to_info(ctx, t, connect));
    let infos: Vec<Result<ffx::TargetInfo>> = info_futures.collect().await;
    let targets = infos.into_iter().collect::<Result<Vec<ffx::TargetInfo>>>()?;
    Ok(targets)
}

async fn get_handle_stream(
    cmd: &ListCommand,
    ctx: &EnvironmentContext,
) -> Result<impl futures::Stream<Item = Result<discovery::TargetHandle>>> {
    let name = cmd.nodename.clone();
    let query = TargetInfoQuery::from(name);
    let stream = ffx_target::get_discovery_stream(query, !cmd.no_usb, !cmd.no_mdns, ctx).await?;
    Ok(stream)
}

async fn list_targets(
    tc_proxy: ffx::TargetCollectionProxy,
    cmd: &ListCommand,
) -> Result<Vec<ffx::TargetInfo>> {
    let (reader, server) = fidl::endpoints::create_endpoints::<ffx::TargetCollectionReaderMarker>();

    tc_proxy.list_targets(
        &ffx::TargetQuery { string_matcher: cmd.nodename.clone(), ..Default::default() },
        reader,
    )?;
    let mut res = Vec::new();
    let mut stream = server.into_stream();
    while let Ok(Some(ffx::TargetCollectionReaderRequest::Next { entry, responder })) =
        stream.try_next().await
    {
        responder.send()?;
        if entry.len() > 0 {
            res.extend(entry);
        } else {
            break;
        }
    }

    Ok(res)
}

///////////////////////////////////////////////////////////////////////////////
// tests

#[cfg(test)]
mod test {
    use super::*;
    use addr::TargetAddr;
    use ffx_list_args::Format;
    use ffx_writer::TestBuffers;
    use fidl_fuchsia_developer_ffx as ffx;
    use fidl_fuchsia_developer_ffx::{TargetInfo as FidlTargetInfo, TargetState};
    use regex::Regex;
    use std::net::IpAddr;

    fn tab_list_cmd(nodename: Option<String>) -> ListCommand {
        ListCommand { nodename, format: Format::Tabular, ..Default::default() }
    }

    fn to_fidl_target(nodename: String) -> FidlTargetInfo {
        let addr: TargetAddr = TargetAddr::new(
            IpAddr::from([0xfe80, 0x0, 0x0, 0x0, 0xdead, 0xbeef, 0xbeef, 0xbeef]),
            3,
            0,
        );
        FidlTargetInfo {
            nodename: Some(nodename),
            addresses: Some(vec![addr.into()]),
            age_ms: Some(101),
            rcs_state: Some(ffx::RemoteControlState::Up),
            target_state: Some(TargetState::Unknown),
            ..Default::default()
        }
    }

    fn setup_fake_target_collection_server(num_tests: usize) -> ffx::TargetCollectionProxy {
        fho::testing::fake_proxy(move |req| match req {
            ffx::TargetCollectionRequest::ListTargets { query, reader, .. } => {
                let reader = reader.into_proxy();
                let fidl_values: Vec<FidlTargetInfo> =
                    if query.string_matcher.as_deref().map(|s| s.is_empty()).unwrap_or(true) {
                        (0..num_tests)
                            .map(|i| format!("Test {}", i))
                            .map(|name| to_fidl_target(name))
                            .collect()
                    } else {
                        let v = query.string_matcher.unwrap();
                        (0..num_tests)
                            .map(|i| format!("Test {}", i))
                            .filter(|t| *t == v)
                            .map(|name| to_fidl_target(name))
                            .collect()
                    };
                fuchsia_async::Task::local(async move {
                    let mut iter = fidl_values.chunks(10);
                    loop {
                        let chunk = iter.next().unwrap_or(&[]);
                        reader.next(&chunk).await.unwrap();
                        if chunk.is_empty() {
                            break;
                        }
                    }
                })
                .detach();
            }
            r => panic!("unexpected request: {:?}", r),
        })
    }

    async fn try_run_list_test(
        num_tests: usize,
        cmd: ListCommand,
        context: &EnvironmentContext,
    ) -> Result<String> {
        let proxy = setup_fake_target_collection_server(num_tests);
        let test_buffers = TestBuffers::default();
        let mut writer = VerifiedMachineWriter::new_test(None, &test_buffers);
        let infos = list_targets(proxy, &cmd).await?;
        show_targets(cmd, infos, &mut writer, context).await?;
        Ok(test_buffers.into_stdout_str())
    }

    async fn run_list_test(
        num_tests: usize,
        cmd: ListCommand,
        context: &EnvironmentContext,
    ) -> String {
        try_run_list_test(num_tests, cmd, context).await.unwrap()
    }

    #[fuchsia::test]
    async fn test_machine_schema() {
        let env = ffx_config::test_init().await.unwrap();
        let proxy = setup_fake_target_collection_server(3);
        let test_buffers = TestBuffers::default();
        let mut writer = VerifiedMachineWriter::new_test(Some(fho::Format::Json), &test_buffers);
        let cmd = ListCommand { format: Format::Tabular, ..Default::default() };
        let infos = list_targets(proxy, &cmd).await.expect("list targets");
        show_targets(cmd, infos, &mut writer, &env.context).await.expect("show_targets");
        let data_str = test_buffers.into_stdout_str();
        let data = serde_json::from_str(&data_str).expect("json value");
        match VerifiedMachineWriter::<Vec<JsonTarget>>::verify_schema(&data) {
            Ok(_) => (),
            Err(e) => {
                println!("Error verifying schema: {e}");
                println!("{data:?}");
            }
        };
    }

    #[fuchsia::test]
    async fn test_list_with_no_devices_and_no_nodename() -> Result<()> {
        let env = ffx_config::test_init().await.unwrap();
        let output = run_list_test(0, tab_list_cmd(None), &env.context).await;
        assert_eq!("".to_string(), output);
        Ok(())
    }

    #[fuchsia::test]
    async fn test_list_with_one_device_and_no_nodename() -> Result<()> {
        let env = ffx_config::test_init().await.unwrap();
        let output = run_list_test(1, tab_list_cmd(None), &env.context).await;
        let value = format!("Test {}", 0);
        let node_listing = Regex::new(&value).expect("test regex");
        assert_eq!(
            1,
            node_listing.find_iter(&output).count(),
            "could not find \"{}\" nodename in output:\n{}",
            value,
            output
        );
        Ok(())
    }

    #[fuchsia::test]
    async fn test_list_with_multiple_devices_and_no_nodename() -> Result<()> {
        let env = ffx_config::test_init().await.unwrap();
        let num_tests = 10;
        let output = run_list_test(num_tests, tab_list_cmd(None), &env.context).await;
        for x in 0..num_tests {
            let value = format!("Test {}", x);
            let node_listing = Regex::new(&value).expect("test regex");
            assert_eq!(
                1,
                node_listing.find_iter(&output).count(),
                "could not find \"{}\" nodename in output:\n{}",
                value,
                output
            );
        }
        Ok(())
    }

    #[fuchsia::test]
    async fn test_list_with_one_device_and_matching_nodename() -> Result<()> {
        let env = ffx_config::test_init().await.unwrap();
        let output = run_list_test(1, tab_list_cmd(Some("Test 0".to_string())), &env.context).await;
        let value = format!("Test {}", 0);
        let node_listing = Regex::new(&value).expect("test regex");
        assert_eq!(
            1,
            node_listing.find_iter(&output).count(),
            "could not find \"{}\" nodename in output:\n{}",
            value,
            output
        );
        Ok(())
    }

    #[fuchsia::test]
    async fn test_list_with_one_device_and_not_matching_nodename() -> Result<()> {
        let env = ffx_config::test_init().await.unwrap();
        let output =
            try_run_list_test(1, tab_list_cmd(Some("blarg".to_string())), &env.context).await;
        assert!(output.is_err());
        Ok(())
    }

    #[fuchsia::test]
    async fn test_list_with_multiple_devices_and_not_matching_nodename() -> Result<()> {
        let env = ffx_config::test_init().await.unwrap();
        let num_tests = 25;
        let output =
            try_run_list_test(num_tests, tab_list_cmd(Some("blarg".to_string())), &env.context)
                .await;
        assert!(output.is_err());
        Ok(())
    }

    #[fuchsia::test]
    async fn test_list_with_multiple_devices_and_matching_nodename() -> Result<()> {
        let env = ffx_config::test_init().await.unwrap();
        let output =
            run_list_test(25, tab_list_cmd(Some("Test 19".to_string())), &env.context).await;
        let value = format!("Test {}", 0);
        let node_listing = Regex::new(&value).expect("test regex");
        assert_eq!(0, node_listing.find_iter(&output).count());
        let value = format!("Test {}", 19);
        let node_listing = Regex::new(&value).expect("test regex");
        assert_eq!(1, node_listing.find_iter(&output).count());
        Ok(())
    }

    #[fuchsia::test]
    async fn test_list_with_address_types_none() -> Result<()> {
        let env = ffx_config::test_init().await.unwrap();
        let num_tests = 25;
        let cmd_none = ListCommand { no_ipv4: true, no_ipv6: true, ..Default::default() };
        let output = try_run_list_test(num_tests, cmd_none, &env.context).await;
        assert!(output.is_err());
        Ok(())
    }

    #[test]
    fn test_address_types_from_cmd() -> Result<()> {
        let cmd_none = ListCommand { no_ipv4: true, no_ipv6: true, ..Default::default() };
        assert_eq!(address_types_from_cmd(&cmd_none), AddressTypes::None);
        let cmd_ipv4_only = ListCommand { no_ipv4: false, no_ipv6: true, ..Default::default() };
        assert_eq!(address_types_from_cmd(&cmd_ipv4_only), AddressTypes::Ipv4Only);
        let cmd_ipv6_only = ListCommand { no_ipv4: true, no_ipv6: false, ..Default::default() };
        assert_eq!(address_types_from_cmd(&cmd_ipv6_only), AddressTypes::Ipv6Only);
        let cmd_all = ListCommand { no_ipv4: false, no_ipv6: false, ..Default::default() };
        assert_eq!(address_types_from_cmd(&cmd_all), AddressTypes::All);
        let cmd_all_default = ListCommand::default();
        assert_eq!(address_types_from_cmd(&cmd_all_default), AddressTypes::All);
        Ok(())
    }

    #[fuchsia::test]
    async fn test_connect_logic() -> Result<()> {
        let env = ffx_config::test_init().await.unwrap();
        // Default value is false (see b/340330010)
        assert_eq!(false, do_connect_to_target(&env.context, &ListCommand::default()).await);
        // When --no-probe -s specified, false
        let cmd = ListCommand { no_probe: true, ..Default::default() };
        assert_eq!(false, do_connect_to_target(&env.context, &cmd).await);
        // If ffx.target-list.local-connect is specified, use it
        env.context
            .query("ffx.target-list.local-connect")
            .level(Some(ffx_config::ConfigLevel::User))
            .set(true.into())
            .await?;
        assert_eq!(true, do_connect_to_target(&env.context, &ListCommand::default()).await);

        Ok(())
    }

    #[fuchsia::test]
    async fn test_sorted_output() -> Result<()> {
        let env = ffx_config::test_init().await.unwrap();
        let cmd = ListCommand::default();
        let test_buffers = TestBuffers::default();
        let mut writer = VerifiedMachineWriter::new_test(None, &test_buffers);
        let ti1 = ffx::TargetInfo {
            nodename: Some(String::from("z")),
            addresses: Some(vec![]),
            rcs_state: Some(ffx::RemoteControlState::Unknown),
            target_state: Some(ffx::TargetState::Unknown),
            ..Default::default()
        };
        let ti2 = ffx::TargetInfo { nodename: Some(String::from("a")), ..ti1.clone() };
        let infos = vec![ti1, ti2];
        show_targets(cmd, infos, &mut writer, &env.context).await?;
        let out: Vec<String> =
            test_buffers.into_stdout_str().lines().map(|s| s.to_string()).collect();
        // Line 0 is the header
        assert!(out[1].starts_with("a"));
        assert!(out[2].starts_with("z"));
        Ok(())
    }

    #[fuchsia::test]
    async fn test_serial_addresses() {
        // USB targets should have an empty list of addresses, not None
        let env = ffx_config::test_init().await.unwrap();
        let handle = Ok(discovery::TargetHandle {
            node_name: Some("nodename".to_string()),
            state: discovery::TargetState::Fastboot(discovery::FastbootTargetState {
                serial_number: "12345678".to_string(),
                connection_state: discovery::FastbootConnectionState::Usb,
            }),
        });
        let stream = futures::stream::once(async { handle });
        let targets = handles_to_infos(stream, &env.context, true).await;
        let targets = targets.unwrap();
        assert_ne!(targets[0].addresses, None);
    }
}
