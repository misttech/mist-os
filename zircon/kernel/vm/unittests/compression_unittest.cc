// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/fit/defer.h>

#include <vm/lz4_compressor.h>

#include "test_helper.h"

#include <ktl/enforce.h>

namespace vm_unittest {

namespace {

#if PAGE_COMPRESSION
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
  EXPECT_TRUE(ktl::holds_alternative<VmCompressor::ZeroTag>(
      lz4->Compress(zero.get(), dst.get(), PAGE_SIZE)));

  // Restricting the output size somewhat significantly should not prevent zero detection.
  EXPECT_TRUE(
      ktl::holds_alternative<VmCompressor::ZeroTag>(lz4->Compress(zero.get(), dst.get(), 64)));

  // Setting a byte should prevent zero detection.
  zero[4] = 1;
  EXPECT_TRUE(ktl::holds_alternative<size_t>(lz4->Compress(zero.get(), dst.get(), PAGE_SIZE)));

  END_TEST;
}
#endif

// Needs to exist so there are non-zero tests if PAGE_COMPRESSION is false.
bool lz4_null_test() {
  BEGIN_TEST;
  END_TEST;
}

}  // namespace

UNITTEST_START_TESTCASE(compression_tests)
#if PAGE_COMPRESSION
VM_UNITTEST(lz4_compress_smoke_test)
VM_UNITTEST(lz4_zero_dedupe_test)
#endif
VM_UNITTEST(lz4_null_test)
UNITTEST_END_TESTCASE(compression_tests, "compression", "Compression tests")

}  // namespace vm_unittest
