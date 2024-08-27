// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/mistos/zx/vmo.h>

#include <zxtest/zxtest.h>

#include "fbl/alloc_checker.h"
#include "fbl/ref_ptr.h"
#include "zircon/errors.h"
#include "zircon/types.h"

namespace {

TEST(Arc, Vmo) {
  zx::vmo vmo;
  ASSERT_OK(zx::vmo::create(1, 0u, &vmo));
  zx_handle_t raw = vmo.get();

  fbl::AllocChecker ac;
  zx::ArcVmo arc_vmo = fbl::MakeRefCountedChecked<zx::Arc<zx::vmo>>(&ac, ktl::move(vmo));
  ASSERT(ac.check());
  ASSERT_TRUE((*arc_vmo)->is_valid());
  ASSERT_FALSE(vmo.is_valid());

  {
    zx::ArcVmo arc_vmo_copy = arc_vmo;
    ASSERT_TRUE((*arc_vmo_copy) == raw);

    zx_info_handle_count_t info;
    EXPECT_OK((*arc_vmo)->get_info(ZX_INFO_HANDLE_COUNT, &info, sizeof(info), nullptr, nullptr));
    EXPECT_EQ(info.handle_count, 1);
  }

  ASSERT_TRUE((*arc_vmo)->is_valid());

  zx_info_handle_count_t info;
  EXPECT_OK((*arc_vmo)->get_info(ZX_INFO_HANDLE_COUNT, &info, sizeof(info), nullptr, nullptr));
  EXPECT_EQ(info.handle_count, 1);

  arc_vmo.reset();

  zx::vmo vmo_from_raw(raw);
  EXPECT_EQ(ZX_ERR_BAD_HANDLE,
            vmo_from_raw.get_info(ZX_INFO_HANDLE_COUNT, &info, sizeof(info), nullptr, nullptr), );
}

}  // namespace
