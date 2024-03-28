// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::error::AvailabilityRoutingError,
    cm_rust::{Availability, ExposeDeclCommon, ExposeSource, OfferDeclCommon, OfferSource},
};

pub fn advance_with_offer(
    current: Availability,
    offer: &impl OfferDeclCommon,
) -> Result<Availability, AvailabilityRoutingError> {
    let result = advance(current, *offer.availability());
    if offer.source() == &OfferSource::Void
        && result == Err(AvailabilityRoutingError::TargetHasStrongerAvailability)
    {
        return Err(AvailabilityRoutingError::OfferFromVoidToRequiredTarget);
    }
    result
}

pub fn advance_with_expose(
    current: Availability,
    expose: &impl ExposeDeclCommon,
) -> Result<Availability, AvailabilityRoutingError> {
    let result = advance(current, *expose.availability());
    if expose.source() == &ExposeSource::Void
        && result == Err(AvailabilityRoutingError::TargetHasStrongerAvailability)
    {
        return Err(AvailabilityRoutingError::ExposeFromVoidToRequiredTarget);
    }
    result
}

impl crate::legacy_router::OfferVisitor for Availability {
    fn visit(&mut self, offer: &cm_rust::OfferDecl) -> Result<(), crate::RoutingError> {
        *self = advance_with_offer(*self, offer)?;
        Ok(())
    }
}

impl crate::legacy_router::ExposeVisitor for Availability {
    fn visit(&mut self, expose: &cm_rust::ExposeDecl) -> Result<(), crate::RoutingError> {
        *self = advance_with_expose(*self, expose)?;
        Ok(())
    }
}

impl crate::legacy_router::CapabilityVisitor for Availability {
    fn visit(&mut self, _: &cm_rust::CapabilityDecl) -> Result<(), crate::RoutingError> {
        Ok(())
    }
}

pub fn advance(
    current: Availability,
    next_availability: Availability,
) -> Result<Availability, AvailabilityRoutingError> {
    let next = availability::advance(current, next_availability)?;
    Ok(next)
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
            source_dictionary: Default::default(),
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
            source_dictionary: Default::default(),
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
        let actual = advance_with_offer(availability, &offer).map(|_| ());
        assert_eq!(actual, expected);
    }

    fn new_expose(availability: Availability) -> ExposeDecl {
        ExposeDecl::Protocol(ExposeProtocolDecl {
            source: ExposeSource::Self_,
            source_name: "fuchsia.examples.Echo".parse().unwrap(),
            source_dictionary: Default::default(),
            target: ExposeTarget::Parent,
            target_name: "fuchsia.examples.Echo".parse().unwrap(),
            availability,
        })
    }

    fn new_void_expose() -> ExposeDecl {
        ExposeDecl::Protocol(ExposeProtocolDecl {
            source: ExposeSource::Void,
            source_name: "fuchsia.examples.Echo".parse().unwrap(),
            source_dictionary: Default::default(),
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
        let actual = advance_with_expose(availability, &expose).map(|_| ());
        assert_eq!(actual, expected);
    }
}
