// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/mistos/util/process.h>
#include <lib/mistos/util/system.h>
#include <lib/mistos/zx/vmar.h>
#include <lib/mistos/zx/vmo.h>

#include <vector>

#include <arch/defines.h>
#include <zxtest/zxtest.h>

namespace {

TEST(VmoTestCase, Create) {
  zx_status_t status;
  zx::vmo vmo[16];

  // allocate a bunch of vmos then free them
  for (size_t i = 0; i < std::size(vmo); i++) {
    status = zx::vmo::create(i * zx_system_get_page_size(), 0, &vmo[i]);
    EXPECT_OK(status, "zx::vmo::create");
  }

  for (size_t i = 0; i < std::size(vmo); i++) {
    auto hptr = vmo[i].release();
    EXPECT_EQ(1, hptr->ref_count_debug(), "release");
  }

  for (size_t i = 0; i < std::size(vmo); i++) {
    EXPECT_FALSE(vmo[i].is_valid());
  }
}

TEST(VmoTestCase, ReadWriteBadLen) {
  zx_status_t status;
  zx::vmo vmo;

  // allocate an object and attempt read/write from it, with bad length
  const size_t len = zx_system_get_page_size() * 4;
  status = zx::vmo::create(len, 0, &vmo);
  EXPECT_OK(status, "zx::vmo::create");

  char* buf = static_cast<char*>(malloc(len));
  auto defer = fit::defer([buf] { free(buf); });
  for (int i = 1; i <= 2; i++) {
    status = vmo.read(buf, 0, std::numeric_limits<size_t>::max() - (zx_system_get_page_size() / i));
    EXPECT_EQ(ZX_ERR_OUT_OF_RANGE, status);
    status =
        vmo.write(buf, 0, std::numeric_limits<size_t>::max() - (zx_system_get_page_size() / i));
    EXPECT_EQ(ZX_ERR_OUT_OF_RANGE, status);
  }
  status = vmo.read(buf, 0, len);
  EXPECT_OK(status, "vmo.read");
  status = vmo.write(buf, 0, len);
  EXPECT_OK(status, "vmo.write");

  // close the handle
  { auto hptr = vmo.release(); }

  EXPECT_FALSE(vmo.is_valid());
}

TEST(VmoTestCase, ReadWrite) {
  zx_status_t status;
  zx::vmo vmo;

  // allocate an object and read/write from it
  const size_t len = zx_system_get_page_size() * 4;
  status = zx::vmo::create(len, 0, &vmo);
  EXPECT_OK(status, "zx::vmo::create");

  char* buf = static_cast<char*>(malloc(len));
  auto defer = fit::defer([buf] { free(buf); });
  status = vmo.read(buf, 0, len);
  EXPECT_OK(status, "vmo.read");

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
  status = vmo.write(buf, 0, len);
  EXPECT_OK(status, "vmo.write");

  // map it
  uintptr_t ptr;
  zx::vmar root_vmar{zx_vmar_root_self()};
  status = root_vmar.map(ZX_VM_PERM_READ | ZX_VM_PERM_WRITE, 0, vmo, 0, len, &ptr);
  EXPECT_OK(status, "root_vmar.map");
  EXPECT_NE(0u, ptr, "root_vmar.map");

  // check that it matches what we last wrote into it
  EXPECT_BYTES_EQ((uint8_t*)buf, (uint8_t*)ptr, len, "mapped buffer");

  status = root_vmar.unmap(ptr, len);
  EXPECT_OK(status, "root_vmar.unmap");

  // close the handle
  { auto hptr = vmo.release(); }

  EXPECT_FALSE(vmo.is_valid());
}

TEST(VmoTestCase, ReadWriteRange) {
  zx_status_t status;
  zx::vmo vmo;

  // allocate an object
  const size_t len = zx_system_get_page_size() * 4;
  status = zx::vmo::create(len, 0, &vmo);
  EXPECT_OK(status, "zx::vmo::create");

  // fail to read past end
  char* buf = static_cast<char*>(malloc(len * 2));
  auto defer = fit::defer([buf] { free(buf); });
  status = vmo.read(buf, 0, len * 2);
  EXPECT_EQ(status, ZX_ERR_OUT_OF_RANGE, "vmo.read past end");

  // Successfully read 0 bytes at end
  status = vmo.read(buf, len, 0);
  EXPECT_OK(status, "vmo.read zero at end");

  // Fail to read 0 bytes past end
  status = vmo.read(buf, len + 1, 0);
  EXPECT_EQ(status, ZX_ERR_OUT_OF_RANGE, "vmo.read zero past end");

  // fail to write past end
  status = vmo.write(buf, 0, len * 2);
  EXPECT_EQ(status, ZX_ERR_OUT_OF_RANGE, "vmo.write past end");

  // Successfully write 0 bytes at end
  status = vmo.write(buf, len, 0);
  EXPECT_OK(status, "vmo.write zero at end");

  // Fail to read 0 bytes past end
  status = vmo.write(buf, len + 1, 0);
  EXPECT_EQ(status, ZX_ERR_OUT_OF_RANGE, "vmo.write zero past end");

  // Test for unsigned wraparound
  status = vmo.read(buf, UINT64_MAX - (len / 2), len);
  EXPECT_EQ(status, ZX_ERR_OUT_OF_RANGE, "vmo.read offset + len wraparound");
  status = vmo.write(buf, UINT64_MAX - (len / 2), len);
  EXPECT_EQ(status, ZX_ERR_OUT_OF_RANGE, "vmo.write offset + len wraparound");

  // close the handle
  { auto hptr = vmo.release(); }

  EXPECT_FALSE(vmo.is_valid());
}

TEST(VmoTestCase, NullAndLenReadWrite) {
  zx_status_t status;
  zx::vmo vmo;

  // allocate an object
  const size_t len = zx_system_get_page_size();
  status = zx::vmo::create(len, 0, &vmo);
  EXPECT_OK(status, "zx::vmo::create");

  // Successfully read 0 bytes with nullptr data
  status = vmo.read(nullptr, 0, 0);
  EXPECT_OK(status, "vmo.read zero with nullptr data");

  // Successfully write 0 bytes with nullptr data
  status = vmo.write(nullptr, 0, 0);
  EXPECT_OK(status, "vmo.write zero with nullptr data");
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

  // Size argument must be set to 0 with unbounded flag.
  EXPECT_EQ(zx::vmo::create(42, ZX_VMO_UNBOUNDED, &vmo), ZX_ERR_INVALID_ARGS);

  // Make a a vmo with ZX_VMO_UNBOUNDED option.
  ASSERT_OK(zx::vmo::create(0, ZX_VMO_UNBOUNDED, &vmo));

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
