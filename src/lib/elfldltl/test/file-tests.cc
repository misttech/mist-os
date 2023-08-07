// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/elfldltl/fd.h>
#include <lib/elfldltl/file.h>
#include <lib/elfldltl/memory.h>
#include <lib/elfldltl/testing/diagnostics.h>
#include <stdio.h>

#include <gmock/gmock.h>
#include <gtest/gtest.h>

#ifdef __Fuchsia__
#include <lib/elfldltl/vmo.h>
#include <lib/zx/vmo.h>
#endif

namespace {

using elfldltl::testing::ExpectedSingleError;
using elfldltl::testing::ExpectOkDiagnostics;

using ::testing::Optional, ::testing::Eq;

class TestFdFile : public ::testing::Test {
 protected:
  static constexpr bool kDestroysHandle = false;

  template <typename Diagnostics>
  using FileT = elfldltl::FdFile<Diagnostics>;

  TestFdFile() {
    f_ = tmpfile();
    EXPECT_NE(f_, nullptr);
  }

  ~TestFdFile() {
    if (f_) {
      fclose(f_);
    }
  }

  void Write(const void* p, size_t size) {
    EXPECT_EQ(fwrite(p, 1, size, f_), size);
    EXPECT_EQ(fflush(f_), 0);
  }

  int GetHandle() const { return fileno(f_); }

 private:
  FILE* f_;
};

class TestUniqueFdFile : public TestFdFile {
 protected:
  static constexpr bool kDestroysHandle = true;

  template <typename Diagnostics>
  using FileT = elfldltl::UniqueFdFile<Diagnostics>;

  fbl::unique_fd GetHandle() { return fbl::unique_fd{TestFdFile::GetHandle()}; }
};

#ifdef __Fuchsia__

class TestVmoFile : public ::testing::Test {
 protected:
  static constexpr bool kDestroysHandle = true;

  template <typename Diagnostics>
  using FileT = elfldltl::VmoFile<Diagnostics>;

  void Write(const void* p, size_t size) {
    ASSERT_EQ(zx::vmo::create(size, 0, &vmo_), ZX_OK);
    ASSERT_EQ(vmo_.write(p, 0, size), ZX_OK);
  }

  zx::vmo GetHandle() { return std::move(vmo_); }

  zx::unowned_vmo Borrow() { return vmo_.borrow(); }

 private:
  zx::vmo vmo_;
};

class TestUnownedVmoFile : public TestVmoFile {
 protected:
  static constexpr bool kDestroysHandle = false;

  template <typename Diagnostics>
  using FileT = elfldltl::UnownedVmoFile<Diagnostics>;

  zx::unowned_vmo GetHandle() { return Borrow(); }
};

#endif  // __Fuchsia__

using FileTypes = ::testing::Types<
#ifdef __Fuchsia__
    TestVmoFile, TestUnownedVmoFile,
#endif
    TestFdFile, TestUniqueFdFile>;

template <class TestFile>
using ElfldltlFileTests = TestFile;

TYPED_TEST_SUITE(ElfldltlFileTests, FileTypes);

TYPED_TEST(ElfldltlFileTests, InvalidFd) {
  ExpectedSingleError expected{"couldn't read file at offset: ", 0};
  using FileT = typename TestFixture::template FileT<decltype(expected.diag())>;
  {
    FileT file{expected.diag()};

    std::optional<int> got = file.template ReadFromFile<int>(0);
    EXPECT_FALSE(got);
  }
  {
    FileT file(this->GetHandle(), expected.diag());

    std::optional<int> got = file.template ReadFromFile<int>(0);
    EXPECT_FALSE(got);
  }
}

TYPED_TEST(ElfldltlFileTests, ReadFromFile) {
  auto diag = ExpectOkDiagnostics();
  using FileT = typename TestFixture::template FileT<decltype(diag)>;

  int i = 123;

  this->Write(&i, sizeof(i));

  FileT file{this->GetHandle(), diag};
  std::optional<int> got = file.template ReadFromFile<int>(0);
  EXPECT_EQ(*got, i);
}

TYPED_TEST(ElfldltlFileTests, ReadArrayFromFile) {
  auto diag = ExpectOkDiagnostics();
  using FileT = typename TestFixture::template FileT<decltype(diag)>;

  const std::array<int, 3> data{123, 456, 789};

  this->Write(data.data(), sizeof(data));

  elfldltl::FixedArrayFromFile<int, 3> allocator;
  FileT file{this->GetHandle(), diag};
  auto got = file.template ReadArrayFromFile<int>(0, allocator, 3);

  ASSERT_TRUE(got);
  cpp20::span<int> span = *got;
  ASSERT_EQ(span.size(), data.size());
  for (size_t i = 0; i < data.size(); i++) {
    EXPECT_EQ(span[i], data[i]);
  }
}

TYPED_TEST(ElfldltlFileTests, Assignment) {
  ExpectedSingleError expected("couldn't read file at offset: ", 0);
  auto diag = [&] {
    if constexpr (TestFixture::kDestroysHandle) {
      return expected.diag();
    } else {
      return ExpectOkDiagnostics();
    }
  }();
  using FileT = typename TestFixture::template FileT<decltype(diag)>;
  int i = 123;

  this->Write(&i, sizeof(i));

  FileT file{this->GetHandle(), diag};
  EXPECT_THAT(file.template ReadFromFile<int>(0), Optional(Eq(i)));
  if constexpr (std::is_copy_assignable_v<FileT>) {
    FileT other = file;
    EXPECT_THAT(other.template ReadFromFile<int>(0), Optional(Eq(i)));
  }
  EXPECT_THAT(file.template ReadFromFile<int>(0), Optional(Eq(i)));
  {
    FileT other = std::move(file);
    EXPECT_THAT(other.template ReadFromFile<int>(0), Optional(Eq(i)));
  }
  if constexpr (TestFixture::kDestroysHandle) {
    EXPECT_FALSE(file.template ReadFromFile<int>(0).has_value());
  }
}

}  // anonymous namespace
