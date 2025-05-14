// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use async_trait::async_trait;
use cm_types::{LongName, Name};
use component_debug::lifecycle::{destroy_instance_in_collection, DestroyError};
use component_debug::query::get_cml_moniker_from_query;
use ffx_command_error::{user_error, Error};
use ffx_component::rcs::{connect_to_lifecycle_controller, connect_to_realm_query};
use ffx_component_destroy_args::DestroyComponentCommand;
use ffx_writer::{ToolIO, VerifiedMachineWriter};
use fho::{FfxContext, FfxMain, FfxTool};
use fidl_fuchsia_sys2 as fsys;
use moniker::Moniker;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::io::Write;
use target_holders::RemoteControlProxyHolder;

#[derive(FfxTool)]
pub struct DestroyTool {
    #[command]
    cmd: DestroyComponentCommand,
    rcs: RemoteControlProxyHolder,
}

fho::embedded_plugin!(DestroyTool);

#[derive(JsonSchema, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Output {
    Destroyed { moniker: String },
    NotDynamicInstanceError { moniker: String },
    GetMonikerFailed { message: String },
}

#[async_trait(?Send)]
impl FfxMain for DestroyTool {
    type Writer = VerifiedMachineWriter<Output>;
    async fn main(self, writer: Self::Writer) -> fho::Result<()> {
        let lifecycle_controller = connect_to_lifecycle_controller(&self.rcs).await?;
        let realm_query = connect_to_realm_query(&self.rcs).await?;

        destroy_cmd(
            &mut DefaultDestroyCmdImpl,
            self.cmd.query,
            lifecycle_controller,
            realm_query,
            writer,
        )
        .await?;
        Ok(())
    }
}

trait DestroyCmdImpl {
    async fn get_cml_moniker_from_query(
        &mut self,
        query: &str,
        realm_query: &fsys::RealmQueryProxy,
    ) -> anyhow::Result<Moniker>;

    async fn destroy_instance_in_collection(
        &mut self,
        lifecycle_controller: &fsys::LifecycleControllerProxy,
        parent: &Moniker,
        collection: &Name,
        child_name: &LongName,
    ) -> Result<(), DestroyError>;
}

struct DefaultDestroyCmdImpl;

impl DestroyCmdImpl for DefaultDestroyCmdImpl {
    async fn get_cml_moniker_from_query(
        &mut self,
        query: &str,
        realm_query: &fsys::RealmQueryProxy,
    ) -> anyhow::Result<Moniker> {
        get_cml_moniker_from_query(query, realm_query).await
    }

    async fn destroy_instance_in_collection(
        &mut self,
        lifecycle_controller: &fsys::LifecycleControllerProxy,
        parent: &Moniker,
        collection: &Name,
        child_name: &LongName,
    ) -> Result<(), DestroyError> {
        destroy_instance_in_collection(lifecycle_controller, parent, collection, child_name).await
    }
}

async fn destroy_cmd(
    imp: &mut impl DestroyCmdImpl,
    query: String,
    lifecycle_controller: fsys::LifecycleControllerProxy,
    realm_query: fsys::RealmQueryProxy,
    mut writer: VerifiedMachineWriter<Output>,
) -> Result<(), Error> {
    let moniker = imp.get_cml_moniker_from_query(&query, &realm_query).await.map_err(|i| {
        let _ = writer.machine(&Output::GetMonikerFailed { message: format!("{i}") });
        Error::Unexpected(i.into())
    })?;

    if !writer.is_machine() {
        writeln!(writer, "Moniker: {}", moniker).bug()?;
        writeln!(writer, "Destroying component instance...").bug()?;
    }

    let parent = moniker.parent().ok_or_else(|| {
        let _ = writer.machine(&Output::NotDynamicInstanceError { moniker: moniker.to_string() });
        user_error!("{} does not reference a dynamic instance", moniker)
    })?;
    let leaf = moniker.leaf().ok_or_else(|| {
        let _ = writer.machine(&Output::NotDynamicInstanceError { moniker: moniker.to_string() });
        user_error!("{} does not reference a dynamic instance", moniker)
    })?;
    let child_name = leaf.name();
    let collection = leaf.collection().ok_or_else(|| {
        let _ = writer.machine(&Output::NotDynamicInstanceError { moniker: moniker.to_string() });
        user_error!("{} does not reference a dynamic instance", moniker)
    })?;

    imp.destroy_instance_in_collection(&lifecycle_controller, &parent, collection, child_name)
        .await
        .bug()?;

    writer
        .machine_or(
            &Output::Destroyed { moniker: moniker.to_string() },
            "Destroyed component instance.".to_owned(),
        )
        .bug()?;
    Ok(())
}

#[cfg(test)]
mod test {
    use super::*;
    use fidl::endpoints::Proxy;

    #[fuchsia::test]
    async fn test_delete() {
        struct Impl;

        impl DestroyCmdImpl for Impl {
            async fn get_cml_moniker_from_query(
                &mut self,
                _query: &str,
                _realm_query: &fsys::RealmQueryProxy,
            ) -> anyhow::Result<Moniker> {
                Ok(Moniker::parse_str("core/foo:foo").unwrap())
            }

            async fn destroy_instance_in_collection(
                &mut self,
                _lifecycle_controller: &fsys::LifecycleControllerProxy,
                _parent: &Moniker,
                _collection: &Name,
                _child_name: &LongName,
            ) -> Result<(), DestroyError> {
                Ok(())
            }
        }

        let (ch, _) = fidl::Channel::create();
        let ch = fidl::AsyncChannel::from_channel(ch);
        let lifecycle_controller = fsys::LifecycleControllerProxy::from_channel(ch);
        let (ch, _) = fidl::Channel::create();
        let ch = fidl::AsyncChannel::from_channel(ch);
        let realm_query = fsys::RealmQueryProxy::from_channel(ch);

        let buffers = ffx_writer::TestBuffers::default();
        let writer =
            VerifiedMachineWriter::new_test(Some(ffx_writer::Format::JsonPretty), &buffers);

        destroy_cmd(&mut Impl, "foo".to_owned(), lifecycle_controller, realm_query, writer)
            .await
            .unwrap();
        let (out, err) = buffers.into_strings();
        assert!(err.is_empty());
        assert_eq!("{\n  \"destroyed\": {\n    \"moniker\": \"core/foo:foo\"\n  }\n}", &out);
    }

    #[fuchsia::test]
    async fn test_root_moniker() {
        struct Impl;

        impl DestroyCmdImpl for Impl {
            async fn get_cml_moniker_from_query(
                &mut self,
                _query: &str,
                _realm_query: &fsys::RealmQueryProxy,
            ) -> anyhow::Result<Moniker> {
                Ok(Moniker::parse_str("foo").unwrap())
            }

            async fn destroy_instance_in_collection(
                &mut self,
                _lifecycle_controller: &fsys::LifecycleControllerProxy,
                _parent: &Moniker,
                _collection: &Name,
                _child_name: &LongName,
            ) -> Result<(), DestroyError> {
                Ok(())
            }
        }

        let (ch, _) = fidl::Channel::create();
        let ch = fidl::AsyncChannel::from_channel(ch);
        let lifecycle_controller = fsys::LifecycleControllerProxy::from_channel(ch);
        let (ch, _) = fidl::Channel::create();
        let ch = fidl::AsyncChannel::from_channel(ch);
        let realm_query = fsys::RealmQueryProxy::from_channel(ch);

        let buffers = ffx_writer::TestBuffers::default();
        let writer =
            VerifiedMachineWriter::new_test(Some(ffx_writer::Format::JsonPretty), &buffers);

        let err =
            destroy_cmd(&mut Impl, "foo".to_owned(), lifecycle_controller, realm_query, writer)
                .await
                .unwrap_err();
        let Error::User(err) = err else { panic!() };
        assert_eq!("foo does not reference a dynamic instance", &err.to_string());

        let (out, err) = buffers.into_strings();
        assert!(err.is_empty());
        assert_eq!(
            "{\n  \"not_dynamic_instance_error\": {\n    \"moniker\": \"foo\"\n  }\n}",
            &out
        );
    }
}
