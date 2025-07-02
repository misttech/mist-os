// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub mod args;

use crate::subcommands::node::common;

use anyhow::Result;
use args::ShowNodeCommand;
use itertools::Itertools;
use prettytable::format::FormatBuilder;
use prettytable::{cell, row, Table};
use std::io::Write;
use {fidl_fuchsia_driver_development as fdd, fidl_fuchsia_driver_framework as fdf};

pub async fn show_node(
    cmd: ShowNodeCommand,
    writer: &mut dyn Write,
    driver_development_proxy: fdd::ManagerProxy,
) -> Result<()> {
    let nodes = fuchsia_driver_dev::get_device_info(&driver_development_proxy, &[], false).await?;
    let node = common::get_single_node_from_query(&cmd.query, nodes).await?;

    let with_style = termion::is_tty(&std::io::stdout());
    print_table(node, with_style, writer)?;

    Ok(())
}

fn print_table(node: fdd::NodeInfo, with_style: bool, writer: &mut dyn Write) -> Result<()> {
    let mut table = Table::new();
    table.set_format(FormatBuilder::new().padding(2, 0).build());

    let parent_count = node.parent_ids.map(|ids| ids.len()).unwrap_or(0);
    let children_count = node.child_ids.map(|ids| ids.len()).unwrap_or(0);
    let moniker = node.moniker.expect("Node does not have a moniker");
    let (_, name) = moniker.rsplit_once('.').unwrap_or(("", &moniker));
    let (state, owner) =
        common::get_state_and_owner(node.quarantined, &node.bound_driver_url, with_style);

    let bus_topo = node
        .bus_topology
        .unwrap_or_default()
        .into_iter()
        .map(|b| {
            (
                b.bus.map(|b| format!("{:?}", b)).unwrap_or_else(|| "No bus".to_string()),
                b.address_stability
                    .map(|a| format!("{:?}", a))
                    .unwrap_or_else(|| "No stability".to_string()),
                b.address
                    .map(|a| match a {
                        fdf::DeviceAddress::IntValue(i) => {
                            format!("{}", i)
                        }
                        fdf::DeviceAddress::ArrayIntValue(items) => {
                            format!("{}", items.iter().map(|u| u.to_string()).join(", "))
                        }
                        fdf::DeviceAddress::CharIntValue(c) => {
                            format!("{}", c)
                        }
                        fdf::DeviceAddress::ArrayCharIntValue(items) => {
                            format!("{}", items.join(", "))
                        }
                        fdf::DeviceAddress::StringValue(s) => {
                            format!("{}", s)
                        }
                        _ => format!("unknown"),
                    })
                    .unwrap_or_else(|| "No Address".to_string()),
            )
        })
        .collect::<Vec<_>>();

    let koid = node.driver_host_koid.map(|k| format!("{}", k)).unwrap_or_default();

    let props = node
        .node_property_list
        .unwrap_or_else(|| vec![])
        .into_iter()
        .map(|p| {
            let key = match p.key {
                fdf::NodePropertyKey::IntValue(i) => format!("{}", i),
                fdf::NodePropertyKey::StringValue(s) => format!("{}", s),
            };
            let value = match p.value {
                fdf::NodePropertyValue::IntValue(i) => format!("{}", i),
                fdf::NodePropertyValue::StringValue(s) => {
                    format!("{}", s)
                }
                fdf::NodePropertyValue::BoolValue(b) => format!("{}", b),
                fdf::NodePropertyValue::EnumValue(e) => format!("{}", e),
                _ => format!("unknown"),
            };
            (key, value)
        })
        .collect::<Vec<_>>();

    let offers = node
        .offer_list
        .unwrap_or_else(|| vec![])
        .into_iter()
        .map(|o| {
            if let fidl_fuchsia_component_decl::Offer::Service(service) = o {
                let service_str = service.target_name.unwrap_or_else(|| "<unknown>".to_string());

                let source_name = if let Some(fidl_fuchsia_component_decl::Ref::Child(ref source)) =
                    service.source.as_ref()
                {
                    source.name.clone()
                } else {
                    "Unknown source".to_string()
                };

                let filter = if let Some(filter) = &service.source_instance_filter {
                    filter.join(", ")
                } else {
                    "All instances".to_string()
                };
                (service_str, source_name, filter)
            } else {
                ("Non-service offer".to_string(), "".to_string(), "".to_string())
            }
        })
        .collect::<Vec<_>>();

    table.add_row(row!(r->"Name:", name));
    table.add_row(row!(r->"Moniker:", moniker));
    table.add_row(row!(r->"Owner:", owner));
    table.add_row(row!(r->"Node State:", state));
    table.add_row(row!(r->"Host Koid:", koid));
    table.add_row(row!(r->"Parent Count:", parent_count));
    table.add_row(row!(r->"Child Count:", children_count));
    table.add_empty_row();
    table.print(writer)?;

    if !bus_topo.is_empty() {
        table = Table::new();
        table.set_format(FormatBuilder::new().padding(2, 0).build());
        table.set_titles(row!("Bus Topology:", "Bus Type", "Stability", "Address"));
        for topo in bus_topo {
            table.add_row(row!("", topo.0, topo.1, topo.2));
        }
        table.add_empty_row();
        table.print(writer)?;
    }

    if !props.is_empty() {
        table = Table::new();
        table.set_format(FormatBuilder::new().padding(2, 0).build());
        table.set_titles(row!("Node Properties:", "Key", "Value"));
        for prop in props {
            table.add_row(row!("", prop.0, prop.1));
        }
        table.add_empty_row();
        table.print(writer)?;
    }

    if !offers.is_empty() {
        table = Table::new();
        table.set_format(FormatBuilder::new().padding(2, 0).build());
        table.set_titles(row!("Node Offers:", "Service", "Source", "Instances"));
        for offer in offers {
            table.add_row(row!("", offer.0, offer.1, offer.2));
        }
        table.add_empty_row();
        table.print(writer)?;
    }

    Ok(())
}
