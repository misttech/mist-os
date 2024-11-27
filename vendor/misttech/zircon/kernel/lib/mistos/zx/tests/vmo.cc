// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/mistos/util/system.h>
#include <lib/mistos/zx/vmar.h>
#include <lib/mistos/zx/vmo.h>
#include <lib/unittest/unittest.h>

namespace unit_testing {
namespace {

#if 0

bool map_read() {
  BEGIN_TEST;

  zx::vmo vmo;

  EXPECT_OK(zx::vmo::create(zx_system_get_page_size() * 2, 0, &vmo));

  uintptr_t vaddr;
  // Map in the first page
  ASSERT_OK(zx::vmar::root_self()->map(ZX_VM_PERM_READ | ZX_VM_PERM_WRITE, 0, vmo, 0,
                                       zx_system_get_page_size(), &vaddr));

  auto unmap = fit::defer([&]() {
    // Cleanup the mapping we created.
    zx::vmar::root_self()->unmap(vaddr, zx_system_get_page_size());
  });

  // Read from the second page of the vmo to mapping.
  // This should succeed and not deadlock in the kernel.
  EXPECT_OK(vmo.read(reinterpret_cast<void*>(vaddr), zx_system_get_page_size(),
                     zx_system_get_page_size()));

  END_TEST;
}
#endif

#if 0
TEST(VmoTestCase, DISABLED_GrowMappedVmo) {
  zx_status_t status;
  zx::vmo vmo;

  // allocate a VMO with an initial size of 2 pages
  size_t vmo_size = zx_system_get_page_size() * 2;
  status = zx::vmo::create(vmo_size, ZX_VMO_RESIZABLE, &vmo);
  EXPECT_OK(status, "vmo_create");

  size_t mapping_size = zx_system_get_page_size() * 8;
  uintptr_t mapping_addr;

  // create an 8 page mapping backed by this VMO
  status = zx::vmar::root_self()->map(ZX_VM_PERM_READ | ZX_VM_PERM_WRITE, 0, vmo, 0, mapping_size,
                                      &mapping_addr);
  ASSERT_OK(status, "vmar_map");
  auto unmap = fit::defer([&]() {
    // Cleanup the mapping we created.
    zx::vmar::root_self()->unmap(mapping_addr, mapping_size);
  });

  // grow the vmo to cover part of the mapping and a partial page
  vmo_size = zx_system_get_page_size() * 6 + zx_system_get_page_size() / 2;

  status = vmo.set_size(vmo_size);
  EXPECT_OK(status, "vmo_set_size");

  // stores to the mapped area past the initial end of the VMO should be reflected in the object
  size_t store_offset_within_vmo = zx_system_get_page_size() * 5;
  *reinterpret_cast<volatile std::byte*>(mapping_addr + store_offset_within_vmo) = std::byte{1};

  std::byte vmo_value;
  status = vmo.read(&vmo_value, store_offset_within_vmo, 1);
  EXPECT_OK(status, "vmo_read");

  // stores to memory past the size set on the VMO but within that page should also be reflected in
  // the object
  size_t store_offset_past_size = vmo_size + 16;
  *reinterpret_cast<volatile std::byte*>(mapping_addr + store_offset_past_size) = std::byte{2};
  status = vmo.read(&vmo_value, store_offset_past_size, 1);
  EXPECT_OK(status, "vmo_read");

  EXPECT_EQ(vmo_value, std::byte{2});

  // writes to the VMO past the initial end of the VMO should be reflected in the mapped area.
  size_t load_offset = zx_system_get_page_size() * 5 + 16;
  std::byte value{3};
  status = vmo.write(&value, load_offset, 1);
  EXPECT_OK(status, "vmo_read");

  EXPECT_EQ(value, *reinterpret_cast<volatile std::byte*>(mapping_addr + load_offset));
}
#endif

// Check that the RESIZE right is set on resizable child creation, and required for a resize.
bool child_resize_right() {
  BEGIN_TEST;

  zx::vmo parent;
  const size_t len = zx_system_get_page_size() * 4;
  ASSERT_OK(zx::vmo::create(len, 0, &parent));

  uint32_t child_types[] = {ZX_VMO_CHILD_SNAPSHOT, ZX_VMO_CHILD_SNAPSHOT_AT_LEAST_ON_WRITE,
                            ZX_VMO_CHILD_SLICE, ZX_VMO_CHILD_SNAPSHOT_MODIFIED};

  for (auto child_type : child_types) {
    zx::vmo vmo;
    ASSERT_OK(parent.create_child(child_type, 0, len, &vmo));

    // A non-resizable VMO does not get the RESIZE right.
    zx_info_handle_basic_t info;
    ASSERT_OK(vmo.get_info(ZX_INFO_HANDLE_BASIC, &info, sizeof(info), nullptr, nullptr));
    EXPECT_EQ(0u, info.rights & ZX_RIGHT_RESIZE);

    // Should not be possible to resize the VMO.
    EXPECT_EQ(ZX_ERR_UNAVAILABLE, vmo.set_size(len + zx_system_get_page_size()));
    vmo.reset();

    // Cannot create resizable slices. Skip the rest of the loop.
    if (child_type == ZX_VMO_CHILD_SLICE) {
      continue;
    }

    // A resizable VMO gets the RESIZE right.
    ASSERT_OK(parent.create_child(child_type | ZX_VMO_CHILD_RESIZABLE, 0, len, &vmo));
    ASSERT_OK(vmo.get_info(ZX_INFO_HANDLE_BASIC, &info, sizeof(info), nullptr, nullptr));
    EXPECT_EQ(info.rights & ZX_RIGHT_RESIZE, ZX_RIGHT_RESIZE);

    // Should be possible to resize the VMO.
    EXPECT_OK(vmo.set_size(len + zx_system_get_page_size()));

    // Remove the RESIZE right. Resizing should fail.
    zx::vmo vmo_dup;
    ASSERT_OK(vmo.duplicate(info.rights & ~ZX_RIGHT_RESIZE, &vmo_dup));
    EXPECT_EQ(ZX_ERR_ACCESS_DENIED, vmo_dup.set_size(zx_system_get_page_size()));
  }

  END_TEST;
}

bool content_size() {
  BEGIN_TEST;

  zx_status_t status;
  zx::vmo vmo;

  size_t len = zx_system_get_page_size() * 4;
  status = zx::vmo::create(len, ZX_VMO_RESIZABLE, &vmo);
  EXPECT_OK(status, "zx::vmo::create");

  uint64_t content_size = 42;
  status = vmo.get_property(ZX_PROP_VMO_CONTENT_SIZE, &content_size, sizeof(content_size));
  EXPECT_OK(status, "get_property");
  EXPECT_EQ(len, content_size);

  uint64_t target_size = len / 3;
  status = vmo.set_property(ZX_PROP_VMO_CONTENT_SIZE, &target_size, sizeof(target_size));
  EXPECT_OK(status, "set_property");

  content_size = 42;
  status = vmo.get_property(ZX_PROP_VMO_CONTENT_SIZE, &content_size, sizeof(content_size));
  EXPECT_OK(status, "get_property");
  EXPECT_EQ(target_size, content_size);

  target_size = len + 15643;
  status = vmo.set_property(ZX_PROP_VMO_CONTENT_SIZE, &target_size, sizeof(target_size));
  EXPECT_OK(status, "set_property");

  content_size = 42;
  status = vmo.get_property(ZX_PROP_VMO_CONTENT_SIZE, &content_size, sizeof(content_size));
  EXPECT_OK(status, "get_property");
  EXPECT_EQ(target_size, content_size);

  target_size = 5461;
  status = vmo.set_size(target_size);
  EXPECT_OK(status, "set_size");

  content_size = 42;
  status = vmo.get_property(ZX_PROP_VMO_CONTENT_SIZE, &content_size, sizeof(content_size));
  EXPECT_OK(status, "get_property");
  EXPECT_EQ(target_size, content_size);

  END_TEST;
}

bool no_write_resizable() {
  BEGIN_TEST;

  zx::vmo vmo;
  ASSERT_OK(zx::vmo::create(zx_system_get_page_size(), 0, &vmo));

  // Show that any way of creating a child that is no write and resizable fails.
  zx::vmo child;
  EXPECT_EQ(ZX_ERR_INVALID_ARGS,
            vmo.create_child(ZX_VMO_CHILD_SNAPSHOT | ZX_VMO_CHILD_NO_WRITE | ZX_VMO_CHILD_RESIZABLE,
                             0, zx_system_get_page_size(), &child));
  EXPECT_EQ(ZX_ERR_INVALID_ARGS,
            vmo.create_child(ZX_VMO_CHILD_SNAPSHOT_AT_LEAST_ON_WRITE | ZX_VMO_CHILD_NO_WRITE |
                                 ZX_VMO_CHILD_RESIZABLE,
                             0, zx_system_get_page_size(), &child));
  EXPECT_EQ(ZX_ERR_INVALID_ARGS,
            vmo.create_child(
                ZX_VMO_CHILD_SNAPSHOT_MODIFIED | ZX_VMO_CHILD_NO_WRITE | ZX_VMO_CHILD_RESIZABLE, 0,
                zx_system_get_page_size(), &child));
  EXPECT_EQ(ZX_ERR_INVALID_ARGS,
            vmo.create_child(ZX_VMO_CHILD_SLICE | ZX_VMO_CHILD_NO_WRITE | ZX_VMO_CHILD_RESIZABLE, 0,
                             zx_system_get_page_size(), &child));
  // Prove that creating a non-resizable one works.
  EXPECT_OK(vmo.create_child(ZX_VMO_CHILD_SNAPSHOT | ZX_VMO_CHILD_NO_WRITE, 0,
                             zx_system_get_page_size(), &child));

  // Do it again with a resziable parent to show the resizability of the parent is irrelevant.
  child.reset();
  vmo.reset();
  ASSERT_OK(zx::vmo::create(zx_system_get_page_size(), ZX_VMO_RESIZABLE, &vmo));
  EXPECT_EQ(ZX_ERR_INVALID_ARGS,
            vmo.create_child(ZX_VMO_CHILD_SNAPSHOT | ZX_VMO_CHILD_NO_WRITE | ZX_VMO_CHILD_RESIZABLE,
                             0, zx_system_get_page_size(), &child));
  EXPECT_EQ(ZX_ERR_INVALID_ARGS,
            vmo.create_child(ZX_VMO_CHILD_SNAPSHOT_AT_LEAST_ON_WRITE | ZX_VMO_CHILD_NO_WRITE |
                                 ZX_VMO_CHILD_RESIZABLE,
                             0, zx_system_get_page_size(), &child));
  EXPECT_EQ(ZX_ERR_INVALID_ARGS,
            vmo.create_child(
                ZX_VMO_CHILD_SNAPSHOT_MODIFIED | ZX_VMO_CHILD_NO_WRITE | ZX_VMO_CHILD_RESIZABLE, 0,
                zx_system_get_page_size(), &child));
  EXPECT_EQ(ZX_ERR_INVALID_ARGS,
            vmo.create_child(ZX_VMO_CHILD_SLICE | ZX_VMO_CHILD_NO_WRITE | ZX_VMO_CHILD_RESIZABLE, 0,
                             zx_system_get_page_size(), &child));
  EXPECT_OK(vmo.create_child(ZX_VMO_CHILD_SNAPSHOT | ZX_VMO_CHILD_NO_WRITE, 0,
                             zx_system_get_page_size(), &child));

  END_TEST;
}

bool vmo_unbounded() {
  BEGIN_TEST;

  zx::vmo vmo;
  zx_info_handle_basic_t info;

  // Can't create a VMO that's both unbounded and resizable.
  EXPECT_EQ(zx::vmo::create(0, ZX_VMO_UNBOUNDED | ZX_VMO_RESIZABLE, &vmo), ZX_ERR_INVALID_ARGS);

  // Make a a vmo with ZX_VMO_UNBOUNDED option.
  ASSERT_OK(zx::vmo::create(42, ZX_VMO_UNBOUNDED, &vmo));

  // Stream size should be set to size argument.
  uint64_t content_size = 0;
  EXPECT_OK(vmo.get_property(ZX_PROP_VMO_CONTENT_SIZE, &content_size, sizeof(content_size)));
  EXPECT_EQ(42u, content_size);

  uint64_t size = 0;
  vmo.get_size(&size);

  // Can't call set_size
  ASSERT_EQ(vmo.set_size(zx_system_get_page_size()), ZX_ERR_UNAVAILABLE);

  // Buffer to test read/writes
  const size_t len = zx_system_get_page_size() * 4;
  char* buf = static_cast<char*>(malloc(len));
  auto defer_buf = fit::defer([buf] { free(buf); });

  // Can read up to end of VMO.
  EXPECT_OK(vmo.read(buf, size - len, len));

  // Can't read off end of VMO (unbounded is a lie).
  EXPECT_EQ(vmo.read(buf, size, len), ZX_ERR_OUT_OF_RANGE);

  // Zero buffer for writes
  memset(buf, 0, len);

  // Can write up to end of VMO
  EXPECT_OK(vmo.write(buf, size - len, len));

  // Can't write off end of VMO
  EXPECT_EQ(vmo.write(buf, size, len), ZX_ERR_OUT_OF_RANGE);

  // Check contents
  char* check = static_cast<char*>(malloc(len));
  auto defer_check = fit::defer([check] { free(check); });

  EXPECT_OK(vmo.read(check, size - len, len));
  EXPECT_EQ(std::strcmp(buf, check), 0);

  // Unbounded clones are not supported, but large clones can be made.
  zx::vmo clone;
  ASSERT_OK(vmo.create_child(ZX_VMO_CHILD_SNAPSHOT, 0, size, &clone));

  // Clone should not have resize right.
  ASSERT_OK(clone.get_info(ZX_INFO_HANDLE_BASIC, &info, sizeof(info), nullptr, nullptr));
  EXPECT_EQ(0u, info.rights & ZX_RIGHT_RESIZE);

  // Clone can read pages form parent
  EXPECT_OK(clone.read(check, size - len, len));
  EXPECT_EQ(0, strcmp(buf, check));

  ASSERT_OK(clone.get_info(ZX_INFO_HANDLE_BASIC, &info, sizeof(info), nullptr, nullptr));
  EXPECT_EQ(0u, info.rights & ZX_RIGHT_RESIZE);
  EXPECT_EQ(ZX_ERR_UNAVAILABLE, clone.set_size(zx_system_get_page_size()));

  vmo.reset();
  clone.reset();

  END_TEST;
}

bool rights_exec() {
  BEGIN_TEST;

  size_t len = zx_system_get_page_size() * 4;
  zx_status_t status;
  zx::vmo vmo;

  // allocate an object
  status = zx::vmo::create(len, 0, &vmo);
  EXPECT_OK(status, "zx::vmo::create");

  // Check that the handle has at least the expected rights.
  // This list should match the list in docs/syscalls/vmo_create.md.
  static const zx_rights_t kExpectedRights =
      ZX_RIGHT_DUPLICATE | ZX_RIGHT_TRANSFER | ZX_RIGHT_WAIT | ZX_RIGHT_READ | ZX_RIGHT_WRITE |
      ZX_RIGHT_MAP | ZX_RIGHT_GET_PROPERTY | ZX_RIGHT_SET_PROPERTY;

  EXPECT_EQ(kExpectedRights, kExpectedRights & vmo.get()->get()->rights());

  status = vmo.replace_as_executable(zx::resource(), &vmo);
  EXPECT_OK(status, "vmo_replace_as_executable");

  EXPECT_EQ(kExpectedRights | ZX_RIGHT_EXECUTE,
            (kExpectedRights | ZX_RIGHT_EXECUTE) & vmo.get()->get()->rights());

  END_TEST;
}

}  // namespace
}  // namespace unit_testing

UNITTEST_START_TESTCASE(mistos_zx_vmo)
UNITTEST("child resize right", unit_testing::child_resize_right)
UNITTEST("content size", unit_testing::content_size)
UNITTEST("no write resizable", unit_testing::no_write_resizable)
UNITTEST("vmo unbounded", unit_testing::vmo_unbounded)
UNITTEST("rights exec", unit_testing::rights_exec)
UNITTEST_END_TESTCASE(mistos_zx_vmo, "mistos_zx_vmo", "mistos vmo test")
