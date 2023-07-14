// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    cm_rust::{ExposeDeclCommon, NativeIntoFidl, OfferDeclCommon},
    fidl::endpoints::{ClientEnd, DiscoverableProtocolMarker, ServerEnd},
    fidl_fuchsia_component as fcomponent,
    fidl_fuchsia_component_decl::Offer,
    fidl_fuchsia_io as fio, fidl_fuchsia_sys2 as fsys,
    moniker::Moniker,
    thiserror::Error,
};
const CAPABILITY_REQUESTED_EVENT: &str = "capability_requested";
const DIR_READY_EVENT: &str = "directory_ready";

#[derive(Debug, Error)]
pub enum RealmValidationError {
    #[error("Realm should expose {}", fcomponent::RealmMarker::PROTOCOL_NAME)]
    RealmProtocol,

    #[error("Realm should offer {} event stream to the test collection", DIR_READY_EVENT)]
    DirectoryReady,

    #[error(
        "Realm should offer {} event stream to the test collection",
        CAPABILITY_REQUESTED_EVENT
    )]
    CapabilityRequested,

    #[error("The realm does not contain '{0}' named collection")]
    TestCollectionNotFound(String),
}

#[derive(Debug, Error)]
pub enum RealmError {
    #[error(transparent)]
    Fidl(#[from] fidl::Error),

    #[error(transparent)]
    Validation(#[from] RealmValidationError),

    #[error("Invalid realm, it should contain test collection: /realm:collection")]
    InvalidRealmStr,

    #[error("cannot resolve provided realm: {0:?}")]
    InstanceNotResolved(component_debug::lifecycle::ResolveError),

    #[error(transparent)]
    BadMoniker(#[from] moniker::MonikerError),

    #[error("Cannot connect to exposed directory: {0:?}")]
    ConnectExposedDir(fsys::OpenError),

    #[error(transparent)]
    GetManifest(#[from] component_debug::realm::GetDeclarationError),
}

pub struct Realm {
    exposed_dir: fio::DirectoryProxy,
    offers: Vec<fidl_fuchsia_component_decl::Offer>,
    realm_str: String,
    test_collection: String,
}

impl std::fmt::Debug for Realm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_fmt(format_args!("realm for moniker {}:{}", self.realm_str, self.test_collection))
    }
}

impl PartialEq for Realm {
    fn eq(&self, other: &Self) -> bool {
        self.realm_str == other.realm_str && self.test_collection == other.test_collection
    }
}

impl Realm {
    pub fn get_realm_client(&self) -> Result<ClientEnd<fcomponent::RealmMarker>, fidl::Error> {
        let (realm_client, server_end) =
            fidl::endpoints::create_endpoints::<fcomponent::RealmMarker>();
        let server_end = ServerEnd::new(server_end.into_channel());
        self.exposed_dir.open(
            fio::OpenFlags::RIGHT_READABLE,
            fio::ModeType::empty(),
            fcomponent::RealmMarker::PROTOCOL_NAME,
            server_end,
        )?;
        Ok(realm_client)
    }

    pub fn offers(&self) -> Vec<fidl_fuchsia_component_decl::Offer> {
        self.offers.clone()
    }

    pub fn collection<'a>(&'a self) -> &'a str {
        self.test_collection.as_str()
    }
}

fn validate_and_get_offers(
    manifest: cm_rust::ComponentDecl,
    test_collection: &str,
) -> Result<Vec<Offer>, RealmValidationError> {
    let collection_found = manifest.collections.iter().any(|c| c.name == test_collection);
    if !collection_found {
        return Err(RealmValidationError::TestCollectionNotFound(test_collection.to_string()));
    }

    let exposes_realm_protocol =
        manifest.exposes.iter().any(|e| *e.target_name() == fcomponent::RealmMarker::PROTOCOL_NAME);
    if !exposes_realm_protocol {
        return Err(RealmValidationError::RealmProtocol);
    }

    let mut directory_ready = false;
    let mut capability_requested = false;
    let mut offers = vec![];
    for offer in manifest.offers {
        if let cm_rust::OfferTarget::Collection(collection) = &offer.target() {
            if collection.as_str() != test_collection {
                continue;
            }

            if let cm_rust::OfferDecl::EventStream(cm_rust::OfferEventStreamDecl {
                target_name,
                source,
                scope,
                ..
            }) = &offer
            {
                if (*target_name == CAPABILITY_REQUESTED_EVENT || *target_name == DIR_READY_EVENT)
                    && source == &cm_rust::OfferSource::Parent
                    && scope
                        .as_ref()
                        .map(|s| {
                            s.iter().any(|s| match s {
                                cm_rust::EventScope::Collection(s) => s.as_str() == test_collection,
                                _ => false,
                            })
                        })
                        .unwrap_or(false)
                {
                    directory_ready = directory_ready || *target_name == DIR_READY_EVENT;
                    capability_requested =
                        capability_requested || *target_name == CAPABILITY_REQUESTED_EVENT;
                }
            }
            offers.push(offer.native_into_fidl());
        }
    }
    if !directory_ready {
        return Err(RealmValidationError::DirectoryReady);
    }
    if !capability_requested {
        return Err(RealmValidationError::CapabilityRequested);
    }
    Ok(offers)
}

pub async fn parse_provided_realm(
    lifecycle_controller: &fsys::LifecycleControllerProxy,
    realm_query: &fsys::RealmQueryProxy,
    realm_str: &str,
) -> Result<Realm, RealmError> {
    let (moniker, test_collection) = match realm_str.rsplit_once(':') {
        Some(s) => s,
        None => {
            return Err(RealmError::InvalidRealmStr);
        }
    };
    let moniker = Moniker::try_from(moniker)?;

    component_debug::lifecycle::resolve_instance(&lifecycle_controller, &moniker)
        .await
        .map_err(RealmError::InstanceNotResolved)?;

    let manifest = component_debug::realm::get_resolved_declaration(&moniker, &realm_query).await?;

    let offers = validate_and_get_offers(manifest, test_collection)?;

    let (exposed_dir, server_end) =
        fidl::endpoints::create_proxy::<fio::DirectoryMarker>().unwrap();
    let server_end = ServerEnd::new(server_end.into_channel());
    realm_query
        .open(
            &moniker.to_string(),
            fsys::OpenDirType::ExposedDir,
            fio::OpenFlags::RIGHT_READABLE,
            fio::ModeType::empty(),
            ".",
            server_end,
        )
        .await?
        .map_err(RealmError::ConnectExposedDir)?;

    Ok(Realm {
        exposed_dir,
        offers,
        realm_str: realm_str.to_string(),
        test_collection: test_collection.to_owned(),
    })
}

#[cfg(test)]
mod test {
    use super::*;
    use assert_matches::assert_matches;
    use cm_rust::FidlIntoNative;

    #[fuchsia::test]
    async fn valid_realm() {
        let lifecycle_controller =
            fuchsia_component::client::connect_to_protocol::<fsys::LifecycleControllerMarker>()
                .unwrap();
        let realm_query =
            fuchsia_component::client::connect_to_protocol::<fsys::RealmQueryMarker>().unwrap();
        let realm =
            parse_provided_realm(&lifecycle_controller, &realm_query, "/test_realm:echo_test_coll")
                .await
                .unwrap();

        assert_eq!(realm.test_collection, "echo_test_coll");
        assert_eq!(realm.realm_str, "/test_realm:echo_test_coll");

        let offers = realm.offers.into_iter().map(|o| o.fidl_into_native()).collect::<Vec<_>>();
        // LogSink & InspectSink are offered to all children
        assert_eq!(offers.len(), 5, "{:?}", offers);
        offers.iter().for_each(|o| {
            assert_eq!(
                o.target(),
                &cm_rust::OfferTarget::Collection("echo_test_coll".parse().unwrap())
            )
        });
        assert!(offers.iter().any(|o| *o.target_name() == CAPABILITY_REQUESTED_EVENT));
        assert!(offers.iter().any(|o| *o.target_name() == DIR_READY_EVENT));
        assert!(offers.iter().any(|o| *o.target_name() == "fidl.examples.routing.echo.Echo"));

        let realm = parse_provided_realm(
            &lifecycle_controller,
            &realm_query,
            "/test_realm:hermetic_test_coll",
        )
        .await
        .unwrap();

        assert_eq!(realm.test_collection, "hermetic_test_coll");
        assert_eq!(realm.realm_str, "/test_realm:hermetic_test_coll");

        let offers = realm.offers.into_iter().map(|o| o.fidl_into_native()).collect::<Vec<_>>();
        // LogSink & InspectSink are offered to all children.
        assert_eq!(offers.len(), 4, "{:?}", offers);
        offers.iter().for_each(|o| {
            assert_eq!(
                o.target(),
                &cm_rust::OfferTarget::Collection("hermetic_test_coll".parse().unwrap())
            )
        });
        assert!(offers.iter().any(|o| *o.target_name() == CAPABILITY_REQUESTED_EVENT));
        assert!(offers.iter().any(|o| *o.target_name() == DIR_READY_EVENT));
    }

    #[fuchsia::test]
    async fn invalid_realm() {
        let lifecycle_controller =
            fuchsia_component::client::connect_to_protocol::<fsys::LifecycleControllerMarker>()
                .unwrap();
        let realm_query =
            fuchsia_component::client::connect_to_protocol::<fsys::RealmQueryMarker>().unwrap();
        assert_matches!(
            parse_provided_realm(
                &lifecycle_controller,
                &realm_query,
                "/nonexistent_realm:test_coll"
            )
            .await,
            Err(RealmError::InstanceNotResolved(_))
        );

        assert_matches!(
            parse_provided_realm(&lifecycle_controller, &realm_query, "/test_realm").await,
            Err(RealmError::InvalidRealmStr)
        );

        assert_matches!(
            parse_provided_realm(&lifecycle_controller, &realm_query, "/test_realm:invalid_col")
                .await,
            Err(RealmError::Validation(RealmValidationError::TestCollectionNotFound(_)))
        );

        assert_matches!(
            parse_provided_realm(
                &lifecycle_controller,
                &realm_query,
                "/test_realm:no_directory_ready_event"
            )
            .await,
            Err(RealmError::Validation(RealmValidationError::DirectoryReady))
        );

        assert_matches!(
            parse_provided_realm(
                &lifecycle_controller,
                &realm_query,
                "/test_realm:no_capability_requested_event"
            )
            .await,
            Err(RealmError::Validation(RealmValidationError::CapabilityRequested))
        );

        assert_matches!(
            parse_provided_realm(
                &lifecycle_controller,
                &realm_query,
                "/no_realm_protocol_realm:hermetic_test_coll"
            )
            .await,
            Err(RealmError::Validation(RealmValidationError::RealmProtocol))
        );
    }
}
