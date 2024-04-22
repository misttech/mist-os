// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/mistos/zx/object.h>
#include <lib/mistos/zx/vmo.h>

#include <set>

#include <zxtest/zxtest.h>

namespace {

TEST(ZxUnowned, UsableInContainers) {
  std::set<zx::unowned_vmo> set;
  zx::vmo vmo;
  ASSERT_OK(zx::vmo::create(0u, 0, &vmo));
  ASSERT_TRUE(vmo.is_valid());
  set.emplace(vmo);
  EXPECT_EQ(set.size(), 1u);
  EXPECT_EQ(*set.begin(), vmo.get());
}

TEST(ZxObject, BorrowReturnsUnownedObjectOfSameHandle) {
  zx::vmo vmo;
  ASSERT_OK(zx::vmo::create(0u, 0, &vmo));

  ASSERT_TRUE(vmo.is_valid());
  ASSERT_EQ(vmo.get(), vmo.borrow());
  ASSERT_EQ(zx::unowned_vmo(vmo), vmo.borrow());
}

}  // namespace
