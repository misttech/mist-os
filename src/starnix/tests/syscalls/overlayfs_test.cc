// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <dirent.h>
#include <fcntl.h>
#include <sys/mount.h>

#include <gtest/gtest.h>

#include "src/lib/files/directory.h"
#include "src/lib/files/file.h"
#include "src/lib/fxl/strings/string_printf.h"
#include "src/starnix/tests/syscalls/test_helper.h"

namespace {

class OverlayFsTest : public ::testing::Test {
 protected:
  void SetUp() override {
    if (!test_helper::HasSysAdmin()) {
      GTEST_SKIP() << "Not running with sysadmin capabilities, skipping suite.";
    }

    ASSERT_FALSE(temp_dir_.path().empty());

    overlay_ = temp_dir_.path() + "/overlay";
    ASSERT_THAT(mkdir(overlay_.c_str(), 0700), SyscallSucceeds());

    lower_ = temp_dir_.path() + "/lower";
    ASSERT_THAT(mkdir(lower_.c_str(), 0700), SyscallSucceeds());

    upper_base_ = temp_dir_.path() + "/upper_base";
    ASSERT_THAT(mkdir(upper_base_.c_str(), 0700), SyscallSucceeds());

    ASSERT_NO_FATAL_FAILURE(InitUpperDirs());
  }

  void InitUpperDirs() {
    upper_ = upper_base_ + "/upper";
    ASSERT_THAT(mkdir(upper_.c_str(), 0700), SyscallSucceeds());

    work_ = upper_base_ + "/work";
    ASSERT_THAT(mkdir(work_.c_str(), 0700), SyscallSucceeds());
  }

  void Mount() {
    std::string options = fxl::StringPrintf("lowerdir=%s,upperdir=%s,workdir=%s", lower_.c_str(),
                                            upper_.c_str(), work_.c_str());
    ASSERT_THAT(mount(nullptr, overlay_.c_str(), "overlay", 0, options.c_str()), SyscallSucceeds());
  }

  void CheckFileContent(const std::string& file, const std::string& expected) {
    std::string content;
    ASSERT_TRUE(files::ReadFileToString((overlay_ + file).c_str(), &content));
    ASSERT_EQ(content, expected);
  }

  test_helper::ScopedTempDir temp_dir_;
  std::string lower_;
  std::string upper_;
  std::string upper_base_;
  std::string work_;
  std::string overlay_;
};

struct DirEntry {
  std::string name;
  uint8_t type;
  ino_t inode_num;
  off_t offset;

  bool operator<(const DirEntry& rhs) const { return name < rhs.name; }
};

void ReadDir(const std::string& path, std::vector<DirEntry>* out) {
  out->clear();
  DIR* dir = opendir(path.c_str());
  if (!dir) {
    FAIL() << "opendir() failed for " << path << " errno=" << errno;
  }
  struct dirent* de;
  errno = 0;
  while ((de = readdir(dir)) != nullptr) {
    out->push_back(DirEntry{
        .name = de->d_name,
        .type = de->d_type,
        .inode_num = de->d_ino,
        .offset = de->d_off,
    });
  }
  closedir(dir);

  if (errno != 0) {
    FAIL() << "readdir() failed with errno=" << errno;
  }

  std::sort(out->begin(), out->end());
}

TEST_F(OverlayFsTest, ListRoot) {
  ASSERT_TRUE(files::WriteFile(lower_ + "/a", "lower/a"));
  ASSERT_TRUE(files::WriteFile(lower_ + "/c", "lower/c"));
  ASSERT_TRUE(files::WriteFile(upper_ + "/b", "upper/b"));
  ASSERT_TRUE(files::WriteFile(upper_ + "/c", "upper/c"));

  ASSERT_NO_FATAL_FAILURE(Mount());

  std::vector<DirEntry> list;
  ASSERT_NO_FATAL_FAILURE(ReadDir(overlay_, &list));

  EXPECT_EQ(list.size(), 5u);

  EXPECT_EQ(list[0].name, ".");
  EXPECT_EQ(list[0].type, DT_DIR);
  EXPECT_EQ(list[1].name, "..");
  EXPECT_EQ(list[1].type, DT_DIR);

  EXPECT_EQ(list[2].name, "a");
  EXPECT_EQ(list[2].type, DT_REG);
  EXPECT_EQ(list[3].name, "b");
  EXPECT_EQ(list[3].type, DT_REG);
  EXPECT_EQ(list[4].name, "c");
  EXPECT_EQ(list[4].type, DT_REG);

  std::vector<DirEntry> lower_list;
  ASSERT_NO_FATAL_FAILURE(ReadDir(lower_, &lower_list));
  std::vector<DirEntry> upper_list;
  ASSERT_NO_FATAL_FAILURE(ReadDir(upper_, &upper_list));

  // Inode number copied from the source file systems.
  EXPECT_EQ(list[2].inode_num, lower_list[2].inode_num);
  EXPECT_EQ(list[3].inode_num, upper_list[2].inode_num);
  EXPECT_EQ(list[4].inode_num, upper_list[3].inode_num);
}

TEST_F(OverlayFsTest, ListSubdir) {
  ASSERT_THAT(mkdir((lower_ + "/sub").c_str(), 0700), SyscallSucceeds());
  ASSERT_TRUE(files::WriteFile(lower_ + "/sub/a", "a"));

  ASSERT_THAT(mkdir((upper_ + "/sub").c_str(), 0700), SyscallSucceeds());
  ASSERT_TRUE(files::WriteFile(upper_ + "/sub/b", "b"));

  ASSERT_NO_FATAL_FAILURE(Mount());

  std::vector<DirEntry> list;
  ASSERT_NO_FATAL_FAILURE(ReadDir(overlay_ + "/sub", &list));
  EXPECT_EQ(list.size(), 4u);

  EXPECT_EQ(list[0].name, ".");
  EXPECT_EQ(list[0].type, DT_DIR);
  EXPECT_EQ(list[1].name, "..");
  EXPECT_EQ(list[1].type, DT_DIR);

  EXPECT_EQ(list[2].name, "a");
  EXPECT_EQ(list[2].type, DT_REG);
  EXPECT_EQ(list[3].name, "b");
  EXPECT_EQ(list[3].type, DT_REG);
}

TEST_F(OverlayFsTest, DirAndFile) {
  ASSERT_TRUE(files::WriteFile(lower_ + "/sub", "a"));

  ASSERT_THAT(mkdir((upper_ + "/sub").c_str(), 0700), SyscallSucceeds());
  ASSERT_TRUE(files::WriteFile(upper_ + "/sub/b", "b"));

  ASSERT_NO_FATAL_FAILURE(Mount());

  std::vector<DirEntry> list;

  ASSERT_NO_FATAL_FAILURE(ReadDir(overlay_, &list));
  EXPECT_EQ(list.size(), 3u);
  EXPECT_EQ(list[2].name, "sub");
  EXPECT_EQ(list[2].type, DT_DIR);

  ASSERT_NO_FATAL_FAILURE(ReadDir(overlay_ + "/sub", &list));
  EXPECT_EQ(list.size(), 3u);
  EXPECT_EQ(list[2].name, "b");
  EXPECT_EQ(list[2].type, DT_REG);

  CheckFileContent("/sub/b", "b");
}

TEST_F(OverlayFsTest, ReadFileLower) {
  ASSERT_TRUE(files::WriteFile(lower_ + "/a", "1"));

  ASSERT_NO_FATAL_FAILURE(Mount());

  CheckFileContent("/a", "1");
}

TEST_F(OverlayFsTest, ReadFileUpper) {
  ASSERT_TRUE(files::WriteFile(upper_ + "/a", "2"));

  ASSERT_NO_FATAL_FAILURE(Mount());

  CheckFileContent("/a", "2");
}

TEST_F(OverlayFsTest, ReadFileBoth) {
  ASSERT_TRUE(files::WriteFile(lower_ + "/a", "1"));
  ASSERT_TRUE(files::WriteFile(upper_ + "/a", "2"));

  ASSERT_NO_FATAL_FAILURE(Mount());

  CheckFileContent("/a", "2");
}

TEST_F(OverlayFsTest, UnmountBase) {
  ASSERT_THAT(mount(nullptr, lower_.c_str(), "tmpfs", 0, nullptr), SyscallSucceeds());
  ASSERT_THAT(mount(nullptr, upper_base_.c_str(), "tmpfs", 0, nullptr), SyscallSucceeds());
  ASSERT_NO_FATAL_FAILURE(InitUpperDirs());

  ASSERT_TRUE(files::WriteFile(lower_ + "/a", "1"));
  ASSERT_TRUE(files::WriteFile(upper_ + "/b", "2"));

  ASSERT_NO_FATAL_FAILURE(Mount());

  // We should be able to unmount base file systems. Overlay should still be accessible.
  ASSERT_THAT(umount(lower_.c_str()), SyscallSucceeds());
  ASSERT_THAT(umount(upper_base_.c_str()), SyscallSucceeds());

  CheckFileContent("/a", "1");
  CheckFileContent("/b", "2");
}

TEST_F(OverlayFsTest, MountSubdir) {
  ASSERT_THAT(mkdir((lower_ + "/sub").c_str(), 0700), SyscallSucceeds());
  ASSERT_TRUE(files::WriteFile(lower_ + "/sub/a", "1"));

  ASSERT_THAT(mkdir((upper_ + "/sub").c_str(), 0700), SyscallSucceeds());
  ASSERT_TRUE(files::WriteFile(upper_ + "/sub/b", "2"));

  // Mount new FS over the subdirs. That shouldn't affect the files visible to overlayfs.
  ASSERT_THAT(mount(nullptr, (lower_ + "/sub").c_str(), "tmpfs", 0, nullptr), SyscallSucceeds());
  ASSERT_THAT(mount(nullptr, (upper_ + "/sub").c_str(), "tmpfs", 0, nullptr), SyscallSucceeds());
  ASSERT_TRUE(files::WriteFile(upper_ + "/sub/c", "3"));

  ASSERT_NO_FATAL_FAILURE(Mount());

  CheckFileContent("/sub/a", "1");
  CheckFileContent("/sub/b", "2");

  std::vector<DirEntry> list;
  ASSERT_NO_FATAL_FAILURE(ReadDir(overlay_ + "/sub", &list));
  EXPECT_EQ(list.size(), 4u);

  EXPECT_EQ(list[2].name, "a");
  EXPECT_EQ(list[3].name, "b");
}

TEST_F(OverlayFsTest, RemovedFiles) {
  // Char device with `type=0` is a removed file marker. It should not be listed in the
  // overlay FS.
  ASSERT_THAT(mknod((lower_ + "/a").c_str(), S_IFCHR, 0), SyscallSucceeds());
  ASSERT_THAT(mknod((lower_ + "/b").c_str(), S_IFCHR, 1), SyscallSucceeds());
  ASSERT_THAT(mknod((upper_ + "/c").c_str(), S_IFCHR, 0), SyscallSucceeds());
  ASSERT_THAT(mknod((upper_ + "/d").c_str(), S_IFCHR, 1), SyscallSucceeds());

  ASSERT_NO_FATAL_FAILURE(Mount());

  std::vector<DirEntry> list;
  ASSERT_NO_FATAL_FAILURE(ReadDir(overlay_, &list));

  EXPECT_EQ(list.size(), 4u);

  EXPECT_EQ(list[0].name, ".");
  EXPECT_EQ(list[0].type, DT_DIR);
  EXPECT_EQ(list[1].name, "..");
  EXPECT_EQ(list[1].type, DT_DIR);

  EXPECT_EQ(list[2].name, "b");
  EXPECT_EQ(list[2].type, DT_CHR);
  EXPECT_EQ(list[3].name, "d");
  EXPECT_EQ(list[3].type, DT_CHR);
}

}  // namespace
