// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fuchsia/ui/composition/internal/cpp/fidl.h>

#include <zxtest/zxtest.h>

#include "src/ui/scenic/lib/utils/helpers.h"
#include "src/ui/scenic/tests/utils/scenic_ctf_test_base.h"

namespace integration_tests {

using fuci_DisplayOwnership = fuchsia::ui::composition::internal::DisplayOwnership;

class DisplayOwnershipIntegrationTest : public ScenicCtfTest {
 protected:
  DisplayOwnershipIntegrationTest() = default;

  void SetUp() override {
    ScenicCtfTest::SetUp();
    ownership_ = ConnectSyncIntoRealm<fuci_DisplayOwnership>();
  }

  fuchsia::ui::composition::internal::DisplayOwnershipSyncPtr ownership_;
};

TEST_F(DisplayOwnershipIntegrationTest, GetEvent) {
  zx::event event;
  ASSERT_EQ(ZX_OK, ownership_->GetEvent(&event));

  EXPECT_TRUE(
      utils::IsEventSignalled(event, fuchsia::ui::composition::internal::SIGNAL_DISPLAY_OWNED));
}

}  // namespace integration_tests
