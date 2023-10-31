// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::error::AvailabilityRoutingError,
    cm_rust::{
        Availability, DirectoryDecl, EventStreamDecl, ExposeDeclCommon, ExposeDirectoryDecl,
        ExposeProtocolDecl, ExposeRunnerDecl, ExposeServiceDecl, ExposeSource, OfferDeclCommon,
        OfferDirectoryDecl, OfferEventStreamDecl, OfferProtocolDecl, OfferRunnerDecl,
        OfferServiceDecl, OfferSource, OfferStorageDecl, ProtocolDecl, RunnerDecl, ServiceDecl,
        StorageDecl,
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

    pub fn advance(
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
            (Availability::SameAsTarget, _) => self.0 = *next_availability,

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
                self.0 = *next_availability,

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

#[derive(Debug, PartialEq, Eq, Clone)]
pub struct AvailabilityVisitor<O, E, C> {
    phantom_offer: std::marker::PhantomData<O>,
    phantom_expose: std::marker::PhantomData<E>,
    phantom_capability: std::marker::PhantomData<C>,

    pub state: AvailabilityState,
}

impl<O, E, C> AvailabilityVisitor<O, E, C> {
    pub fn new(availability: Availability) -> AvailabilityVisitor<O, E, C> {
        AvailabilityVisitor {
            phantom_offer: std::marker::PhantomData,
            phantom_expose: std::marker::PhantomData,
            phantom_capability: std::marker::PhantomData,
            state: AvailabilityState(availability),
        }
    }
}

impl<O, E, C> crate::router::OfferVisitor for AvailabilityVisitor<O, E, C> {
    fn visit(&mut self, offer: &cm_rust::OfferDecl) -> Result<(), crate::RoutingError> {
        self.state.advance_with_offer(offer).map_err(Into::into)
    }
}

impl<O, E, C> crate::router::ExposeVisitor for AvailabilityVisitor<O, E, C> {
    fn visit(&mut self, expose: &cm_rust::ExposeDecl) -> Result<(), crate::RoutingError> {
        self.state.advance_with_expose(expose).map_err(Into::into)
    }
}

impl<O, E, C> crate::router::CapabilityVisitor for AvailabilityVisitor<O, E, C> {
    fn visit(&mut self, _: &cm_rust::CapabilityDecl) -> Result<(), crate::RoutingError> {
        Ok(())
    }
}

pub type AvailabilityProtocolVisitor =
    AvailabilityVisitor<OfferProtocolDecl, ExposeProtocolDecl, ProtocolDecl>;

pub type AvailabilityServiceVisitor =
    AvailabilityVisitor<OfferServiceDecl, ExposeServiceDecl, ServiceDecl>;

pub type AvailabilityDirectoryVisitor =
    AvailabilityVisitor<OfferDirectoryDecl, ExposeDirectoryDecl, DirectoryDecl>;

pub type AvailabilityStorageVisitor = AvailabilityVisitor<OfferStorageDecl, (), StorageDecl>;

pub type AvailabilityEventStreamVisitor =
    AvailabilityVisitor<OfferEventStreamDecl, (), EventStreamDecl>;

pub type AvailabilityRunnerVisitor =
    AvailabilityVisitor<OfferRunnerDecl, ExposeRunnerDecl, RunnerDecl>;

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
            source_dictionary: None,
            target: OfferTarget::static_child("echo".to_string()),
            target_name: "fuchsia.examples.Echo".parse().unwrap(),
            dependency_type: DependencyType::Weak,
            availability,
        })
    }

    fn new_void_offer() -> OfferDecl {
        OfferDecl::Protocol(OfferProtocolDecl {
            source: OfferSource::Void,
            source_name: "fuchsia.examples.Echo".parse().unwrap(),
            source_dictionary: None,
            target: OfferTarget::static_child("echo".to_string()),
            target_name: "fuchsia.examples.Echo".parse().unwrap(),
            dependency_type: DependencyType::Weak,
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
            source_dictionary: None,
            target: ExposeTarget::Parent,
            target_name: "fuchsia.examples.Echo".parse().unwrap(),
            availability,
        })
    }

    fn new_void_expose() -> ExposeDecl {
        ExposeDecl::Protocol(ExposeProtocolDecl {
            source: ExposeSource::Void,
            source_name: "fuchsia.examples.Echo".parse().unwrap(),
            source_dictionary: None,
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
