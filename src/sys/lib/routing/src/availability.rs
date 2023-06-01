// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::error::AvailabilityRoutingError,
    crate::RouteBundle,
    cm_rust::{
        Availability, DirectoryDecl, EventStreamDecl, ExposeDeclCommon, ExposeDirectoryDecl,
        ExposeProtocolDecl, ExposeServiceDecl, ExposeSource, OfferDeclCommon, OfferDirectoryDecl,
        OfferEventStreamDecl, OfferProtocolDecl, OfferServiceDecl, OfferSource, OfferStorageDecl,
        ProtocolDecl, ServiceDecl, StorageDecl, UseDeclCommon,
    },
    std::convert::From,
};

/// Opaque availability type to define new traits like PartialOrd on.
#[derive(Debug, PartialEq, Eq, Clone)]
pub struct AvailabilityState(pub Availability);

/// Allows creating the availability walker from Availability
impl From<Availability> for AvailabilityState {
    fn from(availability: Availability) -> Self {
        AvailabilityState(availability)
    }
}

impl AvailabilityState {
    pub fn advance_with_offer(
        &mut self,
        offer: &dyn OfferDeclCommon,
    ) -> Result<(), AvailabilityRoutingError> {
        let next_availability = offer
            .availability()
            .expect("tried to check availability on an offer that doesn't have that field");
        if offer.source() == &OfferSource::Void {
            match self.advance(next_availability) {
                // Nb: Although an error is returned here, this specific error is ignored during validation
                // because it's acceptable for routing to fail due to an optional capability ending in an offer
                // from `void`.
                Ok(()) => Err(AvailabilityRoutingError::RouteFromVoidToOptionalTarget),
                Err(AvailabilityRoutingError::TargetHasStrongerAvailability) => {
                    Err(AvailabilityRoutingError::OfferFromVoidToRequiredTarget)
                }
                Err(e) => Err(e),
            }
        } else {
            self.advance(next_availability)
        }
    }

    pub fn advance_with_expose(
        &mut self,
        expose: &dyn ExposeDeclCommon,
    ) -> Result<(), AvailabilityRoutingError> {
        let next_availability = expose.availability();
        if expose.source() == &ExposeSource::Void {
            match self.advance(next_availability) {
                // Nb: Although an error is returned here, this specific error is ignored during validation
                // because it's acceptable for routing to fail due to an optional capability ending in an expose
                // from `void`.
                Ok(()) => Err(AvailabilityRoutingError::RouteFromVoidToOptionalTarget),
                Err(AvailabilityRoutingError::TargetHasStrongerAvailability) => {
                    Err(AvailabilityRoutingError::ExposeFromVoidToRequiredTarget)
                }
                Err(e) => Err(e),
            }
        } else {
            self.advance(next_availability)
        }
    }

    fn advance(
        &mut self,
        next_availability: &Availability,
    ) -> Result<(), AvailabilityRoutingError> {
        match (&self.0, &next_availability) {
            // `self` will be `SameAsTarget` when routing starts from an `Offer` or `Expose`. This
            // is to verify as much as possible the correctness of routes involving `Offer` and
            // `Expose` without full knowledge of the `use -> offer -> expose` chain.
            //
            // For the purpose of availability checking, we will skip any checks until we encounter
            // a route declaration that has a known availability.
            (Availability::SameAsTarget, _) => self.0 = next_availability.clone(),

            // If our availability doesn't change, there's nothing to do.
            (Availability::Required, Availability::Required)
            | (Availability::Optional, Availability::Optional)
            | (Availability::Transitional, Availability::Transitional)

            // If the next availability is explicitly a pass-through, there's nothing to do.
            | (Availability::Required, Availability::SameAsTarget)
            | (Availability::Optional, Availability::SameAsTarget)
            | (Availability::Transitional, Availability::SameAsTarget) => (),

            // Increasing the strength of availability as we travel toward the source is allowed.
            (Availability::Optional, Availability::Required)
            | (Availability::Transitional, Availability::Required)
            | (Availability::Transitional, Availability::Optional) =>
                self.0 = next_availability.clone(),

            // Decreasing the strength of availability is not allowed, as that could lead to
            // unsanctioned broken routes.
            (Availability::Optional, Availability::Transitional)
            | (Availability::Required, Availability::Transitional)
            | (Availability::Required, Availability::Optional) =>
                return Err(AvailabilityRoutingError::TargetHasStrongerAvailability),
        }
        Ok(())
    }
}

macro_rules! make_availability_visitor {
    ($name:ident, {
        $(OfferDecl => $offer_decl:ty,)*
        $(ExposeDecl => $expose_decl:ty,)*
        $(CapabilityDecl => $cap_decl:ty,)*
    }) => {
        #[derive(Debug, PartialEq, Eq, Clone)]
        pub struct $name(pub AvailabilityState);

        impl $name {
            pub fn new<U>(use_decl: &U) -> Self where U: UseDeclCommon {
                Self(use_decl.availability().clone().into())
            }

            pub fn new_from_offer<O>(offer_decl: &O) -> Self where O: OfferDeclCommon {
                Self(offer_decl.availability().unwrap_or(&Availability::Required).clone().into())
            }

            pub fn new_from_expose<E>(expose_decl: &E) -> Self where E: ExposeDeclCommon {
                Self(expose_decl.availability().clone().into())
            }

            pub fn new_from_expose_bundle<E>(bundle: &RouteBundle<E>) -> Self where E: ExposeDeclCommon + Clone {
                Self(bundle.availability().clone().into())
            }
        }

        $(
            impl $crate::router::OfferVisitor for $name {
                type OfferDecl = $offer_decl;

                fn visit(&mut self, offer: &Self::OfferDecl) -> Result<(), $crate::error::RoutingError> {
                    self.0.advance_with_offer(offer).map_err(Into::into)
                }
            }
        )*

        $(
            impl $crate::router::ExposeVisitor for $name {
                type ExposeDecl = $expose_decl;

                fn visit(&mut self, expose: &Self::ExposeDecl) -> Result<(), $crate::error::RoutingError> {
                    self.0.advance_with_expose(expose).map_err(Into::into)
                }
            }
        )*

        $(
            impl $crate::router::CapabilityVisitor for $name {
                type CapabilityDecl = $cap_decl;

                fn visit(
                    &mut self,
                    _decl: &Self::CapabilityDecl
                ) -> Result<(), $crate::error::RoutingError> {
                    Ok(())
                }
            }
        )*
    };
}

make_availability_visitor!(AvailabilityServiceVisitor, {
    OfferDecl => OfferServiceDecl,
    ExposeDecl => ExposeServiceDecl,
    CapabilityDecl => ServiceDecl,
});

make_availability_visitor!(AvailabilityProtocolVisitor, {
    OfferDecl => OfferProtocolDecl,
    ExposeDecl => ExposeProtocolDecl,
    CapabilityDecl => ProtocolDecl,
});

make_availability_visitor!(AvailabilityDirectoryVisitor, {
    OfferDecl => OfferDirectoryDecl,
    ExposeDecl => ExposeDirectoryDecl,
    CapabilityDecl => DirectoryDecl,
});

make_availability_visitor!(AvailabilityStorageVisitor, {
    OfferDecl => OfferStorageDecl,
    CapabilityDecl => StorageDecl,
});

make_availability_visitor!(AvailabilityEventStreamVisitor, {
    OfferDecl => OfferEventStreamDecl,
    CapabilityDecl => EventStreamDecl,
});

#[cfg(test)]
mod tests {
    use {
        super::*,
        cm_rust::{DependencyType, ExposeDecl, ExposeTarget, OfferDecl, OfferTarget},
        test_case::test_case,
    };

    fn new_offer(availability: Availability) -> OfferDecl {
        OfferDecl::Protocol(OfferProtocolDecl {
            source: OfferSource::Parent,
            source_name: "fuchsia.examples.Echo".parse().unwrap(),
            target: OfferTarget::static_child("echo".to_string()),
            target_name: "fuchsia.examples.Echo".parse().unwrap(),
            dependency_type: DependencyType::WeakForMigration,
            availability,
        })
    }

    fn new_void_offer() -> OfferDecl {
        OfferDecl::Protocol(OfferProtocolDecl {
            source: OfferSource::Void,
            source_name: "fuchsia.examples.Echo".parse().unwrap(),
            target: OfferTarget::static_child("echo".to_string()),
            target_name: "fuchsia.examples.Echo".parse().unwrap(),
            dependency_type: DependencyType::WeakForMigration,
            availability: Availability::Optional,
        })
    }

    #[test_case(Availability::Optional, new_offer(Availability::Optional), Ok(()))]
    #[test_case(Availability::Optional, new_offer(Availability::Required), Ok(()))]
    #[test_case(Availability::Optional, new_offer(Availability::SameAsTarget), Ok(()))]
    #[test_case(
        Availability::Optional,
        new_offer(Availability::Transitional),
        Err(AvailabilityRoutingError::TargetHasStrongerAvailability)
    )]
    #[test_case(
        Availability::Optional,
        new_void_offer(),
        Err(AvailabilityRoutingError::RouteFromVoidToOptionalTarget)
    )]
    #[test_case(
        Availability::Required,
        new_offer(Availability::Optional),
        Err(AvailabilityRoutingError::TargetHasStrongerAvailability)
    )]
    #[test_case(Availability::Required, new_offer(Availability::Required), Ok(()))]
    #[test_case(Availability::Required, new_offer(Availability::SameAsTarget), Ok(()))]
    #[test_case(
        Availability::Required,
        new_offer(Availability::Transitional),
        Err(AvailabilityRoutingError::TargetHasStrongerAvailability)
    )]
    #[test_case(
        Availability::Required,
        new_void_offer(),
        Err(AvailabilityRoutingError::OfferFromVoidToRequiredTarget)
    )]
    #[test_case(Availability::Transitional, new_offer(Availability::Optional), Ok(()))]
    #[test_case(Availability::Transitional, new_offer(Availability::Required), Ok(()))]
    #[test_case(Availability::Transitional, new_offer(Availability::SameAsTarget), Ok(()))]
    #[test_case(Availability::Transitional, new_offer(Availability::Transitional), Ok(()))]
    #[test_case(
        Availability::Transitional,
        new_void_offer(),
        Err(AvailabilityRoutingError::RouteFromVoidToOptionalTarget)
    )]
    fn offer_tests(
        availability: Availability,
        offer: OfferDecl,
        expected: Result<(), AvailabilityRoutingError>,
    ) {
        let mut current_state: AvailabilityState = availability.into();
        let actual = current_state.advance_with_offer(&offer);
        assert_eq!(actual, expected);
    }

    fn new_expose(availability: Availability) -> ExposeDecl {
        ExposeDecl::Protocol(ExposeProtocolDecl {
            source: ExposeSource::Self_,
            source_name: "fuchsia.examples.Echo".parse().unwrap(),
            target: ExposeTarget::Parent,
            target_name: "fuchsia.examples.Echo".parse().unwrap(),
            availability,
        })
    }

    fn new_void_expose() -> ExposeDecl {
        ExposeDecl::Protocol(ExposeProtocolDecl {
            source: ExposeSource::Void,
            source_name: "fuchsia.examples.Echo".parse().unwrap(),
            target: ExposeTarget::Parent,
            target_name: "fuchsia.examples.Echo".parse().unwrap(),
            availability: Availability::Optional,
        })
    }

    #[test_case(Availability::Optional, new_expose(Availability::Optional), Ok(()))]
    #[test_case(Availability::Optional, new_expose(Availability::Required), Ok(()))]
    #[test_case(Availability::Optional, new_expose(Availability::SameAsTarget), Ok(()))]
    #[test_case(
        Availability::Optional,
        new_expose(Availability::Transitional),
        Err(AvailabilityRoutingError::TargetHasStrongerAvailability)
    )]
    #[test_case(
        Availability::Optional,
        new_void_expose(),
        Err(AvailabilityRoutingError::RouteFromVoidToOptionalTarget)
    )]
    #[test_case(
        Availability::Required,
        new_expose(Availability::Optional),
        Err(AvailabilityRoutingError::TargetHasStrongerAvailability)
    )]
    #[test_case(Availability::Required, new_expose(Availability::Required), Ok(()))]
    #[test_case(Availability::Required, new_expose(Availability::SameAsTarget), Ok(()))]
    #[test_case(
        Availability::Required,
        new_expose(Availability::Transitional),
        Err(AvailabilityRoutingError::TargetHasStrongerAvailability)
    )]
    #[test_case(
        Availability::Required,
        new_void_expose(),
        Err(AvailabilityRoutingError::ExposeFromVoidToRequiredTarget)
    )]
    #[test_case(Availability::Transitional, new_expose(Availability::Optional), Ok(()))]
    #[test_case(Availability::Transitional, new_expose(Availability::Required), Ok(()))]
    #[test_case(Availability::Transitional, new_expose(Availability::SameAsTarget), Ok(()))]
    #[test_case(Availability::Transitional, new_expose(Availability::Transitional), Ok(()))]
    #[test_case(
        Availability::Transitional,
        new_void_expose(),
        Err(AvailabilityRoutingError::RouteFromVoidToOptionalTarget)
    )]
    fn expose_tests(
        availability: Availability,
        expose: ExposeDecl,
        expected: Result<(), AvailabilityRoutingError>,
    ) {
        let mut current_state: AvailabilityState = availability.into();
        let actual = current_state.advance_with_expose(&expose);
        assert_eq!(actual, expected);
    }
}
