// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

mod args;

use crate::args::*;
use anyhow::Result;
use component_debug::cli::*;
use component_debug::config::resolve_raw_config_overrides;
use component_debug::copy::copy_cmd;
use component_debug::explore::Stdout;
use fidl_fuchsia_dash as fdash;
use fidl_fuchsia_sys2 as fsys;
use fuchsia_component::client::{connect_to_protocol, connect_to_protocol_at_path};

pub async fn exec() -> Result<()> {
    let args: ComponentArgs = argh::from_env();

    let writer = std::io::stdout();
    let realm_query =
        connect_to_protocol_at_path::<fsys::RealmQueryMarker>("/svc/fuchsia.sys2.RealmQuery.root")?;
    let route_validator = connect_to_protocol_at_path::<fsys::RouteValidatorMarker>(
        "/svc/fuchsia.sys2.RouteValidator.root",
    )?;
    let lifecycle_controller = connect_to_protocol_at_path::<fsys::LifecycleControllerMarker>(
        "/svc/fuchsia.sys2.LifecycleController.root",
    )?;

    match args.subcommand {
        ComponentSubcommand::Show(args) => show_cmd_print(args.query, realm_query, writer).await,
        ComponentSubcommand::Create(args) => {
            let config_overrides = resolve_raw_config_overrides(
                &realm_query,
                &args.moniker,
                &args.url.to_string(),
                &args.config,
            )
            .await?;
            create_cmd(args.url, args.moniker, config_overrides, lifecycle_controller, writer).await
        }
        ComponentSubcommand::Destroy(args) => {
            destroy_cmd(args.query, lifecycle_controller, realm_query, writer).await
        }
        ComponentSubcommand::Resolve(args) => {
            resolve_cmd(args.query, lifecycle_controller, realm_query, writer).await
        }
        ComponentSubcommand::Explore(args) => {
            // TODO(fxbug.dev/95554): Verify that the optional Launcher protocol is available
            // before connecting.
            let dash_launcher = connect_to_protocol::<fdash::LauncherMarker>()?;
            // TODO(fxbug.dev/127189): Use Stdout::raw instead, when a command is not provided
            let stdout = Stdout::buffered();

            explore_cmd(
                args.query,
                args.ns_layout,
                args.command,
                args.tools,
                dash_launcher,
                realm_query,
                stdout,
            )
            .await
        }
        ComponentSubcommand::Reload(args) => {
            reload_cmd(args.query, lifecycle_controller, realm_query, writer).await
        }
        ComponentSubcommand::Start(args) => {
            start_cmd(args.query, lifecycle_controller, realm_query, writer).await
        }
        ComponentSubcommand::Stop(args) => {
            stop_cmd(args.query, lifecycle_controller, realm_query, writer).await
        }
        ComponentSubcommand::Doctor(args) => {
            doctor_cmd_print(args.query, route_validator, realm_query, writer).await
        }
        ComponentSubcommand::Capability(args) => {
            capability_cmd(args.capability_name, realm_query, writer).await
        }
        ComponentSubcommand::List(args) => {
            list_cmd_print(args.filter, args.verbose, realm_query, writer).await
        }
        ComponentSubcommand::Graph(args) => {
            graph_cmd(args.filter, args.orientation, realm_query, writer).await
        }
        ComponentSubcommand::Run(args) => {
            let config_overrides = resolve_raw_config_overrides(
                &realm_query,
                &args.moniker,
                &args.url.to_string(),
                &args.config,
            )
            .await?;
            run_cmd(
                args.moniker,
                args.url,
                args.recreate,
                args.connect_stdio,
                config_overrides,
                lifecycle_controller,
                writer,
            )
            .await
        }
        ComponentSubcommand::Copy(args) => copy_cmd(&realm_query, args.paths, args.verbose).await,
        ComponentSubcommand::Storage(args) => match args.subcommand {
            StorageSubcommand::Copy(copy_args) => {
                storage_copy_cmd(
                    args.provider,
                    args.capability,
                    copy_args.source_path,
                    copy_args.destination_path,
                    realm_query,
                )
                .await
            }
            StorageSubcommand::Delete(delete_args) => {
                storage_delete_cmd(args.provider, args.capability, delete_args.path, realm_query)
                    .await
            }
            StorageSubcommand::List(list_args) => {
                storage_list_cmd(
                    args.provider,
                    args.capability,
                    list_args.path,
                    realm_query,
                    writer,
                )
                .await
            }
            StorageSubcommand::MakeDirectory(make_dir_args) => {
                storage_make_directory_cmd(
                    args.provider,
                    args.capability,
                    make_dir_args.path,
                    realm_query,
                )
                .await
            }
        },
        ComponentSubcommand::Collection(args) => match args.subcommand {
            CollectionSubcommand::List(_) => collection_list_cmd(realm_query, writer).await,
            CollectionSubcommand::Show(show_args) => {
                collection_show_cmd(show_args.query, realm_query, writer).await
            }
        },
    }
}

#[fuchsia::main(logging = false)]
async fn main() {
    if let Err(e) = exec().await {
        eprintln!("{}", e);
        std::process::exit(1);
    }
}
