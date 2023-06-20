// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/bin/driver_manager/v2/tests/bind_manager_test_base.h"

class MultibindTest : public BindManagerTestBase {};

TEST_F(MultibindTest, MultibindLegacyComposites_CompositeMultibindDisabled) {
  AddAndOrphanNode("node-a", /* enable_multibind */ false);
  AddAndOrphanNode("node-c", /* enable_multibind */ true);
  VerifyNoOngoingBind();

  // Add composite-a.
  AddLegacyComposite("composite-a", {"node-a", "node-c"});
  VerifyLegacyCompositeFragmentIsBound(true, "composite-a", "node-a");
  VerifyLegacyCompositeFragmentIsBound(true, "composite-a", "node-c");
  VerifyLegacyCompositeBuilt(true, "composite-a");

  // With composite-a built, we should have a follow up bind process for it.
  VerifyBindOngoingWithRequests({{"composite-a", 1}});

  // Add composite-b which shares the same fragments as composite-a.
  AddLegacyComposite_EXPECT_QUEUED("composite-b", {"node-a", "node-c"});

  // Complete the ongoing bind. It should kickstart another bind process.
  DriverIndexReplyWithNoMatch("composite-a");
  VerifyBindOngoingWithRequests({{"composite-a", 1}});

  // Composite-b should match with node-c but not node-a.
  VerifyLegacyCompositeFragmentIsBound(false, "composite-b", "node-a");
  VerifyLegacyCompositeFragmentIsBound(true, "composite-b", "node-c");
  VerifyLegacyCompositeBuilt(false, "composite-b");

  DriverIndexReplyWithNoMatch("composite-a");
  VerifyNoOngoingBind();
}

TEST_F(MultibindTest, MultibindLegacyComposites_NoOverlapBind) {
  AddAndOrphanNode("node-a", /* enable_multibind */ true);
  AddAndOrphanNode("node-b", /* enable_multibind */ true);
  AddAndOrphanNode("node-c", /* enable_multibind */ true);
  AddAndOrphanNode("node-d", /* enable_multibind */ true);
  VerifyNoOngoingBind();

  // Add composite-a.
  AddLegacyComposite("composite-a", {"node-a", "node-b", "node-d"});

  // Node-a, node-b, and node-d should match to composite-a, assembling it
  VerifyLegacyCompositeFragmentIsBound(true, "composite-a", "node-a");
  VerifyLegacyCompositeFragmentIsBound(true, "composite-a", "node-b");
  VerifyLegacyCompositeFragmentIsBound(true, "composite-a", "node-d");
  VerifyLegacyCompositeBuilt(true, "composite-a");
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
  VerifyLegacyCompositeFragmentIsBound(true, "composite-b", "node-b");
  VerifyLegacyCompositeFragmentIsBound(true, "composite-b", "node-c");
  VerifyLegacyCompositeBuilt(true, "composite-b");

  VerifyBindOngoingWithRequests({{"composite-b", 1}});
  DriverIndexReplyWithNoMatch("composite-b");
  VerifyNoOngoingBind();
}

TEST_F(MultibindTest, MultibindLegacyComposites_OverlappingBind) {
  AddAndOrphanNode("node-a", /* enable_multibind */ true);
  AddAndOrphanNode("node-c", /* enable_multibind */ true);
  AddAndOrphanNode("node-d", /* enable_multibind */ true);
  VerifyNoOngoingBind();

  // Add composite-a.
  AddLegacyComposite("composite-a", {"node-a", "node-b", "node-d"});

  // Node-a and node-d should match to composite-a.
  VerifyBindOngoingWithRequests({{"node-c", 1}});
  VerifyLegacyCompositeFragmentIsBound(true, "composite-a", "node-a");
  VerifyLegacyCompositeFragmentIsBound(true, "composite-a", "node-d");

  // Add a legacy composite that shares the node-b fragment with composite-a.
  AddLegacyComposite_EXPECT_QUEUED("composite-b", {"node-b", "node-c"});

  // Add node b. It should be queued.
  AddAndBindNode_EXPECT_QUEUED("node-b", /* enable_multibind */ true);

  // Complete the ongoing bind request. This will kickstart a follow up bind process
  // that results in composite-a and composite-b to be built.
  DriverIndexReplyWithNoMatch("node-c");

  VerifyLegacyCompositeFragmentIsBound(true, "composite-a", "node-b");
  VerifyLegacyCompositeBuilt(true, "composite-a");
  VerifyLegacyCompositeBuilt(true, "composite-b");

  // Since there are no other nodes available for bind, another follow up bind process should
  // start for the composites.
  VerifyBindOngoingWithRequests({{"composite-a", 1}, {"composite-b", 1}});
  DriverIndexReplyWithNoMatch("composite-a");
  DriverIndexReplyWithNoMatch("composite-b");
  VerifyNoOngoingBind();
}

TEST_F(MultibindTest, StoredCompositeParentsChangeDuringMultibind) {
  AddAndOrphanNode("node-a", /* enable_multibind */ true);
  AddAndOrphanNode("node-c", /* enable_multibind */ true);
  AddAndOrphanNode("node-d", /* enable_multibind */ true);
  VerifyNoOngoingBind();

  // Add composite-a.
  AddLegacyComposite("composite-a", {"node-a", "node-b", "node-d"});

  // Node-a and node-d should match to composite-a.
  VerifyBindOngoingWithRequests({{"node-c", 1}});
  VerifyLegacyCompositeFragmentIsBound(true, "composite-a", "node-a");
  VerifyLegacyCompositeFragmentIsBound(true, "composite-a", "node-d");

  // Add a legacy composite that shares the node-a fragment with composite-a.
  AddLegacyComposite_EXPECT_QUEUED("composite-b", {"node-a", "node-c"});
  AddLegacyComposite_EXPECT_QUEUED("composite-c", {"node-a", "node-b"});

  // Add node-b. It should be queued.
  AddAndBindNode_EXPECT_QUEUED("node-b", /* enable_multibind */ true);

  // Complete the ongoing bind request. This will kickstart a follow up bind process
  // that results in all composites to be built.
  DriverIndexReplyWithNoMatch("node-c");

  VerifyLegacyCompositeFragmentIsBound(true, "composite-b", "node-a");
  VerifyLegacyCompositeFragmentIsBound(true, "composite-c", "node-a");
  VerifyLegacyCompositeBuilt(true, "composite-a");
  VerifyLegacyCompositeBuilt(true, "composite-b");
  VerifyLegacyCompositeBuilt(true, "composite-c");

  // Since there are no other nodes available for bind, another follow up bind process should
  // start for the composites.
  VerifyBindOngoingWithRequests({{"composite-a", 1}, {"composite-b", 1}, {"composite-c", 1}});
  DriverIndexReplyWithNoMatch("composite-a");
  DriverIndexReplyWithNoMatch("composite-b");
  DriverIndexReplyWithNoMatch("composite-c");
  VerifyNoOngoingBind();
}
