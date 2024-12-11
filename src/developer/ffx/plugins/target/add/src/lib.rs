// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use async_trait::async_trait;
use errors::ffx_error;
use ffx_target::add_manual_target;
use ffx_target_add_args::AddCommand;
use fho::{daemon_protocol, FfxContext, FfxMain, FfxTool, ToolIO, VerifiedMachineWriter};
use fidl_fuchsia_developer_ffx::{TargetCollectionProxy, TargetConnectionError};
use netext::parse_address_parts;
use schemars::JsonSchema;
use serde::Serialize;
use std::io::Write;
use target_errors::FfxTargetError;

#[derive(Debug, Serialize, JsonSchema)]
pub enum CommandStatus {
    /// Successful execution with an optional informational string.
    Ok { message: Option<String> },
    /// Unexpected error with string.
    UnexpectedError { message: String },
    /// A known kind of error that can be reported usefully to the user
    UserError { message: String },
}

#[derive(FfxTool)]
pub struct AddTool {
    #[command]
    cmd: AddCommand,
    #[with(daemon_protocol())]
    target_collection_proxy: TargetCollectionProxy,
}

fho::embedded_plugin!(AddTool);

#[async_trait(?Send)]
impl FfxMain for AddTool {
    type Writer = VerifiedMachineWriter<CommandStatus>;
    async fn main(self, mut writer: Self::Writer) -> fho::Result<()> {
        match add_impl(Some(&mut writer), self.target_collection_proxy, self.cmd).await {
            Ok(_) => {
                writer.machine(&CommandStatus::Ok { message: None })?;
                Ok(())
            }
            Err(fho::Error::User(e)) => {
                writer.machine(&CommandStatus::UserError { message: e.to_string() })?;
                Err(fho::Error::User(e))
            }
            Err(e) => {
                writer.machine(&CommandStatus::UnexpectedError { message: e.to_string() })?;
                Err(e)
            }
        }
    }
}

pub async fn add_impl(
    mut writer: Option<&mut VerifiedMachineWriter<CommandStatus>>,
    target_collection_proxy: TargetCollectionProxy,
    cmd: AddCommand,
) -> fho::Result<()> {
    let (addr, scope, port) =
        parse_address_parts(cmd.addr.as_str()).map_err(|e| ffx_error!("{}", e))?;
    let scope_id = if let Some(scope) = scope {
        match netext::get_verified_scope_id(scope) {
            Ok(res) => res,
            Err(_e) => {
                return Err(ffx_error!(
                    "Cannot add target, as scope ID '{scope}' is not a valid interface name or index"
                )
                .into());
            }
        }
    } else {
        0
    };
    loop {
        let res = add_manual_target(
            &target_collection_proxy,
            addr,
            scope_id,
            port.unwrap_or(0),
            !cmd.nowait,
        )
        .await;
        break match res {
            Ok(()) => Ok(()),
            Err(e) => {
                // target_connection_err @ target_errors::FfxTargetError::TargetConnectionError { err, .. });
                match e.downcast_ref::<FfxTargetError>() {
                    Some(FfxTargetError::TargetConnectionError { err, .. }) => {
                        // This is just copied from ffx/lib/target/src/ssh_connector.rs
                        // This is, unfortunately, an artifact of having to convert rust errors into FIDL
                        // for the error message response from the daemon.
                        // LINT.IfChange
                        use TargetConnectionError::*;
                        match err {
                            Timeout | ConnectionRefused | UnknownNameOrService | NoRouteToHost
                            | NetworkUnreachable | UnknownError => {
                                if let Some(ref mut writer) = writer {
                                    if !writer.is_machine() {
                                        writeln!(writer, "Non-fatal error encountered connecting. Will retry: {e}").bug()?;
                                    }
                                }
                                continue;
                            }
                            _ => Err(e.into()),
                        }
                        // LINT.ThenChange(/src/developer/ffx/lib/target/src/ssh_connector.rs)
                    }
                    _ => Err(e.into()),
                }
            }
        };
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use fho::{Format, TestBuffers};
    use {fidl_fuchsia_developer_ffx as ffx, fidl_fuchsia_net as net};

    fn setup_fake_target_collection<T: 'static + Fn(ffx::TargetAddrInfo) + Send>(
        test: T,
    ) -> TargetCollectionProxy {
        fho::testing::fake_proxy(move |req| match req {
            ffx::TargetCollectionRequest::AddTarget {
                ip, config: _, add_target_responder, ..
            } => {
                let add_target_responder = add_target_responder.into_proxy();
                test(ip);
                add_target_responder.success().unwrap();
            }
            _ => assert!(false),
        })
    }

    #[fuchsia::test]
    async fn test_add() {
        let server = setup_fake_target_collection(|addr| {
            assert_eq!(
                addr,
                ffx::TargetAddrInfo::Ip(ffx::TargetIp {
                    ip: net::IpAddress::Ipv4(net::Ipv4Address {
                        addr: "123.210.123.210"
                            .parse::<std::net::Ipv4Addr>()
                            .unwrap()
                            .octets()
                            .into()
                    }),
                    scope_id: 0,
                })
            )
        });
        add_impl(None, server, AddCommand { addr: "123.210.123.210".to_owned(), nowait: true })
            .await
            .unwrap();
    }

    #[fuchsia::test]
    async fn test_add_port() {
        let server = setup_fake_target_collection(|addr| {
            assert_eq!(
                addr,
                ffx::TargetAddrInfo::IpPort(ffx::TargetIpPort {
                    ip: net::IpAddress::Ipv4(net::Ipv4Address {
                        addr: "123.210.123.210"
                            .parse::<std::net::Ipv4Addr>()
                            .unwrap()
                            .octets()
                            .into()
                    }),
                    scope_id: 0,
                    port: 2310,
                })
            )
        });
        add_impl(
            None,
            server,
            AddCommand { addr: "123.210.123.210:2310".to_owned(), nowait: true },
        )
        .await
        .unwrap();
    }

    #[fuchsia::test]
    async fn test_add_v6() {
        let server = setup_fake_target_collection(|addr| {
            assert_eq!(
                addr,
                ffx::TargetAddrInfo::Ip(ffx::TargetIp {
                    ip: net::IpAddress::Ipv6(net::Ipv6Address {
                        addr: "f000::1".parse::<std::net::Ipv6Addr>().unwrap().octets().into()
                    }),
                    scope_id: 0,
                })
            )
        });
        add_impl(None, server, AddCommand { addr: "f000::1".to_owned(), nowait: true })
            .await
            .unwrap();
    }

    #[fuchsia::test]
    async fn test_add_v6_port() {
        let server = setup_fake_target_collection(|addr| {
            assert_eq!(
                addr,
                ffx::TargetAddrInfo::IpPort(ffx::TargetIpPort {
                    ip: net::IpAddress::Ipv6(net::Ipv6Address {
                        addr: "f000::1".parse::<std::net::Ipv6Addr>().unwrap().octets().into()
                    }),
                    scope_id: 0,
                    port: 65,
                })
            )
        });
        add_impl(None, server, AddCommand { addr: "[f000::1]:65".to_owned(), nowait: true })
            .await
            .unwrap();
    }

    #[fuchsia::test]
    async fn test_add_v6_scope_id() {
        let server = setup_fake_target_collection(|addr| {
            assert_eq!(
                addr,
                ffx::TargetAddrInfo::Ip(ffx::TargetIp {
                    ip: net::IpAddress::Ipv6(net::Ipv6Address {
                        addr: "f000::1".parse::<std::net::Ipv6Addr>().unwrap().octets().into()
                    }),
                    scope_id: 1,
                })
            )
        });
        add_impl(None, server, AddCommand { addr: "f000::1%1".to_owned(), nowait: true })
            .await
            .unwrap();
    }

    #[fuchsia::test]
    async fn test_add_v6_scope_id_port() {
        let server = setup_fake_target_collection(|addr| {
            assert_eq!(
                addr,
                ffx::TargetAddrInfo::IpPort(ffx::TargetIpPort {
                    ip: net::IpAddress::Ipv6(net::Ipv6Address {
                        addr: "f000::1".parse::<std::net::Ipv6Addr>().unwrap().octets().into()
                    }),
                    scope_id: 1,
                    port: 640,
                })
            )
        });
        add_impl(None, server, AddCommand { addr: "[f000::1%1]:640".to_owned(), nowait: true })
            .await
            .unwrap();
    }

    #[fuchsia::test]
    async fn test_machine_output() {
        let server = setup_fake_target_collection(|addr| {
            assert_eq!(
                addr,
                ffx::TargetAddrInfo::IpPort(ffx::TargetIpPort {
                    ip: net::IpAddress::Ipv6(net::Ipv6Address {
                        addr: "f000::1".parse::<std::net::Ipv6Addr>().unwrap().octets().into()
                    }),
                    scope_id: 1,
                    port: 640,
                })
            )
        });
        let tool = AddTool {
            cmd: AddCommand { addr: "[f000::1%1]:640".to_owned(), nowait: true },
            target_collection_proxy: server,
        };

        let buffers = TestBuffers::default();
        let writer = VerifiedMachineWriter::new_test(Some(Format::Json), &buffers);
        tool.main(writer).await.expect("target add");

        let expected = String::from("{\"Ok\":{\"message\":null}}\n");
        let actual = buffers.into_stdout_str();
        assert_eq!(expected, actual)
    }

    #[fuchsia::test]
    async fn test_machine_output_err() {
        let server = setup_fake_target_collection(|addr| {
            assert_eq!(
                addr,
                ffx::TargetAddrInfo::IpPort(ffx::TargetIpPort {
                    ip: net::IpAddress::Ipv6(net::Ipv6Address {
                        addr: "f000::1".parse::<std::net::Ipv6Addr>().unwrap().octets().into()
                    }),
                    scope_id: 1,
                    port: 640,
                })
            )
        });
        let tool = AddTool {
            cmd: AddCommand { addr: "invalid_address-100".into(), nowait: true },
            target_collection_proxy: server,
        };

        let buffers = TestBuffers::default();
        let writer = VerifiedMachineWriter::new_test(Some(Format::Json), &buffers);
        tool.main(writer).await.expect_err("target add");

        let expected = String::from("{\"UserError\":{\"message\":\"Could not parse 'invalid_address-100'. Invalid address\"}}\n");
        let actual = buffers.into_stdout_str();
        assert_eq!(expected, actual)
    }
}
