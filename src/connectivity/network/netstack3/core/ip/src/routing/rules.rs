// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! IP routing rules.

use alloc::vec::Vec;
use core::fmt::Debug;
use core::ops::Deref as _;

use net_types::ip::Ip;
use netstack3_base::{
    DeviceNameMatcher, DeviceWithName, Mark, MarkDomain, MarkStorage, Marks, Matcher, SubnetMatcher,
};

use crate::internal::routing::PacketOrigin;
use crate::RoutingTableId;

/// Table that contains routing rules.
pub struct RulesTable<I: Ip, D> {
    /// Rules of the table.
    rules: Vec<Rule<I, D>>,
}

impl<I: Ip, D> RulesTable<I, D> {
    pub(crate) fn new(main_table_id: RoutingTableId<I, D>) -> Self {
        // TODO(https://fxbug.dev/355059790): If bindings is installing the main table, we should
        // also let the bindings install this default rule.
        Self {
            rules: alloc::vec![Rule {
                matcher: RuleMatcher::match_all_packets(),
                action: RuleAction::Lookup(main_table_id)
            }],
        }
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = &'_ Rule<I, D>> {
        self.rules.iter()
    }

    /// Gets the mutable reference to the rules vector.
    #[cfg(any(test, feature = "testutils"))]
    pub fn rules_mut(&mut self) -> &mut Vec<Rule<I, D>> {
        &mut self.rules
    }

    /// Replaces the rules inside this table.
    pub fn replace(&mut self, new_rules: Vec<Rule<I, D>>) {
        self.rules = new_rules;
    }
}

/// A routing rule.
pub struct Rule<I: Ip, D> {
    /// The matcher of the rule.
    pub matcher: RuleMatcher<I>,
    /// The action of the rule.
    pub action: RuleAction<RoutingTableId<I, D>>,
}

/// The action part of a [`Rule`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuleAction<Lookup> {
    /// Will resolve to unreachable.
    Unreachable,
    /// Lookup in a routing table.
    Lookup(Lookup),
}

/// Matcher for the bound device of locally generated traffic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BoundDeviceMatcher {
    /// The packet is bound to a device which is matched by the matcher.
    DeviceName(DeviceNameMatcher),
    /// There is no bound device.
    Unbound,
}

impl<'a, D: DeviceWithName> Matcher<Option<&'a D>> for BoundDeviceMatcher {
    fn matches(&self, actual: &Option<&'a D>) -> bool {
        match self {
            BoundDeviceMatcher::DeviceName(name_matcher) => {
                name_matcher.required_matches(actual.as_deref())
            }
            BoundDeviceMatcher::Unbound => actual.is_none(),
        }
    }
}

/// Matches with [`PacketOrigin`].
///
/// Note that this matcher doesn't specify the source address/bound address like [`PacketOrigin`]
/// because the user can specify a source address matcher without specifying the direction of the
/// traffic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrafficOriginMatcher {
    /// This only matches packets that are generated locally; the optional interface matcher
    /// can be used to match what device is bound to by `SO_BINDTODEVICE`.
    Local {
        /// The matcher for the bound device.
        bound_device_matcher: Option<BoundDeviceMatcher>,
    },
    /// This only matches non-local packets. The packets must be received from the network.
    NonLocal,
}

impl<'a, I: Ip, D: DeviceWithName> Matcher<PacketOrigin<I, &'a D>> for SubnetMatcher<I::Addr> {
    fn matches(&self, actual: &PacketOrigin<I, &'a D>) -> bool {
        match actual {
            PacketOrigin::Local { bound_address, bound_device: _ } => {
                self.required_matches(bound_address.as_deref())
            }
            PacketOrigin::NonLocal { source_address, incoming_device: _ } => {
                self.matches(source_address.deref())
            }
        }
    }
}

impl<'a, I: Ip, D: DeviceWithName> Matcher<PacketOrigin<I, &'a D>> for TrafficOriginMatcher {
    fn matches(&self, actual: &PacketOrigin<I, &'a D>) -> bool {
        match (self, actual) {
            (
                TrafficOriginMatcher::Local { bound_device_matcher },
                PacketOrigin::Local { bound_address: _, bound_device },
            ) => bound_device_matcher.matches(bound_device),
            (
                TrafficOriginMatcher::NonLocal,
                PacketOrigin::NonLocal { source_address: _, incoming_device: _ },
            ) => true,
            (TrafficOriginMatcher::Local { .. }, PacketOrigin::NonLocal { .. })
            | (TrafficOriginMatcher::NonLocal, PacketOrigin::Local { .. }) => false,
        }
    }
}

/// A matcher to the socket mark.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkMatcher {
    /// Matches a packet if it is unmarked.
    Unmarked,
    /// The packet carries a mark that is in the range after masking.
    Marked {
        /// The mask to apply.
        mask: u32,
        /// Start of the range, inclusive.
        start: u32,
        /// End of the range, inclusive.
        end: u32,
    },
}

impl Matcher<Mark> for MarkMatcher {
    fn matches(&self, Mark(actual): &Mark) -> bool {
        match self {
            MarkMatcher::Unmarked => actual.is_none(),
            MarkMatcher::Marked { mask, start, end } => {
                actual.is_some_and(|actual| (*start..=*end).contains(&(actual & *mask)))
            }
        }
    }
}

/// The 2 mark matchers a rule can specify. All non-none markers must match.
#[derive(Default, Debug, Clone, Copy, PartialEq, Eq)]
pub struct MarkMatchers(MarkStorage<Option<MarkMatcher>>);

impl MarkMatchers {
    /// Creates [`MarkMatcher`]s from an iterator of `(MarkDomain, MarkMatcher)`.
    ///
    /// An unspecified domain will not have a matcher.
    ///
    /// # Panics
    ///
    /// Panics if the same domain is specified more than once.
    pub fn new(matchers: impl IntoIterator<Item = (MarkDomain, MarkMatcher)>) -> Self {
        MarkMatchers(MarkStorage::new(matchers))
    }

    /// Returns an iterator over the mark matchers of all domains.
    pub fn iter(&self) -> impl Iterator<Item = (MarkDomain, &Option<MarkMatcher>)> {
        let Self(storage) = self;
        storage.iter()
    }
}

impl Matcher<Marks> for MarkMatchers {
    fn matches(&self, actual: &Marks) -> bool {
        let Self(matchers) = self;
        matchers.zip_with(actual).all(|(_domain, matcher, actual)| matcher.matches(actual))
    }
}

/// Contains traffic matchers for a given rule.
///
/// `None` fields match all packets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleMatcher<I: Ip> {
    /// Matches on [`PacketOrigin`]'s bound address for a locally generated packet or the source
    /// address of an incoming packet.
    ///
    /// Matches whether the source address of the packet is from the subnet. If the matcher is
    /// specified but the source address is not specified, it resolves to not a match.
    pub source_address_matcher: Option<SubnetMatcher<I::Addr>>,
    /// Matches on [`PacketOrigin`]'s bound device for a locally generated packets or the receiving
    /// device of an incoming packet.
    pub traffic_origin_matcher: Option<TrafficOriginMatcher>,
    /// Matches on [`RuleInput`]'s marks.
    pub mark_matchers: MarkMatchers,
}

impl<I: Ip> RuleMatcher<I> {
    /// Creates a rule matcher that matches all packets.
    pub fn match_all_packets() -> Self {
        RuleMatcher {
            source_address_matcher: None,
            traffic_origin_matcher: None,
            mark_matchers: MarkMatchers::default(),
        }
    }
}

/// Packet properties used as input for the rules engine.
pub struct RuleInput<'a, I: Ip, D> {
    pub(crate) packet_origin: PacketOrigin<I, &'a D>,
    pub(crate) marks: &'a Marks,
}

impl<'a, I: Ip, D: DeviceWithName> Matcher<RuleInput<'a, I, D>> for RuleMatcher<I> {
    fn matches(&self, actual: &RuleInput<'a, I, D>) -> bool {
        let Self { source_address_matcher, traffic_origin_matcher, mark_matchers } = self;
        let RuleInput { packet_origin, marks } = actual;
        source_address_matcher.matches(packet_origin)
            && traffic_origin_matcher.matches(packet_origin)
            && mark_matchers.matches(marks)
    }
}

#[cfg(test)]
mod test {
    use ip_test_macro::ip_test;
    use net_types::ip::Subnet;
    use net_types::SpecifiedAddr;
    use netstack3_base::testutil::{FakeDeviceId, MultipleDevicesId, TestIpExt};
    use test_case::test_case;

    use super::*;

    #[ip_test(I)]
    #[test_case(None, None => true)]
    #[test_case(None, Some(MultipleDevicesId::A) => true)]
    #[test_case(
        Some(BoundDeviceMatcher::Unbound),
        None => true)]
    #[test_case(
        Some(BoundDeviceMatcher::Unbound),
        Some(MultipleDevicesId::A) => false)]
    #[test_case(
        Some(BoundDeviceMatcher::DeviceName(DeviceNameMatcher("A".into()))),
        None => false)]
    #[test_case(
        Some(BoundDeviceMatcher::DeviceName(DeviceNameMatcher("A".into()))),
        Some(MultipleDevicesId::A) => true)]
    #[test_case(
        Some(BoundDeviceMatcher::DeviceName(DeviceNameMatcher("A".into()))),
        Some(MultipleDevicesId::B) => false)]
    fn rule_matcher_matches_bound_device<I: TestIpExt>(
        bound_device_matcher: Option<BoundDeviceMatcher>,
        bound_device: Option<MultipleDevicesId>,
    ) -> bool {
        let matcher = RuleMatcher::<I> {
            traffic_origin_matcher: Some(TrafficOriginMatcher::Local { bound_device_matcher }),
            ..RuleMatcher::match_all_packets()
        };
        let input = RuleInput {
            packet_origin: PacketOrigin::Local {
                bound_address: None,
                bound_device: bound_device.as_ref(),
            },
            marks: &Default::default(),
        };
        matcher.matches(&input)
    }

    #[ip_test(I)]
    #[test_case(None, None => true)]
    #[test_case(None, Some(I::LOOPBACK_ADDRESS) => true)]
    #[test_case(
        Some(<I as TestIpExt>::TEST_ADDRS.subnet),
        None => false)]
    #[test_case(
        Some(<I as TestIpExt>::TEST_ADDRS.subnet),
        Some(<I as TestIpExt>::TEST_ADDRS.local_ip) => true)]
    #[test_case(
        Some(<I as TestIpExt>::TEST_ADDRS.subnet),
        Some(<I as TestIpExt>::get_other_remote_ip_address(1)) => false)]
    fn rule_matcher_matches_local_addr<I: TestIpExt>(
        source_address_subnet: Option<Subnet<I::Addr>>,
        bound_address: Option<SpecifiedAddr<I::Addr>>,
    ) -> bool {
        let matcher = RuleMatcher::<I> {
            source_address_matcher: source_address_subnet.map(SubnetMatcher),
            ..RuleMatcher::match_all_packets()
        };
        let marks = Default::default();
        let input = RuleInput::<'_, _, FakeDeviceId> {
            packet_origin: PacketOrigin::Local { bound_address, bound_device: None },
            marks: &marks,
        };
        matcher.matches(&input)
    }

    #[ip_test(I)]
    #[test_case(None, PacketOrigin::Local {
         bound_address: None,
         bound_device: None
    } => true)]
    #[test_case(None, PacketOrigin::NonLocal {
        source_address: <I as TestIpExt>::TEST_ADDRS.remote_ip,
        incoming_device: &FakeDeviceId
    } => true)]
    #[test_case(Some(TrafficOriginMatcher::Local {
        bound_device_matcher: None
    }), PacketOrigin::Local {
        bound_address: None,
        bound_device: None
    } => true)]
    #[test_case(Some(TrafficOriginMatcher::NonLocal),
        PacketOrigin::NonLocal {
            source_address: <I as TestIpExt>::TEST_ADDRS.remote_ip,
            incoming_device: &FakeDeviceId
        } => true)]
    #[test_case(Some(TrafficOriginMatcher::Local { bound_device_matcher: None }),
        PacketOrigin::NonLocal {
            source_address: <I as TestIpExt>::TEST_ADDRS.remote_ip,
            incoming_device: &FakeDeviceId
        }  => false)]
    #[test_case(Some(TrafficOriginMatcher::NonLocal),
        PacketOrigin::Local {
            bound_address: None,
            bound_device: None
        } => false)]
    fn rule_matcher_matches_locally_generated<I: TestIpExt>(
        traffic_origin_matcher: Option<TrafficOriginMatcher>,
        packet_origin: PacketOrigin<I, &'static FakeDeviceId>,
    ) -> bool {
        let matcher =
            RuleMatcher::<I> { traffic_origin_matcher, ..RuleMatcher::match_all_packets() };
        let marks = Default::default();
        let input = RuleInput::<'_, _, FakeDeviceId> { packet_origin, marks: &marks };
        matcher.matches(&input)
    }

    #[ip_test(I)]
    #[test_case::test_matrix(
            [
                None,
                Some(<I as TestIpExt>::TEST_ADDRS.local_ip),
                Some(<I as TestIpExt>::get_other_remote_ip_address(1))
            ],
            [
                None,
                Some(&MultipleDevicesId::A),
                Some(&MultipleDevicesId::B),
                Some(&MultipleDevicesId::C),
            ],
            [true, false]
        )]
    fn rule_matcher_matches_multiple_conditions<I: TestIpExt>(
        ip: Option<SpecifiedAddr<I::Addr>>,
        device: Option<&'static MultipleDevicesId>,
        locally_generated: bool,
    ) {
        let matcher = RuleMatcher::<I> {
            source_address_matcher: Some(SubnetMatcher(I::TEST_ADDRS.subnet)),
            traffic_origin_matcher: Some(TrafficOriginMatcher::Local {
                bound_device_matcher: Some(BoundDeviceMatcher::DeviceName(DeviceNameMatcher(
                    "A".into(),
                ))),
            }),
            ..RuleMatcher::match_all_packets()
        };

        let packet_origin = if locally_generated {
            PacketOrigin::Local { bound_address: ip, bound_device: device }
        } else {
            let (Some(source_address), Some(incoming_device)) = (ip, device) else {
                return;
            };
            PacketOrigin::NonLocal { source_address, incoming_device }
        };

        let input = RuleInput { packet_origin, marks: &Default::default() };

        if ip == Some(I::TEST_ADDRS.local_ip)
            && (device == Some(&MultipleDevicesId::A))
            && locally_generated
        {
            assert!(matcher.matches(&input))
        } else {
            assert!(!matcher.matches(&input))
        }
    }

    #[test_case(MarkMatcher::Unmarked, Mark(None) => true)]
    #[test_case(MarkMatcher::Unmarked, Mark(Some(0)) => false)]
    #[test_case(MarkMatcher::Marked {
        mask: 1,
        start: 0,
        end: 0,
    }, Mark(None) => false)]
    #[test_case(MarkMatcher::Marked {
        mask: 1,
        start: 0,
        end: 0,
    }, Mark(Some(0)) => true)]
    #[test_case(MarkMatcher::Marked {
        mask: 1,
        start: 0,
        end: 0,
    }, Mark(Some(1)) => false)]
    #[test_case(MarkMatcher::Marked {
        mask: 1,
        start: 0,
        end: 0,
    }, Mark(Some(2)) => true)]
    #[test_case(MarkMatcher::Marked {
        mask: 1,
        start: 0,
        end: 0,
    }, Mark(Some(3)) => false)]
    fn mark_matcher(matcher: MarkMatcher, mark: Mark) -> bool {
        matcher.matches(&mark)
    }

    #[test_case(
        MarkMatchers::new(
            [(MarkDomain::Mark1, MarkMatcher::Unmarked),
            (MarkDomain::Mark2, MarkMatcher::Unmarked)]
        ),
        Marks::new([]) => true
    )]
    #[test_case(
        MarkMatchers::new(
            [(MarkDomain::Mark1, MarkMatcher::Unmarked),
            (MarkDomain::Mark2, MarkMatcher::Unmarked)]
        ),
        Marks::new([(MarkDomain::Mark1, 1)]) => false
    )]
    #[test_case(
        MarkMatchers::new(
            [(MarkDomain::Mark1, MarkMatcher::Unmarked),
            (MarkDomain::Mark2, MarkMatcher::Unmarked)]
        ),
        Marks::new([(MarkDomain::Mark2, 1)]) => false
    )]
    #[test_case(
        MarkMatchers::new(
            [(MarkDomain::Mark1, MarkMatcher::Unmarked),
            (MarkDomain::Mark2, MarkMatcher::Unmarked)]
        ),
        Marks::new([
            (MarkDomain::Mark1, 1),
            (MarkDomain::Mark2, 1),
        ]) => false
    )]
    fn mark_matchers(matchers: MarkMatchers, marks: Marks) -> bool {
        matchers.matches(&marks)
    }
}
