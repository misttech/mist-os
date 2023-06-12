// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! A module for managing Netlink multicast group memberships.
//!
//! A Netlink socket can subscribe to any number of multicast groups, as defined
//! by the Netlink protocol family that the socket is connected to. There are
//! two modes of specifying the multicast group memberships. Mode 1 is referred
//! to as "legacy" throughout this module because it was replaced by mode 2,
//! referred to as "modern", in Linux 2.6.14.
//!     Mode 1: Specifying `nl_groups`, a 32 bit bitmask, when binding the
//!             the socket.
//!     Mode 2: Setting the `NETLINK_ADD_MEMBERSHIP` or
//!            `NETLINK_DROP_MEMBERSHIP` socket option.
//!
//! Note that both mode 1 and mode 2 are supported (for backwards
//! compatibility), and the two modes operate over different sets of constants.
//! The "modern" constants correspond to the index of the set-bit in their
//! "legacy" counterpart. For example, consider this sample of NETLINK_ROUTE
//! constants:
//!     RTNLGRP_LINK:   legacy (1), modern (1),
//!     RTNLGRP_NOTIFY: legacy (2), modern (2),
//!     RTNLGRP_NEIGH:  legacy (4), modern (3),
//!     RTNLGRP_TC:     legacy (8), modern (4),
//!
//! The [`MulticastGroupMemberships`] struct exposed by this module tracks the
//! memberships independently of the mode via which they are set. For example, a
//! `NETLINK_ROUTE` client could bind to `RTMGRP_IPV6_IFADDR` (256), to start
//! receiving IPv6 address events, and later set the `NETLINK_DROP_MEMBERSHIP`
//! socket option to `RTNLGRP_IPV6_IFADDR` (9), to stop receiving events.

use std::marker::PhantomData;

use bit_set::BitSet;
use tracing::warn;

use crate::NETLINK_LOG_TAG;

// Safe "as" conversion because u32::BITS (32) will fit into any usize.
const U32_BITS_USIZE: usize = u32::BITS as usize;

/// A modern (non-legacy) multicast group. Interpreted as a single group.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ModernGroup(pub u32);

impl Into<usize> for ModernGroup {
    fn into(self) -> usize {
        let ModernGroup(group) = self;
        group.try_into().expect("expected usize >= u32")
    }
}

/// An error indicating that a modern group has no mapping to a legacy
/// group.
#[derive(Debug)]
pub struct NoMappingFromModernToLegacyGroupError;

impl TryFrom<ModernGroup> for SingleLegacyGroup {
    type Error = NoMappingFromModernToLegacyGroupError;

    fn try_from(
        ModernGroup(group): ModernGroup,
    ) -> Result<SingleLegacyGroup, NoMappingFromModernToLegacyGroupError> {
        let group = 1 << group;
        if group == 0 {
            Err(NoMappingFromModernToLegacyGroupError)
        } else {
            Ok(SingleLegacyGroup(group))
        }
    }
}

/// A set of legacy multicast groups. Interpreted as a bit mask, where each set
/// bit corresponds to a different group membership.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct LegacyGroups(pub u32);

/// A single legacy multicast group membership. At most 1 bit is set.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SingleLegacyGroup(u32);

impl SingleLegacyGroup {
    /// Returns the group number as a `u32`.
    pub fn inner(&self) -> u32 {
        let SingleLegacyGroup(inner) = self;
        *inner
    }
}

/// Error returned when attempting to convert a u32 into [`SingleLegacyGroup`].
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct MultipleBitsSetError;

impl TryFrom<u32> for SingleLegacyGroup {
    type Error = MultipleBitsSetError;
    fn try_from(value: u32) -> Result<Self, Self::Error> {
        (value.count_ones() <= 1).then_some(SingleLegacyGroup(value)).ok_or(MultipleBitsSetError)
    }
}

/// Multicast group semantics that are specific to a particular protocol family.
pub(crate) trait MulticastCapableNetlinkFamily {
    /// Returns true if the given [`ModernGroup`] is a valid multicast group.
    fn is_valid_group(group: &ModernGroup) -> bool;
}

/// Translate the given legacy group membership to a modern membership.
///
/// Returns `None` if the given legacy_group does not exist in this family.
fn legacy_to_modern<F: MulticastCapableNetlinkFamily>(
    group: SingleLegacyGroup,
) -> Option<ModernGroup> {
    let modern_group = ModernGroup(group.inner().ilog2() + 1);
    F::is_valid_group(&modern_group).then_some(modern_group)
}

/// Manages the current multicast group memberships of a single connection to
/// Netlink.
///
/// Memberships are stored entirely using the modern set of constants. Legacy
/// memberships are translated to their modern equivalent before being stored.
#[derive(Debug)]
pub(crate) struct MulticastGroupMemberships<F: MulticastCapableNetlinkFamily> {
    /// Aspects of multicast group memberships that are family specific.
    family: PhantomData<F>,

    // The current multicast group memberships, stored as modern memberships.
    // Membership in multicast group "N" is determined by whether the "Nth" bit
    // of the `BitSet` is set.
    memberships: BitSet,
}

/// Error returned when attempting to join an invalid [`ModernGroup`].
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct InvalidModernGroupError;

/// Error returned when attempting to join invalid [`LegacyGroups`].
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct InvalidLegacyGroupsError;

impl<F: MulticastCapableNetlinkFamily> MulticastGroupMemberships<F> {
    /// Instantiate a new [`MulticastGroupMemberships`].
    pub(crate) fn new() -> MulticastGroupMemberships<F> {
        MulticastGroupMemberships { family: PhantomData, memberships: Default::default() }
    }

    /// Returns `True` if `self` is a member of `group`.
    pub(crate) fn member_of_group(&self, group: ModernGroup) -> bool {
        self.memberships.contains(group.into())
    }

    /// Adds the given multicast group membership.
    pub(crate) fn add_membership(
        &mut self,
        group: ModernGroup,
    ) -> Result<(), InvalidModernGroupError> {
        let MulticastGroupMemberships { family: _, memberships } = self;
        if !F::is_valid_group(&group) {
            return Err(InvalidModernGroupError);
        }
        let _was_absent: bool = memberships.insert(group.into());
        return Ok(());
    }

    /// Deletes the given multicast group membership.
    pub(crate) fn del_membership(
        &mut self,
        group: ModernGroup,
    ) -> Result<(), InvalidModernGroupError> {
        let MulticastGroupMemberships { family: _, memberships } = self;
        if !F::is_valid_group(&group) {
            return Err(InvalidModernGroupError);
        }
        let _was_present: bool = memberships.remove(group.into());
        return Ok(());
    }

    /// Sets the legacy multicast group memberships.
    ///
    /// Legacy memberships are translated into their modern equivalent before
    /// being written.
    pub(crate) fn set_legacy_memberships(
        &mut self,
        LegacyGroups(requested_groups): LegacyGroups,
    ) -> Result<(), InvalidLegacyGroupsError> {
        let MulticastGroupMemberships { family: _, memberships } = self;
        #[derive(Clone, Copy)]
        enum Mutation {
            None,
            Add(ModernGroup),
            Del(ModernGroup),
        }
        let mut mutations = [Mutation::None; U32_BITS_USIZE];
        // Validate and record all the mutations that will need to be applied.
        for i in 0..U32_BITS_USIZE {
            let raw_legacy_group = 1 << i;
            let legacy_group = raw_legacy_group
                .try_into()
                .expect("raw_legacy_group unexpectedly had multiple bits set");
            let modern_group = legacy_to_modern::<F>(legacy_group);
            let is_member_of_group = requested_groups & raw_legacy_group != 0;
            mutations[i] = match (modern_group, is_member_of_group) {
                (Some(modern_group), true) => Mutation::Add(modern_group),
                (Some(modern_group), false) => Mutation::Del(modern_group),
                (None, true) => {
                    warn!(
                        tag = NETLINK_LOG_TAG,
                        "failed to join legacy groups ({:?}) because of invalid group: {:?}",
                        requested_groups,
                        legacy_group
                    );
                    return Err(InvalidLegacyGroupsError);
                }
                (None, false) => Mutation::None,
            };
        }
        // Apply all of the mutations.
        for mutation in mutations {
            match mutation {
                Mutation::None => {}
                Mutation::Add(group) => {
                    let _was_absent: bool = memberships.insert(group.into());
                }
                Mutation::Del(group) => {
                    let _was_present: bool = memberships.remove(group.into());
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::protocol_family::testutil::{
        FakeProtocolFamily, INVALID_LEGACY_GROUP, INVALID_MODERN_GROUP, LEGACY_GROUP1,
        LEGACY_GROUP2, LEGACY_GROUP3, MODERN_GROUP1, MODERN_GROUP2, MODERN_GROUP3,
    };

    #[test]
    fn test_single_legacy_groups() {
        assert_eq!(0.try_into(), Ok(SingleLegacyGroup(0)));
        assert_eq!(0x00010000.try_into(), Ok(SingleLegacyGroup(0x00010000)));
        assert_eq!(
            <u32 as TryInto<SingleLegacyGroup>>::try_into(0x00010100),
            Err(MultipleBitsSetError {})
        );
    }

    #[test]
    fn test_add_del_membership() {
        let mut memberships = MulticastGroupMemberships::<FakeProtocolFamily>::new();

        assert!(!memberships.member_of_group(MODERN_GROUP1));
        assert!(!memberships.member_of_group(MODERN_GROUP2));
        assert!(!memberships.member_of_group(MODERN_GROUP3));

        // Add one membership, and verify the others are unaffected.
        memberships.add_membership(MODERN_GROUP1).expect("failed to add");
        assert!(memberships.member_of_group(MODERN_GROUP1));
        assert!(!memberships.member_of_group(MODERN_GROUP2));
        assert!(!memberships.member_of_group(MODERN_GROUP3));
        // Add a second & third membership.
        memberships.add_membership(MODERN_GROUP2).expect("failed to add");
        memberships.add_membership(MODERN_GROUP3).expect("failed to add");
        assert!(memberships.member_of_group(MODERN_GROUP1));
        assert!(memberships.member_of_group(MODERN_GROUP2));
        assert!(memberships.member_of_group(MODERN_GROUP3));
        // Remove one membership, and verify the others are unaffected.
        memberships.del_membership(MODERN_GROUP1).expect("failed to del");
        assert!(!memberships.member_of_group(MODERN_GROUP1));
        assert!(memberships.member_of_group(MODERN_GROUP2));
        assert!(memberships.member_of_group(MODERN_GROUP3));
        // Remove the second & third membership.
        memberships.del_membership(MODERN_GROUP2).expect("failed to del");
        memberships.del_membership(MODERN_GROUP3).expect("failed to del");
        assert!(!memberships.member_of_group(MODERN_GROUP1));
        assert!(!memberships.member_of_group(MODERN_GROUP2));
        assert!(!memberships.member_of_group(MODERN_GROUP3));
        // Verify Adding/Deleting an invalid group fails.
        assert_eq!(
            memberships.add_membership(INVALID_MODERN_GROUP),
            Err(InvalidModernGroupError {})
        );
        assert_eq!(
            memberships.del_membership(INVALID_MODERN_GROUP),
            Err(InvalidModernGroupError {})
        );
    }

    #[test]
    fn test_legacy_memberships() {
        let mut memberships = MulticastGroupMemberships::<FakeProtocolFamily>::new();

        assert!(!memberships.member_of_group(MODERN_GROUP1));
        assert!(!memberships.member_of_group(MODERN_GROUP2));
        assert!(!memberships.member_of_group(MODERN_GROUP3));

        // Add one membership and verify the others are unaffected.
        memberships
            .set_legacy_memberships(LegacyGroups(LEGACY_GROUP1))
            .expect("failed to set legacy groups");
        assert!(memberships.member_of_group(MODERN_GROUP1));
        assert!(!memberships.member_of_group(MODERN_GROUP2));
        assert!(!memberships.member_of_group(MODERN_GROUP3));
        // Add a second & third membership.
        memberships
            .set_legacy_memberships(LegacyGroups(LEGACY_GROUP1 | LEGACY_GROUP2 | LEGACY_GROUP3))
            .expect("failed to set legacy groups");
        assert!(memberships.member_of_group(MODERN_GROUP1));
        assert!(memberships.member_of_group(MODERN_GROUP2));
        assert!(memberships.member_of_group(MODERN_GROUP3));
        // Remove one membership and verify the others are unaffected.
        memberships
            .set_legacy_memberships(LegacyGroups(LEGACY_GROUP2 | LEGACY_GROUP3))
            .expect("failed to set legacy_groups");
        assert!(!memberships.member_of_group(MODERN_GROUP1));
        assert!(memberships.member_of_group(MODERN_GROUP2));
        assert!(memberships.member_of_group(MODERN_GROUP3));
        // Remove the second & third membership.
        memberships.set_legacy_memberships(LegacyGroups(0)).expect("failed to set legacy groups");
        assert!(!memberships.member_of_group(MODERN_GROUP1));
        assert!(!memberships.member_of_group(MODERN_GROUP2));
        assert!(!memberships.member_of_group(MODERN_GROUP3));
        // Verify that setting an invalid group fails.
        assert_eq!(
            memberships.set_legacy_memberships(LegacyGroups(INVALID_LEGACY_GROUP)),
            Err(InvalidLegacyGroupsError {})
        );
    }

    #[test]
    fn test_legacy_and_modern_memberships() {
        let mut memberships = MulticastGroupMemberships::<FakeProtocolFamily>::new();

        assert!(!memberships.member_of_group(MODERN_GROUP1));
        assert!(!memberships.member_of_group(MODERN_GROUP2));

        // Add memberships by their legacy group and drop by their modern group.
        memberships
            .set_legacy_memberships(LegacyGroups(LEGACY_GROUP1 | LEGACY_GROUP2))
            .expect("failed to set legacy groups");
        assert!(memberships.member_of_group(MODERN_GROUP1));
        assert!(memberships.member_of_group(MODERN_GROUP2));
        memberships.del_membership(MODERN_GROUP1).expect("failed to del");
        assert!(!memberships.member_of_group(MODERN_GROUP1));
        assert!(memberships.member_of_group(MODERN_GROUP2));
        memberships.del_membership(MODERN_GROUP2).expect("failed to del");
        assert!(!memberships.member_of_group(MODERN_GROUP1));
        assert!(!memberships.member_of_group(MODERN_GROUP2));

        // Add memberships by their modern group and drop by their legacy group.
        memberships.add_membership(MODERN_GROUP1).expect("failed to add");
        memberships.add_membership(MODERN_GROUP2).expect("failed to add");
        assert!(memberships.member_of_group(MODERN_GROUP1));
        assert!(memberships.member_of_group(MODERN_GROUP2));
        memberships
            .set_legacy_memberships(LegacyGroups(LEGACY_GROUP2))
            .expect("failed to set legacy groups");
        assert!(!memberships.member_of_group(MODERN_GROUP1));
        assert!(memberships.member_of_group(MODERN_GROUP2));
        memberships.set_legacy_memberships(LegacyGroups(0)).expect("failed to set legacy groups");
        assert!(!memberships.member_of_group(MODERN_GROUP1));
        assert!(!memberships.member_of_group(MODERN_GROUP2));
    }
}
