// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fuchsia/ui/composition/internal/cpp/fidl.h>
#include <lib/async-loop/testing/cpp/real_loop.h>
#include <lib/sys/component/cpp/testing/realm_builder.h>

#include <zxtest/zxtest.h>

#include "src/ui/scenic/lib/utils/helpers.h"
#include "src/ui/scenic/tests/utils/scenic_realm_builder.h"

namespace integration_tests {

using fuci_DisplayOwnership = fuchsia::ui::composition::internal::DisplayOwnership;
using RealmRoot = component_testing::RealmRoot;

class DisplayOwnershipIntegrationTest : public zxtest::Test, public loop_fixture::RealLoop {
 protected:
  DisplayOwnershipIntegrationTest() = default;

  void SetUp() override {
    realm_ = std::make_unique<RealmRoot>(
        ScenicRealmBuilder().AddRealmProtocol(fuci_DisplayOwnership::Name_).Build());
    ownership_ = realm_->component().Connect<fuci_DisplayOwnership>();
    ownership_.set_error_handler([](zx_status_t status) {
      FAIL("Lost connection to DisplayOwnership: %s", zx_status_get_string(status));
    });
  }

  std::unique_ptr<RealmRoot> realm_;
  fuchsia::ui::composition::internal::DisplayOwnershipPtr ownership_;
};

TEST_F(DisplayOwnershipIntegrationTest, GetEvent) {
  std::optional<zx::event> event;
  ownership_->GetEvent([&event](zx::event e) { event = std::move(e); });
  RunLoopUntil([&event] { return event.has_value(); });
  EXPECT_TRUE(utils::IsEventSignalled(event.value(),
                                      fuchsia::ui::composition::internal::SIGNAL_DISPLAY_OWNED));
}

}  // namespace integration_tests
