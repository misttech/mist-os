// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::Result,
    component_debug::show::{create_table, find_instances},
    errors::ffx_bail,
    ffx_component::rcs::{connect_to_realm_explorer, connect_to_realm_query},
    ffx_component_show_args::ComponentShowCommand,
    ffx_core::ffx_plugin,
    ffx_writer::Writer,
    fidl_fuchsia_developer_remotecontrol as rc,
    std::io::Write,
};

#[ffx_plugin()]
pub async fn show(
    rcs_proxy: rc::RemoteControlProxy,
    #[ffx(machine = Vec<Instance>)] mut writer: Writer,
    cmd: ComponentShowCommand,
) -> Result<()> {
    let ComponentShowCommand { query } = cmd;

    let query_proxy = connect_to_realm_query(&rcs_proxy).await?;
    let explorer_proxy = connect_to_realm_explorer(&rcs_proxy).await?;

    let instances = find_instances(query.clone(), &explorer_proxy, &query_proxy).await?;

    if writer.is_machine() {
        writer.machine(&instances)?;
    } else {
        if instances.is_empty() {
            // TODO(fxbug.dev/104031): Clarify the exit code policy of this plugin.
            ffx_bail!("No matching components found for query \"{}\"", query);
        }

        for instance in instances {
            let table = create_table(instance);
            table.print(&mut writer)?;
            writeln!(&mut writer, "")?;
        }
    }

    Ok(())
}
