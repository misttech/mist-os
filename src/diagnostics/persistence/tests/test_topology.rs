// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::mock_filesystems::TestFs;
use fidl_fuchsia_component_sandbox as fsandbox;
use fidl_fuchsia_diagnostics::ArchiveAccessorMarker;
use fidl_fuchsia_inspect::InspectSinkMarker;
use fidl_fuchsia_logger::LogSinkMarker;
use fidl_fuchsia_update::ListenerMarker;
use fidl_test_persistence_factory::ControllerMarker;
use fuchsia_component_test::{
    Capability, ChildOptions, RealmBuilder, RealmBuilderParams, RealmInstance, Ref, Route,
};

const ARCHIVIST_URL: &str = "archivist-for-embedding#meta/archivist-for-embedding.cm";
const SINGLE_COUNTER_URL: &str = "#meta/single_counter_test_component.cm";
const PERSISTENCE_URL: &str = "#meta/persistence.cm";

pub async fn create(name: &str, fs: &TestFs) -> RealmInstance {
    let builder = RealmBuilder::with_params(RealmBuilderParams::new().realm_name(name))
        .await
        .expect("Failed to create realm builder");

    let single_counter = builder
        .add_child("single_counter", SINGLE_COUNTER_URL, ChildOptions::new())
        .await
        .expect("Failed to create single_counter");
    let persistence = builder
        .add_child("persistence", PERSISTENCE_URL, ChildOptions::new())
        .await
        .expect("Failed to create persistence");
    let config_server = crate::mock_filesystems::create_config_data(&builder)
        .await
        .expect("Failed to create filesystem from config");
    builder
        .add_route(
            Route::new()
                .capability(
                    Capability::directory("config-data")
                        .path("/config/data")
                        .rights(fidl_fuchsia_io::R_STAR_DIR),
                )
                .from(&config_server)
                .to(&persistence),
        )
        .await
        .expect("Failed to add route for /config/data directory");

    let cache_server = fs.serve_cache(&builder).await.expect("Failed to create cache server");
    builder
        .add_route(
            Route::new()
                .capability(
                    Capability::directory("cache")
                        .path("/cache")
                        .rights(fidl_fuchsia_io::RW_STAR_DIR),
                )
                .from(&cache_server)
                .to(&persistence),
        )
        .await
        .expect("Failed to add route for /cache directory");

    let update_server = crate::mock_fidl::handle_update_check_services(&builder)
        .await
        .expect("Failed to create update server");
    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol::<ListenerMarker>())
                .from(&update_server)
                .to(&persistence),
        )
        .await
        .expect("Failed to add route for fuchsia.update.Listener");
    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol::<ControllerMarker>())
                .from(&update_server)
                .to(Ref::parent()),
        )
        .await
        .expect("Failed to add route for fuchsia.update.Controller");

    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol_by_name(
                    "fuchsia.samplertestcontroller.SamplerTestController",
                ))
                .from(&single_counter)
                .to(Ref::parent()),
        )
        .await
        .expect("Failed to add route for fuchsia.samplertestcontroller.SamplerTestController");

    builder
        .add_route(
            Route::new()
                .capability(
                    Capability::protocol_by_name(
                        "fuchsia.diagnostics.persist.DataPersistence-test-service",
                    )
                    .as_("fuchsia.diagnostics.persist.DataPersistence"),
                )
                .from_dictionary("diagnostics-persist-capabilities")
                .from(&persistence)
                .to(Ref::parent()),
        )
        .await
        .expect("Failed to add route for fuchsia.diagnostics.persist.DataPersistence-test-service");

    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol::<fsandbox::CapabilityStoreMarker>())
                .from(Ref::framework())
                .to(&persistence),
        )
        .await
        .expect("Failed to add fuchsia.component.sandbox routes");

    let archivist = builder
        .add_child("archivist", ARCHIVIST_URL, ChildOptions::new().eager())
        .await
        .expect("Failed to create child archivist");

    builder
        .add_route(
            Route::new()
                .capability(
                    Capability::protocol_by_name("fuchsia.tracing.provider.Registry").optional(),
                )
                .capability(Capability::event_stream("capability_requested"))
                .from(Ref::parent())
                .to(&archivist),
        )
        .await
        .expect("Failed to add routes from parent to archivist");

    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol::<ArchiveAccessorMarker>())
                .from_dictionary("diagnostics-accessors")
                .from(&archivist)
                .to(Ref::parent()),
        )
        .await
        .expect("Failed to add route for ArchiveAccessor");

    builder
        .add_route(
            Route::new()
                .capability(
                    Capability::protocol::<ArchiveAccessorMarker>()
                        .as_("fuchsia.diagnostics.ArchiveAccessor.feedback"),
                )
                .from_dictionary("diagnostics-accessors")
                .from(&archivist)
                .to(&persistence),
        )
        .await
        .expect("Failed to add route for fuchsia.diagnostics.ArchiveAccessor");

    // Override the diagnostics dictionary which normally provides InspectSink
    // and LogSink capabilities from the top-level archivist.
    builder
        .add_capability(cm_rust::CapabilityDecl::Dictionary(cm_rust::DictionaryDecl {
            name: "diagnostics".parse().unwrap(),
            source_path: None,
        }))
        .await
        .expect("Failed to override the diagnostics dictionary");

    // Expose InspectSink from local archivist to isolate tests from each other.
    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol::<InspectSinkMarker>())
                .from(&archivist)
                .to(Ref::dictionary("self/diagnostics")),
        )
        .await
        .expect("Failed to add route for InspectSink");

    // Expose LogSink from parent to view component logs in the test output.
    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol::<LogSinkMarker>())
                .from_dictionary("diagnostics")
                .from(Ref::parent())
                .to(Ref::dictionary("self/diagnostics")),
        )
        .await
        .expect("Failed to add route for LogSink");

    builder
        .add_route(
            Route::new()
                .capability(Capability::dictionary("diagnostics"))
                .from(Ref::self_())
                .to(&single_counter)
                .to(&persistence),
        )
        .await
        .expect("Failed to add route for diagnostics dictionary");

    builder
        .add_route(
            Route::new()
                .capability(
                    Capability::protocol_by_name("fuchsia.component.Binder")
                        .as_("fuchsia.component.PersistenceBinder"),
                )
                .from(&persistence)
                .to(Ref::parent()),
        )
        .await
        .expect("Failed to add route for fuchsia.component.Binder");

    builder.build().await.expect("Failed to build test realm")
}
