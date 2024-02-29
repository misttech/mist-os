// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::error::AvailabilityRoutingError,
    cm_rust::{Availability, ExposeDeclCommon, ExposeSource, OfferDeclCommon, OfferSource},
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
        let result = self.advance(offer.availability());
        if offer.source() == &OfferSource::Void
            && result == Err(AvailabilityRoutingError::TargetHasStrongerAvailability)
        {
            return Err(AvailabilityRoutingError::OfferFromVoidToRequiredTarget);
        }
        result
    }

    pub fn advance_with_expose(
        &mut self,
        expose: &dyn ExposeDeclCommon,
    ) -> Result<(), AvailabilityRoutingError> {
        let result = self.advance(expose.availability());
        if expose.source() == &ExposeSource::Void
            && result == Err(AvailabilityRoutingError::TargetHasStrongerAvailability)
        {
            return Err(AvailabilityRoutingError::ExposeFromVoidToRequiredTarget);
        }
        result
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
pub struct AvailabilityVisitor {
    pub state: AvailabilityState,
}

impl AvailabilityVisitor {
    pub fn new(availability: Availability) -> AvailabilityVisitor {
        AvailabilityVisitor { state: AvailabilityState(availability) }
    }
}

impl crate::legacy_router::OfferVisitor for AvailabilityVisitor {
    fn visit(&mut self, offer: &cm_rust::OfferDecl) -> Result<(), crate::RoutingError> {
        self.state.advance_with_offer(offer).map_err(Into::into)
    }
}

impl crate::legacy_router::ExposeVisitor for AvailabilityVisitor {
    fn visit(&mut self, expose: &cm_rust::ExposeDecl) -> Result<(), crate::RoutingError> {
        self.state.advance_with_expose(expose).map_err(Into::into)
    }
}

impl crate::legacy_router::CapabilityVisitor for AvailabilityVisitor {
    fn visit(&mut self, _: &cm_rust::CapabilityDecl) -> Result<(), crate::RoutingError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        cm_rust::{
            DependencyType, ExposeDecl, ExposeProtocolDecl, ExposeTarget, OfferDecl,
            OfferProtocolDecl, OfferTarget,
        },
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
    #[test_case(Availability::Optional, new_void_offer(), Ok(()))]
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
    #[test_case(Availability::Transitional, new_void_offer(), Ok(()))]
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
        Ok(())
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
        Ok(())
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
