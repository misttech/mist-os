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
const PUPPET_URL: &str = "puppet#meta/puppet.cm";

#[derive(Default)]
pub(crate) struct ArchivistRealmFactory;

impl ArchivistRealmFactory {
    pub async fn create_realm(&mut self, options: RealmOptions) -> Result<RealmInstance, Error> {
        let mut params = RealmBuilderParams::new();
        if let Some(realm_name) = options.realm_name {
            params = params.realm_name(realm_name);
        }
        let builder = RealmBuilder::with_params(params).await?;
        let config = options.archivist_config.unwrap_or(ArchivistConfig::default());

        // The following configurations are tweakble.
        builder
            .add_capability(cm_rust::CapabilityDecl::Config(cm_rust::ConfigurationDecl {
                name: "fuchsia.diagnostics.EnableKlog".parse()?,
                value: cm_rust::ConfigValue::Single(cm_rust::ConfigSingleValue::Bool(
                    config.enable_klog.unwrap_or(false),
                )),
            }))
            .await?;
        if let Some(logs_max_cached_original_bytes) = config.logs_max_cached_original_bytes {
            builder
                .add_capability(cm_rust::CapabilityDecl::Config(cm_rust::ConfigurationDecl {
                    name: "fuchsia.diagnostics.LogsMaxCachedOriginalBytes".parse()?,
                    value: cm_rust::ConfigValue::Single(cm_rust::ConfigSingleValue::Uint64(
                        logs_max_cached_original_bytes,
                    )),
                }))
                .await?;
        }
        builder
            .add_capability(cm_rust::CapabilityDecl::Config(cm_rust::ConfigurationDecl {
                name: "fuchsia.diagnostics.PipelinesPath".parse()?,
                value: cm_rust::ConfigValue::Single(cm_rust::ConfigSingleValue::String(
                    config.pipelines_path.unwrap_or("/pkg/data/config".to_string()),
                )),
            }))
            .await?;

        // This child test realm allows us to downscope the event stream offered
        // to archivist to the #test subtree. We need this because it's not possible
        // to downscope an event stream to the ref "self". See
        // https://fxbug.dev/42082439 for more information.
        let test_realm = builder.add_child_realm("test", ChildOptions::new()).await?;
        let archivist =
            test_realm.add_child("archivist", ARCHIVIST_URL, ChildOptions::new()).await?;
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
        let mut self_to_archivist = Route::new()
            .capability(Capability::configuration("fuchsia.diagnostics.EnableKlog"))
            .capability(Capability::configuration("fuchsia.diagnostics.PipelinesPath"));

        // The following config capabilities are routed from "void" to archivist since we use the
        // package default config values for them.
        let mut void_to_archivist = Route::new()
            .capability(Capability::configuration("fuchsia.diagnostics.BindServices"))
            .capability(Capability::configuration(
                "fuchsia.diagnostics.MaximumConcurrentSnapshotsPerReader",
            ))
            .capability(Capability::configuration("fuchsia.diagnostics.NumThreads"))
            .capability(Capability::configuration("fuchsia.diagnostics.AllowSerialLogs"))
            .capability(Capability::configuration("fuchsia.diagnostics.DenySerialLogs"))
            .capability(Capability::configuration("fuchsia.diagnostics.LogToDebuglog"));

        let logs_max_cached_original_bytes_config_capability =
            Capability::configuration("fuchsia.diagnostics.LogsMaxCachedOriginalBytes");
        if config.logs_max_cached_original_bytes.is_none() {
            void_to_archivist =
                void_to_archivist.capability(logs_max_cached_original_bytes_config_capability);
        } else {
            self_to_archivist =
                self_to_archivist.capability(logs_max_cached_original_bytes_config_capability);
        }

        test_realm.add_route(void_to_archivist.from(Ref::void()).to(&archivist)).await?;

        builder.add_route(parent_to_archivist.clone().from(Ref::parent()).to(&test_realm)).await?;
        builder.add_route(self_to_archivist.clone().from(Ref::self_()).to(&test_realm)).await?;
        test_realm.add_route(parent_to_archivist.from(Ref::parent()).to(&archivist)).await?;
        test_realm.add_route(self_to_archivist.from(Ref::parent()).to(&archivist)).await?;

        test_realm
            .add_route(archivist_to_parent.clone().from(&archivist).to(Ref::parent()))
            .await?;
        builder.add_route(archivist_to_parent.from(&test_realm).to(Ref::parent())).await?;
        builder
            .add_route(
                Route::new()
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
                    .capability(Capability::event_stream("capability_requested"))
                    .from(Ref::parent())
                    .to(&archivist),
            )
            .await?;

        // Add the puppet components.
        if let Some(puppet_decls) = options.puppets {
            for decl in puppet_decls {
                let name = decl.name.clone().expect("puppet must have a name");
                let puppet = test_realm.add_child(&name, PUPPET_URL, ChildOptions::new()).await?;

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

                test_realm
                    .add_route(
                        Route::new()
                            .capability(
                                Capability::protocol::<PuppetMarker>()
                                    .as_(decl.unique_protocol_alias()),
                            )
                            .from(&puppet)
                            .to(Ref::parent()),
                    )
                    .await?;

                builder
                    .add_route(
                        Route::new()
                            .capability(Capability::protocol_by_name(decl.unique_protocol_alias()))
                            .from(&test_realm)
                            .to(Ref::parent()),
                    )
                    .await?;
            }
        }
        Ok(builder.build().await?)
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
        let name = self.name.as_ref().unwrap();
        format!("{}.{}", PuppetMarker::PROTOCOL_NAME, name)
    }
}
