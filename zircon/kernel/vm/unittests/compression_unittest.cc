// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/fit/defer.h>

#include <ktl/variant.h>
#include <vm/lz4_compressor.h>
#include <vm/tri_page_storage.h>

#include "test_helper.h"

#include <ktl/enforce.h>

namespace vm_unittest {

namespace {

bool lz4_compress_smoke_test() {
  BEGIN_TEST;

  fbl::RefPtr<VmLz4Compressor> lz4 = VmLz4Compressor::Create();
  ASSERT_TRUE(lz4);

  fbl::AllocChecker ac;
  fbl::Array<char> src = fbl::MakeArray<char>(&ac, PAGE_SIZE);
  ASSERT_TRUE(ac.check());
  fbl::Array<char> compressed = fbl::MakeArray<char>(&ac, PAGE_SIZE);
  ASSERT_TRUE(ac.check());
  fbl::Array<char> uncompressed = fbl::MakeArray<char>(&ac, PAGE_SIZE);
  ASSERT_TRUE(ac.check());

  // Cannot generate random data, as it might not compress, so generate something that would
  // definitely be RLE compressible.
  for (size_t i = 0; i < PAGE_SIZE; i++) {
    src[i] = static_cast<char>((i / 128) & 0xff);
  }

  VmCompressionStrategy::CompressResult result =
      lz4->Compress(src.get(), compressed.get(), PAGE_SIZE);
  EXPECT_TRUE(ktl::holds_alternative<size_t>(result));

  lz4->Decompress(compressed.get(), ktl::get<size_t>(result), uncompressed.get());
  EXPECT_EQ(0, memcmp(src.get(), uncompressed.get(), PAGE_SIZE));

  END_TEST;
}

bool lz4_zero_dedupe_test() {
  BEGIN_TEST;

  fbl::RefPtr<VmLz4Compressor> lz4 = VmLz4Compressor::Create();
  ASSERT_TRUE(lz4);

  fbl::AllocChecker ac;
  fbl::Array<char> zero = fbl::MakeArray<char>(&ac, PAGE_SIZE);
  ASSERT_TRUE(ac.check());
  memset(zero.get(), 0, PAGE_SIZE);
  fbl::Array<char> dst = fbl::MakeArray<char>(&ac, PAGE_SIZE);
  ASSERT_TRUE(ac.check());

  // Check the zero page is determined to be zero.
  EXPECT_TRUE(ktl::holds_alternative<VmCompressionStrategy::ZeroTag>(
      lz4->Compress(zero.get(), dst.get(), PAGE_SIZE)));

  // Restricting the output size somewhat significantly should not prevent zero detection.
  EXPECT_TRUE(ktl::holds_alternative<VmCompressionStrategy::ZeroTag>(
      lz4->Compress(zero.get(), dst.get(), 64)));

  // Setting a byte should prevent zero detection.
  zero[4] = 1;
  EXPECT_TRUE(ktl::holds_alternative<size_t>(lz4->Compress(zero.get(), dst.get(), PAGE_SIZE)));

  END_TEST;
}

void write_zeros(vm_page_t* page, size_t len) {
  DEBUG_ASSERT(page);
  DEBUG_ASSERT(len <= PAGE_SIZE);
  uint8_t* data = static_cast<uint8_t*>(paddr_to_physmap(page->paddr()));
  for (size_t i = 0; i < len; i++) {
    data[i] = 0;
  }
}

void write_pattern(vm_page_t* page, size_t len, uint64_t offset) {
  DEBUG_ASSERT(page);
  DEBUG_ASSERT(len <= PAGE_SIZE);
  uint8_t* data = static_cast<uint8_t*>(paddr_to_physmap(page->paddr()));
  for (size_t i = 0; i < len; i++) {
    data[i] = static_cast<uint8_t>((i + offset) % 256);
  }
}

bool validate_pattern(const void* data, size_t len, uint64_t offset) {
  for (size_t i = 0; i < len; i++) {
    if (static_cast<const uint8_t*>(data)[i] != static_cast<uint8_t>((i + offset % 256))) {
      return false;
    }
  }
  return true;
}

VmCompressedStorage::CompressedRef store(VmTriPageStorage& storage, vm_page_t* page, size_t len) {
  auto [maybe_ref, return_page] = storage.Store(page, len);
  if (return_page) {
    pmm_free_page(return_page);
  }
  ASSERT(maybe_ref.has_value());
  return *maybe_ref;
}

VmCompressedStorage::CompressedRef store_pattern(VmTriPageStorage& storage, size_t len,
                                                 uint64_t offset) {
  vm_page_t* page = nullptr;

  zx_status_t status = pmm_alloc_page(0, &page);
  ASSERT(status == ZX_OK);

  write_pattern(page, len, offset);

  return store(storage, page, len);
}

bool tri_page_storage_smoke_test() {
  BEGIN_TEST;

  VmTriPageStorage storage;

  constexpr uint32_t kMetadata = 0xdeadbeef;
  auto ref = store_pattern(storage, 64, 0);

  // Ensure metadata is 0 until it is set manually.
  EXPECT_EQ(0u, storage.GetMetadata(ref));
  storage.SetMetadata(ref, kMetadata);

  // Ensure the data in storage is correct.
  auto [data, metadata, size] = storage.CompressedData(ref);
  EXPECT_TRUE(validate_pattern(data, 64, 0));
  EXPECT_EQ(kMetadata, storage.GetMetadata(ref));

  // Cleanup.
  storage.Free(ref);

  END_TEST;
}

// Validate that all three slots get used efficiently.
bool tri_page_storage_packing() {
  BEGIN_TEST;

  VmTriPageStorage storage;

  constexpr uint32_t kMetadata[3] = {0xdeadbeef, 0xbadbeef, 0xc0ffee};
  VmCompressedStorage::CompressedRef refs[3] = {
      store_pattern(storage, 64, 0), store_pattern(storage, 64, 1), store_pattern(storage, 64, 2)};
  for (int i = 0; i < 3; i++) {
    storage.SetMetadata(refs[i], kMetadata[i]);
  }

  // These three should have been packed into a single storage page.
  EXPECT_EQ(PAGE_SIZE, (int)storage.GetMemoryUsage().compressed_storage_bytes);

  for (int i = 0; i < 3; i++) {
    auto [data, metadata, size] = storage.CompressedData(refs[i]);
    EXPECT_EQ(64u, size);
    EXPECT_TRUE(validate_pattern(data, 64, i));
    EXPECT_EQ(kMetadata[i], metadata);
  }

  // Cleanup.
  for (auto ref : refs) {
    storage.Free(ref);
  }

  END_TEST;
}

// Ensure slots that become free are considered as candidates for later allocations and that their
// metadata is reset.
bool tri_page_storage_reuse_after_free() {
  BEGIN_TEST;

  VmTriPageStorage storage;

  constexpr uint32_t kMetadata[3] = {0xdeadbeef, 0xbadbeef, 0xc0ffee};
  VmCompressedStorage::CompressedRef refs[3] = {
      store_pattern(storage, 64, 0), store_pattern(storage, 64, 1), store_pattern(storage, 64, 2)};
  for (int i = 0; i < 3; i++) {
    storage.SetMetadata(refs[i], kMetadata[i]);
  }

  // These three should have been packed into a single storage page.
  EXPECT_EQ(PAGE_SIZE, (int)storage.GetMemoryUsage().compressed_storage_bytes);

  storage.Free(refs[0]);
  storage.Free(refs[2]);
  refs[0] = store_pattern(storage, 64, 0);
  refs[2] = store_pattern(storage, 64, 2);

  // Should still only be one page.
  EXPECT_EQ(PAGE_SIZE, (int)storage.GetMemoryUsage().compressed_storage_bytes);

  storage.Free(refs[1]);
  refs[1] = store_pattern(storage, 64, 1);
  EXPECT_EQ(PAGE_SIZE, (int)storage.GetMemoryUsage().compressed_storage_bytes);

  for (int i = 0; i < 3; i++) {
    auto [data, metadata, size] = storage.CompressedData(refs[i]);
    EXPECT_EQ(size, 64u);
    EXPECT_TRUE(validate_pattern(data, 64, i));
    EXPECT_EQ(0u, metadata);
  }

  // Cleanup.
  for (auto ref : refs) {
    storage.Free(ref);
  }

  END_TEST;
}

// Check that if multiple free slots would be available, an optimal one is chosen.
bool tri_page_storage_optimal_bucket() {
  BEGIN_TEST;

  VmTriPageStorage storage;

  constexpr uint64_t kMinSize = 128;
  constexpr uint64_t kMaxSize = PAGE_SIZE - 128;
  constexpr uint64_t kStep = 64;
  constexpr uint64_t kNumItems = (kMaxSize - kMinSize) / kStep;

  auto alloc_size = [](uint64_t item, uint64_t slot) {
    const uint64_t size = kMinSize + kStep * item;
    const uint64_t padding_size = PAGE_SIZE - size;
    return (item % 2) == slot ? size : padding_size;
  };

  uint32_t refs[kNumItems][2];

  for (uint64_t i = 0; i < kNumItems; i++) {
    for (uint64_t j = 0; j < 2; j++) {
      refs[i][j] = store_pattern(storage, alloc_size(i, j), i * 2 + j).value();
    }
    EXPECT_EQ((i + 1) * PAGE_SIZE, storage.GetMemoryUsage().compressed_storage_bytes);
  }

  for (uint64_t i = 0; i < kNumItems; i++) {
    for (int j = 0; j < 2; j++) {
      auto [data, _, size] = storage.CompressedData(VmTriPageStorage::CompressedRef(refs[i][j]));
      EXPECT_EQ(size, alloc_size(i, j));
      EXPECT_TRUE(validate_pattern(data, size, i * 2 + j));
    }
  }

  // Remove each sized allocations to create a hole in each page.
  for (uint64_t i = 0; i < kNumItems; i++) {
    storage.Free(VmTriPageStorage::CompressedRef(refs[i][i % 2]));
  }

  EXPECT_EQ(kNumItems * PAGE_SIZE, storage.GetMemoryUsage().compressed_storage_bytes);

  // Place new allocations in a semi-random order, should slot into all the gaps.
  for (uint64_t i = 0; i < kNumItems; i += 2) {
    refs[i][i % 2] = store_pattern(storage, alloc_size(i, i % 2), i * 2 + (i % 2)).value();
    EXPECT_EQ(kNumItems * PAGE_SIZE, storage.GetMemoryUsage().compressed_storage_bytes);
  }
  for (int i = kNumItems - 1; i > 0; i -= 2) {
    refs[i][i % 2] = store_pattern(storage, alloc_size(i, i % 2), i * 2 + (i % 2)).value();
    EXPECT_EQ(kNumItems * PAGE_SIZE, storage.GetMemoryUsage().compressed_storage_bytes);
  }

  for (uint64_t i = 0; i < kNumItems; i++) {
    for (int j = 0; j < 2; j++) {
      auto [data, _, size] = storage.CompressedData(VmTriPageStorage::CompressedRef(refs[i][j]));
      EXPECT_EQ(size, alloc_size(i, j));
      EXPECT_TRUE(validate_pattern(data, size, i * 2 + j));
    }
  }

  // Cleanup
  for (auto& ref : refs) {
    for (uint32_t j : ref) {
      storage.Free(VmTriPageStorage::CompressedRef(j));
    }
  }

  END_TEST;
}

// Check limits of slot packing and ensure no data corruption
bool tri_page_storage_capacity() {
  BEGIN_TEST;

  VmTriPageStorage storage;

  // Test storage sizes that leave technically leaves 1 byte free, but get packed into a single
  // storage page.
  constexpr uint64_t kTestSizes[3] = {PAGE_SIZE / 4, PAGE_SIZE / 2, PAGE_SIZE / 4 - 1};

  VmCompressedStorage::CompressedRef refs[3] = {store_pattern(storage, kTestSizes[0], 0),
                                                store_pattern(storage, kTestSizes[1], 1),
                                                store_pattern(storage, kTestSizes[2], 2)};
  EXPECT_EQ(PAGE_SIZE, (int)storage.GetMemoryUsage().compressed_storage_bytes);

  for (int i = 0; i < 3; i++) {
    auto [data, _, size] = storage.CompressedData(refs[i]);
    EXPECT_EQ(size, kTestSizes[i]);
    EXPECT_TRUE(validate_pattern(data, size, i));
  }

  // The extra byte might be able to stored. Try replacing each slot with a one byte larger storage
  // and ensure no corruption.
  for (int i = 0; i < 3; i++) {
    storage.Free(refs[i]);
    refs[i] = store_pattern(storage, kTestSizes[i] + 1, i);

    for (int j = 0; j < 3; j++) {
      auto [data, _, size] = storage.CompressedData(refs[j]);
      const uint64_t expected_size = i == j ? kTestSizes[j] + 1 : kTestSizes[j];
      EXPECT_EQ(size, expected_size);
      EXPECT_TRUE(validate_pattern(data, size, j));
    }

    storage.Free(refs[i]);
    refs[i] = store_pattern(storage, kTestSizes[i], i);
    EXPECT_EQ(PAGE_SIZE, (int)storage.GetMemoryUsage().compressed_storage_bytes);
    // Check that the two slots we didn't touch are unchanged. For simplicity we can just therefore
    // check all three slots.
    for (int j = 0; j < 3; j++) {
      auto [data, _, size] = storage.CompressedData(refs[j]);
      EXPECT_EQ(size, kTestSizes[j]);
      EXPECT_TRUE(validate_pattern(data, size, j));
    }
  }

  // Cleanup.
  for (auto ref : refs) {
    storage.Free(ref);
  }

  END_TEST;
}

// Test that an allocation picks the slot that maximizes the next allocation space.
bool tri_page_storage_maxmize_free_space() {
  BEGIN_TEST;

  VmTriPageStorage storage;

  constexpr uint64_t kSmallSize = 128;
  constexpr uint64_t kLargeSize = PAGE_SIZE - kSmallSize * 2;

  // First store something small, that should go into the left slot of the storage page.
  VmCompressedStorage::CompressedRef small_ref1 = store_pattern(storage, kSmallSize, 0);

  // Attempt to store a small and a large item in either order. Regardless of the order both should
  // fit into a single storage page, as the target slot should be chosen to maximize the remaining
  // free space. This means if the small one is inserted first, it must place it in the right slot,
  // leaving the middle slot free for the large item, and if the large item is inserted first it
  // must pick the middle slot and leave the right slot free.
  VmCompressedStorage::CompressedRef large_ref = store_pattern(storage, kLargeSize, 1);
  VmCompressedStorage::CompressedRef small_ref2 = store_pattern(storage, kSmallSize, 2);
  EXPECT_EQ(PAGE_SIZE, (int)storage.GetMemoryUsage().compressed_storage_bytes);

  storage.Free(small_ref2);
  storage.Free(large_ref);

  small_ref2 = store_pattern(storage, kSmallSize, 2);
  large_ref = store_pattern(storage, kLargeSize, 1);
  EXPECT_EQ(PAGE_SIZE, (int)storage.GetMemoryUsage().compressed_storage_bytes);

  storage.Free(small_ref1);
  storage.Free(small_ref2);
  storage.Free(large_ref);

  END_TEST;
}

bool tri_page_storage_small_storage() {
  BEGIN_TEST;

  VmTriPageStorage storage;

  // Store two very small things, these should end up in the left and right slots.
  VmCompressedStorage::CompressedRef small_ref1 = store_pattern(storage, 1, 0);
  VmCompressedStorage::CompressedRef small_ref2 = store_pattern(storage, 1, 1);

  // Store something large to fill up the rest of the page.
  VmCompressedStorage::CompressedRef large_ref = store_pattern(storage, PAGE_SIZE - 64, 0);

  EXPECT_EQ(PAGE_SIZE, (int)storage.GetMemoryUsage().compressed_storage_bytes);

  // Should be able to free one of the small items.
  storage.Free(small_ref1);

  // Storing the small item again might now need to allocate a new page, since the free space
  // created by the free is too small to be considered a bucket.
  small_ref1 = store_pattern(storage, 1, 0);
  EXPECT_EQ(2 * PAGE_SIZE, (int)storage.GetMemoryUsage().compressed_storage_bytes);

  storage.Free(small_ref1);
  storage.Free(small_ref2);
  storage.Free(large_ref);

  END_TEST;
}

// Test that the high-level compression-deconmpression flow preserves page data and metadata.
bool compression_smoke_test() {
  BEGIN_TEST;

  constexpr uint32_t kCompressionThreshhold = static_cast<uint32_t>(PAGE_SIZE) * 70u / 100u;
  fbl::AllocChecker ac;
  fbl::RefPtr<VmLz4Compressor> lz4 = VmLz4Compressor::Create();
  ASSERT_TRUE(lz4);
  fbl::RefPtr<VmTriPageStorage> storage = fbl::MakeRefCountedChecked<VmTriPageStorage>(&ac);
  ASSERT_TRUE(ac.check());
  fbl::RefPtr<VmCompression> compression = fbl::MakeRefCountedChecked<VmCompression>(
      &ac, ktl::move(storage), ktl::move(lz4), kCompressionThreshhold);
  ASSERT_TRUE(ac.check());

  constexpr uint32_t kMetadataStart = 0xdeadbeef;
  constexpr uint32_t kMetadataUpdate = 0xc0ffee;

  // Compress a page full of content and get its ref.
  VmCompressor::CompressedRef compressed_ref{0};
  {
    vm_page_t* page;
    zx_status_t status = pmm_alloc_page(0, &page);
    ASSERT_EQ(ZX_OK, status);
    write_pattern(page, PAGE_SIZE, 0);

    auto compressor_guard = compression->AcquireCompressor();
    VmCompressor& compressor = compressor_guard.get();

    compressor.Arm();
    auto temp_ref =
        compressor.Start(VmCompressor::PageAndMetadata{.page = page, .metadata = kMetadataStart});
    EXPECT_EQ(kMetadataStart, compression->GetMetadata(temp_ref));

    compressor.Compress();

    // Modifying metadata while before acquiring the compression result is legal - the final result
    // should reflect the modifications.
    compression->SetMetadata(temp_ref, kMetadataUpdate);
    EXPECT_EQ(kMetadataUpdate, compression->GetMetadata(temp_ref));

    auto compress_result = compressor.TakeCompressionResult();
    ASSERT_TRUE(ktl::holds_alternative<VmCompressor::CompressedRef>(compress_result));
    compressed_ref = ktl::get<VmCompressor::CompressedRef>(compress_result);
    EXPECT_EQ(kMetadataUpdate, compression->GetMetadata(temp_ref));
    EXPECT_EQ(kMetadataUpdate, compression->GetMetadata(compressed_ref));

    compressor.ReturnTempReference(temp_ref);
    compressor.Finalize();

    pmm_free_page(page);
  }
  ASSERT_NE(0ul, compressed_ref.value());

  // Decompress the previous page and check that the data and metadata were preserved.
  {
    vm_page_t* page;
    zx_status_t status = pmm_alloc_page(0, &page);
    ASSERT_EQ(ZX_OK, status);

    uint32_t page_metadata;
    void* page_data = paddr_to_physmap(page->paddr());
    compression->Decompress(compressed_ref, page_data, &page_metadata);
    EXPECT_TRUE(validate_pattern(page_data, PAGE_SIZE, 0));
    EXPECT_EQ(kMetadataUpdate, page_metadata);

    pmm_free_page(page);
  }

  END_TEST;
}

// Test that compression can detect zero pages as input and flag them appropriately.
bool compression_zero_test() {
  BEGIN_TEST;

  constexpr uint32_t kCompressionThreshhold = static_cast<uint32_t>(PAGE_SIZE) * 70u / 100u;
  fbl::AllocChecker ac;
  fbl::RefPtr<VmLz4Compressor> lz4 = VmLz4Compressor::Create();
  ASSERT_TRUE(lz4);
  fbl::RefPtr<VmTriPageStorage> storage = fbl::MakeRefCountedChecked<VmTriPageStorage>(&ac);
  ASSERT_TRUE(ac.check());
  fbl::RefPtr<VmCompression> compression = fbl::MakeRefCountedChecked<VmCompression>(
      &ac, ktl::move(storage), ktl::move(lz4), kCompressionThreshhold);
  ASSERT_TRUE(ac.check());

  // Compress a page full of zeros and ensure the compression system detects this.
  {
    vm_page_t* page;
    zx_status_t status = pmm_alloc_page(0, &page);
    ASSERT_EQ(ZX_OK, status);
    write_zeros(page, PAGE_SIZE);

    auto compressor_guard = compression->AcquireCompressor();
    VmCompressor& compressor = compressor_guard.get();

    compressor.Arm();
    auto temp_ref = compressor.Start(VmCompressor::PageAndMetadata{.page = page, .metadata = 0});

    compressor.Compress();
    auto compress_result = compressor.TakeCompressionResult();
    EXPECT_TRUE(ktl::holds_alternative<VmCompressor::ZeroTag>(compress_result));

    compressor.ReturnTempReference(temp_ref);
    compressor.Finalize();

    pmm_free_page(page);
  }

  END_TEST;
}

// Test that compression correctly handles failure to compress a page.
bool compression_fail_test() {
  BEGIN_TEST;

  constexpr uint32_t kCompressionThreshhold = 0u;  // Guarantee failure to compress.
  fbl::AllocChecker ac;
  fbl::RefPtr<VmLz4Compressor> lz4 = VmLz4Compressor::Create();
  ASSERT_TRUE(lz4);
  fbl::RefPtr<VmTriPageStorage> storage = fbl::MakeRefCountedChecked<VmTriPageStorage>(&ac);
  ASSERT_TRUE(ac.check());
  fbl::RefPtr<VmCompression> compression = fbl::MakeRefCountedChecked<VmCompression>(
      &ac, ktl::move(storage), ktl::move(lz4), kCompressionThreshhold);
  ASSERT_TRUE(ac.check());

  constexpr uint32_t kMetadataStart = 0xdeadbeef;
  constexpr uint32_t kMetadataUpdate = 0xc0ffee;

  // Compress a page full of content, which fails, and ensure the compression system handles this.
  {
    vm_page_t* page;
    zx_status_t status = pmm_alloc_page(0, &page);
    ASSERT_EQ(ZX_OK, status);
    write_pattern(page, PAGE_SIZE, 0);

    auto compressor_guard = compression->AcquireCompressor();
    VmCompressor& compressor = compressor_guard.get();

    compressor.Arm();
    auto temp_ref =
        compressor.Start(VmCompressor::PageAndMetadata{.page = page, .metadata = kMetadataStart});
    EXPECT_EQ(kMetadataStart, compression->GetMetadata(temp_ref));

    compressor.Compress();

    // Modifying metadata while before acquiring the compression result is legal - the final result
    // should reflect the modifications.
    compression->SetMetadata(temp_ref, kMetadataUpdate);
    EXPECT_EQ(kMetadataUpdate, compression->GetMetadata(temp_ref));

    auto compress_result = compressor.TakeCompressionResult();
    ASSERT_TRUE(ktl::holds_alternative<VmCompressor::FailTag>(compress_result));
    auto fail = ktl::get<VmCompressor::FailTag>(compress_result);
    EXPECT_EQ(page, fail.src_page.page);
    EXPECT_EQ(kMetadataUpdate, fail.src_page.metadata);

    compressor.ReturnTempReference(temp_ref);
    compressor.Finalize();

    pmm_free_page(page);
  }

  END_TEST;
}

// Test that compression correctly handles moving of temp references.
bool compression_move_reference_test() {
  BEGIN_TEST;

  constexpr uint32_t kCompressionThreshhold = static_cast<uint32_t>(PAGE_SIZE) * 70u / 100u;
  fbl::AllocChecker ac;
  fbl::RefPtr<VmLz4Compressor> lz4 = VmLz4Compressor::Create();
  ASSERT_TRUE(lz4);
  fbl::RefPtr<VmTriPageStorage> storage = fbl::MakeRefCountedChecked<VmTriPageStorage>(&ac);
  ASSERT_TRUE(ac.check());
  fbl::RefPtr<VmCompression> compression = fbl::MakeRefCountedChecked<VmCompression>(
      &ac, ktl::move(storage), ktl::move(lz4), kCompressionThreshhold);
  ASSERT_TRUE(ac.check());

  constexpr uint32_t kMetadataStart = 0xdeadbeef;
  constexpr uint32_t kMetadataUpdate = 0xc0ffee;

  // Compress a page full of content but move the ref while compression is in progress.
  {
    vm_page_t* page;
    zx_status_t status = pmm_alloc_page(0, &page);
    ASSERT_EQ(ZX_OK, status);
    write_pattern(page, PAGE_SIZE, 0);

    auto compressor_guard = compression->AcquireCompressor();
    VmCompressor& compressor = compressor_guard.get();

    compressor.Arm();
    auto temp_ref =
        compressor.Start(VmCompressor::PageAndMetadata{.page = page, .metadata = kMetadataStart});
    EXPECT_EQ(kMetadataStart, compression->GetMetadata(temp_ref));

    compressor.Compress();

    // Modifying metadata while before acquiring the compression result is legal - the final result
    // should reflect the modifications.
    compression->SetMetadata(temp_ref, kMetadataUpdate);
    EXPECT_EQ(kMetadataUpdate, compression->GetMetadata(temp_ref));

    // Move the temp reference before acquiring the compression result, which will invalidate it.
    auto maybe_page = compression->MoveReference(temp_ref);
    ASSERT_TRUE(maybe_page.has_value());
    EXPECT_EQ(kMetadataUpdate, maybe_page->metadata);
    EXPECT_TRUE(!compressor.IsTempReferenceInUse());

    // The compression result can still be obtained, but the metadata will be empty.
    auto compress_result = compressor.TakeCompressionResult();
    ASSERT_TRUE(ktl::holds_alternative<VmCompressor::CompressedRef>(compress_result));
    auto compressed_ref = ktl::get<VmCompressor::CompressedRef>(compress_result);
    ASSERT_NE(0ul, compressed_ref.value());
    EXPECT_EQ(0u, compression->GetMetadata(compressed_ref));

    compressor.Free(compressed_ref);
    compressor.Finalize();

    pmm_free_page(maybe_page->page);
    pmm_free_page(page);
  }

  END_TEST;
}

}  // namespace

UNITTEST_START_TESTCASE(compression_tests)
VM_UNITTEST(lz4_compress_smoke_test)
VM_UNITTEST(lz4_zero_dedupe_test)
VM_UNITTEST(tri_page_storage_smoke_test)
VM_UNITTEST(tri_page_storage_packing)
VM_UNITTEST(tri_page_storage_reuse_after_free)
VM_UNITTEST(tri_page_storage_optimal_bucket)
VM_UNITTEST(tri_page_storage_capacity)
VM_UNITTEST(tri_page_storage_maxmize_free_space)
VM_UNITTEST(tri_page_storage_small_storage)
VM_UNITTEST(compression_smoke_test)
VM_UNITTEST(compression_zero_test)
VM_UNITTEST(compression_fail_test)
VM_UNITTEST(compression_move_reference_test)
UNITTEST_END_TESTCASE(compression_tests, "compression", "Compression tests")

}  // namespace vm_unittest
