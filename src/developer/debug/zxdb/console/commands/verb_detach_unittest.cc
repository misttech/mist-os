// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/debug/zxdb/console/commands/verb_detach.h"

#include <gtest/gtest.h>

#include "src/developer/debug/shared/zx_status.h"
#include "src/developer/debug/zxdb/client/mock_remote_api.h"
#include "src/developer/debug/zxdb/client/remote_api_test.h"
#include "src/developer/debug/zxdb/client/session.h"
#include "src/developer/debug/zxdb/client/target_impl.h"
#include "src/developer/debug/zxdb/console/mock_console.h"
#include "src/developer/debug/zxdb/console/nouns.h"
#include "src/lib/fxl/strings/string_printf.h"

namespace zxdb {
namespace {

class TestRemoteAPI : public MockRemoteAPI {
 public:
  void Detach(const debug_ipc::DetachRequest& request,
              fit::callback<void(const Err&, debug_ipc::DetachReply)> cb) override {
    detaches_.push_back(request);

    debug_ipc::DetachReply reply;
    reply.status = debug::Status();

    cb(Err(), reply);
  }

  const std::vector<debug_ipc::DetachRequest>& detaches() const { return detaches_; }

 private:
  std::vector<debug_ipc::DetachRequest> detaches_;
};

class VerbsProcessTest : public RemoteAPITest {
 public:
  TestRemoteAPI* remote_api() const { return remote_api_; }

 protected:
  std::unique_ptr<RemoteAPI> GetRemoteAPIImpl() override {
    auto remote_api = std::make_unique<TestRemoteAPI>();
    remote_api_ = remote_api.get();
    return remote_api;
  }

 private:
  TestRemoteAPI* remote_api_;
};

}  // namespace

TEST_F(VerbsProcessTest, Detach) {
  MockConsole console(&session());

  auto targets = session().system().GetTargetImpls();
  ASSERT_EQ(targets.size(), 1u);

  constexpr uint64_t kProcessKoid = 1;
  const std::string kProcessName = "process-1";

  targets[0]->CreateProcessForTesting(kProcessKoid, kProcessName);

  console.ProcessInputLine("detach");

  // Should've received a detach command.
  ASSERT_EQ(remote_api()->detaches().size(), 1u);
  EXPECT_EQ(remote_api()->detaches()[0].koid, kProcessKoid);

  // Specific detach should work.
  console.ProcessInputLine(fxl::StringPrintf("detach %" PRIu64, kProcessKoid));
  ASSERT_EQ(remote_api()->detaches().size(), 2u);
  EXPECT_EQ(remote_api()->detaches()[1].koid, kProcessKoid);

  // Some random detach should send a specific detach command.
  constexpr uint64_t kSomeOtherKoid = 0x1234;
  console.ProcessInputLine(fxl::StringPrintf("detach %" PRIu64, kSomeOtherKoid));
  ASSERT_EQ(remote_api()->detaches().size(), 3u);
  EXPECT_EQ(remote_api()->detaches()[2].koid, kSomeOtherKoid);
}

TEST_F(VerbsProcessTest, DetachAll) {
  MockConsole console(&session());

  constexpr uint64_t kProcess1Koid = 1;
  constexpr uint64_t kProcess2Koid = 2;
  constexpr uint64_t kProcess3Koid = 3;

  // Create 2 more targets to attach to.
  session().system().CreateNewTarget(nullptr);
  session().system().CreateNewTarget(nullptr);

  auto targets = session().system().GetTargetImpls();
  ASSERT_EQ(targets.size(), 3u);
  targets[0]->CreateProcessForTesting(kProcess1Koid, "process-1");
  targets[1]->CreateProcessForTesting(kProcess2Koid, "process-2");
  targets[2]->CreateProcessForTesting(kProcess3Koid, "process-3");

  console.ProcessInputLine("detach *");

  // Should have detached from everything.
  EXPECT_EQ(remote_api()->detaches().size(), 3u);
  EXPECT_EQ(remote_api()->detaches()[0].koid, kProcess1Koid);
  EXPECT_EQ(remote_api()->detaches()[1].koid, kProcess2Koid);
  EXPECT_EQ(remote_api()->detaches()[2].koid, kProcess3Koid);

  // There should only be the single required target remaining.
  targets = session().system().GetTargetImpls();
  EXPECT_EQ(targets.size(), 1u);
}

}  // namespace zxdb
