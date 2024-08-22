// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Trait definition for matchers.

use alloc::string::String;
use core::fmt::Debug;

use net_types::ip::{IpAddress, Subnet};

use crate::DeviceWithName;

/// Common pattern to define a matcher for a metadata input `T`.
///
/// Used in matching engines like filtering and routing rules.
pub trait Matcher<T> {
    /// Returns whether the provided value matches.
    fn matches(&self, actual: &T) -> bool;

    /// Returns whether the provided value is set and matches.
    fn required_matches(&self, actual: Option<&T>) -> bool {
        actual.map_or(false, |actual| self.matches(actual))
    }
}

/// Implement `Matcher` for optional matchers, so that if a matcher is left
/// unspecified, it matches all inputs by default.
impl<T, O> Matcher<T> for Option<O>
where
    O: Matcher<T>,
{
    fn matches(&self, actual: &T) -> bool {
        self.as_ref().map_or(true, |expected| expected.matches(actual))
    }

    fn required_matches(&self, actual: Option<&T>) -> bool {
        self.as_ref().map_or(true, |expected| expected.required_matches(actual))
    }
}

/// Matcher that matches IP addresses in a subnet.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct SubnetMatcher<A: IpAddress>(pub Subnet<A>);

impl<A: IpAddress> Matcher<A> for SubnetMatcher<A> {
    fn matches(&self, actual: &A) -> bool {
        let Self(matcher) = self;
        matcher.contains(actual)
    }
}

/// Matcher that matches devices with the name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceNameMatcher(pub String);

impl<D: DeviceWithName> Matcher<D> for DeviceNameMatcher {
    fn matches(&self, actual: &D) -> bool {
        let Self(name) = self;
        actual.name_matches(name)
    }
}

#[cfg(test)]
mod tests {
    use alloc::format;

    use ip_test_macro::ip_test;
    use net_types::ip::Ip;

    use super::*;
    use crate::testutil::{FakeDeviceId, TestIpExt};
    use crate::DeviceNameMatcher;

    /// Only matches `true`.
    #[derive(Debug)]
    struct TrueMatcher;

    impl Matcher<bool> for TrueMatcher {
        fn matches(&self, actual: &bool) -> bool {
            *actual
        }
    }

    #[test]
    fn test_optional_matcher_optional_value() {
        assert!(TrueMatcher.matches(&true));
        assert!(!TrueMatcher.matches(&false));

        assert!(TrueMatcher.required_matches(Some(&true)));
        assert!(!TrueMatcher.required_matches(Some(&false)));
        assert!(!TrueMatcher.required_matches(None));

        assert!(Some(TrueMatcher).matches(&true));
        assert!(!Some(TrueMatcher).matches(&false));
        assert!(None::<TrueMatcher>.matches(&true));
        assert!(None::<TrueMatcher>.matches(&false));

        assert!(Some(TrueMatcher).required_matches(Some(&true)));
        assert!(!Some(TrueMatcher).required_matches(Some(&false)));
        assert!(!Some(TrueMatcher).required_matches(None));
        assert!(None::<TrueMatcher>.required_matches(Some(&true)));
        assert!(None::<TrueMatcher>.required_matches(Some(&false)));
        assert!(None::<TrueMatcher>.required_matches(None));
    }

    #[test]
    fn device_name_matcher() {
        let device = FakeDeviceId;
        let positive_matcher = DeviceNameMatcher(FakeDeviceId::FAKE_NAME.into());
        let negative_matcher = DeviceNameMatcher(format!("DONTMATCH-{}", FakeDeviceId::FAKE_NAME));
        assert!(positive_matcher.matches(&device));
        assert!(!negative_matcher.matches(&device));
    }

    #[ip_test(I)]
    fn subnet_matcher<I: Ip + TestIpExt>() {
        let matcher = SubnetMatcher(I::TEST_ADDRS.subnet);
        assert!(matcher.matches(&I::TEST_ADDRS.local_ip));
        assert!(!matcher.matches(&I::get_other_remote_ip_address(1)));
    }
}
