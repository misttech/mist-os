// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_ZX_TESTS_HELPERS_H_
#define ZIRCON_KERNEL_LIB_MISTOS_ZX_TESTS_HELPERS_H_

#include <lib/fit/defer.h>
#include <lib/mistos/util/system.h>
#include <lib/mistos/zx/vmar.h>
#include <lib/mistos/zx/vmo.h>
#include <lib/zx/result.h>

#include <zxtest/zxtest.h>

namespace vmo_test {

static inline void VmoWrite(const zx::vmo& vmo, uint32_t data, uint64_t offset = 0) {
  zx_status_t status = vmo.write(static_cast<void*>(&data), offset, sizeof(data));
  ASSERT_OK(status, "write failed");
}

static inline uint32_t VmoRead(const zx::vmo& vmo, uint64_t offset = 0) {
  uint32_t val = 0;
  zx_status_t status = vmo.read(&val, offset, sizeof(val));
  EXPECT_OK(status, "read failed");
  return val;
}

static inline void VmoCheck(const zx::vmo& vmo, uint32_t expected, uint64_t offset = 0) {
  uint32_t data;
  zx_status_t status = vmo.read(static_cast<void*>(&data), offset, sizeof(data));
  ASSERT_OK(status, "read failed");
  ASSERT_EQ(expected, data);
}

// Creates a vmo with |page_count| pages and writes (page_index + 1) to each page.
static inline void InitPageTaggedVmo(uint32_t page_count, zx::vmo* vmo) {
  zx_status_t status;
  status = zx::vmo::create(page_count * zx_system_get_page_size(), ZX_VMO_RESIZABLE, vmo);
  ASSERT_OK(status, "create failed");
  for (unsigned i = 0; i < page_count; i++) {
    ASSERT_NO_FATAL_FAILURE(VmoWrite(*vmo, i + 1, i * zx_system_get_page_size()));
  }
}

static inline size_t VmoNumChildren(const zx::vmo& vmo) {
  zx_info_vmo_t info;
  if (vmo.get_info(ZX_INFO_VMO, &info, sizeof(info), nullptr, nullptr) != ZX_OK) {
    return UINT64_MAX;
  }
  return info.num_children;
}

static inline size_t VmoPopulatedBytes(const zx::vmo& vmo) {
  zx_info_vmo_t info;
  if (vmo.get_info(ZX_INFO_VMO, &info, sizeof(info), nullptr, nullptr) != ZX_OK) {
    return UINT64_MAX;
  }
  return info.populated_bytes;
}

static inline bool PollVmoPopulatedBytes(const zx::vmo& vmo, size_t expected_bytes) {
  zx_info_vmo_t info;
  while (true) {
    if (vmo.get_info(ZX_INFO_VMO, &info, sizeof(info), nullptr, nullptr) != ZX_OK) {
      return false;
    }
    if (info.populated_bytes == expected_bytes) {
      return true;
    }
    printf("polling again. actual bytes %zu (%zu pages); expected bytes %zu (%zu pages)\n",
           info.populated_bytes, info.populated_bytes / zx_system_get_page_size(), expected_bytes,
           expected_bytes / zx_system_get_page_size());
    zx::nanosleep(zx::deadline_after(zx::msec(50)));
  }
}

// Simple class for managing vmo mappings w/o any external dependencies.
class Mapping {
 public:
  ~Mapping() {
    if (addr_) {
      ZX_ASSERT(zx::vmar::root_self()->unmap(addr_, len_) == ZX_OK);
    }
  }

  zx_status_t Init(const zx::vmo& vmo, size_t len) {
    zx_status_t status =
        zx::vmar::root_self()->map(ZX_VM_PERM_READ | ZX_VM_PERM_WRITE, 0, vmo, 0, len, &addr_);
    len_ = len;
    return status;
  }

  uint32_t* ptr() { return reinterpret_cast<uint32_t*>(addr_); }
  uint8_t* bytes() { return reinterpret_cast<uint8_t*>(addr_); }

 private:
  uint64_t addr_ = 0;
  size_t len_ = 0;
};

}  // namespace vmo_test

#endif  // ZIRCON_KERNEL_LIB_MISTOS_ZX_TESTS_HELPERS_H_
