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

}  // namespace
