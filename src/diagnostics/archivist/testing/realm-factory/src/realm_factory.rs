// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::*;
use fidl::endpoints::DiscoverableProtocolMarker;
use fidl_fuchsia_archivist_test::*;
use fidl_fuchsia_boot as fboot;
use fidl_fuchsia_diagnostics as fdiagnostics;
use fidl_fuchsia_logger as flogger;
use fidl_fuchsia_testing_harness::OperationError;
use fidl_fuchsia_tracing_provider as ftracing;
use fuchsia_component_test::{
    Capability, ChildOptions, RealmBuilder, RealmBuilderParams, RealmInstance, Ref, Route,
};

const ARCHIVIST_URL: &str = "#meta/archivist.cm";
const ARCHIVIST_WITH_KERNEL_LOG_URL: &str = "#meta/archivist-with-kernel-log.cm";
const PUPPET_URL: &str = "#meta/puppet.cm";

#[derive(Default)]
pub(crate) struct ArchivistRealmFactory {
    realm_options: Option<RealmOptions>,
}

impl ArchivistRealmFactory {
    pub fn set_realm_options(&mut self, options: RealmOptions) -> Result<(), Error> {
        match self.realm_options {
            None => {
                self.realm_options.replace(options);
                Ok(())
            }
            Some(_) => Err(realm_proxy::Error::from(OperationError::Invalid).into()),
        }
    }

    pub async fn create_realm(&mut self) -> Result<RealmInstance, Error> {
        let Some(realm_options) = self.realm_options.take() else {
            bail!(realm_proxy::Error::from(OperationError::Invalid));
        };

        let mut params = RealmBuilderParams::new();
        if let Some(realm_name) = realm_options.realm_name {
            params = params.realm_name(realm_name);
        }

        let builder = RealmBuilder::with_params(params).await?;

        // This child test realm allows us to downscope the event stream offered
        // to archivist to the #test subtree. We need this because it's not possible
        // to downscope an event stream to the ref "self". See
        // https://fxbug.dev/132340 for more information.
        let test_realm = builder.add_child_realm("test", ChildOptions::new()).await?;

        let archivist_url = match realm_options.archivist_config.unwrap_or(ArchivistConfig::Default)
        {
            ArchivistConfig::Default => ARCHIVIST_URL,
            ArchivistConfig::WithKernelLog => ARCHIVIST_WITH_KERNEL_LOG_URL,
            ArchivistConfigUnknown!() => unreachable!(),
        };

        let archivist =
            test_realm.add_child("archivist", archivist_url, ChildOptions::new()).await?;

        // LINT.IfChange
        let parent_to_archivist = Route::new()
            .capability(Capability::protocol::<flogger::LogSinkMarker>())
            .capability(Capability::protocol::<ftracing::RegistryMarker>().optional())
            .capability(Capability::protocol::<fboot::ReadOnlyLogMarker>());
        // LINT.ThenChange(//src/diagnostics/archivist/testing/realm-factory/meta/realm-factory.cml)

        let archivist_to_parent = Route::new()
            .capability(Capability::protocol::<fdiagnostics::ArchiveAccessorMarker>())
            .capability(Capability::protocol::<fdiagnostics::LogSettingsMarker>())
            .capability(Capability::protocol::<flogger::LogSinkMarker>())
            .capability(Capability::protocol::<flogger::LogMarker>());

        builder.add_route(parent_to_archivist.clone().from(Ref::parent()).to(&test_realm)).await?;
        test_realm
            .add_route(parent_to_archivist.clone().from(Ref::parent()).to(&archivist))
            .await?;

        test_realm
            .add_route(archivist_to_parent.clone().from(&archivist).to(Ref::parent()))
            .await?;
        builder.add_route(archivist_to_parent.from(&test_realm).to(Ref::parent())).await?;

        builder
            .add_route(
                Route::new()
                    .capability(Capability::event_stream("directory_ready").with_scope(&test_realm))
                    .capability(Capability::event_stream("capability_requested"))
                    .from(Ref::parent())
                    .to(&test_realm),
            )
            .await?;

        test_realm
            .add_route(
                Route::new()
                    .capability(Capability::event_stream("directory_ready"))
                    .capability(Capability::event_stream("capability_requested"))
                    .from(Ref::parent())
                    .to(&archivist),
            )
            .await?;

        // Add the puppet components.
        if let Some(puppet_decls) = realm_options.puppets {
            let archvist_to_puppet = Route::new()
                .capability(Capability::protocol::<fdiagnostics::LogSettingsMarker>())
                .capability(Capability::protocol::<flogger::LogSinkMarker>())
                .capability(Capability::protocol::<flogger::LogMarker>());

            let puppet_protocol = Capability::protocol::<PuppetMarker>();

            for decl in puppet_decls {
                let puppet_name = decl.name;
                let puppet_protocol_alias =
                    format!("{}.{puppet_name}", PuppetMarker::PROTOCOL_NAME);
                let puppet =
                    test_realm.add_child(puppet_name, PUPPET_URL, ChildOptions::new()).await?;

                test_realm
                    .add_route(archvist_to_puppet.clone().from(&archivist).to(&puppet))
                    .await?;

                test_realm
                    .add_route(
                        Route::new()
                            .capability(puppet_protocol.clone())
                            .from(&puppet)
                            .to(Ref::parent()),
                    )
                    .await?;

                builder
                    .add_route(
                        Route::new()
                            .capability(puppet_protocol.clone().as_(puppet_protocol_alias))
                            .from(&test_realm)
                            .to(Ref::parent()),
                    )
                    .await?;
            }
        }

        Ok(builder.build().await?)
    }
}
