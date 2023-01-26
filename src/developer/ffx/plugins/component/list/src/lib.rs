// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::Result,
    component_debug::list::{create_table, get_all_instances, Instance, InstanceState},
    ffx_component::rcs::{connect_to_realm_explorer, connect_to_realm_query},
    ffx_component_list_args::ComponentListCommand,
    ffx_core::ffx_plugin,
    ffx_writer::Writer,
    fidl_fuchsia_developer_remotecontrol as rc,
    serde::{Deserialize, Serialize},
};

#[derive(Deserialize, Serialize)]
pub struct SerializableInstance {
    // Moniker of the component. This is the full path in the component hierarchy.
    pub name: String,

    // URL of the component.
    pub url: String,

    // True if component is of appmgr/CMX type.
    // False if it is of the component_manager/CML type.
    pub is_cmx: bool,

    // CML components may not always be running.
    // Always true for CMX components.
    pub is_running: bool,
}

impl From<Instance> for SerializableInstance {
    fn from(i: Instance) -> Self {
        let is_running = i.state == InstanceState::Started;
        SerializableInstance {
            name: i.moniker.to_string(),
            url: i.url.unwrap_or_default(),
            is_cmx: i.is_cmx,
            is_running,
        }
    }
}

#[ffx_plugin()]
pub async fn list(
    rcs_proxy: rc::RemoteControlProxy,
    #[ffx(machine = Vec<ListComponent>)] mut writer: Writer,
    cmd: ComponentListCommand,
) -> Result<()> {
    let ComponentListCommand { filter, verbose } = cmd;
    let query_proxy = connect_to_realm_query(&rcs_proxy).await?;
    let explorer_proxy = connect_to_realm_explorer(&rcs_proxy).await?;

    let instances = get_all_instances(&explorer_proxy, &query_proxy, filter).await?;

    if writer.is_machine() {
        let instances: Vec<SerializableInstance> =
            instances.into_iter().map(SerializableInstance::from).collect();
        writer.machine(&instances)?;
    } else if verbose {
        let table = create_table(instances);
        table.print(&mut writer)?;
    } else {
        for instance in instances {
            writer.line(instance.moniker.to_string())?;
        }
    }
    Ok(())
}
