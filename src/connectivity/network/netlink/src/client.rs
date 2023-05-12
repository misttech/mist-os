// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! A module for managing individual clients (aka sockets) of Netlink.

use std::sync::{Arc, Mutex};

use crate::multicast_groups::{
    InvalidLegacyGroupsError, InvalidModernGroupError, LegacyGroups, ModernGroup,
    MulticastCapableNetlinkFamily, MulticastGroupMemberships,
};

/// The internal half of a Netlink client, with the external half being provided
/// by ['ExternalClient'].
pub(crate) struct InternalClient<F: MulticastCapableNetlinkFamily> {
    /// The client's current multicast group memberships
    group_memberships: Arc<Mutex<MulticastGroupMemberships<F>>>,
}

impl<F: MulticastCapableNetlinkFamily> InternalClient<F> {
    /// Returns true if this client is a member of the provided group.
    // TODO(https://issuetracker.google.com/280483454): Use this method to check
    // multicast group memberships.
    #[allow(dead_code)]
    pub(crate) fn member_of_group(&self, group: ModernGroup) -> bool {
        self.group_memberships.lock().unwrap().member_of_group(group)
    }
}

/// The external half of a Netlink client, with the internal half being provided
/// by ['InternalClient'].
pub(crate) struct ExternalClient<F: MulticastCapableNetlinkFamily> {
    /// The client's current multicast group memberships
    group_memberships: Arc<Mutex<MulticastGroupMemberships<F>>>,
}

impl<F: MulticastCapableNetlinkFamily> ExternalClient<F> {
    /// Adds the given multicast group membership.
    pub(crate) fn add_membership(&self, group: ModernGroup) -> Result<(), InvalidModernGroupError> {
        self.group_memberships.lock().unwrap().add_membership(group)
    }

    /// Deletes the given multicast group membership.
    pub(crate) fn del_membership(&self, group: ModernGroup) -> Result<(), InvalidModernGroupError> {
        self.group_memberships.lock().unwrap().del_membership(group)
    }

    /// Sets the legacy multicast group memberships.
    pub(crate) fn set_legacy_memberships(
        &self,
        legacy_memberships: LegacyGroups,
    ) -> Result<(), InvalidLegacyGroupsError> {
        self.group_memberships.lock().unwrap().set_legacy_memberships(legacy_memberships)
    }
}

// Instantiate a new client pair.
pub(crate) fn new_client_pair<F: MulticastCapableNetlinkFamily>(
) -> (ExternalClient<F>, InternalClient<F>) {
    let group_memberships = Arc::new(Mutex::new(MulticastGroupMemberships::new()));
    (
        ExternalClient { group_memberships: group_memberships.clone() },
        InternalClient { group_memberships },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::protocol_family::testutil::{FakeProtocolFamily, MODERN_GROUP1, MODERN_GROUP2};

    // Verify that multicast group membership changes applied to the external
    // client are observed on the internal client.
    #[test]
    fn test_group_memberships() {
        let (external_client, internal_client) = new_client_pair::<FakeProtocolFamily>();

        assert!(!internal_client.member_of_group(MODERN_GROUP1));
        assert!(!internal_client.member_of_group(MODERN_GROUP2));

        // Add one membership and verify the other is unaffected.
        external_client.add_membership(MODERN_GROUP1).expect("failed to add membership");
        assert!(internal_client.member_of_group(MODERN_GROUP1));
        assert!(!internal_client.member_of_group(MODERN_GROUP2));
        // Add the second membership.
        external_client.add_membership(MODERN_GROUP2).expect("failed to add membership");
        assert!(internal_client.member_of_group(MODERN_GROUP1));
        assert!(internal_client.member_of_group(MODERN_GROUP2));
        // Delete the first membership and verify the other is unaffected.
        external_client.del_membership(MODERN_GROUP1).expect("failed to del membership");
        assert!(!internal_client.member_of_group(MODERN_GROUP1));
        assert!(internal_client.member_of_group(MODERN_GROUP2));
        // Delete the second membership.
        external_client.del_membership(MODERN_GROUP2).expect("failed to del membership");
        assert!(!internal_client.member_of_group(MODERN_GROUP1));
        assert!(!internal_client.member_of_group(MODERN_GROUP2));
    }
}
