// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BIN_DRIVER_MANAGER_V2_TESTS_BIND_MANAGER_TEST_BASE_H_
#define SRC_DEVICES_BIN_DRIVER_MANAGER_V2_TESTS_BIND_MANAGER_TEST_BASE_H_

#include <fidl/fuchsia.driver.index/cpp/fidl.h>

#include "src/devices/bin/driver_manager/v2/bind_manager.h"
#include "src/devices/bin/driver_manager/v2/node.h"
#include "src/lib/testing/loop_fixture/test_loop_fixture.h"

// Test class to expose protected functions.
class TestBindManager : public dfv2::BindManager {
 public:
  TestBindManager(dfv2::BindManagerBridge* bridge, dfv2::NodeManager* node_manager,
                  async_dispatcher_t* dispatcher)
      : BindManager(bridge, node_manager, dispatcher) {}

  const std::unordered_map<std::string, std::weak_ptr<dfv2::Node>>& GetOrphanedNodes() const {
    return orphaned_nodes();
  }

  bool GetBindAllOngoing() const { return bind_all_ongoing(); }

  std::vector<dfv2::BindRequest> GetPendingRequests() const { return pending_bind_requests(); }

  const std::vector<dfv2::NodeBindingInfoResultCallback>& GetPendingOrphanRebindCallbacks() const {
    return pending_orphan_rebind_callbacks();
  }

  dfv2::CompositeDeviceManager& GetLegacyCompositeManager() { return legacy_composite_manager(); }
};

class TestDriverIndex final : public fidl::WireServer<fuchsia_driver_index::DriverIndex> {
 public:
  explicit TestDriverIndex(async_dispatcher_t* dispatcher) : dispatcher_(dispatcher) {}

  void MatchDriver(MatchDriverRequestView request, MatchDriverCompleter::Sync& completer) override;
  void WaitForBaseDrivers(WaitForBaseDriversCompleter::Sync& completer) override;
  void AddCompositeNodeSpec(AddCompositeNodeSpecRequestView request,
                            AddCompositeNodeSpecCompleter::Sync& completer) override;

  zx::result<fidl::ClientEnd<fuchsia_driver_index::DriverIndex>> Connect();

  // Pop the next completer with the |id| in |completers_| and reply with |result|.
  void ReplyWithMatch(uint32_t id, zx::result<fuchsia_driver_index::MatchedDriver> result);

  void VerifyRequestCount(uint32_t id, size_t expected_count);
  size_t NumOfMatchRequests() const { return match_request_count_; }

 private:
  // Maps a queue of completers to its associated node's instance ID.
  // The instance ID is extracted from the node properties. A completer is added to the queue
  // whenever MatchDriver() is called.
  std::unordered_map<uint32_t, std::queue<MatchDriverCompleter::Async>> completers_;

  size_t match_request_count_ = 0;
  async_dispatcher_t* dispatcher_;
};

class TestBindManagerBridge final : public dfv2::BindManagerBridge {
 public:
  explicit TestBindManagerBridge(fidl::WireClient<fuchsia_driver_index::DriverIndex> client)
      : client_(std::move(client)) {}
  ~TestBindManagerBridge() = default;

  zx::result<std::vector<CompositeNodeAndDriver>> BindToParentSpec(
      fuchsia_driver_index::wire::MatchedCompositeNodeParentInfo match_info,
      std::weak_ptr<dfv2::Node> node, bool enable_multibind) override {
    return zx::error(ZX_ERR_NOT_SUPPORTED);
  }

  zx::result<std::string> StartDriver(
      dfv2::Node& node, fuchsia_driver_index::wire::MatchedDriverInfo driver_info) override {
    return zx::ok("");
  }

  void RequestMatchFromDriverIndex(
      fuchsia_driver_index::wire::MatchDriverArgs args,
      fit::callback<void(fidl::WireUnownedResult<fuchsia_driver_index::DriverIndex::MatchDriver>&)>
          match_callback) override {
    client_->MatchDriver(args).Then(std::move(match_callback));
  }

 private:
  fidl::WireClient<fuchsia_driver_index::DriverIndex> client_;
};

class TestNodeManager : public dfv2::NodeManager {
 public:
  void Bind(dfv2::Node& node, std::shared_ptr<dfv2::BindResultTracker> result_tracker) override {
    bind_manager_->Bind(node, {}, std::move(result_tracker));
  }

  zx::result<dfv2::DriverHost*> CreateDriverHost() override { return zx::ok(nullptr); }
  void DestroyDriverComponent(
      dfv2::Node& node,
      fit::callback<void(fidl::WireUnownedResult<fuchsia_component::Realm::DestroyChild>& result)>
          callback) override {}
  zx::result<> StartDriver(dfv2::Node& node, std::string_view url,
                           fuchsia_driver_index::DriverPackageType package_type) override {
    return zx::ok();
  }

  void set_bind_manager(TestBindManager* bind_manager) { bind_manager_ = bind_manager; }

 private:
  TestBindManager* bind_manager_;
};

class BindManagerTestBase : public gtest::TestLoopFixture {
 public:
  struct BindManagerData {
    size_t driver_index_request_count;
    size_t orphan_nodes_count;
    size_t pending_bind_count;
    size_t pending_orphan_rebind_count;
  };

  void SetUp() override;
  void TearDown() override;

  BindManagerData CurrentBindManagerData() const;
  void VerifyBindManagerData(BindManagerData expected);

  // Creates a node and adds it to orphaned nodes by invoking bind with a failed match.
  // Should only be called when there's no ongoing bind. The node should not
  // already exist.
  void AddAndOrphanNode(std::string name);

  // Create a node and invoke Bind() for it.
  // If EXPECT_BIND_START, the function verifies that it started a new bind process.
  // If EXPECT_QUEUED, the function verifies that it queued new bind request.
  void AddAndBindNode(std::string name);
  void AddAndBindNode_EXPECT_BIND_START(std::string name);
  void AddAndBindNode_EXPECT_QUEUED(std::string name);

  // Adds a legacy composite.
  // If EXPECT_QUEUED, the function verifies that it queues a TryBindAllAvailable callback.
  void AddLegacyComposite(std::string composite, std::vector<std::string> fragment_names);
  void AddLegacyComposite_EXPECT_QUEUED(std::string composite,
                                        std::vector<std::string> fragment_names);

  // Invoke Bind() for the node with the given |name|. The node should already exist.
  // If EXPECT_BIND_START, the function verifies that it started a new bind process.
  // If EXPECT_QUEUED, the function verifies that it queued new bind request.
  void InvokeBind(std::string name);
  void InvokeBind_EXPECT_BIND_START(std::string name);
  void InvokeBind_EXPECT_QUEUED(std::string name);

  // Invoke TryBindAllAvailable().
  // If EXPECT_BIND_START, the function verifies that it started a new bind process.
  // If EXPECT_QUEUED, the function verifies that it queues a TryBindAllAvailable callback.
  void InvokeTryBindAllAvailable();
  void InvokeTryBindAllAvailable_EXPECT_BIND_START();
  void InvokeTryBindAllAvailable_EXPECT_QUEUED();

  // Invoke DriverIndex reply for a match request for |node|.
  void DriverIndexReplyWithDriver(std::string node);
  void DriverIndexReplyWithNoMatch(std::string node);

  void VerifyNoOngoingBind();
  void VerifyNoQueuedBind();

  // Verifies that there's a ongoing bind process with an expected list of requests.
  // Each pair in |expected_requests| contains the node name and expected number of match requests.
  void VerifyBindOngoingWithRequests(std::vector<std::pair<std::string, size_t>> expected_requests);

  // Verify that the orphaned nodes set in BindManager contains |expected_nodes|.
  void VerifyOrphanedNodes(std::vector<std::string> expected_nodes);

  void VerifyLegacyCompositeFragmentIsBound(std::string composite, std::string fragment_name);
  void VerifyLegacyCompositeBuilt(std::string composite);

  void VerifyPendingBindRequestCount(size_t expected);

 protected:
  std::unordered_map<std::string, std::shared_ptr<dfv2::Node>> nodes() const { return nodes_; }
  std::unordered_map<std::string, uint32_t> instance_ids() const { return instance_ids_; }

 private:
  std::shared_ptr<dfv2::Node> CreateNode(const std::string name);

  // Gets the instance ID for |node_name| from |instance_ids_|. Adds a new entry with a
  // unique instance ID if it's missing.
  uint32_t GetOrAddInstanceId(std::string node_name);

  InspectManager inspect_{dispatcher()};

  std::unique_ptr<TestDriverIndex> driver_index_;
  std::unique_ptr<TestBindManagerBridge> bridge_;
  TestNodeManager node_manager_;
  std::unique_ptr<TestBindManager> bind_manager_;

  std::unordered_map<std::string, std::shared_ptr<dfv2::Node>> nodes_;

  // Maps each node to a unique instance id. The instance id is used to the node's
  // property for binding.
  std::unordered_map<std::string, uint32_t> instance_ids_;

  std::shared_ptr<dfv2::Node> root_;
  std::optional<Devnode> root_devnode_;
  std::optional<Devfs> devfs_;

  fidl::Arena<> arena_;
};

#endif  // SRC_DEVICES_BIN_DRIVER_MANAGER_V2_TESTS_BIND_MANAGER_TEST_BASE_H_
