// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/fit/defer.h>
#include <lib/maybe-standalone-test/maybe-standalone.h>
#include <lib/zx/bti.h>
#include <lib/zx/iommu.h>
#include <lib/zx/result.h>
#include <zircon/syscalls.h>
#include <zircon/syscalls/iommu.h>

#include <zxtest/zxtest.h>

#include "helpers.h"
#include "test_thread.h"
#include "userpager.h"

namespace pager_tests {

// Smoke test
TEST(Snapshot, Smoke) {
  UserPager pager;
  ASSERT_TRUE(pager.Init());

  Vmo* vmo;
  ASSERT_TRUE(pager.CreateVmo(1, &vmo));
  ASSERT_NOT_NULL(vmo);

  // Create first level clone. Should work with either kind of snapshot.
  auto clone = vmo->Clone(ZX_VMO_CHILD_SNAPSHOT_MODIFIED);

  // Fork a page in the clone, supplying the initial content as needed.
  TestThread t([clone = clone.get()]() -> bool {
    *reinterpret_cast<uint64_t*>(clone->base_addr()) = 0xdead1eaf;
    return true;
  });
  ASSERT_TRUE(t.Start());
  ASSERT_TRUE(pager.WaitForPageRead(vmo, 0, 1, ZX_TIME_INFINITE));
  ASSERT_TRUE(pager.SupplyPages(vmo, 0, 1));
  ASSERT_TRUE(t.Wait());
  EXPECT_EQ(*reinterpret_cast<uint64_t*>(clone->base_addr()), 0xdead1eaf);

  // Now snapshot-ish the clone.
  auto snapshot = clone->Clone(ZX_VMO_CHILD_SNAPSHOT_MODIFIED);

  // Both should see the same previous modification.
  EXPECT_EQ(*reinterpret_cast<uint64_t*>(clone->base_addr()), 0xdead1eaf);
  EXPECT_EQ(*reinterpret_cast<uint64_t*>(snapshot->base_addr()), 0xdead1eaf);

  // Modifying clone should not modify the snapshot.
  ASSERT_TRUE(snapshot->PollPopulatedBytes(0));
  *reinterpret_cast<uint64_t*>(clone->base_addr()) = clone->key();

  // Page in hidden node should now be attributed to snapshot.
  ASSERT_TRUE(snapshot->PollPopulatedBytes(zx_system_get_page_size()));

  EXPECT_EQ(*reinterpret_cast<uint64_t*>(clone->base_addr()), clone->key());
  EXPECT_EQ(*reinterpret_cast<uint64_t*>(snapshot->base_addr()), 0xdead1eaf);

  // Check attribution.
  ASSERT_TRUE(vmo->PollPopulatedBytes(zx_system_get_page_size()));
  ASSERT_TRUE(clone->PollPopulatedBytes(zx_system_get_page_size()));
}

}  // namespace pager_tests
