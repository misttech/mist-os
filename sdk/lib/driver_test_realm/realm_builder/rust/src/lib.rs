// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::{Context as _, Result},
    fidl::endpoints::{DiscoverableProtocolMarker, ServiceMarker},
    fidl_fuchsia_driver_test as fdt, fidl_fuchsia_io as fio,
    fuchsia_component_test::{Capability, ChildOptions, RealmBuilder, RealmInstance, Ref, Route},
};

pub const COMPONENT_NAME: &str = "driver_test_realm";
pub const DRIVER_TEST_REALM_URL: &str = "#meta/driver_test_realm.cm";

#[async_trait::async_trait]
pub trait DriverTestRealmBuilder {
    /// Set up the DriverTestRealm component in the RealmBuilder realm.
    /// This configures proper input/output routing of capabilities.
    /// This takes a `manifest_url` to use, which is used by tests that need to
    /// specify a custom driver test realm.
    async fn driver_test_realm_manifest_setup(&self, manifest_url: &str) -> Result<&Self>;
    /// Set up the DriverTestRealm component in the RealmBuilder realm.
    /// This configures proper input/output routing of capabilities.
    async fn driver_test_realm_setup(&self) -> Result<&Self>;

    async fn driver_test_realm_add_expose<S: ServiceMarker>(&self) -> Result<&Self>;
    async fn driver_test_realm_add_offer<P: DiscoverableProtocolMarker>(
        &self,
        from: Ref,
    ) -> Result<&Self>;
}

#[async_trait::async_trait]
impl DriverTestRealmBuilder for RealmBuilder {
    async fn driver_test_realm_manifest_setup(&self, manifest_url: &str) -> Result<&Self> {
        let driver_realm =
            self.add_child(COMPONENT_NAME, manifest_url, ChildOptions::new().eager()).await?;
        self.add_route(
            Route::new()
                .capability(Capability::protocol_by_name("fuchsia.logger.LogSink"))
                .capability(Capability::protocol_by_name("fuchsia.process.Launcher"))
                .capability(Capability::protocol_by_name("fuchsia.sys.Launcher"))
                .from(Ref::parent())
                .to(&driver_realm),
        )
        .await?;
        self.add_route(
            Route::new()
                .capability(Capability::protocol_by_name(
                    "fuchsia.driver.development.DriverDevelopment",
                ))
                .capability(Capability::protocol_by_name(
                    "fuchsia.driver.registrar.DriverRegistrar",
                ))
                .capability(Capability::protocol_by_name("fuchsia.driver.test.Realm"))
                .capability(Capability::directory("dev-topological"))
                .capability(Capability::directory("dev-class"))
                .from(&driver_realm)
                .to(Ref::parent()),
        )
        .await?;
        Ok(&self)
    }

    async fn driver_test_realm_setup(&self) -> Result<&Self> {
        self.driver_test_realm_manifest_setup(DRIVER_TEST_REALM_URL).await
    }

    async fn driver_test_realm_add_expose<S: ServiceMarker>(&self) -> Result<&Self> {
        let mut decl = self.get_component_decl(COMPONENT_NAME).await?;
        decl.capabilities.push(cm_rust::CapabilityDecl::Service(cm_rust::ServiceDecl {
            name: S::SERVICE_NAME.parse().expect("service name is not a valid capability name"),
            source_path: Some(("/svc/".to_owned() + S::SERVICE_NAME).parse().unwrap()),
        }));
        decl.exposes.push(cm_rust::ExposeDecl::Service(cm_rust::ExposeServiceDecl {
            source: cm_rust::ExposeSource::Self_,
            source_name: S::SERVICE_NAME.parse().unwrap(),
            target_name: S::SERVICE_NAME.parse().unwrap(),
            target: cm_rust::ExposeTarget::Parent,
            availability: cm_rust::Availability::Required,
        }));
        self.replace_component_decl(COMPONENT_NAME, decl).await?;
        self.add_route(
            Route::new()
                .capability(Capability::service::<S>())
                .from(Ref::child(COMPONENT_NAME))
                .to(Ref::parent()),
        )
        .await?;
        Ok(&self)
    }

    async fn driver_test_realm_add_offer<P: DiscoverableProtocolMarker>(
        &self,
        from: Ref,
    ) -> Result<&Self> {
        let mut decl = self.get_component_decl(COMPONENT_NAME).await?;
        decl.offers.push(cm_rust::OfferDecl::Protocol(cm_rust::OfferProtocolDecl {
            source: cm_rust::OfferSource::Parent,
            source_name: P::PROTOCOL_NAME.parse().unwrap(),
            target_name: P::PROTOCOL_NAME.parse().unwrap(),
            target: cm_rust::OfferTarget::Collection("realm_builder".to_string()),
            dependency_type: cm_rust::DependencyType::Strong,
            availability: cm_rust::Availability::Required,
        }));
        self.replace_component_decl(COMPONENT_NAME, decl).await?;
        self.add_route(
            Route::new()
                .capability(Capability::protocol::<P>())
                .from(from)
                .to(Ref::child(COMPONENT_NAME)),
        )
        .await?;
        Ok(&self)
    }
}

#[async_trait::async_trait]
pub trait DriverTestRealmInstance {
    /// Connect to the DriverTestRealm in this Instance and call Start with `args`.
    async fn driver_test_realm_start(&self, args: fdt::RealmArgs) -> Result<()>;

    /// Connect to the /dev/ directory hosted by  DriverTestRealm in this Instance.
    fn driver_test_realm_connect_to_dev(&self) -> Result<fio::DirectoryProxy>;
}

#[async_trait::async_trait]
impl DriverTestRealmInstance for RealmInstance {
    async fn driver_test_realm_start(&self, args: fdt::RealmArgs) -> Result<()> {
        let config = self.root.connect_to_protocol_at_exposed_dir::<fdt::RealmMarker>()?;
        let () = config
            .start(args)
            .await
            .context("DriverTestRealm Start failed")?
            .map_err(fuchsia_zircon_status::Status::from_raw)
            .context("DriverTestRealm Start failed")?;
        Ok(())
    }

    fn driver_test_realm_connect_to_dev(&self) -> Result<fio::DirectoryProxy> {
        fuchsia_fs::directory::open_directory_no_describe(
            self.root.get_exposed_dir(),
            "dev-topological",
            fio::OpenFlags::empty(),
        )
        .map_err(Into::into)
    }
}
