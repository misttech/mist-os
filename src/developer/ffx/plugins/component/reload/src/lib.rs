// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::Result,
    component_debug::lifecycle::{resolve_instance, start_instance, unresolve_instance},
    ffx_component::{
        format::{format_action_error, format_resolve_error, format_start_error},
        query::get_cml_moniker_from_query,
        rcs::{connect_to_lifecycle_controller, connect_to_realm_explorer},
    },
    ffx_component_reload_args::ReloadComponentCommand,
    ffx_core::ffx_plugin,
    fidl_fuchsia_developer_remotecontrol as rc, fidl_fuchsia_sys2 as fsys,
    moniker::{AbsoluteMoniker, AbsoluteMonikerBase, RelativeMoniker, RelativeMonikerBase},
};

#[ffx_plugin()]
pub async fn reload(rcs_proxy: rc::RemoteControlProxy, cmd: ReloadComponentCommand) -> Result<()> {
    let lifecycle_controller = connect_to_lifecycle_controller(&rcs_proxy).await?;

    let realm_explorer = connect_to_realm_explorer(&rcs_proxy).await?;
    let moniker = get_cml_moniker_from_query(&cmd.query, &realm_explorer).await?;

    println!("Moniker: {}", moniker);

    reload_impl(lifecycle_controller, moniker, &mut std::io::stdout()).await
}

async fn reload_impl<W: std::io::Write>(
    lifecycle_controller: fsys::LifecycleControllerProxy,
    moniker: AbsoluteMoniker,
    writer: &mut W,
) -> Result<()> {
    // Convert the absolute moniker into a relative moniker w.r.t. root.
    // LifecycleController expects relative monikers only.
    let moniker_relative = RelativeMoniker::scope_down(&AbsoluteMoniker::root(), &moniker).unwrap();

    writeln!(writer, "Unresolving component instance...")?;
    unresolve_instance(&lifecycle_controller, &moniker_relative)
        .await
        .map_err(|e| format_action_error(&moniker, e))?;

    writeln!(writer, "Resolving component instance...")?;
    resolve_instance(&lifecycle_controller, &moniker_relative)
        .await
        .map_err(|e| format_resolve_error(&moniker, e))?;

    writeln!(writer, "Starting component instance...")?;
    start_instance(&lifecycle_controller, &moniker_relative)
        .await
        .map_err(|e| format_start_error(&moniker, e))?;

    writeln!(writer, "Reloaded component instance!")?;
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
    ) -> fsys::LifecycleControllerProxy {
        let (lifecycle_controller, mut stream) =
            create_proxy_and_stream::<fsys::LifecycleControllerMarker>().unwrap();

        fuchsia_async::Task::local(async move {
            // Expect 3 requests: Unresolve, Resolve, Start.
            match stream.try_next().await.unwrap().unwrap() {
                fsys::LifecycleControllerRequest::UnresolveInstance { moniker, responder } => {
                    assert_eq!(expected_moniker, moniker);
                    responder.send(&mut Ok(())).unwrap();
                }
                r => panic!(
                    "Unexpected Lifecycle Controller request when expecting Unresolve: {:?}",
                    r
                ),
            }
            match stream.try_next().await.unwrap().unwrap() {
                fsys::LifecycleControllerRequest::ResolveInstance { moniker, responder } => {
                    assert_eq!(expected_moniker, moniker);
                    responder.send(&mut Ok(())).unwrap();
                }
                r => {
                    panic!(
                        "Unexpected Lifecycle Controller request when expecting Resolve: {:?}",
                        r
                    )
                }
            }
            match stream.try_next().await.unwrap().unwrap() {
                fsys::LifecycleControllerRequest::StartInstance {
                    moniker,
                    binder: _,
                    responder,
                } => {
                    assert_eq!(expected_moniker, moniker);
                    responder.send(&mut Ok(())).unwrap();
                }
                r => {
                    panic!("Unexpected Lifecycle Controller request when expecting Start: {:?}", r)
                }
            }
        })
        .detach();
        lifecycle_controller
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_success() -> Result<()> {
        let mut output = Vec::new();
        let lifecycle_controller = setup_fake_lifecycle_controller("./core/ffx-laboratory:test");
        let response = reload_impl(
            lifecycle_controller,
            AbsoluteMoniker::parse_str("/core/ffx-laboratory:test").unwrap(),
            &mut output,
        )
        .await;
        response.unwrap();
        Ok(())
    }
}
