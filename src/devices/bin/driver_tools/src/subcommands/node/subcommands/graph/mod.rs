// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub mod args;
use crate::subcommands::node::common;
use crate::subcommands::node::subcommands::graph::args::GraphOrientation;

use anyhow::{format_err, Result};
use args::GraphNodeCommand;
use fidl_fuchsia_driver_development as fdd;
use std::io::Write;

const DIGRAPH_PREFIX: &str = r#"digraph {
     forcelabels = true; splines="ortho"; ranksep = 1.2; nodesep = 0.5;
     node [ shape = "box" color = " #2a5b4f" penwidth = 2.25 fontname = "prompt medium" fontsize = 10 margin = 0.22 ];
     edge [ color = " #37474f" penwidth = 1 style = dashed fontname = "roboto mono" fontsize = 10 ];"#;

pub async fn graph_node(
    cmd: GraphNodeCommand,
    writer: &mut dyn Write,
    driver_development_proxy: fdd::ManagerProxy,
) -> Result<()> {
    let nodes = fuchsia_driver_dev::get_device_info(&driver_development_proxy, &[], false).await?;
    let nodes = common::filter_nodes(nodes, cmd.filter)?;
    let node_map = common::create_node_map(&nodes)?;

    writeln!(writer, "{}", DIGRAPH_PREFIX)?;
    match cmd.orientation {
        GraphOrientation::TopToBottom => writeln!(writer, r#"    rankdir = "TB""#).unwrap(),
        GraphOrientation::LeftToRight => writeln!(writer, r#"    rankdir = "LR""#).unwrap(),
    };

    for node in nodes.iter() {
        print_graph_node(node, writer)?;
    }

    for node in nodes.iter() {
        if let Some(child_ids) = &node.child_ids {
            for id in child_ids.iter().rev() {
                if let Some(child) = node_map.get(&id) {
                    print_graph_edge(node, writer, child)?;
                }
            }
        }
    }

    writeln!(writer, "}}")?;

    Ok(())
}

fn print_graph_node(node: &fdd::NodeInfo, writer: &mut dyn Write) -> Result<()> {
    let moniker = node.moniker.as_ref().ok_or_else(|| format_err!("Node missing moniker"))?;
    let (_, name) = moniker.rsplit_once('.').unwrap_or(("", &moniker));

    writeln!(
        writer,
        "     \"{}\" [label=\"{}\"]",
        node.id.as_ref().ok_or_else(|| format_err!("Node missing id"))?,
        name,
    )?;
    Ok(())
}

fn print_graph_edge(
    node: &fdd::NodeInfo,
    writer: &mut dyn Write,
    child: &fdd::NodeInfo,
) -> Result<()> {
    writeln!(
        writer,
        "     \"{}\" -> \"{}\"",
        node.id.as_ref().ok_or_else(|| format_err!("Node missing id"))?,
        child.id.as_ref().ok_or_else(|| format_err!("Child node missing id"))?
    )?;
    Ok(())
}
