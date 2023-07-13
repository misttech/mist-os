// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/elfldltl/fd.h>
#include <lib/elfldltl/file.h>
#include <lib/elfldltl/memory.h>
#include <lib/elfldltl/testing/diagnostics.h>
#include <stdio.h>

#include <gtest/gtest.h>

#ifdef __Fuchsia__
#include <lib/elfldltl/vmo.h>
#include <lib/zx/vmo.h>
#endif

namespace {

using elfldltl::testing::ExpectedSingleError;
using elfldltl::testing::ExpectOkDiagnostics;

struct TestFdFile {
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

struct TestUniqueFdFile : public TestFdFile {
  template <typename Diagnostics>
  using FileT = elfldltl::UniqueFdFile<Diagnostics>;

  fbl::unique_fd GetHandle() { return fbl::unique_fd{TestFdFile::GetHandle()}; }
};

#ifdef __Fuchsia__

struct TestVmoFile {
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

struct TestUnownedVmoFile : public TestVmoFile {
  template <typename Diagnostics>
  using FileT = elfldltl::UnownedVmoFile<Diagnostics>;

  zx::unowned_vmo GetHandle() { return Borrow(); }
};

#else  // __Fuchsia__

using TestUnownedVmoFile = struct {};

#endif  // __Fuchsia__

template <typename Test>
void TestPlatforms(Test&& test) {
  test(TestFdFile{});
  test(TestUniqueFdFile{});
#ifdef __Fuchsia__
  test(TestVmoFile{});
  test(TestUnownedVmoFile{});
#endif
}

auto InvalidFd = [](auto&& test_file) {
  ExpectedSingleError expected("couldn't read file at offset: ", 0);
  using FileT =
      typename std::decay_t<decltype(test_file)>::template FileT<decltype(expected.diag())>;

  {
    FileT file{expected.diag()};

    std::optional<int> got = file.template ReadFromFile<int>(0);
    EXPECT_FALSE(got);
  }
  {
    FileT file(test_file.GetHandle(), expected.diag());

    std::optional<int> got = file.template ReadFromFile<int>(0);
    EXPECT_FALSE(got);
  }
};

TEST(ElfldltlFileTests, InvalidFd) { TestPlatforms(InvalidFd); }

auto ReadFromFile = [](auto&& test_file) {
  auto diag = ExpectOkDiagnostics();
  using FileT = typename std::decay_t<decltype(test_file)>::template FileT<decltype(diag)>;
  int i = 123;

  test_file.Write(&i, sizeof(i));

  FileT file{test_file.GetHandle(), diag};
  std::optional<int> got = file.template ReadFromFile<int>(0);
  EXPECT_EQ(*got, i);
};

TEST(ElfldltlFileTests, ReadFromFile) { TestPlatforms(ReadFromFile); }

auto ReadArrayFromFile = [](auto&& test_file) {
  auto diag = ExpectOkDiagnostics();
  using FileT = typename std::decay_t<decltype(test_file)>::template FileT<decltype(diag)>;
  const std::array<int, 3> data{123, 456, 789};

  test_file.Write(data.data(), sizeof(data));

  elfldltl::FixedArrayFromFile<int, 3> allocator;
  FileT file{test_file.GetHandle(), diag};
  auto got = file.template ReadArrayFromFile<int>(0, allocator, 3);

  ASSERT_TRUE(got);
  cpp20::span<int> span = *got;
  ASSERT_EQ(span.size(), data.size());
  for (size_t i = 0; i < data.size(); i++) {
    EXPECT_EQ(span[i], data[i]);
  }
};

TEST(ElfldltlFileTests, ReadArrayFromFile) { TestPlatforms(ReadArrayFromFile); }

auto Assignment = [](auto&& test_file) {
  using TestFileT = std::decay_t<std::decay_t<decltype(test_file)>>;
  constexpr bool DestroysHandle =
      std::is_same_v<TestFileT, TestUniqueFdFile> || std::is_same_v<TestFileT, TestUnownedVmoFile>;

  ExpectedSingleError expected("couldn't read file at offset: ", 0);
  auto diag = [&] {
    if constexpr (DestroysHandle) {
      return expected.diag();
    } else {
      return ExpectOkDiagnostics();
    }
  }();
  using FileT = typename TestFileT::template FileT<decltype(diag)>;
  int i = 123;

  test_file.Write(&i, sizeof(i));

  // If we ever switch to gtest, use EXPECT_THAT(..., Optional(...));
  auto expect_optional = [](const auto& optional, const auto& expected) {
    ASSERT_TRUE(optional);
    EXPECT_EQ(*optional, expected);
  };

  FileT file{test_file.GetHandle(), diag};
  expect_optional(file.template ReadFromFile<int>(0), i);
  if constexpr (std::is_copy_assignable_v<FileT>) {
    FileT other = file;
    expect_optional(other.template ReadFromFile<int>(0), i);
  }
  expect_optional(file.template ReadFromFile<int>(0), i);
  {
    FileT other = std::move(file);
    expect_optional(other.template ReadFromFile<int>(0), i);
  }
  if (DestroysHandle) {
    EXPECT_FALSE(file.template ReadFromFile<int>(0).has_value());
  }
};

TEST(ElfldltlFileTests, Assignment) { TestPlatforms(Assignment); }

}  // anonymous namespace
