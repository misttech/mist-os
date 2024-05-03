// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/mistos/util/process.h>
#include <lib/mistos/util/system.h>
#include <lib/mistos/zx/vmo.h>

#include <vector>

#include <arch/defines.h>
#include <zxtest/zxtest.h>

namespace {

TEST(VmoCloneTestCase, SizeAlign) {
  zx::vmo vmo;
  zx_status_t status = zx::vmo::create(0, 0, &vmo);
  EXPECT_OK(status, "zx::vmo::create");

  // create clones with different sizes, make sure the created size is a multiple of a page size
  for (uint64_t s = 0; s < zx_system_get_page_size() * 4; s++) {
    zx::vmo clone_vmo;
    EXPECT_OK(vmo.create_child(ZX_VMO_CHILD_SNAPSHOT, 0, s, &clone_vmo), "vm_clone");

    // should be the size rounded up to the nearest page boundary
    uint64_t size = 0x99999999;
    status = clone_vmo.get_size(&size);
    EXPECT_OK(status, "clone_vmo.get_size");
    EXPECT_EQ(fbl::round_up(s, static_cast<size_t>(zx_system_get_page_size())), size,
              "clone_vmo.get_size");

    // close the handle
    {
      auto hptr = clone_vmo.release();
    }

    EXPECT_FALSE(clone_vmo.is_valid());
  }

  // close the handle
  {
    auto hptr = vmo.release();
  }

  EXPECT_FALSE(vmo.is_valid());
}

// Tests that a vmo's name propagates to its child.
TEST(VmoCloneTestCase, NameProperty) {
  zx::vmo vmo;
  zx::vmo clone_vmo[2];

  // create a vmo
  const size_t size = zx_system_get_page_size() * 4;
  EXPECT_OK(zx::vmo::create(size, 0, &vmo), "vm_object_create");
  EXPECT_OK(vmo.set_property(ZX_PROP_NAME, "test1", 5), "zx_object_set_property");

  // clone it
  clone_vmo[0].reset();
  EXPECT_OK(vmo.create_child(ZX_VMO_CHILD_SNAPSHOT, 0, size, &clone_vmo[0]), "vm_clone");
  EXPECT_TRUE(clone_vmo[0].is_valid(), "vm_clone_handle");
  char name[ZX_MAX_NAME_LEN];
  EXPECT_OK(clone_vmo[0].get_property(ZX_PROP_NAME, name, ZX_MAX_NAME_LEN),
            "zx_object_get_property");
  EXPECT_TRUE(!strcmp(name, "test1"), "get_name");

  // clone it a second time w/o the rights property
  // EXPECT_OK(zx_handle_replace(vmo, ZX_DEFAULT_VMO_RIGHTS & ~ZX_RIGHTS_PROPERTY, &vmo));
  clone_vmo[1].reset();
  EXPECT_OK(vmo.create_child(ZX_VMO_CHILD_SNAPSHOT, 0, size, &clone_vmo[1]), "vm_clone");
  EXPECT_TRUE(clone_vmo[1].is_valid(), "vm_clone_handle");
  EXPECT_OK(clone_vmo[1].get_property(ZX_PROP_NAME, name, ZX_MAX_NAME_LEN),
            "zx_object_get_property");
  EXPECT_FALSE(!strcmp(name, "test1"), "get_name");

  // close the original handle
  {
    auto hptr = vmo.release();
  }

  EXPECT_FALSE(vmo.is_valid());

  // close the clone handles
  for (auto& h : clone_vmo) {
    auto hptr = h.release();
    EXPECT_FALSE(h.is_valid());
  }
}

}  // namespace
