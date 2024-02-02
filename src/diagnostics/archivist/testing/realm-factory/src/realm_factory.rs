// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use anyhow::*;
use fidl::endpoints::DiscoverableProtocolMarker;
use fidl_fuchsia_archivist_test::*;
use fidl_fuchsia_boot as fboot;
use fidl_fuchsia_diagnostics as fdiagnostics;
use fidl_fuchsia_inspect as finspect;
use fidl_fuchsia_logger as flogger;
use fidl_fuchsia_tracing_provider as ftracing;
use fuchsia_component_test::{
    Capability, ChildOptions, RealmBuilder, RealmBuilderParams, RealmInstance, Ref, Route,
};

const ARCHIVIST_URL: &str = "#meta/archivist.cm";
const ARCHIVIST_WITH_KERNEL_LOG_URL: &str = "#meta/archivist-with-kernel-log.cm";
const PUPPET_URL: &str = "#meta/puppet.cm";

#[derive(Default)]
pub(crate) struct ArchivistRealmFactory;

impl ArchivistRealmFactory {
    pub async fn create_realm(&mut self, options: RealmOptions) -> Result<RealmInstance, Error> {
        let mut params = RealmBuilderParams::new();
        if let Some(realm_name) = options.realm_name {
            params = params.realm_name(realm_name);
        }
        let builder = RealmBuilder::with_params(params).await?;
        // This child test realm allows us to downscope the event stream offered
        // to archivist to the #test subtree. We need this because it's not possible
        // to downscope an event stream to the ref "self". See
        // https://fxbug.dev/42082439 for more information.
        let test_realm = builder.add_child_realm("test", ChildOptions::new()).await?;
        let archivist_url =
            options.archivist_config.unwrap_or(ArchivistConfig::Default).component_url();
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
            .capability(Capability::protocol::<finspect::InspectSinkMarker>())
            .capability(Capability::protocol::<flogger::LogMarker>());
        builder.add_route(parent_to_archivist.clone().from(Ref::parent()).to(&test_realm)).await?;
        test_realm
            .add_route(parent_to_archivist.clone().from(Ref::parent()).to(&archivist))
            .await?;
        test_realm
            .add_route(archivist_to_parent.clone().from(&archivist).to(Ref::parent()))
            .await?;
        // Route archivist's configuration from void so the values come from the package.
        test_realm
            .add_route(
                Route::new()
                    .capability(Capability::configuration("fuchsia.diagnostics.BindServices"))
                    .capability(Capability::configuration(
                        "fuchsia.diagnostics.LogsMaxCachedOriginalBytes",
                    ))
                    .capability(Capability::configuration(
                        "fuchsia.diagnostics.MaximumConcurrentSnapshotsPerReader",
                    ))
                    .capability(Capability::configuration("fuchsia.diagnostics.NumThreads"))
                    .capability(Capability::configuration("fuchsia.diagnostics.AllowSerialLogs"))
                    .capability(Capability::configuration("fuchsia.diagnostics.DenySerialLogs"))
                    .from(Ref::void())
                    .to(&archivist),
            )
            .await?;
        builder.add_route(archivist_to_parent.from(&test_realm).to(Ref::parent())).await?;
        builder
            .add_route(
                Route::new()
                    .capability(Capability::event_stream("directory_ready").with_scope(&test_realm))
                    .capability(
                        Capability::event_stream("capability_requested").with_scope(&test_realm),
                    )
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
        if let Some(puppet_decls) = options.puppets {
            for decl in puppet_decls {
                let from_puppet = Route::new().capability(
                    Capability::protocol::<PuppetMarker>().as_(decl.unique_protocol_alias()),
                );
                let from_test_realm = Route::new()
                    .capability(Capability::protocol_by_name(decl.unique_protocol_alias()));
                let puppet =
                    test_realm.add_child(&decl.name, PUPPET_URL, ChildOptions::new()).await?;
                test_realm
                    .add_route(
                        Route::new()
                            .capability(Capability::protocol::<fdiagnostics::LogSettingsMarker>())
                            .capability(Capability::protocol::<flogger::LogSinkMarker>())
                            .capability(Capability::protocol::<finspect::InspectSinkMarker>())
                            .capability(Capability::protocol::<flogger::LogMarker>())
                            .from(&archivist)
                            .to(&puppet),
                    )
                    .await?;
                test_realm.add_route(from_puppet.from(&puppet).to(Ref::parent())).await?;
                builder.add_route(from_test_realm.from(&test_realm).to(Ref::parent())).await?;
            }
        }
        Ok(builder.build().await?)
    }
}

trait ArchivistConfigExt {
    fn component_url(&self) -> String;
}

impl ArchivistConfigExt for ArchivistConfig {
    fn component_url(&self) -> String {
        match self {
            ArchivistConfig::Default => ARCHIVIST_URL,
            ArchivistConfig::WithKernelLog => ARCHIVIST_WITH_KERNEL_LOG_URL,
            ArchivistConfigUnknown!() => unreachable!(),
        }
        .to_string()
    }
}

trait PuppetDeclExt {
    // A unique alias for the puppet's protocol.
    //
    // The test suite connects to the puppet using this alias.
    fn unique_protocol_alias(&self) -> String;
}

impl PuppetDeclExt for PuppetDecl {
    fn unique_protocol_alias(&self) -> String {
        format!("{}.{}", PuppetMarker::PROTOCOL_NAME, self.name)
    }
}
