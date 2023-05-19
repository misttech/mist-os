// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        cli::format::format_resolve_error, lifecycle::resolve_instance,
        query::get_cml_moniker_from_query,
    },
    anyhow::Result,
    fidl_fuchsia_sys2 as fsys,
    moniker::{AbsoluteMoniker, AbsoluteMonikerBase, RelativeMoniker, RelativeMonikerBase},
};

pub async fn resolve_cmd<W: std::io::Write>(
    query: String,
    lifecycle_controller: fsys::LifecycleControllerProxy,
    realm_query: fsys::RealmQueryProxy,
    mut writer: W,
) -> Result<()> {
    let moniker = get_cml_moniker_from_query(&query, &realm_query).await?;

    writeln!(writer, "Moniker: {}", moniker)?;
    writeln!(writer, "Resolving component instance...")?;

    // Convert the absolute moniker into a relative moniker w.r.t. root.
    // LifecycleController expects relative monikers only.
    let relative_moniker = RelativeMoniker::scope_down(&AbsoluteMoniker::root(), &moniker).unwrap();

    resolve_instance(&lifecycle_controller, &relative_moniker)
        .await
        .map_err(|e| format_resolve_error(&moniker, e))?;

    writeln!(writer, "Resolved component instance!")?;
    Ok(())
}

#[cfg(test)]
mod test {
    use {
        super::*, crate::test_utils::serve_realm_query_instances,
        fidl::endpoints::create_proxy_and_stream, futures::TryStreamExt,
    };

    fn setup_fake_lifecycle_controller(
        expected_moniker: &'static str,
    ) -> fsys::LifecycleControllerProxy {
        let (lifecycle_controller, mut stream) =
            create_proxy_and_stream::<fsys::LifecycleControllerMarker>().unwrap();
        fuchsia_async::Task::local(async move {
            let req = stream.try_next().await.unwrap().unwrap();
            match req {
                fsys::LifecycleControllerRequest::ResolveInstance { moniker, responder } => {
                    assert_eq!(expected_moniker, moniker);
                    responder.send(Ok(())).unwrap();
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
        let lifecycle_controller = setup_fake_lifecycle_controller("./core/ffx-laboratory:test");
        let realm_query = serve_realm_query_instances(vec![fsys::Instance {
            moniker: Some("./core/ffx-laboratory:test".to_string()),
            url: Some("fuchsia-pkg://fuchsia.com/test#meta/test.cml".to_string()),
            instance_id: None,
            resolved_info: None,
            ..Default::default()
        }]);
        let response = resolve_cmd(
            "/core/ffx-laboratory:test".to_string(),
            lifecycle_controller,
            realm_query,
            &mut output,
        )
        .await;
        response.unwrap();
        Ok(())
    }
}
