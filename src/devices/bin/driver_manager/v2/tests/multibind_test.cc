// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/bin/driver_manager/v2/tests/bind_manager_test_base.h"

class MultibindTest : public BindManagerTestBase {};

TEST_F(MultibindTest, MultibindLegacyComposites_CompositeMultibindDisabled) {
  AddAndOrphanNode("node-a", /* enable_multibind */ false);
  AddAndOrphanNode("node-c", /* enable_multibind */ true);
  VerifyNoOngoingBind();

  // Add composite-a. Since node-c can multibind to composites, there will
  // be a request sent to the Driver Index to match it to a composite node spec.
  AddLegacyComposite("composite-a", {"node-a", "node-c"});
  VerifyLegacyCompositeFragmentIsBound(true, "composite-a", "node-a");
  VerifyLegacyCompositeFragmentIsBound(true, "composite-a", "node-c");
  VerifyLegacyCompositeBuilt(true, "composite-a");
  VerifyBindOngoingWithRequests({{"node-c", 1}});

  // Add composite-b which shares the same fragments as composite-a.
  AddLegacyComposite_EXPECT_QUEUED("composite-b", {"node-a", "node-c"});

  // Complete the ongoing bind. It should kickstart another bind process.
  DriverIndexReplyWithNoMatch("node-c");
  VerifyMultibindNodes({"node-c"});
  VerifyBindOngoingWithRequests({{"composite-a", 1}, {"node-c", 1}});

  // Composite-b should match with node-c but not node-a.
  VerifyLegacyCompositeFragmentIsBound(false, "composite-b", "node-a");
  VerifyLegacyCompositeFragmentIsBound(true, "composite-b", "node-c");
  VerifyLegacyCompositeBuilt(false, "composite-b");

  DriverIndexReplyWithNoMatch("composite-a");
  DriverIndexReplyWithNoMatch("node-c");

  VerifyMultibindNodes({"node-c"});
  VerifyNoOngoingBind();
}

TEST_F(MultibindTest, MultibindSpec_CompositeMultibindDisabled) {
  AddAndOrphanNode("node-a", /* enable_multibind */ false);
  AddAndOrphanNode("node-c", /* enable_multibind */ true);
  VerifyNoOngoingBind();

  // Add composite-a. Since node-c can multibind to composites, there will
  // be a request sent to the Driver Index to match it to a composite node spec.
  AddCompositeNodeSpec("composite-a", {"node-a", "node-c"});
  VerifyBindOngoingWithRequests({{"node-a", 1}, {"node-c", 1}});

  AddCompositeNodeSpec_EXPECT_QUEUED("composite-b", {"node-a", "node-c"});

  // Complete the ongoing bind. It should kickstart another bind process.
  DriverIndexReplyWithComposite("node-c", {{"composite-a", 1}});
  DriverIndexReplyWithComposite("node-a", {{"composite-a", 0}});
  VerifyCompositeNodeExists(true, "composite-a");
  VerifyMultibindNodes({"node-c"});

  VerifyBindOngoingWithRequests({{"node-c", 1}});
  DriverIndexReplyWithComposite("node-c", {{"composite-a", 0}, {"composite-b", 1}});
  VerifyCompositeNodeExists(false, "composite-b");

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

  // Node-a, node-b, and node-d should match to composite-a, assembling it.
  VerifyLegacyCompositeFragmentIsBound(true, "composite-a", "node-a");
  VerifyLegacyCompositeFragmentIsBound(true, "composite-a", "node-b");
  VerifyLegacyCompositeFragmentIsBound(true, "composite-a", "node-d");
  VerifyLegacyCompositeBuilt(true, "composite-a");
  VerifyPendingBindRequestCount(1);

  // Node-c should not match to anything. Complete the ongoing bind process and
  // another one should follow up for composite-a. Complete that one as well.
  VerifyBindOngoingWithRequests({{"node-a", 1}, {"node-b", 1}, {"node-c", 1}, {"node-d", 1}});
  DriverIndexReplyWithNoMatch("node-a");
  DriverIndexReplyWithNoMatch("node-b");
  DriverIndexReplyWithNoMatch("node-c");
  DriverIndexReplyWithNoMatch("node-d");

  VerifyMultibindNodes({"node-a", "node-b", "node-d"});
  VerifyBindOngoingWithRequests({{"composite-a", 1}});
  DriverIndexReplyWithDriver("composite-a");
  VerifyNoOngoingBind();
  VerifyMultibindNodes({"node-a", "node-b", "node-d"});

  // Add a legacy composite that shares a fragment with composite-a. It should be built.
  AddLegacyComposite("composite-b", {"node-b", "node-c"});
  VerifyLegacyCompositeFragmentIsBound(true, "composite-b", "node-b");
  VerifyLegacyCompositeFragmentIsBound(true, "composite-b", "node-c");
  VerifyLegacyCompositeBuilt(true, "composite-b");

  VerifyBindOngoingWithRequests({{"node-a", 1}, {"node-b", 1}, {"node-c", 1}, {"node-d", 1}});
  DriverIndexReplyWithNoMatch("node-a");
  DriverIndexReplyWithNoMatch("node-b");
  DriverIndexReplyWithNoMatch("node-c");
  DriverIndexReplyWithNoMatch("node-d");

  VerifyMultibindNodes({"node-a", "node-b", "node-c", "node-d"});
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
  VerifyBindOngoingWithRequests({{"node-a", 1}, {"node-c", 1}, {"node-d", 1}});
  VerifyLegacyCompositeFragmentIsBound(true, "composite-a", "node-a");
  VerifyLegacyCompositeFragmentIsBound(true, "composite-a", "node-d");

  // Add a legacy composite that shares the node-b fragment with composite-a.
  AddLegacyComposite_EXPECT_QUEUED("composite-b", {"node-b", "node-c"});

  // Add node b. It should be queued.
  AddAndBindNode_EXPECT_QUEUED("node-b", /* enable_multibind */ true);

  // Complete the ongoing bind request. This will kickstart a follow up bind process
  // that results in composite-a and composite-b to be built.
  DriverIndexReplyWithNoMatch("node-a");
  DriverIndexReplyWithNoMatch("node-c");
  DriverIndexReplyWithNoMatch("node-d");
  VerifyMultibindNodes({"node-a", "node-d"});

  VerifyLegacyCompositeFragmentIsBound(true, "composite-a", "node-b");
  VerifyLegacyCompositeBuilt(true, "composite-a");
  VerifyLegacyCompositeBuilt(true, "composite-b");

  // There are two for node-b from a pending bind request and multibind.
  VerifyBindOngoingWithRequests({
      {"node-a", 1},
      {"node-b", 1},
      {"node-c", 1},
      {"node-d", 1},
  });

  // Complete the ongoing bind. The new nodes should be added in the multibind set.
  DriverIndexReplyWithNoMatch("node-a");
  DriverIndexReplyWithNoMatch("node-b");
  DriverIndexReplyWithNoMatch("node-c");
  DriverIndexReplyWithNoMatch("node-d");
  VerifyMultibindNodes({"node-a", "node-b", "node-c", "node-d"});

  // We should get a follow up bind process for the newly assembled composites.
  VerifyBindOngoingWithRequests({{"composite-a", 1}, {"composite-b", 1}});
  DriverIndexReplyWithNoMatch("composite-a");
  DriverIndexReplyWithNoMatch("composite-b");
  VerifyNoOngoingBind();
}

TEST_F(MultibindTest, StoredMultibindNodesChangeDuringMultibind) {
  AddAndOrphanNode("node-a", /* enable_multibind */ true);
  AddAndOrphanNode("node-c", /* enable_multibind */ true);
  AddAndOrphanNode("node-d", /* enable_multibind */ true);
  VerifyNoOngoingBind();

  // Add composite-a.
  AddLegacyComposite("composite-a", {"node-a", "node-b", "node-d"});

  // Node-a and node-d should match to composite-a.
  VerifyBindOngoingWithRequests({{"node-a", 1}, {"node-c", 1}, {"node-d", 1}});
  VerifyLegacyCompositeFragmentIsBound(true, "composite-a", "node-a");
  VerifyLegacyCompositeFragmentIsBound(true, "composite-a", "node-d");

  // Add a legacy composite that shares the node-a fragment with composite-a.
  AddLegacyComposite_EXPECT_QUEUED("composite-b", {"node-a", "node-c"});
  AddLegacyComposite_EXPECT_QUEUED("composite-c", {"node-a", "node-b"});

  // Add node-b. It should be queued.
  AddAndBindNode_EXPECT_QUEUED("node-b", /* enable_multibind */ true);

  // Complete the ongoing bind request. This will kickstart a follow up bind process
  // that results in all composites to be built.
  DriverIndexReplyWithNoMatch("node-a");
  DriverIndexReplyWithNoMatch("node-c");
  DriverIndexReplyWithNoMatch("node-d");
  VerifyMultibindNodes({"node-a", "node-d"});

  VerifyLegacyCompositeFragmentIsBound(true, "composite-b", "node-a");
  VerifyLegacyCompositeFragmentIsBound(true, "composite-c", "node-a");
  VerifyLegacyCompositeBuilt(true, "composite-a");
  VerifyLegacyCompositeBuilt(true, "composite-b");
  VerifyLegacyCompositeBuilt(true, "composite-c");

  // Complete the ongoing bind process. We should follow up with a new process to bind
  // the composites.
  VerifyBindOngoingWithRequests({{"node-a", 1}, {"node-b", 1}, {"node-c", 1}, {"node-d", 1}});
  DriverIndexReplyWithNoMatch("node-a");
  DriverIndexReplyWithNoMatch("node-b");
  DriverIndexReplyWithNoMatch("node-c");
  DriverIndexReplyWithNoMatch("node-d");
  VerifyMultibindNodes({"node-a", "node-b", "node-c", "node-d"});

  VerifyBindOngoingWithRequests({{"composite-a", 1}, {"composite-b", 1}, {"composite-c", 1}});
  DriverIndexReplyWithNoMatch("composite-a");
  DriverIndexReplyWithNoMatch("composite-b");
  DriverIndexReplyWithNoMatch("composite-c");
  VerifyNoOngoingBind();
}

TEST_F(MultibindTest, MultibindSpecs_NoOverlapBind) {
  AddAndOrphanNode("node-a", /* enable_multibind */ true);
  AddAndOrphanNode("node-b", /* enable_multibind */ true);
  AddAndOrphanNode("node-c", /* enable_multibind */ true);
  AddAndOrphanNode("node-d", /* enable_multibind */ true);
  VerifyNoOngoingBind();

  // Add composite-a. It should trigger bind all available.
  AddCompositeNodeSpec("composite-a", {"node-a", "node-b", "node-d"});
  VerifyBindOngoingWithRequests({{"node-a", 1}, {"node-b", 1}, {"node-c", 1}, {"node-d", 1}});

  // Match associated nodes to composite-a.
  DriverIndexReplyWithComposite("node-a", {{"composite-a", 0}});
  DriverIndexReplyWithComposite("node-b", {{"composite-a", 1}});
  DriverIndexReplyWithNoMatch("node-c");
  DriverIndexReplyWithComposite("node-d", {{"composite-a", 2}});

  VerifyMultibindNodes({"node-a", "node-b", "node-d"});
  VerifyCompositeNodeExists(true, "composite-a");
  VerifyNoOngoingBind();

  // Add a spec that shares a node with composite-a. It should trigger bind all available.
  AddCompositeNodeSpec("composite-b", {"node-b", "node-c"});
  VerifyBindOngoingWithRequests({{"node-a", 1}, {"node-b", 1}, {"node-c", 1}, {"node-d", 1}});

  DriverIndexReplyWithComposite("node-a", {{"composite-a", 0}});
  DriverIndexReplyWithComposite("node-b", {{"composite-a", 1}, {"composite-b", 0}});
  DriverIndexReplyWithComposite("node-c", {{"composite-b", 1}});
  DriverIndexReplyWithComposite("node-d", {{"composite-a", 2}});

  VerifyCompositeNodeExists(true, "composite-a");
  VerifyNoOngoingBind();
}

TEST_F(MultibindTest, MultibindSpecs_OverlapBind) {
  AddAndOrphanNode("node-a", /* enable_multibind */ true);
  AddAndOrphanNode("node-b", /* enable_multibind */ true);
  AddAndOrphanNode("node-c", /* enable_multibind */ true);
  AddAndOrphanNode("node-d", /* enable_multibind */ true);
  VerifyNoOngoingBind();

  // Add composite-a. It should trigger bind all available.
  AddCompositeNodeSpec("composite-a", {"node-a", "node-b", "node-d"});
  VerifyBindOngoingWithRequests({{"node-a", 1}, {"node-b", 1}, {"node-c", 1}, {"node-d", 1}});

  // Add a new spec while the nodes are matched.
  DriverIndexReplyWithComposite("node-a", {{"composite-a", 0}});
  DriverIndexReplyWithComposite("node-b", {{"composite-a", 1}});
  AddCompositeNodeSpec_EXPECT_QUEUED("composite-b", {"node-b", "node-c"});
  DriverIndexReplyWithNoMatch("node-c");
  DriverIndexReplyWithComposite("node-d", {{"composite-a", 2}});

  // We should have a new bind ongoing process.
  VerifyBindOngoingWithRequests({{"node-a", 1}, {"node-b", 1}, {"node-c", 1}, {"node-d", 1}});
  VerifyMultibindNodes({"node-a", "node-b", "node-d"});
  VerifyCompositeNodeExists(true, "composite-a");

  // Match the nodes to the composites.
  DriverIndexReplyWithComposite("node-a", {{"composite-a", 0}});
  DriverIndexReplyWithComposite("node-b", {{"composite-a", 1}, {"composite-b", 0}});
  DriverIndexReplyWithNoMatch("node-c");
  DriverIndexReplyWithComposite("node-d", {{"composite-a", 2}, {"composite-b", 1}});

  VerifyMultibindNodes({"node-a", "node-b", "node-d"});
  VerifyCompositeNodeExists(true, "composite-b");
  VerifyNoOngoingBind();
}

TEST_F(MultibindTest, MultibindLegacyAndSpecComposites_Nonoverlap) {
  AddAndOrphanNode("node-a", /* enable_multibind */ true);
  AddAndOrphanNode("node-b", /* enable_multibind */ true);
  AddAndOrphanNode("node-c", /* enable_multibind */ true);
  AddAndOrphanNode("node-d", /* enable_multibind */ true);
  VerifyNoOngoingBind();

  // Add composite-a.
  AddLegacyComposite("composite-a", {"node-a", "node-b", "node-d"});

  // Node-a, node-b, and node-d should match to composite-a, assembling it.
  VerifyLegacyCompositeFragmentIsBound(true, "composite-a", "node-a");
  VerifyLegacyCompositeFragmentIsBound(true, "composite-a", "node-b");
  VerifyLegacyCompositeFragmentIsBound(true, "composite-a", "node-d");
  VerifyLegacyCompositeBuilt(true, "composite-a");
  VerifyPendingBindRequestCount(1);

  // Complete the ongoing bind.
  VerifyBindOngoingWithRequests({{"node-a", 1}, {"node-b", 1}, {"node-c", 1}, {"node-d", 1}});
  DriverIndexReplyWithNoMatch("node-a");
  DriverIndexReplyWithNoMatch("node-b");
  DriverIndexReplyWithNoMatch("node-c");
  DriverIndexReplyWithNoMatch("node-d");
  VerifyMultibindNodes({"node-a", "node-b", "node-d"});

  // Verify and complete the follow up bind for composite-a.
  VerifyBindOngoingWithRequests({{"composite-a", 1}});
  DriverIndexReplyWithNoMatch("composite-a");
  VerifyOrphanedNodes({"node-a.composite-a", "node-c"});
  VerifyNoOngoingBind();

  // Add a new spec. This should trigger a bind all available.
  AddCompositeNodeSpec_EXPECT_BIND_START("composite-b", {"node-b", "node-c"});
  VerifyBindOngoingWithRequests(
      {{"composite-a", 1}, {"node-a", 1}, {"node-b", 1}, {"node-c", 1}, {"node-d", 1}});
  DriverIndexReplyWithNoMatch("node-a");
  DriverIndexReplyWithComposite("node-b", {{"composite-b", 0}});
  DriverIndexReplyWithComposite("node-c", {{"composite-b", 1}});
  DriverIndexReplyWithNoMatch("node-d");
  DriverIndexReplyWithNoMatch("composite-a");

  VerifyCompositeNodeExists(true, "composite-b");

  VerifyOrphanedNodes({"node-a.composite-a"});
  VerifyMultibindNodes({"node-a", "node-b", "node-c", "node-d"});
  VerifyNoOngoingBind();
}

TEST_F(MultibindTest, AddSpecsThenNodes) {
  // Add composite-a and composite-b
  AddCompositeNodeSpec("composite-a", {"node-a", "node-b", "node-c"});
  AddCompositeNodeSpec("composite-b", {"node-b", "node-d"});
  VerifyNoOngoingBind();

  AddAndBindNode("node-a", /* enable_multibind */ true);
  VerifyBindOngoingWithRequests({{"node-a", 1}});
  DriverIndexReplyWithComposite("node-a", {{"composite-a", 0}});
  VerifyMultibindNodes({"node-a"});
  VerifyNoOngoingBind();

  AddAndBindNode("node-b", /* enable_multibind */ true);
  VerifyBindOngoingWithRequests({{"node-b", 1}});
  DriverIndexReplyWithComposite("node-b", {{"composite-a", 1}, {"composite-b", 0}});
  VerifyMultibindNodes({"node-a", "node-b"});
  VerifyNoOngoingBind();

  AddAndBindNode("node-c", /* enable_multibind */ false);
  VerifyBindOngoingWithRequests({{"node-c", 1}});
  DriverIndexReplyWithComposite("node-c", {{"composite-a", 2}});
  VerifyMultibindNodes({"node-a", "node-b"});
  VerifyCompositeNodeExists(true, "composite-a");
  VerifyNoOngoingBind();

  AddAndBindNode("node-d", /* enable_multibind */ true);
  VerifyBindOngoingWithRequests({{"node-d", 1}});
  DriverIndexReplyWithComposite("node-d", {{"composite-b", 1}});
  VerifyMultibindNodes({"node-a", "node-b", "node-d"});
  VerifyCompositeNodeExists(true, "composite-b");
  VerifyNoOngoingBind();
}

TEST_F(MultibindTest, MultibindLegacyAndSpecComposites_Overlap) {
  AddAndOrphanNode("node-a", /* enable_multibind */ true);
  AddAndOrphanNode("node-b", /* enable_multibind */ true);
  AddAndOrphanNode("node-c", /* enable_multibind */ true);
  AddAndOrphanNode("node-d", /* enable_multibind */ true);
  VerifyNoOngoingBind();

  // Add composite-a.
  AddLegacyComposite("composite-a", {"node-a", "node-b", "node-d"});

  // Node-a, node-b, and node-d should match to composite-a, assembling it.
  VerifyLegacyCompositeFragmentIsBound(true, "composite-a", "node-a");
  VerifyLegacyCompositeFragmentIsBound(true, "composite-a", "node-b");
  VerifyLegacyCompositeFragmentIsBound(true, "composite-a", "node-d");
  VerifyLegacyCompositeBuilt(true, "composite-a");
  VerifyPendingBindRequestCount(1);

  // During the ongoing bind, add a spec after node-b is unmatched.
  VerifyBindOngoingWithRequests({{"node-a", 1}, {"node-b", 1}, {"node-c", 1}, {"node-d", 1}});
  DriverIndexReplyWithNoMatch("node-b");
  AddCompositeNodeSpec_EXPECT_QUEUED("composite-b", {"node-b", "node-c"});

  // Match node-c to the spec and node-d to nothing. We should follow up with another bind process.
  DriverIndexReplyWithComposite("node-c", {{"composite-b", 0}});
  DriverIndexReplyWithNoMatch("node-d");
  DriverIndexReplyWithNoMatch("node-a");

  VerifyMultibindNodes({"node-a", "node-b", "node-c", "node-d"});
  VerifyBindOngoingWithRequests(
      {{"composite-a", 1}, {"node-a", 1}, {"node-b", 1}, {"node-c", 1}, {"node-d", 1}});

  DriverIndexReplyWithNoMatch("node-a");
  DriverIndexReplyWithNoMatch("node-b");
  DriverIndexReplyWithComposite("node-c", {{"composite-b", 1}});
  DriverIndexReplyWithNoMatch("node-d");
  DriverIndexReplyWithNoMatch("composite-a");

  VerifyCompositeNodeExists(true, "composite-b");

  VerifyOrphanedNodes({"node-a.composite-a"});
  VerifyMultibindNodes({"node-a", "node-b", "node-c", "node-d"});
  VerifyNoOngoingBind();
}
