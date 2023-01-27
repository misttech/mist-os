// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::Result,
    component_debug::{
        doctor::{create_tables, validate_routes},
        query::get_cml_moniker_from_query,
    },
    errors::ffx_bail,
    ffx_component::rcs::{
        connect_to_realm_explorer, connect_to_realm_query, connect_to_route_validator,
    },
    ffx_component_doctor_args::DoctorCommand,
    ffx_core::ffx_plugin,
    ffx_writer::Writer,
    fidl_fuchsia_developer_remotecontrol as rc, fidl_fuchsia_sys2 as fsys,
    moniker::{AbsoluteMoniker, AbsoluteMonikerBase, RelativeMoniker, RelativeMonikerBase},
    std::io::Write,
};

/// Perform a series of diagnostic checks on a component at runtime.
#[ffx_plugin()]
pub async fn doctor(
    rcs: rc::RemoteControlProxy,
    cmd: DoctorCommand,
    #[ffx(machine = Analysis)] mut writer: Writer,
) -> Result<()> {
    let realm_explorer = connect_to_realm_explorer(&rcs).await?;
    let realm_query = connect_to_realm_query(&rcs).await?;
    let route_validator = connect_to_route_validator(&rcs).await?;

    // Check the moniker.
    let moniker = get_cml_moniker_from_query(&cmd.query, &realm_explorer).await?;

    // Convert the absolute moniker into a relative moniker w.r.t. root.
    // LifecycleController expects relative monikers only.
    let relative_moniker = RelativeMoniker::scope_down(&AbsoluteMoniker::root(), &moniker).unwrap();

    // Query the Component Manager for information about this instance.
    writeln!(writer, "Moniker: {}", &moniker)?;

    // Obtain the basic information about the component.
    let (info, state) = match realm_query.get_instance_info(&relative_moniker.to_string()).await? {
        Ok((info, state)) => (info, state),
        Err(fsys::RealmQueryError::InstanceNotFound) => {
            ffx_bail!("Component manager could not find an instance with the moniker: {}\n\
                       Use `ffx component list` or `ffx component show` to find the correct moniker of your instance.",
                &moniker
            );
        }
        Err(e) => {
            ffx_bail!(
                "Component manager returned an unexpected error: {:?}\n\
                 Please report this to the Component Framework team.",
                e
            );
        }
    };

    // Print the basic information.
    write!(writer, "URL: {}\n", info.url)?;
    let instance_id = if let Some(i) = info.instance_id { i } else { "None".to_string() };
    write!(writer, "Instance ID: {}\n\n", instance_id)?;

    if state.is_none() {
        ffx_bail!(
            "Instance is not resolved.\n\
             Use `ffx component resolve {}` to resolve this component.",
            moniker
        );
    }

    let reports = validate_routes(&route_validator, relative_moniker).await?;

    if writer.is_machine() {
        writer.machine(&reports)?;
    } else {
        let (use_table, expose_table) = create_tables(reports);
        use_table.print(&mut writer)?;
        writeln!(&mut writer, "")?;

        expose_table.print(&mut writer)?;
        writeln!(&mut writer, "")?;
    }

    Ok(())
}
