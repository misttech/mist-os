// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/storage/f2fs/test/compatibility/v2/compatibility.h"

namespace f2fs {
namespace {

using FileCompatibilityTest = GuestTest<F2fsDebianGuest>;

TEST_F(FileCompatibilityTest, WriteVerifyLinuxToFuchsia) {
  // TODO(fxbug.dev/115142): larger filesize for slow test
  constexpr uint32_t kVerifyPatternSize = 256 * 1024;  // 256 KB
  constexpr uint32_t num_blocks = kVerifyPatternSize / kBlockSize;
  const std::string filename = "alpha";

  // TODO: Support various mkfs options such as
  // "-O extra_attr"
  // "-O extra_attr,project_quota"
  // "-O extra_attr,inode_checksum"
  // "-O extra_attr,inode_crtime"
  // "-O extra_attr,compression"
  std::string mkfs_option_list[] = {""};

  for (std::string_view mkfs_option : mkfs_option_list) {
    // File write on Linux
    {
      GetEnclosedGuest().GetLinuxOperator().Mkfs(mkfs_option);
      GetEnclosedGuest().GetLinuxOperator().Mount();

      auto umount = fit::defer([&] { GetEnclosedGuest().GetLinuxOperator().Umount(); });

      auto test_file = GetEnclosedGuest().GetLinuxOperator().Open(
          std::string(kLinuxPathPrefix) + filename, O_CREAT | O_RDWR, 0644);
      ASSERT_TRUE(test_file->IsValid());
      test_file->WritePattern(num_blocks);
    }

    // Verify on Fuchsia
    {
      GetEnclosedGuest().GetFuchsiaOperator().Fsck();
      GetEnclosedGuest().GetFuchsiaOperator().Mount();

      auto umount = fit::defer([&] { GetEnclosedGuest().GetFuchsiaOperator().Umount(); });

      auto test_file = GetEnclosedGuest().GetFuchsiaOperator().Open(filename, O_RDWR, 0644);
      ASSERT_TRUE(test_file->IsValid());
      test_file->VerifyPattern(num_blocks);
    }
  }
}

TEST_F(FileCompatibilityTest, WriteVerifyFuchsiaToLinux) {
  // TODO(fxbug.dev/115142): larger filesize for slow test
  constexpr uint32_t kVerifyPatternSize = 256 * 1024;  // 256 KB
  constexpr uint32_t num_blocks = kVerifyPatternSize / kBlockSize;
  const std::string filename = "alpha";

  // File write on Fuchsia
  {
    GetEnclosedGuest().GetFuchsiaOperator().Mkfs();
    GetEnclosedGuest().GetFuchsiaOperator().Mount();

    auto umount = fit::defer([&] { GetEnclosedGuest().GetFuchsiaOperator().Umount(); });

    auto test_file = GetEnclosedGuest().GetFuchsiaOperator().Open(filename, O_CREAT | O_RDWR, 0644);
    ASSERT_TRUE(test_file->IsValid());
    test_file->WritePattern(num_blocks);
  }

  // Verify on Linux
  {
    GetEnclosedGuest().GetLinuxOperator().Fsck();
    GetEnclosedGuest().GetLinuxOperator().Mount();

    auto umount = fit::defer([&] { GetEnclosedGuest().GetLinuxOperator().Umount(); });

    auto testfile = GetEnclosedGuest().GetLinuxOperator().Open(
        std::string(kLinuxPathPrefix) + filename, O_RDWR, 0644);
    ASSERT_TRUE(testfile->IsValid());
    testfile->VerifyPattern(num_blocks);
  }
}

void CompareStat(const struct stat &a, const struct stat &b) {
  EXPECT_EQ(a.st_ino, b.st_ino);
  EXPECT_EQ(a.st_mode, b.st_mode);
  EXPECT_EQ(a.st_nlink, b.st_nlink);
  EXPECT_EQ(a.st_size, b.st_size);
  EXPECT_EQ(a.st_ctime, b.st_ctime);
  EXPECT_EQ(a.st_mtime, b.st_mtime);
  ASSERT_EQ(a.st_blocks, b.st_blocks);
}

TEST_F(FileCompatibilityTest, VerifyAttributesLinuxToFuchsia) {
  std::vector<std::pair<std::string, struct stat>> test_set{};

  // Create files with various file modes
  {
    GetEnclosedGuest().GetLinuxOperator().Mkfs();
    GetEnclosedGuest().GetLinuxOperator().Mount();

    auto umount = fit::defer([&] { GetEnclosedGuest().GetLinuxOperator().Umount(); });

    for (mode_t mode = 0; mode <= (S_IRWXU | S_IRWXG | S_IRWXO); ++mode) {
      std::string filename = "child";
      filename.append("_").append(std::to_string(mode));

      auto testfile = GetEnclosedGuest().GetLinuxOperator().Open(
          std::string(kLinuxPathPrefix) + filename, O_CREAT, mode);
      ASSERT_TRUE(testfile->IsValid());

      testfile->Fchmod(mode);

      struct stat file_stat {};
      ASSERT_EQ(testfile->Fstat(file_stat), 0);

      test_set.push_back({filename, file_stat});
    }
  }

  // Verify on Fuchsia
  {
    GetEnclosedGuest().GetFuchsiaOperator().Fsck();
    GetEnclosedGuest().GetFuchsiaOperator().Mount();

    auto umount = fit::defer([&] { GetEnclosedGuest().GetFuchsiaOperator().Umount(); });

    for (auto [name, stat_from_linux] : test_set) {
      auto child_file = GetEnclosedGuest().GetFuchsiaOperator().Open(name, O_RDONLY, 0644);
      ASSERT_TRUE(child_file->IsValid());

      struct stat child_stat {};
      ASSERT_EQ(child_file->Fstat(child_stat), 0);
      CompareStat(child_stat, stat_from_linux);
    }
  }
}

TEST_F(FileCompatibilityTest, TruncateLinuxToFuchsia) {
  constexpr uint32_t kVerifyPatternSize = 256 * 1024;  // 256 KB
  constexpr uint32_t num_blocks = kVerifyPatternSize / kBlockSize;
  constexpr off_t kTruncateSize = 64 * 1024;  // 64 KB
  constexpr uint32_t num_blocks_truncated = kTruncateSize / kBlockSize;

  const std::string extend_filename = "extend";
  const std::string shrink_filename = "shrink";

  {
    GetEnclosedGuest().GetLinuxOperator().Mkfs();
    GetEnclosedGuest().GetLinuxOperator().Mount();

    auto umount = fit::defer([&] { GetEnclosedGuest().GetLinuxOperator().Umount(); });

    auto extend_file = GetEnclosedGuest().GetLinuxOperator().Open(
        std::string(kLinuxPathPrefix) + extend_filename, O_RDWR | O_CREAT, 0755);
    ASSERT_TRUE(extend_file->IsValid());
    ASSERT_EQ(extend_file->Ftruncate(kTruncateSize), 0);

    auto shrink_file = GetEnclosedGuest().GetLinuxOperator().Open(
        std::string(kLinuxPathPrefix) + shrink_filename, O_RDWR | O_CREAT, 0755);
    ASSERT_TRUE(shrink_file->IsValid());
    shrink_file->WritePattern(num_blocks);

    ASSERT_EQ(shrink_file->Ftruncate(kTruncateSize), 0);
  }

  // Verify on Fuchsia
  {
    GetEnclosedGuest().GetFuchsiaOperator().Fsck();
    GetEnclosedGuest().GetFuchsiaOperator().Mount();

    auto umount = fit::defer([&] { GetEnclosedGuest().GetFuchsiaOperator().Umount(); });

    auto extend_file = GetEnclosedGuest().GetFuchsiaOperator().Open(extend_filename, O_RDWR, 0755);
    ASSERT_TRUE(extend_file->IsValid());

    struct stat extend_file_stat {};
    ASSERT_EQ(extend_file->Fstat(extend_file_stat), 0);
    ASSERT_EQ(extend_file_stat.st_size, kTruncateSize);

    uint32_t buffer[kBlockSize / sizeof(uint32_t)];

    for (uint32_t i = 0; i < kTruncateSize / sizeof(buffer); ++i) {
      ASSERT_EQ(extend_file->Read(buffer, sizeof(buffer)), static_cast<ssize_t>(sizeof(buffer)));

      for (uint32_t j = 0; j < sizeof(buffer) / sizeof(uint32_t); ++j) {
        ASSERT_EQ(buffer[j], static_cast<uint32_t>(0));
      }
    }

    auto shrink_file = GetEnclosedGuest().GetFuchsiaOperator().Open(shrink_filename, O_RDWR, 0755);
    ASSERT_TRUE(shrink_file->IsValid());

    struct stat shrink_file_stat {};
    ASSERT_EQ(shrink_file->Fstat(shrink_file_stat), 0);
    ASSERT_EQ(shrink_file_stat.st_size, kTruncateSize);

    shrink_file->VerifyPattern(num_blocks_truncated);
  }
}

TEST_F(FileCompatibilityTest, TruncateFuchsiaToLinux) {
  constexpr uint32_t kVerifyPatternSize = 256 * 1024;  // 256 KB
  constexpr uint32_t num_blocks = kVerifyPatternSize / kBlockSize;
  constexpr off_t kTruncateSize = 64 * 1024;  // 64 KB
  constexpr uint32_t num_blocks_truncated = kTruncateSize / kBlockSize;

  const std::string extend_filename = "extend";
  const std::string shrink_filename = "shrink";

  {
    GetEnclosedGuest().GetFuchsiaOperator().Mkfs();
    GetEnclosedGuest().GetFuchsiaOperator().Mount();

    auto umount = fit::defer([&] { GetEnclosedGuest().GetFuchsiaOperator().Umount(); });

    auto extend_file =
        GetEnclosedGuest().GetFuchsiaOperator().Open(extend_filename, O_RDWR | O_CREAT, 0755);
    ASSERT_TRUE(extend_file->IsValid());
    ASSERT_EQ(extend_file->Ftruncate(kTruncateSize), 0);

    auto shrink_file =
        GetEnclosedGuest().GetFuchsiaOperator().Open(shrink_filename, O_RDWR | O_CREAT, 0755);
    ASSERT_TRUE(shrink_file->IsValid());
    shrink_file->WritePattern(num_blocks);

    ASSERT_EQ(shrink_file->Ftruncate(kTruncateSize), 0);
  }

  // Verify on Linux
  {
    GetEnclosedGuest().GetLinuxOperator().Fsck();
    GetEnclosedGuest().GetLinuxOperator().Mount();

    auto umount = fit::defer([&] { GetEnclosedGuest().GetLinuxOperator().Umount(); });

    auto extend_file = GetEnclosedGuest().GetLinuxOperator().Open(
        std::string(kLinuxPathPrefix) + extend_filename, O_RDWR, 0755);
    ASSERT_TRUE(extend_file->IsValid());

    struct stat extend_file_stat {};
    ASSERT_EQ(extend_file->Fstat(extend_file_stat), 0);
    ASSERT_EQ(extend_file_stat.st_size, kTruncateSize);

    // Check zero-filled
    std::string result;
    GetEnclosedGuest().GetLinuxOperator().ExecuteWithAssert(
        {"<" + GetEnclosedGuest().GetLinuxOperator().ConvertPath(std::string(kLinuxPathPrefix) +
                                                                 extend_filename),
         "tr -d '\\0'", "|", "read -n 1", "||", "echo -n \"0\""},
        &result);

    ASSERT_EQ(result, "0");

    auto shrink_file = GetEnclosedGuest().GetLinuxOperator().Open(
        std::string(kLinuxPathPrefix) + shrink_filename, O_RDWR, 0755);
    ASSERT_TRUE(shrink_file->IsValid());
    shrink_file->VerifyPattern(num_blocks_truncated);
  }
}

char GetRandomFileNameChar() {
  // Ascii number [0x21 ~ 0x7E except 0x2E('.') and 0x2F('/')] is availble for file name character.
  constexpr char kLowerBound = 0x21;
  constexpr char kUpperBound = 0x7E;

  std::set<char> exclude = {'.', '/'};

  char random_char = 0;
  do {
    random_char = rand() % (kUpperBound - kLowerBound + 1) + kLowerBound;
  } while (exclude.find(random_char) != exclude.end());

  return random_char;
}

std::vector<std::string> GetRandomFileNameSet() {
  constexpr int kMaxFilenameLength = 255;
  std::vector<std::string> file_name_set;

  for (int len = 1; len <= kMaxFilenameLength; ++len) {
    std::string file_name = "/";
    for (int i = 0; i < len; ++i) {
      file_name.push_back(GetRandomFileNameChar());
    }
    file_name_set.push_back(file_name);
  }
  return file_name_set;
}

TEST_F(FileCompatibilityTest, FileNameTestLinuxToFuchsia) {
  srand(testing::UnitTest::GetInstance()->random_seed());

  auto file_name_set = GetRandomFileNameSet();
  {
    GetEnclosedGuest().GetLinuxOperator().Mkfs();
    GetEnclosedGuest().GetLinuxOperator().Mount();

    auto umount = fit::defer([&] { GetEnclosedGuest().GetLinuxOperator().Umount(); });

    for (auto file_name : file_name_set) {
      auto file = GetEnclosedGuest().GetLinuxOperator().Open(
          std::string(kLinuxPathPrefix) + file_name, O_RDWR | O_CREAT, 0644);
      ASSERT_TRUE(file->IsValid());
    }
  }

  {
    GetEnclosedGuest().GetFuchsiaOperator().Fsck();
    GetEnclosedGuest().GetFuchsiaOperator().Mount();

    auto umount = fit::defer([&] { GetEnclosedGuest().GetFuchsiaOperator().Umount(); });

    for (auto file_name : file_name_set) {
      auto file = GetEnclosedGuest().GetFuchsiaOperator().Open(file_name, O_RDONLY, 0644);
      ASSERT_TRUE(file->IsValid());
    }
  }
}

TEST_F(FileCompatibilityTest, FileNameTestFuchsiaToLinux) {
  srand(testing::UnitTest::GetInstance()->random_seed());

  auto file_name_set = GetRandomFileNameSet();
  {
    GetEnclosedGuest().GetFuchsiaOperator().Mkfs();
    GetEnclosedGuest().GetFuchsiaOperator().Mount();

    auto umount = fit::defer([&] { GetEnclosedGuest().GetFuchsiaOperator().Umount(); });

    for (auto file_name : file_name_set) {
      auto file = GetEnclosedGuest().GetFuchsiaOperator().Open(file_name, O_RDWR | O_CREAT, 0644);
      ASSERT_TRUE(file->IsValid());
    }
  }

  {
    GetEnclosedGuest().GetLinuxOperator().Fsck();
    GetEnclosedGuest().GetLinuxOperator().Mount();

    auto umount = fit::defer([&] { GetEnclosedGuest().GetLinuxOperator().Umount(); });

    for (auto file_name : file_name_set) {
      auto file = GetEnclosedGuest().GetLinuxOperator().Open(
          std::string(kLinuxPathPrefix) + file_name, O_RDONLY, 0644);
      ASSERT_TRUE(file->IsValid());
    }
  }
}

TEST_F(FileCompatibilityTest, FileRenameTestLinuxToFuchsia) {
  std::vector<std::string> dir_paths = {"/d_a", "/d_a/d_b", "/d_c"};
  std::vector<std::pair<std::string, std::string>> rename_from_to = {
      {"/f_0", "/f_0_"},
      {"/f_1", "/d_c/f_1_"},
      {"/d_a/f_a0", "/d_c/f_a0_"},
      {"/d_a/d_b/f_ab0", "/d_c/f_ab0"}};
  {
    GetEnclosedGuest().GetLinuxOperator().Mkfs();
    GetEnclosedGuest().GetLinuxOperator().Mount();

    auto umount = fit::defer([&] { GetEnclosedGuest().GetLinuxOperator().Umount(); });

    for (auto dir_name : dir_paths) {
      GetEnclosedGuest().GetLinuxOperator().Mkdir(std::string(kLinuxPathPrefix) + dir_name, 0644);
    }

    // Create
    for (auto [file_name_from, file_name_to] : rename_from_to) {
      auto file = GetEnclosedGuest().GetLinuxOperator().Open(
          std::string(kLinuxPathPrefix) + file_name_from, O_RDWR | O_CREAT, 0644);
      ASSERT_TRUE(file->IsValid());
    }

    // Rename
    for (auto [file_name_from, file_name_to] : rename_from_to) {
      GetEnclosedGuest().GetLinuxOperator().Rename(std::string(kLinuxPathPrefix) + file_name_from,
                                                   std::string(kLinuxPathPrefix) + file_name_to);
    }
  }

  {
    GetEnclosedGuest().GetFuchsiaOperator().Fsck();
    GetEnclosedGuest().GetFuchsiaOperator().Mount();

    auto umount = fit::defer([&] { GetEnclosedGuest().GetFuchsiaOperator().Umount(); });

    for (auto [file_name_from, file_name_to] : rename_from_to) {
      auto file = GetEnclosedGuest().GetFuchsiaOperator().Open(file_name_from, O_RDONLY, 0644);
      ASSERT_FALSE(file->IsValid());

      file = GetEnclosedGuest().GetFuchsiaOperator().Open(file_name_to, O_RDONLY, 0644);
      ASSERT_TRUE(file->IsValid());
    }
  }
}

TEST_F(FileCompatibilityTest, FileRenameTestFuchsiaToLinux) {
  std::vector<std::string> dir_paths = {"/d_a", "/d_a/d_b", "/d_c"};
  std::vector<std::pair<std::string, std::string>> rename_from_to = {
      {"/f_0", "/f_0_"},
      {"/f_1", "/d_c/f_1_"},
      {"/d_a/f_a0", "/d_c/f_a0_"},
      {"/d_a/d_b/f_ab0", "/d_c/f_ab0"}};
  {
    GetEnclosedGuest().GetFuchsiaOperator().Mkfs();
    GetEnclosedGuest().GetFuchsiaOperator().Mount();

    auto umount = fit::defer([&] { GetEnclosedGuest().GetFuchsiaOperator().Umount(); });

    for (auto dir_name : dir_paths) {
      GetEnclosedGuest().GetFuchsiaOperator().Mkdir(dir_name, 0644);
    }

    // Create
    for (auto [file_name_from, file_name_to] : rename_from_to) {
      auto file =
          GetEnclosedGuest().GetFuchsiaOperator().Open(file_name_from, O_RDWR | O_CREAT, 0644);
      ASSERT_TRUE(file->IsValid());
    }

    // Rename
    for (auto [file_name_from, file_name_to] : rename_from_to) {
      GetEnclosedGuest().GetFuchsiaOperator().Rename(file_name_from, file_name_to);
    }
  }

  {
    GetEnclosedGuest().GetLinuxOperator().Fsck();
    GetEnclosedGuest().GetLinuxOperator().Mount();

    auto umount = fit::defer([&] { GetEnclosedGuest().GetLinuxOperator().Umount(); });

    for (auto [file_name_from, file_name_to] : rename_from_to) {
      auto file = GetEnclosedGuest().GetLinuxOperator().Open(
          std::string(kLinuxPathPrefix) + file_name_from, O_RDONLY, 0644);
      ASSERT_FALSE(file->IsValid());

      file = GetEnclosedGuest().GetLinuxOperator().Open(
          std::string(kLinuxPathPrefix) + file_name_to, O_RDONLY, 0644);
      ASSERT_TRUE(file->IsValid());
    }
  }
}

TEST_F(FileCompatibilityTest, FileReadExceedFileSizeOnFuchsia) {
  srand(testing::UnitTest::GetInstance()->random_seed());

  constexpr uint32_t kDataSize = 7 * 1024;      // 7kb
  constexpr uint32_t kReadLocation = 5 * 1024;  // 5kb

  const std::string filename = "alpha";

  char w_buf[kDataSize];
  for (size_t i = 0; i < kDataSize; ++i) {
    w_buf[i] = static_cast<char>(rand() % 128);
  }

  // Write on Host
  {
    GetEnclosedGuest().GetLinuxOperator().Mkfs();
    GetEnclosedGuest().GetLinuxOperator().Mount();

    auto umount = fit::defer([&] { GetEnclosedGuest().GetLinuxOperator().Umount(); });

    auto test_file = GetEnclosedGuest().GetLinuxOperator().Open(
        std::string(kLinuxPathPrefix) + filename, O_RDWR | O_CREAT, 0644);
    ASSERT_TRUE(test_file->IsValid());

    ASSERT_EQ(test_file->Write(w_buf, sizeof(w_buf)), static_cast<ssize_t>(sizeof(w_buf)));
  }

  // Verify on Fuchsia. Try reading excess file size.
  {
    GetEnclosedGuest().GetFuchsiaOperator().Fsck();
    GetEnclosedGuest().GetFuchsiaOperator().Mount();

    auto umount = fit::defer([&] { GetEnclosedGuest().GetFuchsiaOperator().Umount(); });

    auto test_file = GetEnclosedGuest().GetFuchsiaOperator().Open(filename, O_RDWR, 0644);
    ASSERT_TRUE(test_file->IsValid());

    char r_buf[kReadLocation + kPageSize];
    ASSERT_EQ(test_file->Read(r_buf, kReadLocation), static_cast<ssize_t>(kReadLocation));
    ASSERT_EQ(test_file->Read(&(r_buf[kReadLocation]), kPageSize),
              static_cast<ssize_t>(kDataSize - kReadLocation));

    ASSERT_EQ(memcmp(r_buf, w_buf, kDataSize), 0);
  }
}

}  // namespace
}  // namespace f2fs
