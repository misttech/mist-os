// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::show::TargetData;
use addr::TargetAddr;
use anyhow::{anyhow, bail, Result};
use async_lock::Mutex;
use async_trait::async_trait;
use fho::{
    deferred, moniker, Deferred, DirectConnector, FfxMain, FfxTool, ToolIO, VerifiedMachineWriter,
};
use fidl_fuchsia_buildinfo::ProviderProxy;
use fidl_fuchsia_developer_ffx::{TargetAddrInfo, TargetProxy};
use fidl_fuchsia_developer_remotecontrol::RemoteControlProxy;
use fidl_fuchsia_feedback::{DeviceIdProviderProxy, LastRebootInfoProviderProxy};
use fidl_fuchsia_hwinfo::{Architecture, BoardProxy, DeviceProxy, ProductProxy};
use fidl_fuchsia_update_channelcontrol::ChannelControlProxy;
use show::{
    AddressData, BoardData, BuildData, DeviceData, ProductData, TargetShowInfo, UpdateData,
};
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;
use timeout::timeout;
use {ffx_target, ffx_target_show_args as args};

mod show;

#[derive(FfxTool)]
pub struct ShowTool {
    #[command]
    cmd: args::TargetShow,
    rcs_proxy: RemoteControlProxy,
    connector: Option<Arc<dyn DirectConnector>>, // Returns Some(dc) only if we have a direct connection
    target_proxy: Deferred<TargetProxy>,
    #[with(moniker("/core/system-update"))]
    channel_control_proxy: ChannelControlProxy,
    #[with(moniker("/core/hwinfo"))]
    board_proxy: BoardProxy,
    #[with(moniker("/core/hwinfo"))]
    device_proxy: DeviceProxy,
    #[with(moniker("/core/hwinfo"))]
    product_proxy: ProductProxy,
    #[with(moniker("/core/build-info"))]
    build_info_proxy: ProviderProxy,
    #[with(deferred(moniker("/core/feedback_id")))]
    device_id_proxy: Deferred<DeviceIdProviderProxy>,
    #[with(moniker("/core/feedback"))]
    last_reboot_info_proxy: LastRebootInfoProviderProxy,
}

fho::embedded_plugin!(ShowTool);

#[async_trait(?Send)]
impl FfxMain for ShowTool {
    type Writer = VerifiedMachineWriter<TargetShowInfo>;
    /// Main entry point for the `show` subcommand.
    async fn main(self, mut writer: Self::Writer) -> fho::Result<()> {
        self.show_cmd(&mut writer).await.map_err(|e| e.into())
    }
}

impl ShowTool {
    async fn show_cmd(self, writer: &mut <ShowTool as fho::FfxMain>::Writer) -> Result<()> {
        // To add more show information, add a `gather_*_show(*) call to this
        // list, as well as the labels in the Ok() and vec![] just below.
        let show = match futures::try_join!(
            gather_target_show(
                self.rcs_proxy,
                self.connector,
                self.target_proxy,
                self.last_reboot_info_proxy
            ),
            gather_board_show(self.board_proxy),
            gather_device_show(self.device_proxy, self.device_id_proxy),
            gather_product_show(self.product_proxy),
            gather_update_show(self.channel_control_proxy),
            gather_build_info_show(self.build_info_proxy),
        ) {
            Ok((target, board, device, product, update, build)) => {
                TargetShowInfo { target, board, device, product, update, build }
            }
            Err(e) => bail!(e),
        };
        if writer.is_machine() {
            writer.machine(&show)?;
        } else {
            show::output_for_human(&show, &self.cmd, writer)?;
        }
        Ok(())
    }
}

async fn gather_target_info_direct(
    connection: Arc<Mutex<Option<ffx_target::Connection>>>,
) -> Result<(AddressData, Option<fidl_fuchsia_developer_ffx::CompatibilityInfo>)> {
    let conn = connection.lock().await;
    let conn = conn.as_ref().ok_or_else(|| anyhow!("No connection available"))?;
    // If we've gotten a connection, we must have an address we connected to
    let addr = conn.device_address().expect("No address in connection?");
    let ad = AddressData { host: format!("{}", addr.ip()), port: addr.port() };
    Ok((ad, conn.compatibility_info().map(|ci| ci.into())))
}

async fn gather_target_info_from_daemon(
    target_proxy: TargetProxy,
) -> Result<(AddressData, Option<fidl_fuchsia_developer_ffx::CompatibilityInfo>)> {
    let addr_info = timeout(Duration::from_secs(1), target_proxy.get_ssh_address())
        .await?
        .map_err(|e| anyhow!("Failed to get ssh address: {:?}", e))?;
    let addr = TargetAddr::from(&addr_info);
    let port = match addr_info {
        TargetAddrInfo::Ip(_info) => 22,
        TargetAddrInfo::IpPort(info) => info.port,
    };
    let ssh_address = match addr.ip() {
        IpAddr::V4(ip) => AddressData { host: ip.to_string(), port },
        IpAddr::V6(ip) => AddressData {
            host: format!(
                "[{ip}{}]",
                if addr.scope_id() != 0 { format!("%{}", addr.scope_id()) } else { "".into() }
            ),
            port,
        },
    };
    let host = target_proxy.identity().await?;
    Ok((ssh_address, host.compatibility))
}

/// Determine target information.
async fn gather_target_show(
    rcs_proxy: RemoteControlProxy,
    connector: Option<Arc<dyn DirectConnector>>,
    target_proxy: Deferred<TargetProxy>,
    last_reboot_info_proxy: LastRebootInfoProviderProxy,
) -> Result<TargetData> {
    let host = rcs_proxy
        .identify_host()
        .await?
        .map_err(|e| anyhow!("Failed to identify host: {:?}", e))?;
    let name = host.nodename;
    let (ssh_address, compat) = if let Some(dc) = connector {
        gather_target_info_direct(dc.connection().await?).await?
    } else {
        gather_target_info_from_daemon(target_proxy.await?).await?
    };
    let (compatibility_state, compatibility_message) = match compat {
        Some(compatibility) => {
            (compat_info::CompatibilityState::from(compatibility.state), compatibility.message)
        }
        None => (
            compat_info::CompatibilityState::Absent,
            "Compatibility information is not available".to_string(),
        ),
    };

    let info = last_reboot_info_proxy.get().await?;

    Ok(TargetData {
        name: name.unwrap_or_else(|| "".into()),
        ssh_address,
        compatibility_state,
        compatibility_message,
        last_reboot_graceful: info.graceful.unwrap_or(false),
        last_reboot_reason: info.reason.map(|r| format!("{r:?}")),
        uptime_nanos: info.uptime.unwrap_or(-1),
    })
}

/// Determine the build info for the target.
async fn gather_build_info_show(build: ProviderProxy) -> Result<BuildData> {
    let info = build.get_build_info().await?;

    Ok(BuildData {
        version: info.version,
        product: info.product_config,
        board: info.board_config,
        commit: info.latest_commit_date,
    })
}

fn arch_to_string(arch: Option<Architecture>) -> Option<String> {
    match arch {
        Some(Architecture::X64) => Some("x64".to_string()),
        Some(Architecture::Arm64) => Some("arm64".to_string()),
        _ => None,
    }
}

/// Determine the device info for the device.
async fn gather_board_show(board: BoardProxy) -> Result<BoardData> {
    let info = board.get_info().await?;
    Ok(BoardData {
        name: info.name,
        revision: info.revision,
        instruction_set: arch_to_string(info.cpu_architecture),
    })
}

/// Determine the device info for the device.
async fn gather_device_show(
    device: DeviceProxy,
    device_id_proxy: Deferred<DeviceIdProviderProxy>,
) -> Result<DeviceData> {
    let info = device.get_info().await?;
    let mut device = DeviceData {
        serial_number: info.serial_number,
        retail_sku: info.retail_sku,
        retail_demo: info.is_retail_demo,
        device_id: None,
    };
    match device_id_proxy.await {
        Ok(device_id) => {
            let id_info = device_id.get_id().await?;
            device.device_id = Some(id_info)
        }
        Err(e) => {
            tracing::warn!("Error getting device id proxy: {e}");
            device.device_id = None;
        }
    };
    Ok(device)
}

/// Determine the product info for the device.
async fn gather_product_show(product: ProductProxy) -> Result<ProductData> {
    let info = product.get_info().await?;

    Ok(ProductData {
        audio_amplifier: info.audio_amplifier,
        build_date: info.build_date,
        build_name: info.build_name,
        colorway: info.colorway,
        display: info.display,
        emmc_storage: info.emmc_storage,
        language: info.language,
        regulatory_domain: info.regulatory_domain.map(|d| d.country_code.unwrap_or_default()),
        locale_list: info
            .locale_list
            .map(|l| l.iter().map(|ll| ll.id.to_string()).collect())
            .unwrap_or(vec![]),
        manufacturer: info.manufacturer,
        microphone: info.microphone,
        model: info.model,
        name: info.name,
        nand_storage: info.nand_storage,
        memory: info.memory,
        sku: info.sku,
    })
}

/// Determine the update show of the device, including update channels.
async fn gather_update_show(channel_control: ChannelControlProxy) -> Result<UpdateData> {
    let current_channel = channel_control.get_current().await?;
    let next_channel = channel_control.get_target().await?;

    Ok(UpdateData { current_channel, next_channel })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ffx_target::FidlPipe;
    use fho::{Format, TestBuffers};
    use fidl_fuchsia_buildinfo::{BuildInfo, ProviderRequest};
    use fidl_fuchsia_developer_ffx::{TargetInfo, TargetIp, TargetRequest};
    use fidl_fuchsia_developer_remotecontrol::{IdentifyHostResponse, RemoteControlRequest};
    use fidl_fuchsia_feedback::{
        DeviceIdProviderRequest, LastReboot, LastRebootInfoProviderRequest, RebootReason,
    };
    use fidl_fuchsia_hwinfo::{
        BoardInfo, BoardRequest, DeviceInfo, DeviceRequest, ProductInfo, ProductRequest,
    };
    use fidl_fuchsia_intl::RegulatoryDomain;
    use fidl_fuchsia_net::{IpAddress, Ipv4Address};
    use fidl_fuchsia_update_channelcontrol::ChannelControlRequest;
    use serde_json::Value;

    const IPV4_ADDR: [u8; 4] = [127, 0, 0, 1];

    const TEST_OUTPUT_HUMAN: &'static str = "\
        Target: \
        \n    Name: \u{1b}[38;5;2m\"fake_fuchsia_device\"\u{1b}[m\
        \n    SSH Address: \u{1b}[38;5;2m\"127.0.0.1:22\"\u{1b}[m\
        \n    Compatibility state: \u{1b}[38;5;2m\"Absent\"\u{1b}[m\
        \n    Compatibility message: \u{1b}[38;5;2m\"Compatibility information is not available\"\u{1b}[m\
        \n    Last Reboot Graceful: \"true\"\
        \n    Last Reboot Reason: \"ZbiSwap\"\
        \n    Uptime (ns): \"65000\"\
        \nBoard: \
        \n    Name: \"fake_name\"\
        \n    Revision: \"fake_revision\"\
        \n    Instruction set: \"x64\"\
        \nDevice: \
        \n    Serial number: \"fake_serial\"\
        \n    Retail SKU: \"fake_sku\"\
        \n    Is retail demo: false\
        \n    Device ID: \"fake_device_id\"\
        \nProduct: \
        \n    Audio amplifier: \"fake_audio_amplifier\"\
        \n    Build date: \"fake_build_date\"\
        \n    Build name: \"fake_build_name\"\
        \n    Colorway: \"fake_colorway\"\
        \n    Display: \"fake_display\"\
        \n    EMMC storage: \"fake_emmc_storage\"\
        \n    Language: \"fake_language\"\
        \n    Regulatory domain: \"fake_regulatory_domain\"\
        \n    Locale list: []\
        \n    Manufacturer: \"fake_manufacturer\"\
        \n    Microphone: \"fake_microphone\"\
        \n    Model: \"fake_model\"\
        \n    Name: \"fake_name\"\
        \n    NAND storage: \"fake_nand_storage\"\
        \n    Memory: \"fake_memory\"\
        \n    SKU: \"fake_sku\"\
        \nUpdate: \
        \n    Current channel: \"fake_channel\"\
        \n    Next channel: \"fake_target\"\
        \nBuild: \
        \n    Version: \"fake_version\"\
        \n    Product: \"fake_product\"\
        \n    Board: \"fake_board\"\
        \n    Commit: \"fake_commit\"\
        \n";

    fn setup_fake_target_server() -> Deferred<TargetProxy> {
        Deferred::from_output(Ok({
            fho::testing::fake_proxy(move |req| match req {
                TargetRequest::GetSshAddress { responder, .. } => {
                    responder
                        .send(&TargetAddrInfo::Ip(TargetIp {
                            ip: IpAddress::Ipv4(Ipv4Address { addr: IPV4_ADDR }),
                            scope_id: 1,
                        }))
                        .expect("fake ssh address");
                }
                TargetRequest::Identity { responder, .. } => {
                    let addrs = vec![TargetAddrInfo::Ip(TargetIp {
                        ip: IpAddress::Ipv4(Ipv4Address { addr: IPV4_ADDR }),
                        scope_id: 1,
                    })];
                    let nodename = Some("fake_fuchsia_device".to_string());
                    responder
                        .send(&TargetInfo {
                            nodename,
                            addresses: Some(addrs),
                            ..Default::default()
                        })
                        .unwrap();
                }
                _ => assert!(false),
            })
        }))
    }

    fn setup_fake_device_id_server() -> DeviceIdProviderProxy {
        fho::testing::fake_proxy(move |req| match req {
            DeviceIdProviderRequest::GetId { responder } => {
                responder.send("fake_device_id").unwrap();
            }
        })
    }

    fn setup_fake_build_info_server() -> ProviderProxy {
        fho::testing::fake_proxy(move |req| match req {
            ProviderRequest::GetBuildInfo { responder } => {
                responder
                    .send(&BuildInfo {
                        version: Some("fake_version".to_string()),
                        product_config: Some("fake_product".to_string()),
                        board_config: Some("fake_board".to_string()),
                        latest_commit_date: Some("fake_commit".to_string()),
                        ..Default::default()
                    })
                    .unwrap();
            }
        })
    }

    fn setup_fake_board_server() -> BoardProxy {
        fho::testing::fake_proxy(move |req| match req {
            BoardRequest::GetInfo { responder } => {
                responder
                    .send(&BoardInfo {
                        name: Some("fake_name".to_string()),
                        revision: Some("fake_revision".to_string()),
                        cpu_architecture: Some(Architecture::X64),
                        ..Default::default()
                    })
                    .unwrap();
            }
        })
    }

    fn setup_fake_last_reboot_info_server() -> LastRebootInfoProviderProxy {
        fho::testing::fake_proxy(move |req| match req {
            LastRebootInfoProviderRequest::Get { responder } => {
                responder
                    .send(&LastReboot {
                        graceful: Some(true),
                        uptime: Some(65000),
                        reason: Some(RebootReason::ZbiSwap),
                        ..Default::default()
                    })
                    .unwrap();
            }
        })
    }

    #[fuchsia::test]
    async fn test_show_cmd_impl() {
        let buffers = TestBuffers::default();
        let output = VerifiedMachineWriter::<TargetShowInfo>::new_test(None, &buffers);
        let tool = ShowTool {
            cmd: args::TargetShow { ..Default::default() },
            rcs_proxy: setup_fake_rcs_server(),
            connector: None,
            target_proxy: setup_fake_target_server(),
            channel_control_proxy: setup_fake_channel_control_server(),
            board_proxy: setup_fake_board_server(),
            device_proxy: setup_fake_device_server(),
            product_proxy: setup_fake_product_server(),
            build_info_proxy: setup_fake_build_info_server(),
            device_id_proxy: Deferred::from_output(Ok(setup_fake_device_id_server())),
            last_reboot_info_proxy: setup_fake_last_reboot_info_server(),
        };
        tool.main(output).await.expect("show tool main");
        // Convert to a readable string instead of using a byte string and comparing that. Unless
        // you can read u8 arrays well, this helps debug the output.
        let (stdout, _stderr) = buffers.into_strings();
        // Test line by line so it is easier to debug:
        let mut lineno = 0;
        let mut expected_iter = TEST_OUTPUT_HUMAN.lines().into_iter();
        for actual in stdout.lines() {
            lineno += 1;
            if let Some(expected) = expected_iter.next() {
                assert_eq!(
                    actual, expected,
                    "line {lineno} actual != expected {actual} vs. {expected}"
                )
            }
        }
        let remaining: Vec<&str> = expected_iter.collect();
        assert!(remaining.is_empty(), "Missing lines from actual input: {remaining:?}");
    }

    #[fuchsia::test]
    async fn test_gather_board_show() {
        let test_proxy = setup_fake_board_server();
        let result = gather_board_show(test_proxy).await.expect("gather board show");
        assert_eq!(result.name, Some("fake_name".to_string()));
        assert_eq!(result.revision, Some("fake_revision".to_string()));
    }

    fn setup_fake_device_server() -> DeviceProxy {
        fho::testing::fake_proxy(move |req| match req {
            DeviceRequest::GetInfo { responder } => {
                responder
                    .send(&DeviceInfo {
                        serial_number: Some("fake_serial".to_string()),
                        is_retail_demo: Some(false),
                        retail_sku: Some("fake_sku".to_string()),
                        ..Default::default()
                    })
                    .unwrap();
            }
        })
    }

    #[fuchsia::test]
    async fn test_gather_device_show() {
        let test_proxy = setup_fake_device_server();
        let device_id_proxy = Deferred::from_output(Ok(setup_fake_device_id_server()));
        let result =
            gather_device_show(test_proxy, device_id_proxy).await.expect("gather device show");
        assert_eq!(result.serial_number, Some("fake_serial".to_string()));
        assert_eq!(result.retail_sku, Some("fake_sku".to_string()));
        assert_eq!(result.retail_demo, Some(false))
    }

    fn setup_fake_product_server() -> ProductProxy {
        fho::testing::fake_proxy(move |req| match req {
            ProductRequest::GetInfo { responder } => {
                responder
                    .send(&ProductInfo {
                        sku: Some("fake_sku".to_string()),
                        language: Some("fake_language".to_string()),
                        regulatory_domain: Some(RegulatoryDomain {
                            country_code: Some("fake_regulatory_domain".to_string()),
                            ..Default::default()
                        }),
                        locale_list: Some(vec![]),
                        name: Some("fake_name".to_string()),
                        audio_amplifier: Some("fake_audio_amplifier".to_string()),
                        build_date: Some("fake_build_date".to_string()),
                        build_name: Some("fake_build_name".to_string()),
                        colorway: Some("fake_colorway".to_string()),
                        display: Some("fake_display".to_string()),
                        emmc_storage: Some("fake_emmc_storage".to_string()),
                        manufacturer: Some("fake_manufacturer".to_string()),
                        memory: Some("fake_memory".to_string()),
                        microphone: Some("fake_microphone".to_string()),
                        model: Some("fake_model".to_string()),
                        nand_storage: Some("fake_nand_storage".to_string()),
                        ..Default::default()
                    })
                    .unwrap();
            }
        })
    }

    #[fuchsia::test]
    async fn test_gather_product_show() {
        let test_proxy = setup_fake_product_server();
        let result = gather_product_show(test_proxy).await.expect("gather product show");
        assert_eq!(result.audio_amplifier, Some("fake_audio_amplifier".to_string()));
        assert_eq!(result.build_date, Some("fake_build_date".to_string()));
        assert_eq!(result.name, Some("fake_name".to_string()));
        assert_eq!(result.build_name, Some("fake_build_name".to_string()));
        assert_eq!(result.colorway, Some("fake_colorway".to_string()));
    }

    fn setup_fake_rcs_server() -> RemoteControlProxy {
        fho::testing::fake_proxy(move |req| match req {
            RemoteControlRequest::IdentifyHost { responder } => {
                let response = IdentifyHostResponse {
                    nodename: Some(String::from("fake_fuchsia_device")),
                    ..Default::default()
                };
                responder.send(Ok(&response)).unwrap();
            }
            _ => unreachable!(),
        })
    }

    fn setup_fake_channel_control_server() -> ChannelControlProxy {
        fho::testing::fake_proxy(move |req| match req {
            ChannelControlRequest::GetCurrent { responder } => {
                responder.send("fake_channel").unwrap();
            }
            ChannelControlRequest::GetTarget { responder } => {
                responder.send("fake_target").unwrap();
            }
            _ => assert!(false),
        })
    }

    #[fuchsia::test]
    async fn test_gather_update_show() {
        let test_proxy = setup_fake_channel_control_server();
        let result = gather_update_show(test_proxy).await.expect("gather update show");
        assert_eq!(result.current_channel, "fake_channel".to_string());
        assert_eq!(result.next_channel, "fake_target".to_string());
    }

    #[fuchsia::test]
    async fn test_arch_to_string() {
        assert_eq!(arch_to_string(Some(Architecture::X64)), Some("x64".to_string()));
        assert_eq!(arch_to_string(Some(Architecture::Arm64)), Some("arm64".to_string()));
        assert_eq!(arch_to_string(None), None);
    }

    #[fuchsia::test]
    async fn test_verify_machine_schema() {
        let buffers = TestBuffers::default();
        let mut output =
            <ShowTool as FfxMain>::Writer::new_test(Some(Format::JsonPretty), &buffers);
        let tool = ShowTool {
            cmd: args::TargetShow { ..Default::default() },
            rcs_proxy: setup_fake_rcs_server(),
            connector: None,
            target_proxy: setup_fake_target_server(),
            channel_control_proxy: setup_fake_channel_control_server(),
            board_proxy: setup_fake_board_server(),
            device_proxy: setup_fake_device_server(),
            product_proxy: setup_fake_product_server(),
            build_info_proxy: setup_fake_build_info_server(),
            device_id_proxy: Deferred::from_output(Ok(setup_fake_device_id_server())),
            last_reboot_info_proxy: setup_fake_last_reboot_info_server(),
        };
        tool.show_cmd(&mut output).await.expect("main");
        let (stdout, _stderr) = buffers.into_strings();
        let data: Value = serde_json::from_str(&stdout).expect("Valid JSON");
        match <ShowTool as FfxMain>::Writer::verify_schema(&data) {
            Ok(_) => (),
            Err(e) => {
                println!("Error verifying schema: {e}");
                println!("{data:?}");
            }
        };
    }

    fn setup_fake_direct_connector() -> Arc<dyn DirectConnector> {
        let mut dc = fho::MockDirectConnector::new();
        dc.expect_connection().return_once(|| {
            let device_address = std::net::SocketAddr::new("127.0.0.1".parse().unwrap(), 22);
            let fidl_pipe = FidlPipe::fake(Some(device_address));
            let conn = ffx_target::Connection::fake(fidl_pipe);
            Box::pin(async { Ok(Arc::new(Mutex::new(Some(conn)))) })
        });
        Arc::new(dc)
    }

    #[fuchsia::test]
    async fn test_show_cmd_impl_direct_connection() {
        let buffers = TestBuffers::default();
        let output = VerifiedMachineWriter::<TargetShowInfo>::new_test(None, &buffers);
        let tool = ShowTool {
            cmd: args::TargetShow { ..Default::default() },
            rcs_proxy: setup_fake_rcs_server(),
            connector: Some(setup_fake_direct_connector()),
            target_proxy: setup_fake_target_server(),
            channel_control_proxy: setup_fake_channel_control_server(),
            board_proxy: setup_fake_board_server(),
            device_proxy: setup_fake_device_server(),
            product_proxy: setup_fake_product_server(),
            build_info_proxy: setup_fake_build_info_server(),
            device_id_proxy: Deferred::from_output(Ok(setup_fake_device_id_server())),
            last_reboot_info_proxy: setup_fake_last_reboot_info_server(),
        };
        tool.main(output).await.expect("show tool main");
        // Convert to a readable string instead of using a byte string and comparing that. Unless
        // you can read u8 arrays well, this helps debug the output.
        let (stdout, _stderr) = buffers.into_strings();
        // Test line by line so it is easier to debug:
        let mut lineno = 0;
        let mut expected_iter = TEST_OUTPUT_HUMAN.lines().into_iter();
        for actual in stdout.lines() {
            lineno += 1;
            if let Some(expected) = expected_iter.next() {
                assert_eq!(
                    actual, expected,
                    "line {lineno} actual != expected {actual} vs. {expected}"
                )
            }
        }
        let remaining: Vec<&str> = expected_iter.collect();
        assert!(remaining.is_empty(), "Missing lines from actual input: {remaining:?}");
    }
}
