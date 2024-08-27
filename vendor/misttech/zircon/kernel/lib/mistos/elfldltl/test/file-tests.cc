// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/elfldltl/file.h>
#include <lib/elfldltl/memory.h>
#include <lib/elfldltl/testing/diagnostics.h>
#include <lib/mistos/elfldltl/vmo.h>
#include <lib/mistos/zx/vmo.h>
#include <stdio.h>

#include <zxtest/zxtest.h>

namespace {

class TestVmoFile : public zxtest::Test {
 protected:
  static constexpr bool kDestroysHandle = true;

  static constexpr elfldltl::ZirconError kInvalidFdError{ZX_ERR_BAD_HANDLE};

  template <typename Diagnostics>
  using FileT = elfldltl::VmoFile<Diagnostics>;

  // The fixture starts with an empty VMO so that reads on GetHandle() will
  // fail with ZX_ERR_OUT_OF_RANGE rather than ZX_ERR_BAD_HANDLE.
  TestVmoFile() { EXPECT_EQ(zx::vmo::create(0, 0, &vmo_), ZX_OK); }

  void Write(const void* p, size_t size) {
    // This replaces the empty VMO.
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

using ExpectOkDiagnosticsType = decltype(elfldltl::testing::ExpectOkDiagnostics());

template <class TestFile>
class ElfldltlFileTests : public TestFile {
 public:
  using OkFileT = typename TestFile::template FileT<ExpectOkDiagnosticsType>;
  using offset_type = typename OkFileT::offset_type;
  using unsigned_offset_type = std::make_unsigned_t<offset_type>;

  static constexpr unsigned_offset_type kZeroOffset = 0;
  static constexpr elfldltl::FileOffset kZeroFileOffset{kZeroOffset};

  static constexpr auto MakeExpectedInvalidFd() {
    return elfldltl::testing::ExpectedSingleError{
        "cannot read ", sizeof(int), " bytes", kZeroFileOffset, ": ", TestFile::kInvalidFdError,
    };
  }

  static constexpr auto MakeExpectedEof() {
    return elfldltl::testing::ExpectedSingleError{
        "cannot read ", sizeof(int), " bytes", kZeroFileOffset, ": ", "reached end of file",
    };
  }

  using InvalidFdDiagnostics = decltype(MakeExpectedInvalidFd());
  using InvalidFdFileT = typename TestFile::template FileT<InvalidFdDiagnostics>;

  using EofDiagnostics = decltype(MakeExpectedEof());
  using EofFileT = typename TestFile::template FileT<EofDiagnostics>;
};

template <typename TestFixture>
void InvalidFd() {
  auto expected = TestFixture::MakeExpectedInvalidFd();
  typename TestFixture::InvalidFdFileT file{expected};

  std::optional<int> got = file.template ReadFromFile<int>(0);
  EXPECT_FALSE(got);
}

TEST(TestVmoFile, InvalidFd) {
  ASSERT_NO_FATAL_FAILURE(InvalidFd<ElfldltlFileTests<TestVmoFile>>());
}

TEST(TestUnownedVmoFile, InvalidFd) {
  ASSERT_NO_FATAL_FAILURE(InvalidFd<ElfldltlFileTests<TestUnownedVmoFile>>());
}

#define Eof(TestFixture)                                            \
  auto expected = TestFixture::MakeExpectedEof();                   \
  typename TestFixture::EofFileT file{this->GetHandle(), expected}; \
                                                                    \
  std::optional<int> got = file.template ReadFromFile<int>(0);      \
  EXPECT_EQ(got, std::nullopt);

#define ReadFromFile(TestFixture)                              \
  int i = 123;                                                 \
                                                               \
  this->Write(&i, sizeof(i));                                  \
                                                               \
  auto diag = elfldltl::testing::ExpectOkDiagnostics();        \
  typename TestFixture::OkFileT file{this->GetHandle(), diag}; \
                                                               \
  std::optional<int> got = file.template ReadFromFile<int>(0); \
  EXPECT_EQ(got, std::optional<int>(i));

#define ReadArrayFromFile(TestFixture)                              \
  constexpr int kData[] = {123, 456, 789};                          \
                                                                    \
  this->Write(kData, sizeof(kData));                                \
                                                                    \
  auto diag = elfldltl::testing::ExpectOkDiagnostics();             \
  typename TestFixture::OkFileT file{this->GetHandle(), diag};      \
                                                                    \
  elfldltl::FixedArrayFromFile<int, 3> allocator;                   \
  auto got = file.template ReadArrayFromFile<int>(0, allocator, 3); \
                                                                    \
  ASSERT_TRUE(got);                                                 \
  cpp20::span<const int> data = *got;                               \
  EXPECT_BYTES_EQ(data.data(), kData, data.size_bytes());

#define Assignment(TestFixture)                                              \
  auto test_assignment = [this](auto&& diag) {                               \
    using Diagnostics = std::decay_t<decltype(diag)>;                        \
    using FileT = typename TestFixture::template FileT<Diagnostics>;         \
                                                                             \
    int i = 123;                                                             \
                                                                             \
    this->Write(&i, sizeof(i));                                              \
                                                                             \
    FileT file{this->GetHandle(), diag};                                     \
    EXPECT_EQ(file.template ReadFromFile<int>(0), std::optional<int>(i));    \
    if constexpr (std::is_copy_assignable_v<decltype(file)>) {               \
      auto other = file;                                                     \
      EXPECT_EQ(other.template ReadFromFile<int>(0), std::optional<int>(i)); \
    }                                                                        \
    EXPECT_EQ(file.template ReadFromFile<int>(0), std::optional<int>(i));    \
    {                                                                        \
      auto other = std::move(file);                                          \
      EXPECT_EQ(other.template ReadFromFile<int>(0), std::optional<int>(i)); \
    }                                                                        \
    if constexpr (TestFixture::kDestroysHandle) {                            \
      EXPECT_FALSE(file.template ReadFromFile<int>(0).has_value());          \
    }                                                                        \
  };                                                                         \
                                                                             \
  if constexpr (TestFixture::kDestroysHandle) {                              \
    test_assignment(TestFixture::MakeExpectedInvalidFd());                   \
  } else {                                                                   \
    test_assignment(elfldltl::testing::ExpectOkDiagnostics());               \
  }

using TestVmoFileTestFixture = ElfldltlFileTests<TestVmoFile>;
TEST_F(TestVmoFileTestFixture, Eof){Eof(TestVmoFileTestFixture)}

TEST_F(TestVmoFileTestFixture, ReadFromFile){ReadFromFile(TestVmoFileTestFixture)}

TEST_F(TestVmoFileTestFixture, ReadArrayFromFile){ReadArrayFromFile(TestVmoFileTestFixture)}

TEST_F(TestVmoFileTestFixture, Assignment) {
  Assignment(TestVmoFileTestFixture)
}

using TestUnownedVmoFileTestFixture = ElfldltlFileTests<TestUnownedVmoFile>;
TEST_F(TestUnownedVmoFileTestFixture, Eof){Eof(TestUnownedVmoFileTestFixture)}

TEST_F(TestUnownedVmoFileTestFixture, ReadFromFile){ReadFromFile(TestUnownedVmoFileTestFixture)}

TEST_F(TestUnownedVmoFileTestFixture,
       ReadArrayFromFile){ReadArrayFromFile(TestUnownedVmoFileTestFixture)}

TEST_F(TestUnownedVmoFileTestFixture, Assignment) {
  Assignment(TestUnownedVmoFileTestFixture)
}

}  // anonymous namespace
