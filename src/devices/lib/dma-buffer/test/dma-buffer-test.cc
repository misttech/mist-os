// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/dma-buffer/buffer.h>
#include <lib/dma-buffer/phys-iter.h>
#include <lib/fake-object/object.h>
#include <lib/zx/result.h>
#include <lib/zx/vmo.h>

#include <map>
#include <memory>
#include <mutex>

#include <gtest/gtest.h>

namespace {

const zx::bti kFakeBti(42);

struct VmoMetadata {
  size_t size = 0;
  uint32_t alignment_log2 = 0;
  zx_handle_t bti_handle = ZX_HANDLE_INVALID;
  uint32_t cache_policy = 0;
  zx_paddr_t start_phys = 0;
  void* virt = nullptr;
  bool contiguous = false;
};

bool unpinned = false;

class VmoWrapper : public fake_object::Object {
 public:
  explicit VmoWrapper(zx::vmo vmo) : fake_object::Object(ZX_OBJ_TYPE_VMO), vmo_(std::move(vmo)) {}
  zx::unowned_vmo vmo() { return vmo_.borrow(); }
  VmoMetadata& metadata() { return metadata_; }

 private:
  zx::vmo vmo_;
  VmoMetadata metadata_ = {};
};

extern "C" {
zx_status_t zx_vmo_create_contiguous(zx_handle_t bti_handle, size_t size, uint32_t alignment_log2,
                                     zx_handle_t* out) {
  zx::vmo vmo = {};
  zx_status_t status = _zx_vmo_create(size, 0, vmo.reset_and_get_address());
  if (status != ZX_OK) {
    return status;
  }

  auto vmo_wrapper = std::make_shared<VmoWrapper>(std::move(vmo));
  vmo_wrapper->metadata().alignment_log2 = alignment_log2;
  vmo_wrapper->metadata().bti_handle = bti_handle;
  vmo_wrapper->metadata().size = size;
  zx::result add_res = fake_object::FakeHandleTable().Add(std::move(vmo_wrapper));
  if (add_res.is_ok()) {
    *out = add_res.value();
  }
  return add_res.status_value();
}

zx_status_t zx_vmo_create(uint64_t size, uint32_t options, zx_handle_t* out) {
  zx::vmo vmo = {};
  zx_status_t status = _zx_vmo_create(size, options, vmo.reset_and_get_address());
  if (status != ZX_OK) {
    return status;
  }

  auto vmo_wrapper = std::make_shared<VmoWrapper>(std::move(vmo));
  vmo_wrapper->metadata().size = size;
  zx::result add_res = fake_object::FakeHandleTable().Add(std::move(vmo_wrapper));
  if (add_res.is_ok()) {
    *out = add_res.value();
  }
  return add_res.status_value();
}

zx_status_t zx_vmar_map(zx_handle_t vmar_handle, zx_vm_option_t options, uint64_t vmar_offset,
                        zx_handle_t vmo_handle, uint64_t vmo_offset, uint64_t len,
                        zx_vaddr_t* mapped_addr) {
  zx::result get_res = fake_object::FakeHandleTable().Get(vmo_handle);
  if (!get_res.is_ok()) {
    return get_res.status_value();
  }
  std::shared_ptr<VmoWrapper> vmo = std::static_pointer_cast<VmoWrapper>(get_res.value());

  zx_status_t status = _zx_vmar_map(vmar_handle, options, vmar_offset, vmo->vmo()->get(),
                                    vmo_offset, len, mapped_addr);
  if (status == ZX_OK) {
    vmo->metadata().virt = reinterpret_cast<void*>(*mapped_addr);
  }
  return status;
}

zx_status_t zx_vmo_set_cache_policy(zx_handle_t handle, uint32_t cache_policy) {
  zx::result get_res = fake_object::FakeHandleTable().Get(handle);
  if (!get_res.is_ok()) {
    return get_res.status_value();
  }
  std::shared_ptr<VmoWrapper> vmo = std::static_pointer_cast<VmoWrapper>(get_res.value());
  vmo->metadata().cache_policy = cache_policy;
  return ZX_OK;
}

zx_status_t zx_bti_pin(zx_handle_t bti_handle, uint32_t options, zx_handle_t vmo_handle,
                       uint64_t offset, uint64_t size, zx_paddr_t* addrs, size_t addrs_count,
                       zx_handle_t* out) {
  static uint64_t current_phys = 0;
  static std::mutex phys_lock;

  if (bti_handle != kFakeBti.get()) {
    return ZX_ERR_BAD_HANDLE;
  }

  if (options & ZX_BTI_CONTIGUOUS) {
    if (addrs_count != 1) {
      return ZX_ERR_INVALID_ARGS;
    }
  } else {
    const auto num_pages =
        fbl::round_up(size, zx_system_get_page_size()) / zx_system_get_page_size();
    if (addrs_count != num_pages) {
      return ZX_ERR_INVALID_ARGS;
    }
  }

  zx::result get_res = fake_object::FakeHandleTable().Get(vmo_handle);
  if (!get_res.is_ok()) {
    return get_res.status_value();
  }
  std::shared_ptr<VmoWrapper> vmo = std::static_pointer_cast<VmoWrapper>(get_res.value());

  std::lock_guard lock(phys_lock);
  vmo->metadata().start_phys = current_phys;
  *addrs = current_phys;
  current_phys += vmo->metadata().size;
  *out = ZX_HANDLE_INVALID;
  return ZX_OK;
}

zx_status_t zx_pmt_unpin(zx_handle_t handle) {
  if (handle == ZX_HANDLE_INVALID) {
    unpinned = true;
  }
  return ZX_OK;
}

}  // extern "C"

}  // namespace

namespace dma_buffer {
TEST(DmaBufferTests, InitWithCacheEnabled) {
  unpinned = false;
  {
    std::unique_ptr<ContiguousBuffer> buffer;
    const size_t size = zx_system_get_page_size() * 4;
    const size_t alignment = 2;
    auto factory = CreateBufferFactory();
    ASSERT_EQ(ZX_OK, factory->CreateContiguous(kFakeBti, size, alignment, &buffer));
    auto test_f = [&buffer, size](fake_object::Object* obj) -> bool {
      auto vmo = static_cast<VmoWrapper*>(obj);
      ZX_ASSERT(vmo->metadata().alignment_log2 == alignment);
      ZX_ASSERT(vmo->metadata().bti_handle == kFakeBti.get());
      ZX_ASSERT(vmo->metadata().cache_policy == 0);
      ZX_ASSERT(vmo->metadata().size == size);
      ZX_ASSERT(buffer->virt() == vmo->metadata().virt);
      ZX_ASSERT(buffer->size() == vmo->metadata().size);
      ZX_ASSERT(buffer->phys() == vmo->metadata().start_phys);
      return false;
    };
    fake_object::FakeHandleTable().ForEach(ZX_OBJ_TYPE_VMO, test_f);
  }
  ASSERT_TRUE(unpinned);
}

TEST(DmaBufferTests, InitWithCacheDisabled) {
  unpinned = false;
  {
    std::unique_ptr<PagedBuffer> buffer;
    auto factory = CreateBufferFactory();
    ASSERT_EQ(ZX_OK, factory->CreatePaged(kFakeBti, zx_system_get_page_size(), false, &buffer));
    auto test_f = [&buffer](fake_object::Object* object) -> bool {
      auto vmo = static_cast<VmoWrapper*>(object);
      ZX_ASSERT(vmo->metadata().alignment_log2 == 0);
      ZX_ASSERT(vmo->metadata().cache_policy == ZX_CACHE_POLICY_UNCACHED_DEVICE);
      ZX_ASSERT(vmo->metadata().size == zx_system_get_page_size());
      ZX_ASSERT(buffer->virt() == vmo->metadata().virt);
      ZX_ASSERT(buffer->size() == vmo->metadata().size);
      ZX_ASSERT(buffer->phys()[0] == vmo->metadata().start_phys);
      return false;
    };
    fake_object::FakeHandleTable().ForEach(ZX_OBJ_TYPE_VMO, test_f);
  }
  ASSERT_TRUE(unpinned);
}

TEST(DmaBufferTests, InitCachedMultiPageBuffer) {
  unpinned = false;
  {
    std::unique_ptr<ContiguousBuffer> buffer;
    auto factory = CreateBufferFactory();
    ASSERT_EQ(ZX_OK,
              factory->CreateContiguous(kFakeBti, zx_system_get_page_size() * 4, 0, &buffer));
    auto test_f = [&buffer](fake_object::Object* object) -> bool {
      auto vmo = static_cast<VmoWrapper*>(object);
      ZX_ASSERT(vmo->metadata().alignment_log2 == 0);
      ZX_ASSERT(vmo->metadata().cache_policy == 0);
      ZX_ASSERT(vmo->metadata().bti_handle == kFakeBti.get());
      ZX_ASSERT(vmo->metadata().size == zx_system_get_page_size() * 4);
      ZX_ASSERT(buffer->virt() == vmo->metadata().virt);
      ZX_ASSERT(buffer->size() == vmo->metadata().size);
      ZX_ASSERT(buffer->phys() == vmo->metadata().start_phys);
      return false;
    };
    fake_object::FakeHandleTable().ForEach(ZX_OBJ_TYPE_VMO, test_f);
  }
  ASSERT_TRUE(unpinned);
}

using Param = struct {
  // The description here will get rendered in the test name, along with a stringified variant of
  // the testing input. In total, it can be used to identify each individual test case in the suite.
  const char* test_desc;

  // PhysIter ctor inputs.
  zx_paddr_t* chunk_list;
  uint64_t chunk_count;
  size_t chunk_size;
  zx_off_t vmo_offset;
  size_t buf_length;
  size_t max_length;

  // Expected outputs.
  uint loop_ct;  // Number of times to increment iterator for full buffer.
  zx_paddr_t* want_addr;
  size_t* want_size;
};

const size_t kPageSize{4096};  // Page size, for maximum brevity.
const size_t kHalfPage{2048};  // Half a page.

// clang-format off
const auto kCases = testing::Values(
    Param(/* test_desc   */ "SimplePageBoundary",
          /* chunk_list  */ (zx_paddr_t[]){kPageSize, 2 * kPageSize},
          /* chunk_count */ 2,
          /* chunk_size  */ kPageSize,
          /* vmo_offset  */ 0,
          /* buf_length  */ 2 * kPageSize,
          /* max_length  */ UINT64_MAX,
          /* loop_ct     */ 2,
          /* want_addr   */ (zx_paddr_t[]){kPageSize, 2 * kPageSize},
          /* want_size   */ (size_t[]){kPageSize, kPageSize}),

    Param(/* test_desc   */ "NonContiguousPages",
          /* chunk_list  */ (zx_paddr_t[]){kPageSize, 3 * kPageSize, 5 * kPageSize},
          /* chunk_count */ 3,
          /* chunk_size  */ kPageSize,
          /* vmo_offset  */ 0,
          /* buf_length  */ 3 * kPageSize,
          /* max_length  */ UINT64_MAX,
          /* loop_ct     */ 3,
          /* want_addr   */ (zx_paddr_t[]){kPageSize, 3 * kPageSize, 5 * kPageSize},
          /* want_size   */ (size_t[]){kPageSize, kPageSize, kPageSize}),

    Param(/* test_desc   */ "PartialFirstPageContiguous",
          /* chunk_list  */ (zx_paddr_t[]){kPageSize, 2 * kPageSize, 3 * kPageSize},
          /* chunk_count */ 3,
          /* chunk_size  */ kPageSize,
          /* vmo_offset  */ kPageSize - 5,
          /* buf_length  */ 2 * kPageSize + 5,
          /* max_length  */ UINT64_MAX,
          /* loop_ct     */ 3,
          /* want_addr   */ (zx_paddr_t[]){2 * kPageSize - 5, 2 * kPageSize, 3 * kPageSize},
          /* want_size   */ (size_t[]){5UL, kPageSize, kPageSize}),

    Param(/* test_desc   */ "PartialFirstPageNonContiguous",
          /* chunk_list  */ (zx_paddr_t[]){kPageSize, 3 * kPageSize, 5 * kPageSize},
          /* chunk_count */ 3,
          /* chunk_size  */ kPageSize,
          /* vmo_offset  */ kPageSize - 5,
          /* buf_length  */ 2 * kPageSize + 5,
          /* max_length  */ UINT64_MAX,
          /* loop_ct     */ 3,
          /* want_addr   */ (zx_paddr_t[]){2 * kPageSize - 5, 3 * kPageSize, 5 * kPageSize},
          /* want_size   */ (size_t[]){5UL, kPageSize, kPageSize}),

    Param(/* test_desc   */ "PartialLastPageContiguous",
          /* chunk_list  */ (zx_paddr_t[]){kPageSize, 2 * kPageSize, 3 * kPageSize},
          /* chunk_count */ 3,
          /* chunk_size  */ kPageSize,
          /* vmo_offset  */ 0,
          /* buf_length  */ 2 * kPageSize + 5,
          /* max_length  */ UINT64_MAX,
          /* loop_ct     */ 3,
          /* want_addr   */ (zx_paddr_t[]){kPageSize, 2 * kPageSize, 3 * kPageSize},
          /* want_size   */ (size_t[]){kPageSize, kPageSize, 5UL}),

    Param(/* test_desc   */ "PartialLastPageNonContiguous",
          /* chunk_list  */ (zx_paddr_t[]){kPageSize, 3 * kPageSize, 5 * kPageSize},
          /* chunk_count */ 3,
          /* chunk_size  */ kPageSize,
          /* vmo_offset  */ 0,
          /* buf_length  */ 2 * kPageSize + 5,
          /* max_length  */ UINT64_MAX,
          /* loop_ct     */ 3,
          /* want_addr   */ (zx_paddr_t[]){kPageSize, 3 * kPageSize, 5 * kPageSize},
          /* want_size   */ (size_t[]){kPageSize, kPageSize, 5UL}),

    Param(/* test_desc   */ "SubChunkMaxLength",
          /* chunk_list  */ (zx_paddr_t[]){kPageSize, 2 * kPageSize},
          /* chunk_count */ 2,
          /* chunk_size  */ kPageSize,
          /* vmo_offset  */ 0,
          /* buf_length  */ 2 * kPageSize,
          /* max_length  */ kHalfPage,
          /* loop_ct     */ 4,
          /* want_addr   */ (zx_paddr_t[]){2 * kHalfPage, 3 * kHalfPage, 4 * kHalfPage, 5 * kHalfPage},
          /* want_size   */ (size_t[]){kHalfPage, kHalfPage, kHalfPage, kHalfPage}),

    Param(/* test_desc   */ "ZxBtiContiguousLargeBuffer",
          /* chunk_list  */ (zx_paddr_t[]){kPageSize},
          /* chunk_count */ 1,
          /* chunk_size  */ kPageSize,
          /* vmo_offset  */ 0,
          /* buf_length  */ 1UL << 20, // 1MiB.
          /* max_length  */ UINT64_MAX,
          /* loop_ct     */ 1,
          /* want_addr   */ (zx_paddr_t[]){kPageSize},
          /* want_size   */ (size_t[]){1UL << 20}),

    Param(/* test_desc   */ "ZxBtiCompressContiguous",
          /* chunk_list  */ (zx_paddr_t[]){kHalfPage, 2 * kHalfPage, 3 * kHalfPage},
          /* chunk_count */ 3,
          /* chunk_size  */ kHalfPage,
          /* vmo_offset  */ 0,
          /* buf_length  */ 3 * kHalfPage,
          /* max_length  */ UINT64_MAX,
          /* loop_ct     */ 3,
          /* want_addr   */ (zx_paddr_t[]){kHalfPage, 2 * kHalfPage, 3 * kHalfPage},
          /* want_size   */ (size_t[]){kHalfPage, kHalfPage, kHalfPage}),

    Param(/* test_desc   */ "ZxBtiCompressNonContiguous",
          /* chunk_list  */ (zx_paddr_t[]){kHalfPage, 3 * kHalfPage, 5 * kHalfPage},
          /* chunk_count */ 3,
          /* chunk_size  */ kHalfPage,
          /* vmo_offset  */ 0,
          /* buf_length  */ 3 * kHalfPage,
          /* max_length  */ UINT64_MAX,
          /* loop_ct     */ 3,
          /* want_addr   */ (zx_paddr_t[]){kHalfPage, 3 * kHalfPage, 5 * kHalfPage},
          /* want_size   */ (size_t[]){kHalfPage, kHalfPage, kHalfPage}));
// clang-format on

class Parameterized : public testing::TestWithParam<Param> {};

TEST_P(Parameterized, TestRun) {
  auto p = GetParam();

  PhysIter phys_iter{p.chunk_list, p.chunk_count, p.chunk_size,
                     p.vmo_offset, p.buf_length,  p.max_length};
  auto itr = phys_iter.begin();

  uint i;
  for (i = 0; i < p.loop_ct; i++) {
    EXPECT_NE(itr, phys_iter.end()) << "i=" << i << std::endl;
    auto [addr, size] = *(itr++);
    EXPECT_EQ(p.want_addr[i], addr) << "i=" << i << std::endl;
    EXPECT_EQ(p.want_size[i], size) << "i=" << i << std::endl;
  }

  EXPECT_EQ(itr, phys_iter.end()) << "i=" << i << std::endl;
}

INSTANTIATE_TEST_SUITE_P(PhysIterTest, Parameterized, kCases,
                         [](const testing::TestParamInfo<Parameterized::ParamType>& info) {
                           std::stringstream test_name;
                           test_name << info.index << "_" << info.param.test_desc;
                           return test_name.str();
                         });

}  // namespace dma_buffer
