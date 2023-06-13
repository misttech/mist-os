// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/bin/driver_manager/v2/tests/bind_manager_test_base.h"

class MultibindTest : public BindManagerTestBase {};

TEST_F(MultibindTest, MultibindLegacyComposites_NoOverlapBind) {
  AddAndOrphanNode("node-a");
  AddAndOrphanNode("node-b");
  AddAndOrphanNode("node-c");
  AddAndOrphanNode("node-d");
  VerifyNoOngoingBind();

  // Add composite-a.
  AddLegacyComposite("composite-a", {"node-a", "node-b", "node-d"});

  // Node-a, node-b, and node-d should match to composite-a, assembling it
  VerifyLegacyCompositeFragmentIsBound("composite-a", "node-a");
  VerifyLegacyCompositeFragmentIsBound("composite-a", "node-b");
  VerifyLegacyCompositeFragmentIsBound("composite-a", "node-d");
  VerifyLegacyCompositeBuilt("composite-a");
  VerifyPendingBindRequestCount(1);

  // Node-c should not match to anything. Complete the ongoing bind process and
  // another one should follow up for composite-a. Complete that one as well.
  VerifyBindOngoingWithRequests({{"node-c", 1}});
  DriverIndexReplyWithNoMatch("node-c");
  VerifyBindOngoingWithRequests({{"composite-a", 1}});
  DriverIndexReplyWithDriver("composite-a");
  VerifyNoOngoingBind();

  // Add a legacy composite that shares a fragment with composite-a. It should be built.
  AddLegacyComposite("composite-b", {"node-b", "node-c"});
  VerifyLegacyCompositeFragmentIsBound("composite-b", "node-b");
  VerifyLegacyCompositeFragmentIsBound("composite-b", "node-c");
  VerifyLegacyCompositeBuilt("composite-b");

  VerifyBindOngoingWithRequests({{"composite-b", 1}});
  DriverIndexReplyWithNoMatch("composite-b");
  VerifyNoOngoingBind();
}

TEST_F(MultibindTest, MultibindLegacyComposites_OverlappingBind) {
  AddAndOrphanNode("node-a");
  AddAndOrphanNode("node-c");
  AddAndOrphanNode("node-d");
  VerifyNoOngoingBind();

  // Add composite-a.
  AddLegacyComposite("composite-a", {"node-a", "node-b", "node-d"});

  // Node-a and node-d should match to composite-a.
  VerifyBindOngoingWithRequests({{"node-c", 1}});
  VerifyLegacyCompositeFragmentIsBound("composite-a", "node-a");
  VerifyLegacyCompositeFragmentIsBound("composite-a", "node-d");

  // Add a legacy composite that shares the node-b fragment with composite-a.
  AddLegacyComposite_EXPECT_QUEUED("composite-b", {"node-b", "node-c"});

  // Add node b. It should be queued.
  AddAndBindNode_EXPECT_QUEUED("node-b");

  // Complete the ongoing bind request. This will kickstart a follow up bind process
  // that results in composite-a and composite-b to be built.
  DriverIndexReplyWithNoMatch("node-c");

  VerifyLegacyCompositeFragmentIsBound("composite-a", "node-b");
  VerifyLegacyCompositeBuilt("composite-a");
  VerifyLegacyCompositeBuilt("composite-b");

  // Since there are no other nodes available for bind, another follow up bind process should
  // start for the composites.
  VerifyBindOngoingWithRequests({{"composite-a", 1}, {"composite-b", 1}});
  DriverIndexReplyWithNoMatch("composite-a");
  DriverIndexReplyWithNoMatch("composite-b");
  VerifyNoOngoingBind();
}
