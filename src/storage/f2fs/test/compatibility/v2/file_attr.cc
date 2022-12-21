// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/storage/f2fs/test/compatibility/v2/compatibility.h"

namespace f2fs {
namespace {

using FileAttrCompatibilityTest = F2fsGuestTest;

void CompareStat(const struct stat &a, const struct stat &b) {
  EXPECT_EQ(a.st_ino, b.st_ino);
  EXPECT_EQ(a.st_mode, b.st_mode);
  EXPECT_EQ(a.st_nlink, b.st_nlink);
  EXPECT_EQ(a.st_size, b.st_size);
  EXPECT_EQ(a.st_ctime, b.st_ctime);
  EXPECT_EQ(a.st_mtime, b.st_mtime);
  ASSERT_EQ(a.st_blocks, b.st_blocks);
}

TEST_F(FileAttrCompatibilityTest, VerifyAttributesLinuxToFuchsia) {
  srand(testing::UnitTest::GetInstance()->random_seed());

  std::vector<std::pair<std::string, struct stat>> test_set{};

  // Create files with various file modes
  {
    GetEnclosedGuest().GetLinuxOperator().Mkfs();
    GetEnclosedGuest().GetLinuxOperator().Mount();

    auto umount = fit::defer([&] { GetEnclosedGuest().GetLinuxOperator().Umount(); });

    for (mode_t mode = 0; mode <= (S_IRWXU | S_IRWXG | S_IRWXO); mode += rand() % 32 + 1) {
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

char GetRandomFileNameChar() {
  const char kValidChars[] =
      "0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz()<>+-=_,:;";
  return kValidChars[rand() % (sizeof(kValidChars) - 1)];
}

std::vector<std::string> GetRandomFileNameSet() {
  constexpr int kMaxFilenameLength = 255;
  std::vector<std::string> file_name_set;

  for (int len = 1; len <= kMaxFilenameLength; len += rand() % 20 + 1) {
    std::string file_name = "/";
    for (int i = 0; i < len; ++i) {
      file_name.push_back(GetRandomFileNameChar());
    }
    file_name_set.push_back(file_name);
  }
  return file_name_set;
}

TEST_F(FileAttrCompatibilityTest, FileNameTestLinuxToFuchsia) {
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

TEST_F(FileAttrCompatibilityTest, FileNameTestFuchsiaToLinux) {
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

TEST_F(FileAttrCompatibilityTest, FileRenameTestLinuxToFuchsia) {
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

TEST_F(FileAttrCompatibilityTest, FileRenameTestFuchsiaToLinux) {
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

}  // namespace
}  // namespace f2fs
