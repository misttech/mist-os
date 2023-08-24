// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <dirent.h>
#include <fcntl.h>
#include <stdio.h>
#include <sys/mount.h>
#include <unistd.h>

#include <fstream>

#include <gtest/gtest.h>

#include "src/lib/files/directory.h"
#include "src/lib/files/file.h"
#include "src/lib/fxl/strings/string_printf.h"
#include "src/starnix/tests/syscalls/test_helper.h"

using testing::ElementsAre;
using testing::IsEmpty;

namespace {

struct DirEntry {
  std::string name;
  uint8_t type;
  ino_t inode_num;
  off_t offset;

  static DirEntry Dir(const std::string& name) {
    return DirEntry{
        .name = name,
        .type = DT_DIR,
    };
  }

  static DirEntry CharDev(const std::string& name) {
    return DirEntry{
        .name = name,
        .type = DT_CHR,
    };
  }

  static DirEntry File(const std::string& name) {
    return DirEntry{
        .name = name,
        .type = DT_REG,
    };
  }

  bool operator<(const DirEntry& rhs) const { return name < rhs.name; }
  bool operator==(const DirEntry& rhs) const { return name == rhs.name && type == rhs.type; }
};

std::ostream& operator<<(std::ostream& stream, DirEntry const& value) {
  return stream << "DirEntry(\"" << value.name << "\", type=" << static_cast<int>(value.type)
                << ")";
}

std::vector<DirEntry> ReadDir(const std::string& path) {
  std::vector<DirEntry> result;

  DIR* dir = opendir(path.c_str());
  if (!dir) {
    ADD_FAILURE() << "opendir() failed for " << path << " errno=" << errno;
    return result;
  }
  struct dirent* de;
  errno = 0;

  bool found_dot = false;
  bool found_dot_dot = false;

  while ((de = readdir(dir)) != nullptr) {
    if (std::string(de->d_name) == ".") {
      EXPECT_FALSE(found_dot);
      EXPECT_EQ(de->d_type, DT_DIR);
      found_dot = true;
    } else if (std::string(de->d_name) == "..") {
      EXPECT_FALSE(found_dot_dot);
      EXPECT_EQ(de->d_type, DT_DIR);
      found_dot_dot = true;
    } else {
      result.push_back(DirEntry{
          .name = de->d_name,
          .type = de->d_type,
          .inode_num = de->d_ino,
          .offset = de->d_off,
      });
    }
  }
  closedir(dir);

  if (errno != 0) {
    ADD_FAILURE() << "readdir() failed with errno=" << errno;
    return result;
  }

  EXPECT_TRUE(found_dot);
  EXPECT_TRUE(found_dot_dot);

  std::sort(result.begin(), result.end());

  return result;
}

std::string ReadFileContent(const std::string& path) {
  std::string content;
  EXPECT_TRUE(files::ReadFileToString(path.c_str(), &content));
  return content;
}

bool IsWhiteout(const std::string& path) {
  struct stat s;
  return stat(path.c_str(), &s) == 0 && (s.st_mode & S_IFMT) == S_IFCHR && s.st_rdev == 0;
}

std::string Readlink(const std::string& path) {
  std::string result;
  result.resize(256);
  ssize_t r = readlink(path.c_str(), result.data(), static_cast<int>(result.size()));
  if (r < 0) {
    ADD_FAILURE() << "readlink(" << path << ") failed with errno=" << errno;
    return "";
  }
  result.resize(r);
  return result;
}

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

  std::string ReadOverlayFileContent(const std::string& file) {
    return ReadFileContent(overlay_ + file);
  }

  test_helper::ScopedTempDir temp_dir_;
  std::string lower_;
  std::string upper_;
  std::string upper_base_;
  std::string work_;
  std::string overlay_;
};

TEST_F(OverlayFsTest, ListRoot) {
  ASSERT_TRUE(files::WriteFile(lower_ + "/a", "lower/a"));
  ASSERT_TRUE(files::WriteFile(lower_ + "/c", "lower/c"));
  ASSERT_TRUE(files::WriteFile(upper_ + "/b", "upper/b"));
  ASSERT_TRUE(files::WriteFile(upper_ + "/c", "upper/c"));

  ASSERT_NO_FATAL_FAILURE(Mount());

  std::vector<DirEntry> list = ReadDir(overlay_);
  EXPECT_THAT(list, ElementsAre(DirEntry::File("a"), DirEntry::File("b"), DirEntry::File("c")));

  std::vector<DirEntry> lower_list = ReadDir(lower_);
  std::vector<DirEntry> upper_list = ReadDir(upper_);

  // Inode number copied from the source file systems.
  EXPECT_EQ(list[0].inode_num, lower_list[0].inode_num);
  EXPECT_EQ(list[1].inode_num, upper_list[0].inode_num);
  EXPECT_EQ(list[2].inode_num, upper_list[1].inode_num);
}

TEST_F(OverlayFsTest, ListSubdir) {
  ASSERT_THAT(mkdir((lower_ + "/sub").c_str(), 0700), SyscallSucceeds());
  ASSERT_TRUE(files::WriteFile(lower_ + "/sub/a", "a"));

  ASSERT_THAT(mkdir((upper_ + "/sub").c_str(), 0700), SyscallSucceeds());
  ASSERT_TRUE(files::WriteFile(upper_ + "/sub/b", "b"));

  ASSERT_NO_FATAL_FAILURE(Mount());

  EXPECT_EQ(ReadDir(overlay_ + "/sub"), (std::vector{DirEntry::File("a"), DirEntry::File("b")}));
}

TEST_F(OverlayFsTest, DirAndFile) {
  ASSERT_TRUE(files::WriteFile(lower_ + "/sub", "a"));

  ASSERT_THAT(mkdir((upper_ + "/sub").c_str(), 0700), SyscallSucceeds());
  ASSERT_TRUE(files::WriteFile(upper_ + "/sub/b", "b"));

  ASSERT_NO_FATAL_FAILURE(Mount());

  EXPECT_EQ(ReadDir(overlay_), (std::vector{DirEntry::Dir("sub")}));
  EXPECT_EQ(ReadDir(overlay_ + "/sub"), (std::vector{DirEntry::File("b")}));

  EXPECT_EQ(ReadOverlayFileContent("/sub/b"), "b");
}

TEST_F(OverlayFsTest, ReadFileLower) {
  ASSERT_TRUE(files::WriteFile(lower_ + "/a", "1"));

  ASSERT_NO_FATAL_FAILURE(Mount());

  EXPECT_EQ(ReadOverlayFileContent("/a"), "1");
}

TEST_F(OverlayFsTest, ReadFileUpper) {
  ASSERT_TRUE(files::WriteFile(upper_ + "/a", "2"));

  ASSERT_NO_FATAL_FAILURE(Mount());

  EXPECT_EQ(ReadOverlayFileContent("/a"), "2");
}

TEST_F(OverlayFsTest, ReadFileBoth) {
  ASSERT_TRUE(files::WriteFile(lower_ + "/a", "1"));
  ASSERT_TRUE(files::WriteFile(upper_ + "/a", "2"));

  ASSERT_NO_FATAL_FAILURE(Mount());

  EXPECT_EQ(ReadOverlayFileContent("/a"), "2");
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

  EXPECT_EQ(ReadOverlayFileContent("/a"), "1");
  EXPECT_EQ(ReadOverlayFileContent("/b"), "2");
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

  EXPECT_EQ(ReadOverlayFileContent("/sub/a"), "1");
  EXPECT_EQ(ReadOverlayFileContent("/sub/b"), "2");

  EXPECT_EQ(ReadDir(overlay_ + "/sub"), (std::vector{DirEntry::File("a"), DirEntry::File("b")}));
}

TEST_F(OverlayFsTest, RemovedFiles) {
  // Char device with `type=0` is a removed file marker. It should not be listed in the
  // overlay FS.
  ASSERT_THAT(mknod((lower_ + "/a").c_str(), S_IFCHR, 0), SyscallSucceeds());
  ASSERT_THAT(mknod((lower_ + "/b").c_str(), S_IFCHR, 1), SyscallSucceeds());
  ASSERT_THAT(mknod((upper_ + "/c").c_str(), S_IFCHR, 0), SyscallSucceeds());
  ASSERT_THAT(mknod((upper_ + "/d").c_str(), S_IFCHR, 1), SyscallSucceeds());

  ASSERT_NO_FATAL_FAILURE(Mount());

  EXPECT_EQ(ReadDir(overlay_), (std::vector{DirEntry::CharDev("b"), DirEntry::CharDev("d")}));
}

TEST_F(OverlayFsTest, UnlinkLower) {
  ASSERT_THAT(mkdir((lower_ + "/s").c_str(), 0700), SyscallSucceeds());
  ASSERT_TRUE(files::WriteFile(lower_ + "/s/a", "a"));

  ASSERT_NO_FATAL_FAILURE(Mount());

  // Remove the file.
  ASSERT_THAT(unlink((overlay_ + "/s/a").c_str()), SyscallSucceeds());
  EXPECT_THAT(ReadDir(overlay_ + "/s"), IsEmpty());
  EXPECT_THAT(open((overlay_ + "/s/a").c_str(), O_RDONLY), SyscallFailsWithErrno(ENOENT));

  // Whiteout should be created in the upper dir.
  EXPECT_TRUE(IsWhiteout(upper_ + "/s/a"));

  // Remove the directory.
  ASSERT_THAT(rmdir((overlay_ + "/s").c_str()), SyscallSucceeds());
  EXPECT_THAT(ReadDir(overlay_), IsEmpty());
  EXPECT_THAT(open((overlay_ + "/s").c_str(), O_RDONLY), SyscallFailsWithErrno(ENOENT));

  // Whiteout should be created in the upper dir.
  EXPECT_TRUE(IsWhiteout(upper_ + "/s"));

  // Lower file should not be touched.
  EXPECT_EQ(ReadFileContent(lower_ + "/s/a"), "a");
}

TEST_F(OverlayFsTest, UnlinkNonEmptyDir) {
  ASSERT_THAT(mkdir((upper_ + "/d").c_str(), 0700), SyscallSucceeds());
  ASSERT_THAT(mkdir((lower_ + "/d").c_str(), 0700), SyscallSucceeds());
  ASSERT_TRUE(files::WriteFile(lower_ + "/d/a", "a"));

  ASSERT_NO_FATAL_FAILURE(Mount());

  // Try removing non-empty dir. This is expected to fail.
  ASSERT_THAT(rmdir((overlay_ + "/d").c_str()), SyscallFailsWithErrno(ENOTEMPTY));

  // Directory should still exist.
  EXPECT_EQ(ReadDir(overlay_), (std::vector{DirEntry::Dir("d")}));
  EXPECT_EQ(ReadDir(overlay_ + "/d"), (std::vector{DirEntry::File("a")}));

  // No changes to the lower and upper FS.
  EXPECT_THAT(ReadDir(upper_ + "/d"), IsEmpty());
  EXPECT_EQ(ReadDir(lower_ + "/d"), (std::vector{DirEntry::File("a")}));
}

TEST_F(OverlayFsTest, UnlinkUpper) {
  ASSERT_THAT(mkdir((upper_ + "/s").c_str(), 0700), SyscallSucceeds());
  ASSERT_TRUE(files::WriteFile(upper_ + "/s/a", "a"));

  ASSERT_NO_FATAL_FAILURE(Mount());

  ASSERT_THAT(unlink((overlay_ + "/s/a").c_str()), SyscallSucceeds());

  EXPECT_THAT(ReadDir(overlay_ + "/s"), IsEmpty());
  EXPECT_THAT(open((overlay_ + "/s/a").c_str(), O_RDONLY), SyscallFailsWithErrno(ENOENT));

  EXPECT_THAT(ReadDir(lower_), IsEmpty());
  EXPECT_THAT(ReadDir(upper_ + "/s"), IsEmpty());

  // Remove the directory.
  ASSERT_THAT(rmdir((overlay_ + "/s").c_str()), SyscallSucceeds());
  EXPECT_THAT(open((overlay_ + "/s").c_str(), O_RDONLY), SyscallFailsWithErrno(ENOENT));

  EXPECT_THAT(ReadDir(overlay_), IsEmpty());
  EXPECT_THAT(ReadDir(lower_), IsEmpty());
  EXPECT_THAT(ReadDir(upper_), IsEmpty());
}

TEST_F(OverlayFsTest, UnlinkBoth) {
  ASSERT_THAT(mkdir((lower_ + "/s").c_str(), 0700), SyscallSucceeds());
  ASSERT_TRUE(files::WriteFile(lower_ + "/s/a", "a"));
  ASSERT_THAT(mkdir((upper_ + "/s").c_str(), 0700), SyscallSucceeds());
  ASSERT_TRUE(files::WriteFile(upper_ + "/s/a", "a"));

  ASSERT_NO_FATAL_FAILURE(Mount());

  // Remove the file.
  ASSERT_THAT(unlink((overlay_ + "/s/a").c_str()), SyscallSucceeds());
  EXPECT_THAT(ReadDir(overlay_ + "/s"), IsEmpty());
  EXPECT_THAT(open((overlay_ + "/s/a").c_str(), O_RDONLY), SyscallFailsWithErrno(ENOENT));

  // Whiteout should be created in the upper dir.
  EXPECT_TRUE(IsWhiteout(upper_ + "/s/a"));

  // Remove the directory.
  ASSERT_THAT(rmdir((overlay_ + "/s").c_str()), SyscallSucceeds());
  EXPECT_THAT(ReadDir(overlay_), IsEmpty());
  EXPECT_THAT(open((overlay_ + "/s").c_str(), O_RDONLY), SyscallFailsWithErrno(ENOENT));

  // Whiteout should be created in the upper dir.
  EXPECT_TRUE(IsWhiteout(upper_ + "/s"));

  // Lower file should not be touched.
  EXPECT_EQ(ReadFileContent(lower_ + "/s/a"), "a");
}

TEST_F(OverlayFsTest, RecreateDir) {
  ASSERT_THAT(mkdir((lower_ + "/s").c_str(), 0700), SyscallSucceeds());
  ASSERT_TRUE(files::WriteFile(lower_ + "/s/a", "a"));

  ASSERT_NO_FATAL_FAILURE(Mount());

  // Remove the file.
  ASSERT_THAT(unlink((overlay_ + "/s/a").c_str()), SyscallSucceeds());
  EXPECT_THAT(ReadDir(overlay_ + "/s"), IsEmpty());

  // Whiteout should be created in the upper dir.
  EXPECT_TRUE(IsWhiteout(upper_ + "/s/a"));

  // Remove the directory.
  ASSERT_THAT(rmdir((overlay_ + "/s").c_str()), SyscallSucceeds());
  EXPECT_THAT(ReadDir(overlay_), IsEmpty());

  // Whiteout should be created in the upper dir.
  EXPECT_TRUE(IsWhiteout(upper_ + "/s"));

  // Create the dir again.
  ASSERT_THAT(mkdir((overlay_ + "/s").c_str(), 0700), SyscallSucceeds());

  // The new dir should be empty.
  EXPECT_THAT(ReadDir(overlay_ + "/s"), IsEmpty());
  EXPECT_THAT(open((overlay_ + "/s/a").c_str(), O_RDONLY), SyscallFailsWithErrno(ENOENT));

  // Lower file should not be touched.
  EXPECT_EQ(ReadFileContent(lower_ + "/s/a"), "a");
}

TEST_F(OverlayFsTest, RecreateFile) {
  ASSERT_THAT(mkdir((lower_ + "/s").c_str(), 0700), SyscallSucceeds());
  ASSERT_TRUE(files::WriteFile(lower_ + "/s/a", "a"));

  ASSERT_NO_FATAL_FAILURE(Mount());

  // Remove the file.
  ASSERT_THAT(unlink((overlay_ + "/s/a").c_str()), SyscallSucceeds());
  EXPECT_THAT(ReadDir(overlay_ + "/s"), IsEmpty());
  EXPECT_THAT(open((overlay_ + "/s/a").c_str(), O_RDONLY), SyscallFailsWithErrno(ENOENT));

  // Try creating the file again
  ASSERT_TRUE(files::WriteFile(overlay_ + "/s/a", "b"));
  EXPECT_EQ(ReadFileContent(overlay_ + "/s/a"), "b");

  // It should be written to the upper dir.
  EXPECT_EQ(ReadFileContent(upper_ + "/s/a"), "b");

  // Lower file should not be touched.
  EXPECT_EQ(ReadFileContent(lower_ + "/s/a"), "a");
}

TEST_F(OverlayFsTest, CreateDir) {
  ASSERT_THAT(mkdir((lower_ + "/d1").c_str(), 0700), SyscallSucceeds());
  ASSERT_THAT(mkdir((lower_ + "/d2").c_str(), 0700), SyscallSucceeds());

  ASSERT_THAT(mkdir((upper_ + "/d1").c_str(), 0700), SyscallSucceeds());
  ASSERT_THAT(mkdir((upper_ + "/d3").c_str(), 0700), SyscallSucceeds());

  ASSERT_NO_FATAL_FAILURE(Mount());

  // Make new dirs.
  ASSERT_THAT(mkdir((overlay_ + "/d1/1").c_str(), 0700), SyscallSucceeds());
  ASSERT_THAT(mkdir((overlay_ + "/d2/2").c_str(), 0700), SyscallSucceeds());
  ASSERT_THAT(mkdir((overlay_ + "/d3/3").c_str(), 0700), SyscallSucceeds());

  // Verify that the dirs were created.
  EXPECT_EQ(ReadDir(overlay_ + "/d1"), (std::vector{DirEntry::Dir("1")}));
  EXPECT_EQ(ReadDir(overlay_ + "/d2"), (std::vector{DirEntry::Dir("2")}));
  EXPECT_EQ(ReadDir(overlay_ + "/d3"), (std::vector{DirEntry::Dir("3")}));

  // New dirs are empty.
  EXPECT_THAT(ReadDir(overlay_ + "/d1/1"), IsEmpty());
  EXPECT_THAT(ReadDir(overlay_ + "/d2/2"), IsEmpty());
  EXPECT_THAT(ReadDir(overlay_ + "/d3/3"), IsEmpty());

  // Lower FS should not change.
  EXPECT_EQ(ReadDir(lower_), (std::vector{DirEntry::Dir("d1"), DirEntry::Dir("d2")}));
  EXPECT_THAT(ReadDir(lower_ + "/d1"), IsEmpty());
  EXPECT_THAT(ReadDir(lower_ + "/d2"), IsEmpty());

  // New parent dir created in the upper FS.
  EXPECT_EQ(ReadDir(upper_),
            (std::vector{DirEntry::Dir("d1"), DirEntry::Dir("d2"), DirEntry::Dir("d3")}));
}

TEST_F(OverlayFsTest, CreateFile) {
  ASSERT_THAT(mkdir((lower_ + "/d").c_str(), 0700), SyscallSucceeds());

  ASSERT_NO_FATAL_FAILURE(Mount());

  ASSERT_TRUE(files::WriteFile(overlay_ + "/d/a", "foo"));
  EXPECT_EQ(ReadFileContent(overlay_ + "/d/a"), "foo");

  // New file should be created in the upper dir.
  EXPECT_EQ(ReadDir(upper_), (std::vector{DirEntry::Dir("d")}));
  EXPECT_EQ(ReadFileContent(upper_ + "/d/a"), "foo");

  // Lower FS should not change.
  EXPECT_THAT(ReadDir(lower_ + "/d"), IsEmpty());
}

TEST_F(OverlayFsTest, CreateSymlink) {
  ASSERT_THAT(mkdir((lower_ + "/d").c_str(), 0700), SyscallSucceeds());

  ASSERT_NO_FATAL_FAILURE(Mount());

  ASSERT_THAT(symlink("symlink_target", (overlay_ + "/d/a").c_str()), SyscallSucceeds());
  EXPECT_EQ(Readlink(overlay_ + "/d/a"), "symlink_target");

  // New file should be created in the upper dir.
  EXPECT_EQ(ReadDir(upper_), (std::vector{DirEntry::Dir("d")}));
  EXPECT_EQ(Readlink(upper_ + "/d/a"), "symlink_target");

  // Lower FS should not change.
  EXPECT_THAT(ReadDir(lower_ + "/d"), IsEmpty());
}

TEST_F(OverlayFsTest, RewriteFile) {
  ASSERT_THAT(mkdir((lower_ + "/d").c_str(), 0700), SyscallSucceeds());
  ASSERT_TRUE(files::WriteFile(lower_ + "/d/a", "a"));

  ASSERT_NO_FATAL_FAILURE(Mount());

  ASSERT_TRUE(files::WriteFile(overlay_ + "/d/a", "foo"));
  EXPECT_EQ(ReadFileContent(overlay_ + "/d/a"), "foo");

  // New file should be created in the upper dir.
  EXPECT_EQ(ReadDir(upper_), (std::vector{DirEntry::Dir("d")}));
  EXPECT_EQ(ReadFileContent(upper_ + "/d/a"), "foo");

  // Lower FS should not change.
  EXPECT_EQ(ReadFileContent(lower_ + "/d/a"), "a");
}

TEST_F(OverlayFsTest, AppendFile) {
  ASSERT_THAT(mkdir((lower_ + "/d").c_str(), 0700), SyscallSucceeds());
  ASSERT_TRUE(files::WriteFile(lower_ + "/d/a", "a-"));

  ASSERT_NO_FATAL_FAILURE(Mount());

  {
    std::ofstream stream;
    stream.open(overlay_ + "/d/a", std::ios_base::app);
    stream << "foo";
    ASSERT_FALSE(stream.fail());
  }

  EXPECT_EQ(ReadFileContent(overlay_ + "/d/a"), "a-foo");

  // New file should be created in the upper dir.
  EXPECT_EQ(ReadDir(upper_), (std::vector{DirEntry::Dir("d")}));
  EXPECT_EQ(ReadFileContent(upper_ + "/d/a"), "a-foo");

  // Lower FS should not change.
  EXPECT_EQ(ReadFileContent(lower_ + "/d/a"), "a-");
}

TEST_F(OverlayFsTest, Link) {
  ASSERT_THAT(mkdir((lower_ + "/d").c_str(), 0700), SyscallSucceeds());
  ASSERT_TRUE(files::WriteFile(lower_ + "/d/a", "a"));

  ASSERT_NO_FATAL_FAILURE(Mount());

  ASSERT_THAT(link((overlay_ + "/d/a").c_str(), (overlay_ + "/d/b").c_str()), SyscallSucceeds());

  // New file should be created in the upper dir.
  auto list = ReadDir(upper_ + "/d");
  EXPECT_EQ(list, (std::vector{DirEntry::File("a"), DirEntry::File("b")}));
  EXPECT_EQ(list[0].inode_num, list[1].inode_num);

  // Write new content. Both files should be updated.
  ASSERT_TRUE(files::WriteFile(overlay_ + "/d/a", "new"));
  EXPECT_EQ(ReadFileContent(upper_ + "/d/a"), "new");
  EXPECT_EQ(ReadFileContent(upper_ + "/d/b"), "new");

  // Lower FS should not change.
  EXPECT_EQ(ReadDir(lower_ + "/d"), (std::vector{DirEntry::File("a")}));
  EXPECT_EQ(ReadFileContent(lower_ + "/d/a"), "a");
}

TEST_F(OverlayFsTest, UpdateAfterOpen) {
  ASSERT_TRUE(files::WriteFile(lower_ + "/a", "a"));

  ASSERT_NO_FATAL_FAILURE(Mount());

  std::ifstream file(overlay_ + "/a", std::ios::in);

  ASSERT_TRUE(files::WriteFile(overlay_ + "/a", "updated"));

  // Start reading from `file`. We should get new content, written after the file was open.
  std::string data;
  file >> data;
  EXPECT_EQ(data, "updated");
}

TEST_F(OverlayFsTest, RenameFile) {
  ASSERT_THAT(mkdir((lower_ + "/d1").c_str(), 0700), SyscallSucceeds());
  ASSERT_TRUE(files::WriteFile(lower_ + "/d1/a", "a"));

  ASSERT_NO_FATAL_FAILURE(Mount());

  ASSERT_THAT(mkdir((overlay_ + "/d2").c_str(), 0700), SyscallSucceeds());
  ASSERT_THAT(rename((overlay_ + "/d1/a").c_str(), (overlay_ + "/d2/b").c_str()),
              SyscallSucceeds());

  EXPECT_EQ(ReadDir(overlay_), (std::vector{DirEntry::Dir("d1"), DirEntry::Dir("d2")}));
  EXPECT_EQ(ReadFileContent(overlay_ + "/d2/b"), "a");

  // New file should be created in the upper FS.
  EXPECT_EQ(ReadFileContent(upper_ + "/d2/b"), "a");

  // Lower FS should not change.
  EXPECT_EQ(ReadDir(lower_), (std::vector{DirEntry::Dir("d1")}));
  EXPECT_EQ(ReadFileContent(lower_ + "/d1/a"), "a");
}

TEST_F(OverlayFsTest, RenameNonexistent) {
  ASSERT_THAT(mkdir((lower_ + "/d1").c_str(), 0700), SyscallSucceeds());
  ASSERT_TRUE(files::WriteFile(lower_ + "/d1/a", "a"));

  ASSERT_NO_FATAL_FAILURE(Mount());

  ASSERT_THAT(rename((overlay_ + "/d1/b").c_str(), (overlay_ + "/d1/c").c_str()),
              SyscallFailsWithErrno(ENOENT));
}

TEST_F(OverlayFsTest, RenameDir) {
  ASSERT_THAT(mkdir((lower_ + "/d1").c_str(), 0700), SyscallSucceeds());
  ASSERT_TRUE(files::WriteFile(lower_ + "/d1/a", "a"));
  ASSERT_THAT(mkdir((upper_ + "/d1").c_str(), 0700), SyscallSucceeds());
  ASSERT_TRUE(files::WriteFile(upper_ + "/d1/a", "a"));

  ASSERT_NO_FATAL_FAILURE(Mount());

  ASSERT_THAT(rename((overlay_ + "/d1").c_str(), (overlay_ + "/d2").c_str()),
              SyscallFailsWithErrno(EXDEV));

  // Lower FS should not change.
  EXPECT_EQ(ReadDir(lower_), (std::vector{DirEntry::Dir("d1")}));
  EXPECT_EQ(ReadFileContent(lower_ + "/d1/a"), "a");
}

}  // namespace
