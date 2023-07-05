// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::Error,
    async_trait::async_trait,
    donut_lib, ffx_wlan_client_args as arg_types, ffx_wlan_common,
    fho::{moniker, FfxMain, FfxTool, SimpleWriter},
    fidl_fuchsia_wlan_policy as wlan_policy,
};

#[derive(FfxTool)]
pub struct ClientTool {
    #[command]
    cmd: arg_types::ClientCommand,
    #[with(moniker("/core/wlancfg"))]
    client_provider: wlan_policy::ClientProviderProxy,
    #[with(moniker("/core/wlancfg"))]
    client_listener: wlan_policy::ClientListenerProxy,
}

fho::embedded_plugin!(ClientTool);

#[async_trait(?Send)]
impl FfxMain for ClientTool {
    type Writer = SimpleWriter;
    async fn main(self, _writer: Self::Writer) -> fho::Result<()> {
        handle_client_command(self.client_provider, self.client_listener, self.cmd).await?;
        Ok(())
    }
}

async fn handle_client_command(
    client_provider: wlan_policy::ClientProviderProxy,
    client_listener: wlan_policy::ClientListenerProxy,
    cmd: arg_types::ClientCommand,
) -> Result<(), Error> {
    let (client_controller, _) = ffx_wlan_common::get_client_controller(client_provider).await?;
    let listener_stream = ffx_wlan_common::get_client_listener_stream(client_listener)?;

    match cmd.subcommand {
        arg_types::ClientSubCommand::BatchConfig(batch_cmd) => match batch_cmd.subcommand {
            arg_types::BatchConfigSubCommand::Dump(arg_types::Dump {}) => {
                let saved_networks =
                    donut_lib::handle_get_saved_networks(&client_controller).await?;
                donut_lib::print_serialized_saved_networks(saved_networks)
            }
            arg_types::BatchConfigSubCommand::Restore(arg_types::Restore { serialized_config }) => {
                donut_lib::restore_serialized_config(client_controller, serialized_config).await
            }
        },
        arg_types::ClientSubCommand::Connect(connect_args) => {
            let security = connect_args.security_type.map(|s| s.into());
            donut_lib::handle_connect(
                client_controller,
                listener_stream,
                connect_args.ssid,
                security,
            )
            .await
        }
        arg_types::ClientSubCommand::List(arg_types::ListSavedNetworks {}) => {
            let saved_networks = donut_lib::handle_get_saved_networks(&client_controller).await?;
            donut_lib::print_saved_networks(saved_networks)
        }
        arg_types::ClientSubCommand::Listen(arg_types::Listen {}) => {
            donut_lib::handle_listen(listener_stream).await
        }
        arg_types::ClientSubCommand::RemoveNetwork(remove_args) => {
            let donut_args = donut_lib::opts::RemoveArgs::from(remove_args);
            let security = donut_args.parse_security();
            let credential = donut_args.try_parse_credential()?;
            donut_lib::handle_remove_network(
                client_controller,
                donut_args.ssid.into_bytes(),
                security,
                credential,
            )
            .await
        }
        arg_types::ClientSubCommand::SaveNetwork(config_args) => {
            let network_config = wlan_policy::NetworkConfig::from(config_args);
            donut_lib::handle_save_network(client_controller, network_config).await
        }
        arg_types::ClientSubCommand::Scan(arg_types::Scan {}) => {
            let scan_results = donut_lib::handle_scan(client_controller).await?;
            donut_lib::print_scan_results(scan_results)
        }
        arg_types::ClientSubCommand::Start(arg_types::StartClientConnections {}) => {
            donut_lib::handle_start_client_connections(client_controller).await
        }
        arg_types::ClientSubCommand::Stop(arg_types::StopClientConnections {}) => {
            donut_lib::handle_stop_client_connections(client_controller).await
        }
    }
}
