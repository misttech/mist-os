// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/mistos/util/system.h>
#include <lib/mistos/zx/vmar.h>
#include <lib/mistos/zx/vmo.h>
#include <zircon/process.h>
#include <zircon/syscalls.h>

#include <arch/defines.h>
#include <fbl/algorithm.h>
#include <zxtest/zxtest.h>

namespace {

TEST(VmoTestCase, Create) {
  zx_status_t status;
  zx_handle_t vmo[16];

  // allocate a bunch of vmos then free them
  for (size_t i = 0; i < std::size(vmo); i++) {
    status = zx_vmo_create(i * zx_system_get_page_size(), 0, &vmo[i]);
    EXPECT_OK(status, "vm_object_create");
  }

  for (size_t i = 0; i < std::size(vmo); i++) {
    status = zx_handle_close(vmo[i]);
    EXPECT_OK(status, "handle_close");
  }
}

TEST(VmoTestCase, ReadWriteBadLen) {
  zx_status_t status;
  zx_handle_t vmo;

  // allocate an object and attempt read/write from it, with bad length
  const size_t len = zx_system_get_page_size() * 4;
  status = zx_vmo_create(len, 0, &vmo);
  EXPECT_OK(status, "vm_object_create");

  char* buf = static_cast<char*>(malloc(len));
  auto defer = fit::defer([buf] { free(buf); });
  for (int i = 1; i <= 2; i++) {
    status = zx_vmo_read(vmo, buf, 0,
                         std::numeric_limits<size_t>::max() - (zx_system_get_page_size() / i));
    EXPECT_EQ(ZX_ERR_OUT_OF_RANGE, status);
    status = zx_vmo_write(vmo, buf, 0,
                          std::numeric_limits<size_t>::max() - (zx_system_get_page_size() / i));
    EXPECT_EQ(ZX_ERR_OUT_OF_RANGE, status);
  }
  status = zx_vmo_read(vmo, buf, 0, len);
  EXPECT_OK(status, "vmo_read");
  status = zx_vmo_write(vmo, buf, 0, len);
  EXPECT_OK(status, "vmo_write");

  // close the handle
  status = zx_handle_close(vmo);
  EXPECT_OK(status, "handle_close");
}

TEST(VmoTestCase, ReadWrite) {
  zx_status_t status;
  zx_handle_t vmo;

  // allocate an object and read/write from it
  const size_t len = zx_system_get_page_size() * 4;
  status = zx_vmo_create(len, 0, &vmo);
  EXPECT_OK(status, "vm_object_create");

  char* buf = static_cast<char*>(malloc(len));
  auto defer = fit::defer([buf] { free(buf); });
  status = zx_vmo_read(vmo, buf, 0, len);
  EXPECT_OK(status, "vm_object_read");

  // make sure it's full of zeros
  size_t count = 0;
  while (count < len) {
    auto c = buf[count];
    EXPECT_EQ(c, 0, "zero test");
    if (c != 0) {
      printf("char at offset %#zx is bad\n", count);
    }
    count++;
  }

  memset(buf, 0x99, len);
  status = zx_vmo_write(vmo, buf, 0, len);
  EXPECT_OK(status, "vm_object_write");

  // map it
  uintptr_t ptr;
  status =
      zx_vmar_map(zx_vmar_root_self(), ZX_VM_PERM_READ | ZX_VM_PERM_WRITE, 0, vmo, 0, len, &ptr);
  EXPECT_OK(status, "vm_map");
  EXPECT_NE(0u, ptr, "vm_map");

  // check that it matches what we last wrote into it
  EXPECT_BYTES_EQ((uint8_t*)buf, (uint8_t*)ptr, len, "mapped buffer");

  status = zx_vmar_unmap(zx_vmar_root_self(), ptr, len);
  EXPECT_OK(status, "vm_unmap");

  // close the handle
  status = zx_handle_close(vmo);
  EXPECT_OK(status, "handle_close");
}

TEST(VmoTestCase, ReadWriteRange) {
  zx_status_t status;
  zx_handle_t vmo;

  // allocate an object
  const size_t len = zx_system_get_page_size() * 4;
  status = zx_vmo_create(len, 0, &vmo);
  EXPECT_OK(status, "vm_object_create");

  // fail to read past end
  auto sizeof_buf = len * 2;
  char* buf = static_cast<char*>(malloc(len * 2));
  auto defer = fit::defer([buf] { free(buf); });
  status = zx_vmo_read(vmo, buf, 0, sizeof_buf);
  EXPECT_EQ(status, ZX_ERR_OUT_OF_RANGE, "vm_object_read past end");

  // Successfully read 0 bytes at end
  status = zx_vmo_read(vmo, buf, len, 0);
  EXPECT_OK(status, "vm_object_read zero at end");

  // Fail to read 0 bytes past end
  status = zx_vmo_read(vmo, buf, len + 1, 0);
  EXPECT_EQ(status, ZX_ERR_OUT_OF_RANGE, "vm_object_read zero past end");

  // fail to write past end
  status = zx_vmo_write(vmo, buf, 0, sizeof_buf);
  EXPECT_EQ(status, ZX_ERR_OUT_OF_RANGE, "vm_object_write past end");

  // Successfully write 0 bytes at end
  status = zx_vmo_write(vmo, buf, len, 0);
  EXPECT_OK(status, "vm_object_write zero at end");

  // Fail to read 0 bytes past end
  status = zx_vmo_write(vmo, buf, len + 1, 0);
  EXPECT_EQ(status, ZX_ERR_OUT_OF_RANGE, "vm_object_write zero past end");

  // Test for unsigned wraparound
  status = zx_vmo_read(vmo, buf, UINT64_MAX - (len / 2), len);
  EXPECT_EQ(status, ZX_ERR_OUT_OF_RANGE, "vm_object_read offset + len wraparound");
  status = zx_vmo_write(vmo, buf, UINT64_MAX - (len / 2), len);
  EXPECT_EQ(status, ZX_ERR_OUT_OF_RANGE, "vm_object_write offset + len wraparound");

  // close the handle
  status = zx_handle_close(vmo);
  EXPECT_OK(status, "handle_close");
}

TEST(VmoTestCase, Map) {
  zx_status_t status;
  zx_handle_t vmo;
  uintptr_t ptr[3] = {};

  // allocate a vmo
  status = zx_vmo_create(4 * zx_system_get_page_size(), 0, &vmo);
  EXPECT_OK(status, "vm_object_create");

  // do a regular map
  ptr[0] = 0;
  status = zx_vmar_map(zx_vmar_root_self(), ZX_VM_PERM_READ, 0, vmo, 0, zx_system_get_page_size(),
                       &ptr[0]);
  EXPECT_OK(status, "map");
  EXPECT_NE(0u, ptr[0], "map address");
  // printf("mapped %#" PRIxPTR "\n", ptr[0]);

  // try to map something completely out of range without any fixed mapping, should succeed
  ptr[2] = UINTPTR_MAX;
  status = zx_vmar_map(zx_vmar_root_self(), ZX_VM_PERM_READ, 0, vmo, 0, zx_system_get_page_size(),
                       &ptr[2]);
  EXPECT_OK(status, "map");
  EXPECT_NE(0u, ptr[2], "map address");

  // try to map something completely out of range fixed, should fail
  uintptr_t map_addr;
  status = zx_vmar_map(zx_vmar_root_self(), ZX_VM_PERM_READ | ZX_VM_SPECIFIC, UINTPTR_MAX, vmo, 0,
                       zx_system_get_page_size(), &map_addr);
  EXPECT_EQ(ZX_ERR_INVALID_ARGS, status, "map");

  // cleanup
  status = zx_handle_close(vmo);
  EXPECT_OK(status, "handle_close");

  for (auto p : ptr) {
    if (p) {
      status = zx_vmar_unmap(zx_vmar_root_self(), p, zx_system_get_page_size());
      EXPECT_OK(status, "unmap");
    }
  }
}

TEST(VmoTestCase, MapRead) {
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
}

TEST(VmoTestCase, Resize) {
  zx_status_t status;
  zx_handle_t vmo;

  // allocate an object
  size_t len = zx_system_get_page_size() * 4;
  status = zx_vmo_create(len, ZX_VMO_RESIZABLE, &vmo);
  EXPECT_OK(status, "vm_object_create");

  // get the size that we set it to
  uint64_t size = 0x99999999;
  status = zx_vmo_get_size(vmo, &size);
  EXPECT_OK(status, "vm_object_get_size");
  EXPECT_EQ(len, size, "vm_object_get_size");

  // try to resize it
  len += zx_system_get_page_size();
  status = zx_vmo_set_size(vmo, len);
  EXPECT_OK(status, "vm_object_set_size");

  // get the size again
  size = 0x99999999;
  status = zx_vmo_get_size(vmo, &size);
  EXPECT_OK(status, "vm_object_get_size");
  EXPECT_EQ(len, size, "vm_object_get_size");

  // try to resize it to a ludicrous size
  status = zx_vmo_set_size(vmo, UINT64_MAX);
  EXPECT_EQ(ZX_ERR_OUT_OF_RANGE, status, "vm_object_set_size too big");

  // resize it to a non aligned size
  status = zx_vmo_set_size(vmo, len + 1);
  EXPECT_OK(status, "vm_object_set_size");

  // size should be rounded up to the next page boundary
  size = 0x99999999;
  status = zx_vmo_get_size(vmo, &size);
  EXPECT_OK(status, "vm_object_get_size");
  EXPECT_EQ(fbl::round_up(len + 1u, static_cast<size_t>(zx_system_get_page_size())), size,
            "vm_object_get_size");
  len = fbl::round_up(len + 1u, static_cast<size_t>(zx_system_get_page_size()));

  // map it
  uintptr_t ptr;
  status = zx_vmar_map(zx_vmar_root_self(), ZX_VM_PERM_READ, 0, vmo, 0, len, &ptr);
  EXPECT_OK(status, "vm_map");
  EXPECT_NE(ptr, 0, "vm_map");

  // attempt to map expecting an non resizable vmo.
  uintptr_t ptr2;
  status = zx_vmar_map(zx_vmar_root_self(), ZX_VM_PERM_READ | ZX_VM_REQUIRE_NON_RESIZABLE, 0, vmo,
                       0, len, &ptr2);
  EXPECT_EQ(ZX_ERR_NOT_SUPPORTED, status, "vm_map");

  // resize it with it mapped
  status = zx_vmo_set_size(vmo, size);
  EXPECT_OK(status, "vm_object_set_size");

  // unmap it
  status = zx_vmar_unmap(zx_vmar_root_self(), ptr, len);
  EXPECT_OK(status, "unmap");

  // close the handle
  status = zx_handle_close(vmo);
  EXPECT_OK(status, "handle_close");
}

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

// Check that non-resizable VMOs cannot get resized.
TEST(VmoTestCase, NoResize) {
  const size_t len = zx_system_get_page_size() * 4;
  zx_handle_t vmo = ZX_HANDLE_INVALID;

  zx_vmo_create(len, 0, &vmo);

  EXPECT_NE(vmo, ZX_HANDLE_INVALID);

  zx_status_t status;
  status = zx_vmo_set_size(vmo, len + zx_system_get_page_size());
  EXPECT_EQ(ZX_ERR_UNAVAILABLE, status, "vm_object_set_size");

  status = zx_vmo_set_size(vmo, len - zx_system_get_page_size());
  EXPECT_EQ(ZX_ERR_UNAVAILABLE, status, "vm_object_set_size");

  size_t size;
  status = zx_vmo_get_size(vmo, &size);
  EXPECT_OK(status, "vm_object_get_size");
  EXPECT_EQ(len, size, "vm_object_get_size");

  uintptr_t ptr;
  status = zx_vmar_map(zx_vmar_root_self(),
                       ZX_VM_PERM_READ | ZX_VM_PERM_WRITE | ZX_VM_REQUIRE_NON_RESIZABLE, 0, vmo, 0,
                       len, &ptr);
  ASSERT_OK(status, "vm_map");
  ASSERT_NE(ptr, 0, "vm_map");

  status = zx_vmar_unmap(zx_vmar_root_self(), ptr, len);
  EXPECT_OK(status, "unmap");

  status = zx_handle_close(vmo);
  EXPECT_OK(status, "handle_close");
}

// Check that the RESIZE right is set on resizable child creation, and required for a resize.
TEST(VmoTestCase, ChildResizeRight) {
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
    EXPECT_EQ(info.rights & ZX_RIGHT_RESIZE, 0);

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
}

TEST(VmoTestCase, SizeAlign) {
  for (uint64_t s = 0; s < zx_system_get_page_size() * 4; s++) {
    zx_handle_t vmo;

    // create a new object with nonstandard size
    zx_status_t status = zx_vmo_create(s, 0, &vmo);
    EXPECT_OK(status, "vm_object_create");

    // should be the size rounded up to the nearest page boundary
    uint64_t size = 0x99999999;
    status = zx_vmo_get_size(vmo, &size);
    EXPECT_OK(status, "vm_object_get_size");
    EXPECT_EQ(fbl::round_up(s, static_cast<size_t>(zx_system_get_page_size())), size,
              "vm_object_get_size");

    // close the handle
    EXPECT_OK(zx_handle_close(vmo), "handle_close");
  }
}

TEST(VmoTestCase, ResizeAlign) {
  // resize a vmo with a particular size and test that the resulting size is aligned on a page
  // boundary.
  zx_handle_t vmo;
  zx_status_t status = zx_vmo_create(0, ZX_VMO_RESIZABLE, &vmo);
  EXPECT_OK(status, "vm_object_create");

  for (uint64_t s = 0; s < zx_system_get_page_size() * 4; s++) {
    // set the size of the object
    status = zx_vmo_set_size(vmo, s);
    EXPECT_OK(status, "vm_object_create");

    // should be the size rounded up to the nearest page boundary
    uint64_t size = 0x99999999;
    status = zx_vmo_get_size(vmo, &size);
    EXPECT_OK(status, "vm_object_get_size");
    EXPECT_EQ(fbl::round_up(s, static_cast<size_t>(zx_system_get_page_size())), size,
              "vm_object_get_size");
  }

  // close the handle
  EXPECT_OK(zx_handle_close(vmo), "handle_close");
}

TEST(VmoTestCase, ContentSize) {
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
}

void RightsTestMapHelper(zx_handle_t vmo, size_t len, uint32_t flags, bool expect_success,
                         zx_status_t fail_err_code) {
  uintptr_t ptr;

  zx_status_t r = zx_vmar_map(zx_vmar_root_self(), flags, 0, vmo, 0, len, &ptr);
  if (expect_success) {
    EXPECT_OK(r);

    r = zx_vmar_unmap(zx_vmar_root_self(), ptr, len);
    EXPECT_OK(r, "unmap");
  } else {
    EXPECT_EQ(fail_err_code, r);
  }
}

zx_rights_t GetHandleRights(zx_handle_t h) {
  zx_info_handle_basic_t info;
  zx_status_t s =
      zx_object_get_info(h, ZX_INFO_HANDLE_BASIC, &info, sizeof(info), nullptr, nullptr);
  if (s != ZX_OK) {
    EXPECT_OK(s);  // Poison the test
    return 0;
  }
  return info.rights;
}

void ChildPermsTestHelper(const zx_handle_t vmo) {
  // Read out the current rights;
  const zx_rights_t parent_rights = GetHandleRights(vmo);

  // Make different kinds of children and ensure we get the correct rights.
  zx_handle_t child;
  EXPECT_OK(zx_vmo_create_child(vmo, ZX_VMO_CHILD_SNAPSHOT, 0, zx_system_get_page_size(), &child));
  EXPECT_EQ(GetHandleRights(child),
            (parent_rights | ZX_RIGHT_GET_PROPERTY | ZX_RIGHT_SET_PROPERTY | ZX_RIGHT_WRITE) &
                ~ZX_RIGHT_EXECUTE);
  EXPECT_OK(zx_handle_close(child));

  EXPECT_OK(zx_vmo_create_child(vmo, ZX_VMO_CHILD_SNAPSHOT | ZX_VMO_CHILD_NO_WRITE, 0,
                                zx_system_get_page_size(), &child));
  EXPECT_EQ(GetHandleRights(child),
            (parent_rights | ZX_RIGHT_GET_PROPERTY | ZX_RIGHT_SET_PROPERTY) & ~ZX_RIGHT_WRITE);
  EXPECT_OK(zx_handle_close(child));

  EXPECT_OK(zx_vmo_create_child(vmo, ZX_VMO_CHILD_SLICE, 0, zx_system_get_page_size(), &child));
  EXPECT_EQ(GetHandleRights(child), parent_rights | ZX_RIGHT_GET_PROPERTY | ZX_RIGHT_SET_PROPERTY);
  EXPECT_OK(zx_handle_close(child));

  EXPECT_OK(zx_vmo_create_child(vmo, ZX_VMO_CHILD_SLICE | ZX_VMO_CHILD_NO_WRITE, 0,
                                zx_system_get_page_size(), &child));
  EXPECT_EQ(GetHandleRights(child),
            (parent_rights | ZX_RIGHT_GET_PROPERTY | ZX_RIGHT_SET_PROPERTY) & ~ZX_RIGHT_WRITE);
  EXPECT_OK(zx_handle_close(child));
}

TEST(VmoTestCase, Rights) {
  char buf[4096];
  size_t len = zx_system_get_page_size() * 4;
  zx_status_t status;
  zx_handle_t vmo, vmo2;

  // allocate an object
  status = zx_vmo_create(len, 0, &vmo);
  EXPECT_OK(status, "vm_object_create");

  // Check that the handle has at least the expected rights.
  // This list should match the list in docs/syscalls/vmo_create.md.
  static const zx_rights_t kExpectedRights =
      ZX_RIGHT_DUPLICATE | ZX_RIGHT_TRANSFER | ZX_RIGHT_WAIT | ZX_RIGHT_READ | ZX_RIGHT_WRITE |
      ZX_RIGHT_MAP | ZX_RIGHT_GET_PROPERTY | ZX_RIGHT_SET_PROPERTY;
  EXPECT_EQ(kExpectedRights, kExpectedRights & GetHandleRights(vmo));

  // test that we can read/write it
  status = zx_vmo_read(vmo, buf, 0, 0);
  EXPECT_EQ(0, status, "vmo_read");
  status = zx_vmo_write(vmo, buf, 0, 0);
  EXPECT_EQ(0, status, "vmo_write");

  vmo2 = ZX_HANDLE_INVALID;
  zx_handle_duplicate(vmo, ZX_RIGHT_READ, &vmo2);
  status = zx_vmo_read(vmo2, buf, 0, 0);
  EXPECT_EQ(0, status, "vmo_read");
  status = zx_vmo_write(vmo2, buf, 0, 0);
  EXPECT_EQ(ZX_ERR_ACCESS_DENIED, status, "vmo_write");
  zx_handle_close(vmo2);

  vmo2 = ZX_HANDLE_INVALID;
  zx_handle_duplicate(vmo, ZX_RIGHT_WRITE, &vmo2);
  status = zx_vmo_read(vmo2, buf, 0, 0);
  EXPECT_EQ(ZX_ERR_ACCESS_DENIED, status, "vmo_read");
  status = zx_vmo_write(vmo2, buf, 0, 0);
  EXPECT_EQ(0, status, "vmo_write");
  zx_handle_close(vmo2);

  vmo2 = ZX_HANDLE_INVALID;
  zx_handle_duplicate(vmo, 0, &vmo2);
  status = zx_vmo_read(vmo2, buf, 0, 0);
  EXPECT_EQ(ZX_ERR_ACCESS_DENIED, status, "vmo_read");
  status = zx_vmo_write(vmo2, buf, 0, 0);
  EXPECT_EQ(ZX_ERR_ACCESS_DENIED, status, "vmo_write");
  zx_handle_close(vmo2);

  // full perm test
  ASSERT_NO_FATAL_FAILURE(ChildPermsTestHelper(vmo));
  ASSERT_NO_FATAL_FAILURE(RightsTestMapHelper(vmo, len, 0, true, 0));
  ASSERT_NO_FATAL_FAILURE(RightsTestMapHelper(vmo, len, ZX_VM_PERM_READ, true, 0));
  ASSERT_NO_FATAL_FAILURE(
      RightsTestMapHelper(vmo, len, ZX_VM_PERM_WRITE, false, ZX_ERR_INVALID_ARGS));
  ASSERT_NO_FATAL_FAILURE(
      RightsTestMapHelper(vmo, len, ZX_VM_PERM_READ | ZX_VM_PERM_WRITE, true, 0));

  // try most of the permutations of mapping and clone a vmo with various rights dropped
  vmo2 = ZX_HANDLE_INVALID;
  zx_handle_duplicate(vmo, ZX_RIGHT_READ | ZX_RIGHT_WRITE | ZX_RIGHT_DUPLICATE, &vmo2);
  ASSERT_NO_FATAL_FAILURE(ChildPermsTestHelper(vmo2));
  ASSERT_NO_FATAL_FAILURE(RightsTestMapHelper(vmo2, len, 0, false, ZX_ERR_ACCESS_DENIED));
  ASSERT_NO_FATAL_FAILURE(
      RightsTestMapHelper(vmo2, len, ZX_VM_PERM_READ, false, ZX_ERR_ACCESS_DENIED));
  ASSERT_NO_FATAL_FAILURE(
      RightsTestMapHelper(vmo2, len, ZX_VM_PERM_WRITE, false, ZX_ERR_ACCESS_DENIED));
  ASSERT_NO_FATAL_FAILURE(RightsTestMapHelper(vmo2, len, ZX_VM_PERM_READ | ZX_VM_PERM_WRITE, false,
                                              ZX_ERR_ACCESS_DENIED));
  zx_handle_close(vmo2);

  vmo2 = ZX_HANDLE_INVALID;
  zx_handle_duplicate(vmo, ZX_RIGHT_READ | ZX_RIGHT_MAP | ZX_RIGHT_DUPLICATE, &vmo2);
  ASSERT_NO_FATAL_FAILURE(ChildPermsTestHelper(vmo2));
  ASSERT_NO_FATAL_FAILURE(RightsTestMapHelper(vmo2, len, 0, true, 0));
  ASSERT_NO_FATAL_FAILURE(RightsTestMapHelper(vmo2, len, ZX_VM_PERM_READ, true, 0));
  ASSERT_NO_FATAL_FAILURE(
      RightsTestMapHelper(vmo2, len, ZX_VM_PERM_WRITE, false, ZX_ERR_INVALID_ARGS));
  ASSERT_NO_FATAL_FAILURE(RightsTestMapHelper(vmo2, len, ZX_VM_PERM_READ | ZX_VM_PERM_WRITE, false,
                                              ZX_ERR_ACCESS_DENIED));
  zx_handle_close(vmo2);

  vmo2 = ZX_HANDLE_INVALID;
  zx_handle_duplicate(vmo, ZX_RIGHT_WRITE | ZX_RIGHT_MAP | ZX_RIGHT_DUPLICATE, &vmo2);
  ASSERT_NO_FATAL_FAILURE(RightsTestMapHelper(vmo2, len, 0, true, 0));
  ASSERT_NO_FATAL_FAILURE(
      RightsTestMapHelper(vmo2, len, ZX_VM_PERM_READ, false, ZX_ERR_ACCESS_DENIED));
  ASSERT_NO_FATAL_FAILURE(
      RightsTestMapHelper(vmo2, len, ZX_VM_PERM_WRITE, false, ZX_ERR_INVALID_ARGS));
  ASSERT_NO_FATAL_FAILURE(RightsTestMapHelper(vmo2, len, ZX_VM_PERM_READ | ZX_VM_PERM_WRITE, false,
                                              ZX_ERR_ACCESS_DENIED));
  zx_handle_close(vmo2);

  vmo2 = ZX_HANDLE_INVALID;
  zx_handle_duplicate(vmo, ZX_RIGHT_READ | ZX_RIGHT_WRITE | ZX_RIGHT_MAP | ZX_RIGHT_DUPLICATE,
                      &vmo2);
  ASSERT_NO_FATAL_FAILURE(ChildPermsTestHelper(vmo2));
  ASSERT_NO_FATAL_FAILURE(RightsTestMapHelper(vmo2, len, 0, true, 0));
  ASSERT_NO_FATAL_FAILURE(RightsTestMapHelper(vmo2, len, ZX_VM_PERM_READ, true, 0));
  ASSERT_NO_FATAL_FAILURE(
      RightsTestMapHelper(vmo2, len, ZX_VM_PERM_WRITE, false, ZX_ERR_INVALID_ARGS));
  ASSERT_NO_FATAL_FAILURE(
      RightsTestMapHelper(vmo2, len, ZX_VM_PERM_READ | ZX_VM_PERM_WRITE, true, 0));
  zx_handle_close(vmo2);

  // test that we can get/set a property on it
  const char* set_name = "test vmo";
  status = zx_object_set_property(vmo, ZX_PROP_NAME, set_name, sizeof(set_name));
  EXPECT_OK(status, "set_property");
  char get_name[ZX_MAX_NAME_LEN];
  status = zx_object_get_property(vmo, ZX_PROP_NAME, get_name, sizeof(get_name));
  EXPECT_OK(status, "get_property");
  EXPECT_STREQ(set_name, get_name, "vmo name");

  // close the handle
  status = zx_handle_close(vmo);
  EXPECT_OK(status, "handle_close");

  // Use wrong handle with wrong permission, and expect wrong type not
  // ZX_ERR_ACCESS_DENIED
  vmo = ZX_HANDLE_INVALID;
  vmo2 = ZX_HANDLE_INVALID;
  status = zx_port_create(0, &vmo);
  EXPECT_OK(status, "zx_port_create");
  status = zx_handle_duplicate(vmo, 0, &vmo2);
  EXPECT_OK(status, "zx_handle_duplicate");
  status = zx_vmo_read(vmo2, buf, 0, 0);
  EXPECT_EQ(ZX_ERR_WRONG_TYPE, status, "vmo_read wrong type");

  // close the handle
  status = zx_handle_close(vmo);
  EXPECT_OK(status, "handle_close");
  status = zx_handle_close(vmo2);
  EXPECT_OK(status, "handle_close");
}

TEST(VmoTestCase, NoWriteResizable) {
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
}

TEST(VmoTestCase, VmoUnbounded) {
  zx::vmo vmo;
  zx_info_handle_basic_t info;

  // Can't create a VMO that's both unbounded and resizable.
  EXPECT_EQ(zx::vmo::create(0, ZX_VMO_UNBOUNDED | ZX_VMO_RESIZABLE, &vmo), ZX_ERR_INVALID_ARGS);

  // Make a a vmo with ZX_VMO_UNBOUNDED option.
  ASSERT_OK(zx::vmo::create(42, ZX_VMO_UNBOUNDED, &vmo));

  // Stream size should be set to size argument.
  uint64_t content_size = 0;
  EXPECT_OK(vmo.get_property(ZX_PROP_VMO_CONTENT_SIZE, &content_size, sizeof(content_size)));
  EXPECT_EQ(content_size, 42);

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
  EXPECT_EQ(info.rights & ZX_RIGHT_RESIZE, 0);

  // Clone can read pages form parent
  EXPECT_OK(clone.read(check, size - len, len));
  EXPECT_EQ(std::strcmp(buf, check), 0);

  ASSERT_OK(clone.get_info(ZX_INFO_HANDLE_BASIC, &info, sizeof(info), nullptr, nullptr));
  EXPECT_EQ(info.rights & ZX_RIGHT_RESIZE, 0);
  EXPECT_EQ(ZX_ERR_UNAVAILABLE, clone.set_size(zx_system_get_page_size()));

#if 0
  // Test the flag on a pager-backed VMO
  zx::vmo pvmo;
  zx::pager pager;
  ASSERT_OK(zx::pager::create(0, &pager));
  zx::port port;
  ASSERT_OK(zx::port::create(0, &port));

  // Can't create a VMO that's both unbounded and resizable.
  EXPECT_EQ(pager.create_vmo(ZX_VMO_UNBOUNDED | ZX_VMO_RESIZABLE, port, 0, 0, &pvmo),
            ZX_ERR_INVALID_ARGS);

  // Size argument must be set to 0 with unbounded flag.
  EXPECT_EQ(pager.create_vmo(ZX_VMO_UNBOUNDED, port, 0, 42, &pvmo), ZX_ERR_INVALID_ARGS);

  ASSERT_OK(pager.create_vmo(ZX_VMO_UNBOUNDED, port, 0, 0, &pvmo));

  // Unbounded VMO does not get the RESIZE right, or be able to be resized.
  ASSERT_OK(vmo.get_info(ZX_INFO_HANDLE_BASIC, &info, sizeof(info), nullptr, nullptr));
  EXPECT_EQ(info.rights & ZX_RIGHT_RESIZE, 0);
  EXPECT_EQ(ZX_ERR_UNAVAILABLE, vmo.set_size(len + zx_system_get_page_size()));

  // Create a clone, which should not be resizable.
  ASSERT_OK(vmo.create_child(ZX_VMO_CHILD_SNAPSHOT_AT_LEAST_ON_WRITE, 0, len, &clone));

  ASSERT_OK(clone.get_info(ZX_INFO_HANDLE_BASIC, &info, sizeof(info), nullptr, nullptr));
  EXPECT_EQ(info.rights & ZX_RIGHT_RESIZE, 0);
  EXPECT_EQ(ZX_ERR_UNAVAILABLE, clone.set_size(zx_system_get_page_size()));
#endif

  vmo.reset();
  clone.reset();
}

}  // namespace
