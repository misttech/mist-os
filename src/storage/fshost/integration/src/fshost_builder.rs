// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    fidl_fuchsia_fshost as ffshost, fidl_fuchsia_io as fio, fidl_fuchsia_logger as flogger,
    fidl_fuchsia_process as fprocess, fidl_fuchsia_update_verify as ffuv,
    fuchsia_component_test::{Capability, ChildOptions, ChildRef, RealmBuilder, Ref, Route},
    std::collections::HashMap,
};

pub trait IntoValueSpec {
    fn into_value_spec(self) -> cm_rust::ConfigValueSpec;
}

impl IntoValueSpec for bool {
    fn into_value_spec(self) -> cm_rust::ConfigValueSpec {
        cm_rust::ConfigValueSpec {
            value: cm_rust::ConfigValue::Single(cm_rust::ConfigSingleValue::Bool(self)),
        }
    }
}

impl IntoValueSpec for u64 {
    fn into_value_spec(self) -> cm_rust::ConfigValueSpec {
        cm_rust::ConfigValueSpec {
            value: cm_rust::ConfigValue::Single(cm_rust::ConfigSingleValue::Uint64(self)),
        }
    }
}

impl IntoValueSpec for String {
    fn into_value_spec(self) -> cm_rust::ConfigValueSpec {
        cm_rust::ConfigValueSpec {
            value: cm_rust::ConfigValue::Single(cm_rust::ConfigSingleValue::String(self)),
        }
    }
}

impl<'a> IntoValueSpec for &'a str {
    fn into_value_spec(self) -> cm_rust::ConfigValueSpec {
        self.to_string().into_value_spec()
    }
}

/// Builder for the fshost component. This handles configuring the fshost component to use and
/// structured config overrides to set, as well as setting up the expected protocols to be routed
/// between the realm builder root and the fshost child when the test realm is built.
///
/// Any desired additional config overrides should be added to this builder. New routes for exposed
/// capabilities from the fshost component or offered capabilities to the fshost component should
/// be added to the [`FshostBuilder::build`] function below.
#[derive(Debug, Clone)]
pub struct FshostBuilder {
    component_name: &'static str,
    config_values: HashMap<&'static str, cm_rust::ConfigValueSpec>,
}

impl FshostBuilder {
    pub fn new(component_name: &'static str) -> FshostBuilder {
        FshostBuilder { component_name, config_values: HashMap::new() }
    }

    pub fn set_config_value(&mut self, key: &'static str, value: impl IntoValueSpec) -> &mut Self {
        assert!(
            self.config_values.insert(key, value.into_value_spec()).is_none(),
            "Attempted to insert duplicate config value '{}'!",
            key
        );
        self
    }

    pub(crate) async fn build(self, realm_builder: &RealmBuilder) -> ChildRef {
        let fshost_url = format!("#meta/{}.cm", self.component_name);
        tracing::info!(%fshost_url, "building test fshost instance");
        let fshost = realm_builder
            .add_child("test-fshost", fshost_url, ChildOptions::new().eager())
            .await
            .unwrap();

        realm_builder.init_mutable_config_from_package(&fshost).await.unwrap();

        // fshost config overrides
        for (key, value) in self.config_values {
            realm_builder.set_config_value(&fshost, key, value).await.unwrap()
        }

        realm_builder
            .add_route(
                Route::new()
                    .capability(Capability::protocol::<ffshost::AdminMarker>())
                    .capability(Capability::protocol::<ffshost::BlockWatcherMarker>())
                    .capability(Capability::protocol::<ffuv::BlobfsVerifierMarker>())
                    .capability(Capability::directory("blob").rights(fio::RW_STAR_DIR))
                    .capability(Capability::directory("data").rights(fio::RW_STAR_DIR))
                    .capability(Capability::directory("tmp").rights(fio::RW_STAR_DIR))
                    .from(&fshost)
                    .to(Ref::parent()),
            )
            .await
            .unwrap();

        realm_builder
            .add_route(
                Route::new()
                    .capability(Capability::protocol::<flogger::LogSinkMarker>())
                    .capability(Capability::protocol::<fprocess::LauncherMarker>())
                    .from(Ref::parent())
                    .to(&fshost),
            )
            .await
            .unwrap();

        realm_builder
            .add_route(
                Route::new()
                    .capability(Capability::directory("volumes").rights(fio::RW_STAR_DIR))
                    .from(&fshost)
                    .to(Ref::parent()),
            )
            .await
            .unwrap();

        fshost
    }
}
