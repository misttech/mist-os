// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::Result,
    component_debug::lifecycle::destroy_instance_in_collection,
    errors::ffx_error,
    ffx_component::{
        format::format_destroy_error,
        query::get_cml_moniker_from_query,
        rcs::{connect_to_lifecycle_controller, connect_to_realm_explorer},
    },
    ffx_component_destroy_args::DestroyComponentCommand,
    ffx_core::ffx_plugin,
    fidl_fuchsia_developer_remotecontrol as rc, fidl_fuchsia_sys2 as fsys,
    moniker::{
        AbsoluteMoniker, AbsoluteMonikerBase, ChildMonikerBase, RelativeMoniker,
        RelativeMonikerBase,
    },
};

#[ffx_plugin]
pub async fn destroy(
    rcs_proxy: rc::RemoteControlProxy,
    cmd: DestroyComponentCommand,
) -> Result<()> {
    let lifecycle_controller = connect_to_lifecycle_controller(&rcs_proxy).await?;

    let realm_explorer = connect_to_realm_explorer(&rcs_proxy).await?;
    let moniker = get_cml_moniker_from_query(&cmd.query, &realm_explorer).await?;

    destroy_impl(lifecycle_controller, moniker, &mut std::io::stdout()).await
}

async fn destroy_impl<W: std::io::Write>(
    lifecycle_controller: fsys::LifecycleControllerProxy,
    moniker: AbsoluteMoniker,
    writer: &mut W,
) -> Result<()> {
    writeln!(writer, "Moniker: {}", moniker)?;
    writeln!(writer, "Destroying component instance...")?;

    let parent = moniker.parent().ok_or(ffx_error!("Error: {} is not a valid moniker", moniker))?;
    let leaf = moniker.leaf().ok_or(ffx_error!("Error: {} is not a valid moniker", moniker))?;
    let child_name = leaf.name();
    let collection = leaf
        .collection()
        .ok_or(ffx_error!("Error: {} does not reference a dynamic instance", moniker))?;

    // Convert the absolute moniker into a relative moniker w.r.t. root.
    // LifecycleController expects relative monikers only.
    let parent_relative = RelativeMoniker::scope_down(&AbsoluteMoniker::root(), &parent).unwrap();

    destroy_instance_in_collection(&lifecycle_controller, &parent_relative, collection, child_name)
        .await
        .map_err(|e| format_destroy_error(&moniker, e))?;

    writeln!(writer, "Destroyed component instance!")?;
    Ok(())
}
////////////////////////////////////////////////////////////////////////////////
// tests

#[cfg(test)]
mod test {
    use {
        super::*, fidl::endpoints::create_proxy_and_stream, futures::TryStreamExt,
        moniker::AbsoluteMonikerBase,
    };

    fn setup_fake_lifecycle_controller(
        expected_moniker: &'static str,
        expected_collection: &'static str,
        expected_name: &'static str,
    ) -> fsys::LifecycleControllerProxy {
        let (lifecycle_controller, mut stream) =
            create_proxy_and_stream::<fsys::LifecycleControllerMarker>().unwrap();
        fuchsia_async::Task::local(async move {
            let req = stream.try_next().await.unwrap().unwrap();
            match req {
                fsys::LifecycleControllerRequest::DestroyInstance {
                    parent_moniker,
                    child,
                    responder,
                } => {
                    assert_eq!(expected_moniker, parent_moniker);
                    assert_eq!(expected_collection, child.collection.unwrap());
                    assert_eq!(expected_name, child.name);
                    responder.send(&mut Ok(())).unwrap();
                }
                _ => panic!("Unexpected Lifecycle Controller request"),
            }
        })
        .detach();
        lifecycle_controller
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_success() -> Result<()> {
        let mut output = Vec::new();
        let lifecycle_controller =
            setup_fake_lifecycle_controller("./core", "ffx-laboratory", "test");
        let response = destroy_impl(
            lifecycle_controller,
            AbsoluteMoniker::parse_str("/core/ffx-laboratory:test").unwrap(),
            &mut output,
        )
        .await;
        response.unwrap();
        Ok(())
    }
}
