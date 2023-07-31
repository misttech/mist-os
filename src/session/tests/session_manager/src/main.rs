// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#[cfg(test)]
mod tests {
    use {
        diagnostics_hierarchy::DiagnosticsHierarchy,
        diagnostics_reader::{ArchiveReader, Inspect},
        fidl::endpoints::create_proxy,
        fidl_fuchsia_component as fcomponent, fidl_fuchsia_io as fio,
        fuchsia_component::client::connect_to_protocol,
        fuchsia_component_test::{ChildOptions, ChildRef, DirectoryContents, RealmBuilder},
        session_manager_lib,
    };

    const SESSION_URL: &'static str = "hello-world-session#meta/hello-world-session.cm";

    /// Passes if the root session launches successfully. This tells us:
    ///     - session_manager is able to use the Realm service to launch a component.
    ///     - the root session was started in the "session" collection.
    #[fuchsia::test]
    async fn launch_root_session() {
        let realm =
            connect_to_protocol::<fcomponent::RealmMarker>().expect("could not connect to Realm");

        let session_url = String::from(SESSION_URL);
        println!("Session url: {}", &session_url);

        // `launch_session()` requires an initial exposed-diretory request, so create, pass and
        // immediately close a `Directory` channel.
        let (_exposed_dir, exposed_dir_server_end) =
            create_proxy::<fio::DirectoryMarker>().unwrap();

        session_manager_lib::startup::launch_session(&session_url, exposed_dir_server_end, &realm)
            .await
            .expect("Failed to launch session");
    }

    // Add a `session_manager` component to the `RealmBuilder`, with the given
    // config options set.
    async fn add_session_manager(
        builder: &RealmBuilder,
        session_url: String,
        autolaunch: bool,
    ) -> anyhow::Result<ChildRef> {
        let child_ref = builder
            .add_child("session-manager", "#meta/session_manager.cm", ChildOptions::new().eager())
            .await?;
        builder.init_mutable_config_to_empty(&child_ref).await?;
        builder.set_config_value_string(&child_ref, "session_url", session_url).await?;
        builder.set_config_value_bool(&child_ref, "autolaunch", autolaunch).await?;
        Ok(child_ref)
    }

    async fn get_session_manager_inspect(
        realm: &fuchsia_component_test::RealmInstance,
        selector: &str,
    ) -> anyhow::Result<DiagnosticsHierarchy> {
        ArchiveReader::new()
            .add_selector(format!(
                "realm_builder\\:{}/session-manager:{}",
                realm.root.child_name(),
                selector
            ))
            .snapshot::<Inspect>()
            .await?
            .pop()
            .ok_or(anyhow::anyhow!("inspect data had no snapshot"))?
            .payload
            .ok_or(anyhow::anyhow!("inspect snapshot had no payload"))
    }

    #[fuchsia::test]
    async fn test_autolaunch_launches() -> anyhow::Result<()> {
        let builder = RealmBuilder::new().await?;

        add_session_manager(
            &builder,
            "hello-world-session#meta/hello-world-session.cm".to_string(),
            true,
        )
        .await?;

        let realm = builder.build().await?;

        // NOTE(hjfreyer): In theory there's a race condition here if
        // session_manager exports inspect data before launching the session.
        // I've run this many times and haven't seen it flake, but if it starts
        // flaking, it may have to do with that.
        let inspect = get_session_manager_inspect(&realm, "root/session_started_at").await?;

        // Assert the session has been launched once.
        assert_eq!(
            1,
            inspect
                .get_child("session_started_at")
                .expect("session_started_at is not none")
                .children
                .len()
        );

        realm.destroy().await?;
        Ok(())
    }

    #[fuchsia::test]
    async fn test_noautolaunch_does_not_launch() -> anyhow::Result<()> {
        let builder = RealmBuilder::new().await?;

        add_session_manager(
            &builder,
            "hello-world-session#meta/hello-world-session.cm".to_string(),
            false,
        )
        .await?;

        let realm = builder.build().await?;

        let inspect = get_session_manager_inspect(&realm, "root/session_started_at").await?;

        // No sessions should have launched.
        assert_eq!(0, inspect.get_child("session_started_at").unwrap().children.len());

        realm.destroy().await?;
        Ok(())
    }

    #[fuchsia::test]
    async fn noautolaunch_file_overrides_structured_config() -> anyhow::Result<()> {
        let builder = RealmBuilder::new().await?;

        let session_manager = add_session_manager(
            &builder,
            "hello-world-session#meta/hello-world-session.cm".to_string(),
            true,
        )
        .await?;

        builder
            .read_only_directory(
                "root-data",
                vec![&session_manager],
                DirectoryContents::new().add_file("session-manager/noautolaunch", ""),
            )
            .await?;

        let realm = builder.build().await?;

        let inspect = get_session_manager_inspect(&realm, "root/session_started_at").await?;

        // No sessions should have launched.
        assert_eq!(0, inspect.get_child("session_started_at").unwrap().children.len());

        realm.destroy().await?;
        Ok(())
    }
}
