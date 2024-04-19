// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <dirent.h>
#include <fcntl.h>
#include <poll.h>
#include <sys/mount.h>
#include <sys/stat.h>
#include <sys/statfs.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <sys/xattr.h>
#include <unistd.h>

#include <algorithm>
#include <condition_variable>
#include <cstddef>
#include <cstdint>
#include <memory>
#include <mutex>
#include <optional>
#include <thread>
#include <vector>

#include <fbl/unique_fd.h>
#include <gtest/gtest.h>
#include <linux/capability.h>
#include <linux/fuse.h>
#include <linux/magic.h>

#include "src/lib/fxl/strings/string_printf.h"
#include "src/starnix/tests/syscalls/cpp/test_helper.h"

#define OK_OR_RETURN(x) \
  {                     \
    auto result = (x);  \
    if (!result) {      \
      return result;    \
    }                   \
  }

constexpr char kOverlayFsPath[] = "OVERLAYFS_PATH";

class FuseTest : public ::testing::Test {
 public:
  void SetUp() override {
    if (!test_helper::HasSysAdmin()) {
      GTEST_SKIP() << "Not running with sysadmin capabilities, skipping suite.";
    }
  }

  void TearDown() override {
    if (base_dir_) {
      if (umount(GetMountDir().c_str()) != 0) {
        FAIL() << "Unable to umount: " << strerror(errno);
      }
      base_dir_.reset();
    }
  }

 protected:
  std::string GetOverlayFsPath() {
    if (getenv(kOverlayFsPath)) {
      return getenv(kOverlayFsPath);
    } else {
      return "data/fuse-overlayfs";
    }
  }

  std::string GetMountDir() { return *base_dir_ + "/merge"; }

  testing::AssertionResult MkDir(const std::string& directory) {
    if (mkdir(directory.c_str(), 0700) != 0) {
      return testing::AssertionFailure()
             << "Unable to create '" << directory << "': " << strerror(errno);
    }
    return testing::AssertionSuccess();
  }

  testing::AssertionResult Mount() {
    if (access(GetOverlayFsPath().c_str(), R_OK | X_OK) != 0) {
      return testing::AssertionFailure()
             << "Unable to find fuse binary at: " << GetOverlayFsPath() << "(set OVERLAYFS_PATH)";
    }

    std::string base_dir = "/tmp/fuse_base_dir_XXXXXX";
    if (mkdtemp(const_cast<char*>(base_dir.c_str())) == nullptr) {
      return testing::AssertionFailure()
             << "Unable to create temporary directory: " << strerror(errno);
    }
    std::string lowerdir = base_dir + "/lower";
    OK_OR_RETURN(MkDir(lowerdir));
    std::string upperdir = base_dir + "/upper";
    OK_OR_RETURN(MkDir(upperdir));
    std::string workdir = base_dir + "/work";
    OK_OR_RETURN(MkDir(workdir));
    std::string mergedir = base_dir + "/merge";
    OK_OR_RETURN(MkDir(mergedir));
    std::string witness_name = "witness";
    {
      test_helper::ScopedFD witness(
          open((lowerdir + "/" + witness_name).c_str(), O_RDWR | O_CREAT, 0600));
      if (!witness.is_valid()) {
        return testing::AssertionFailure() << "Unable to create witness file: " << strerror(errno);
      }
      if (write(witness.get(), "hello\n", 6) != 6) {
        return testing::AssertionFailure()
               << "Unable to insert data in witness file: " << strerror(errno);
      }
    }
    pid_t child_pid = fork_helper_.RunInForkedProcess([&] {
      std::string configuration =
          "lowerdir=" + lowerdir + ",upperdir=" + upperdir + ",workdir=" + workdir;
      execl(GetOverlayFsPath().c_str(), GetOverlayFsPath().c_str(), "-f", "-o",
            configuration.c_str(), mergedir.c_str(), NULL);
    });
    if (child_pid <= 0) {
      return testing::AssertionFailure() << "Unable to fork to start the fuse server process";
    }
    base_dir_ = base_dir;

    std::string witness = mergedir + "/" + witness_name;
    for (int i = 0; i < 20 && access(witness.c_str(), R_OK) != 0; ++i) {
      usleep(100000);
    }
    if (access(witness.c_str(), R_OK) != 0) {
      return testing::AssertionFailure() << "Unable to see witness file. Mount failed?";
    }
    return testing::AssertionSuccess();
  }

  test_helper::ForkHelper fork_helper_;
  std::optional<std::string> base_dir_;
};

class FuseServer {
 public:
  FuseServer() : FuseServer(0, S_IFREG) {}
  FuseServer(uint32_t want_init_flags, uint32_t file_type)
      : want_init_flags_(want_init_flags), file_type_(file_type) {}

  virtual ~FuseServer() {}

  const test_helper::ScopedFD& fuse_fd() { return fuse_fd_; }

  testing::AssertionResult Mount(const std::string& path) {
    OK_OR_RETURN(OpenFuseDevice());

    struct stat stat_buffer;
    if (stat(path.c_str(), &stat_buffer) == -1) {
      return testing::AssertionFailure() << "Failed to stat mount path: " << strerror(errno);
    }

    std::string options =
        fxl::StringPrintf("fd=%i,rootmode=%o,user_id=%u,group_id=%u", fuse_fd_.get(),
                          stat_buffer.st_mode & S_IFMT, getuid(), getgid());
    if (mount("fuse", path.c_str(), "fuse", 0, options.c_str()) == -1) {
      return testing::AssertionFailure() << "Failed to mount fuse device: " << strerror(errno);
    }
    return testing::AssertionSuccess();
  }

  bool ServeOnce() {
    std::vector<std::byte> buffer;
    bool unmounted = false;
    EXPECT_TRUE(ReadRequest(&buffer, &unmounted));
    if (unmounted) {
      return false;
    }
    EXPECT_TRUE(HandleFuseMessage(buffer));
    return true;
  }

  template <typename R = void>
  R WaitForInit(std::function<R()> f = std::function<R()>()) {
    std::unique_lock guard(init_mtx_);
    init_cv_.wait(guard, [&]() { return init_done_; });
    if (f) {
      return f();
    }
    return R();
  }

  testing::AssertionResult SendInitResponse(const fuse_in_header* in_header, uint32_t flags) {
    fuse_init_out init_out = {
        .major = FUSE_KERNEL_VERSION,
        .minor = FUSE_KERNEL_MINOR_VERSION,
        .flags = flags,
    };
    return WriteStructResponse(in_header, init_out);
  }

 protected:
  virtual testing::AssertionResult HandleFuseMessage(const std::vector<std::byte>& message) {
    const struct fuse_in_header* in_header =
        reinterpret_cast<const struct fuse_in_header*>(message.data());
    // The operation-specific payload for the fuse request begins after the header.
    const void* in_payload = in_header + 1;
    switch (in_header->opcode) {
      case FUSE_INIT: {
        struct fuse_init_in init_in = {};
        memcpy(&init_in, in_payload, sizeof(init_in));
        OK_OR_RETURN(HandleInit(in_header, &init_in, message));
        break;
      }
      case FUSE_ACCESS: {
        struct fuse_access_in access_in = {};
        memcpy(&access_in, in_payload, sizeof(access_in));
        OK_OR_RETURN(HandleAccess(in_header, &access_in, message));
        break;
      }
      case FUSE_GETATTR: {
        struct fuse_getattr_in getattr_in = {};
        memcpy(&getattr_in, in_payload, sizeof(getattr_in));
        OK_OR_RETURN(HandleGetAttr(in_header, &getattr_in, message));
        break;
      }
      case FUSE_LOOKUP: {
        OK_OR_RETURN(HandleLookup(in_header, message));
        break;
      }
      case FUSE_OPENDIR:
      case FUSE_OPEN: {
        struct fuse_open_in open_in = {};
        memcpy(&open_in, in_payload, sizeof(open_in));
        OK_OR_RETURN(HandleOpen(in_header, &open_in, message));
        break;
      }
      case FUSE_FLUSH: {
        struct fuse_flush_in flush_in = {};
        memcpy(&flush_in, in_payload, sizeof(flush_in));
        OK_OR_RETURN(HandleFlush(in_header, &flush_in, message));
        break;
      }
      case FUSE_RELEASEDIR:
      case FUSE_RELEASE: {
        struct fuse_release_in release_in = {};
        memcpy(&release_in, in_payload, sizeof(release_in));
        OK_OR_RETURN(HandleRelease(in_header, &release_in, message));
        break;
      }
      case FUSE_GETXATTR: {
        OK_OR_RETURN(WriteDataFreeResponse(in_header, -ENOSYS));
        break;
      }
      case FUSE_BATCH_FORGET:
      case FUSE_FORGET:
        // no-op; these don't expect a response.
        break;
      default:
        return testing::AssertionFailure() << "Unknown FUSE opcode: " << in_header->opcode;
    }
    return testing::AssertionSuccess();
  }

  void NotifyInitWaiters(std::function<void()> f = std::function<void()>()) {
    std::unique_lock guard(init_mtx_);
    init_done_ = true;
    if (f) {
      f();
    }
    init_cv_.notify_all();
  }

  virtual testing::AssertionResult HandleInit(const struct fuse_in_header* in_header,
                                              const struct fuse_init_in* init_in,
                                              const std::vector<std::byte>& message) {
    EXPECT_EQ(init_in->flags & want_init_flags_, want_init_flags_);
    OK_OR_RETURN(SendInitResponse(in_header, want_init_flags_));
    NotifyInitWaiters();
    return testing::AssertionSuccess();
  }

  virtual testing::AssertionResult HandleAccess(const struct fuse_in_header* in_header,
                                                const struct fuse_access_in* access_in,
                                                const std::vector<std::byte>& message) {
    return WriteAckResponse(in_header);
  }

  virtual testing::AssertionResult HandleGetAttr(const struct fuse_in_header* in_header,
                                                 const struct fuse_getattr_in* getattr_in,
                                                 const std::vector<std::byte>& message) {
    fuse_attr_out attr_out = {};
    PopulateDefaultAttr(in_header->nodeid, attr_out.attr);
    return WriteStructResponse(in_header, attr_out);
  }

  virtual testing::AssertionResult HandleLookup(const struct fuse_in_header* in_header,
                                                const std::vector<std::byte>& message) {
    fuse_entry_out entry_out;
    PopulateDefaultEntry(entry_out);
    return WriteStructResponse(in_header, entry_out);
  }

  virtual testing::AssertionResult HandleOpen(const struct fuse_in_header* in_header,
                                              const struct fuse_open_in* open_in,
                                              const std::vector<std::byte>& message) {
    struct fuse_open_out open_out = {};
    open_out.fh = GetNextFileHandle();
    return WriteStructResponse(in_header, open_out);
  }

  virtual testing::AssertionResult HandleFlush(const struct fuse_in_header* in_header,
                                               const struct fuse_flush_in* flush_in,
                                               const std::vector<std::byte>& message) {
    return WriteAckResponse(in_header);
  }

  virtual testing::AssertionResult HandleRelease(const struct fuse_in_header* in_header,
                                                 const struct fuse_release_in* release_in,
                                                 const std::vector<std::byte>& message) {
    return WriteAckResponse(in_header);
  }

  testing::AssertionResult WriteDataFreeResponse(const struct fuse_in_header* in_header,
                                                 int32_t error) {
    fuse_out_header out_header = {
        .len = sizeof(fuse_out_header),
        .error = error,
        .unique = in_header->unique,
    };

    auto data = reinterpret_cast<std::byte*>(&out_header);
    std::vector<std::byte> response(data, data + sizeof(out_header));
    return WriteResponse(response);
  }

  testing::AssertionResult WriteAckResponse(const struct fuse_in_header* in_header) {
    return WriteDataFreeResponse(in_header, /* error= */ 0);
  }

  template <typename Data>
  testing::AssertionResult WriteStructResponse(const struct fuse_in_header* in_header, Data data) {
    struct fuse_out_header out_header = {};
    uint32_t payload_len = sizeof(Data);
    uint32_t response_len = payload_len + sizeof(out_header);
    out_header.len = response_len;
    out_header.unique = in_header->unique;
    std::vector<std::byte> response(response_len);
    memcpy(response.data(), &out_header, sizeof(out_header));
    memcpy(response.data() + sizeof(out_header), &data, sizeof(Data));
    return WriteResponse(response);
  }

  testing::AssertionResult WriteResponse(std::vector<std::byte> response) {
    ssize_t actual = HANDLE_EINTR(write(fuse_fd_.get(), response.data(), response.size()));
    if (actual != static_cast<ssize_t>(response.size())) {
      return testing::AssertionFailure()
             << "Failed to write FUSE response: Got " << actual << " Expected " << response.size()
             << ": " << strerror(errno);
    }
    return testing::AssertionSuccess();
  }

  uint64_t GetNextFileHandle() { return next_fh_++; }

  void PopulateDefaultEntry(fuse_entry_out& entry_out) {
    entry_out = {
        .nodeid = next_nodeid_++,
        .generation = 1,
    };
    PopulateDefaultAttr(entry_out.nodeid, entry_out.attr);
  }

  void PopulateDefaultAttr(uint64_t ino, fuse_attr& attr) {
    attr = {
        .ino = ino,
        // Consider the root node as a directory and everything else as the
        // specified kind. The node will also have read/write/exec permissions
        // for the owning user, group and world. For current testing needs,
        // this is sufficient.
        .mode = static_cast<uint32_t>(ino == FUSE_ROOT_ID ? S_IFDIR : file_type_) | S_IRWXU |
                S_IRWXG | S_IRWXO,
    };
  }

 private:
  testing::AssertionResult ReadRequest(std::vector<std::byte>* request, bool* unmounted) {
    // There doesn't seem to be a good value to use for the max request size. We just pick
    // something large that works for our cases.
    const size_t kMaxRequestSize = 64ul * FUSE_MIN_READ_BUFFER;
    request->resize(kMaxRequestSize);
    ssize_t actual = HANDLE_EINTR(read(fuse_fd_.get(), request->data(), request->size()));
    if (actual == -1) {
      if (errno == ENODEV) {
        request->clear();
        *unmounted = true;
        return testing::AssertionSuccess();
        ;
      }
      return testing::AssertionFailure() << "Failed to read FUSE request: " << strerror(errno);
    }
    request->resize(actual);
    *unmounted = false;
    return testing::AssertionSuccess();
  }

  testing::AssertionResult OpenFuseDevice() {
    fuse_fd_ = test_helper::ScopedFD(open("/dev/fuse", O_RDWR));
    if (!fuse_fd_.is_valid()) {
      return testing::AssertionFailure() << "Failed to open /dev/fuse: " << strerror(errno);
    }
    return testing::AssertionSuccess();
  }

  test_helper::ScopedFD fuse_fd_;
  uint64_t next_fh_ = 1;
  uint64_t next_nodeid_ = 2;  // 1 is reserved for the root.

  std::mutex init_mtx_;
  std::condition_variable init_cv_;
  bool init_done_ = false;

  uint32_t want_init_flags_;
  uint32_t file_type_;
};

class FuseServerTest : public ::testing::Test {
 public:
  void SetUp() override {
    if (!test_helper::HasSysAdmin()) {
      GTEST_SKIP() << "Not running with sysadmin capabilities, skipping suite.";
    }
  }

  void TearDown() override {
    if (mount_dir_) {
      if (umount(mount_dir_->c_str()) != 0) {
        FAIL() << "Unable to umount: " << strerror(errno);
      }
      mount_dir_.reset();
      server_thread_.join();
    }
  }

 protected:
  std::string GetMountDir() { return *mount_dir_; }

  testing::AssertionResult Mount(std::shared_ptr<FuseServer> server) {
    std::string mount_dir = "/tmp/fuse_mount_dir_XXXXXX";
    if (mkdtemp(const_cast<char*>(mount_dir.c_str())) == nullptr) {
      return testing::AssertionFailure()
             << "Unable to create temporary directory: " << strerror(errno);
    }

    OK_OR_RETURN(server->Mount(mount_dir));
    mount_dir_ = mount_dir;
    server_ = std::move(server);

    server_thread_ = std::thread([this] {
      while (server_->ServeOnce()) {
      }
    });

    return testing::AssertionSuccess();
  }

 private:
  std::optional<std::string> mount_dir_;
  std::shared_ptr<FuseServer> server_;
  std::thread server_thread_;
};

TEST_F(FuseTest, ReadWriteUnMountedDevFuse) {
  fbl::unique_fd fuse_fd(open("/dev/fuse", O_RDWR));
  ASSERT_TRUE(fuse_fd.is_valid());
  char buffer[1024];
  ASSERT_EQ(read(fuse_fd.get(), buffer, 1024), -1);
  ASSERT_EQ(errno, EPERM);
  ASSERT_EQ(write(fuse_fd.get(), buffer, 1024), -1);
  ASSERT_EQ(errno, EPERM);
}

TEST_F(FuseTest, Mount) { ASSERT_TRUE(Mount()); }

TEST_F(FuseTest, Stats) {
  ASSERT_TRUE(Mount());
  std::string mounted_witness = GetMountDir() + "/witness";
  std::string original_witness = *base_dir_ + "/lower/witness";
  test_helper::ScopedFD fd(open(mounted_witness.c_str(), O_RDONLY));
  ASSERT_TRUE(fd.is_valid());
  struct stat mounted_stats;
  ASSERT_EQ(fstat(fd.get(), &mounted_stats), 0);
  fd = test_helper::ScopedFD(open(original_witness.c_str(), O_RDONLY));
  ASSERT_TRUE(fd.is_valid());
  struct stat original_stats;
  ASSERT_EQ(fstat(fd.get(), &original_stats), 0);
  fd.reset();

  // Check that the stat of the mounted file are the same as the origin one,
  // except for the fs id.
  ASSERT_NE(mounted_stats.st_dev, original_stats.st_dev);
  // Clobber st_dev and check the rest of the data is the same.
  mounted_stats.st_dev = 0;
  original_stats.st_dev = 0;
  ASSERT_EQ(memcmp(&mounted_stats, &original_stats, sizeof(struct stat)), 0);
}

TEST_F(FuseTest, Read) {
  ASSERT_TRUE(Mount());
  std::string mounted_witness = GetMountDir() + "/witness";
  test_helper::ScopedFD fd(open(mounted_witness.c_str(), O_RDONLY));
  ASSERT_TRUE(fd.is_valid());
  char buffer[100];
  ASSERT_EQ(read(fd.get(), buffer, 100), 6);
  ASSERT_EQ(strncmp(buffer, "hello\n", 6), 0);
}

TEST_F(FuseTest, NoFile) {
  ASSERT_TRUE(Mount());
  std::string mounted_witness = GetMountDir() + "/unexistent";
  test_helper::ScopedFD fd(open(mounted_witness.c_str(), O_RDONLY));
  ASSERT_FALSE(fd.is_valid());
  ASSERT_EQ(errno, ENOENT);
}

TEST_F(FuseTest, Mknod) {
  ASSERT_TRUE(Mount());
  std::string filename = GetMountDir() + "/file";
  test_helper::ScopedFD fd(open(filename.c_str(), O_WRONLY | O_CREAT));
  ASSERT_TRUE(fd.is_valid());
}

TEST_F(FuseTest, Write) {
  ASSERT_TRUE(Mount());
  std::string filename = GetMountDir() + "/file";
  test_helper::ScopedFD fd(open(filename.c_str(), O_WRONLY | O_CREAT));
  ASSERT_TRUE(fd.is_valid());
  EXPECT_EQ(write(fd.get(), "hello\n", 6), 6);
}

TEST_F(FuseTest, Statfs) {
  ASSERT_TRUE(Mount());
  struct statfs stats;
  ASSERT_EQ(statfs((GetMountDir() + "/witness").c_str(), &stats), 0);
  ASSERT_EQ(stats.f_type, FUSE_SUPER_MAGIC);
}

TEST_F(FuseTest, Seek) {
  ASSERT_TRUE(Mount());
  test_helper::ScopedFD fd(open((GetMountDir() + "/witness").c_str(), O_RDONLY));
  ASSERT_TRUE(fd.is_valid());
  char buffer[100];
  ASSERT_EQ(read(fd.get(), buffer, 100), 6);
  ASSERT_EQ(strncmp(buffer, "hello\n", 6), 0);
  ASSERT_EQ(lseek(fd.get(), 0, SEEK_CUR), 6);
  ASSERT_EQ(lseek(fd.get(), -5, SEEK_END), 1);
  ASSERT_EQ(read(fd.get(), buffer, 100), 5);
  ASSERT_EQ(strncmp(buffer, "ello\n", 5), 0);

  ASSERT_EQ(lseek(fd.get(), 1, SEEK_DATA), 1);
  ASSERT_EQ(lseek(fd.get(), 7, SEEK_DATA), -1);
  ASSERT_EQ(errno, ENXIO);
  ASSERT_EQ(lseek(fd.get(), 0, SEEK_HOLE), 6);
  ASSERT_EQ(lseek(fd.get(), 7, SEEK_HOLE), -1);
  ASSERT_EQ(errno, ENXIO);
}

TEST_F(FuseTest, Poll) {
  ASSERT_TRUE(Mount());
  test_helper::ScopedFD fd(open((GetMountDir() + "/witness").c_str(), O_RDONLY));
  ASSERT_TRUE(fd.is_valid());
  struct pollfd poll_struct;
  poll_struct.fd = fd.get();
  poll_struct.events = -1;
  ASSERT_EQ(poll(&poll_struct, 1, 0), 1);
  ASSERT_EQ(poll_struct.revents, POLLIN | POLLOUT | POLLRDNORM | POLLWRNORM);
}

TEST_F(FuseTest, Mkdir) {
  ASSERT_TRUE(Mount());
  std::string dirname = GetMountDir() + "/dir";
  ASSERT_EQ(open(dirname.c_str(), O_RDONLY), -1);
  ASSERT_EQ(mkdir(dirname.c_str(), 0777), 0);
  test_helper::ScopedFD fd(open(dirname.c_str(), O_RDONLY));
  ASSERT_TRUE(fd.is_valid());
  struct stat stats;
  ASSERT_EQ(fstat(fd.get(), &stats), 0);
  ASSERT_TRUE(S_ISDIR(stats.st_mode));
}

TEST_F(FuseTest, Symlink) {
  ASSERT_TRUE(Mount());
  std::string witness = GetMountDir() + "/witness";
  std::string link = GetMountDir() + "/symlink";
  ASSERT_EQ(symlink(witness.c_str(), link.c_str()), 0);
  struct stat stats;
  ASSERT_EQ(lstat(link.c_str(), &stats), 0);
  ASSERT_TRUE(S_ISLNK(stats.st_mode));
  std::vector<char> buffer;
  buffer.resize(100);
  ASSERT_EQ(readlink(link.c_str(), &buffer[0], buffer.size()),
            static_cast<ssize_t>(witness.size()));
  buffer.resize(witness.size());
  ASSERT_EQ(memcmp(&buffer[0], &witness[0], witness.size()), 0);
}

TEST_F(FuseTest, Link) {
  ASSERT_TRUE(Mount());
  std::string witness = GetMountDir() + "/witness";
  std::string linkname = GetMountDir() + "/link";
  ASSERT_EQ(link(witness.c_str(), linkname.c_str()), 0);
  struct stat stats;
  ASSERT_EQ(lstat(linkname.c_str(), &stats), 0);
  ASSERT_TRUE(S_ISREG(stats.st_mode));
  ino_t ino = stats.st_ino;
  ASSERT_EQ(lstat(witness.c_str(), &stats), 0);
  ASSERT_EQ(ino, stats.st_ino);
}

TEST_F(FuseTest, Unlink) {
  ASSERT_TRUE(Mount());
  std::string witness = GetMountDir() + "/witness";
  ASSERT_TRUE(test_helper::ScopedFD(open(witness.c_str(), O_RDONLY)).is_valid());
  ASSERT_EQ(unlink(witness.c_str()), 0);
  ASSERT_FALSE(test_helper::ScopedFD(open(witness.c_str(), O_RDONLY)).is_valid());
}

TEST_F(FuseTest, Truncate) {
  ASSERT_TRUE(Mount());
  std::string file = GetMountDir() + "/file";
  test_helper::ScopedFD fd(open(file.c_str(), O_WRONLY | O_CREAT));
  ASSERT_TRUE(fd.is_valid());
  ASSERT_EQ(write(fd.get(), "hello", 5), 5);
  fd.reset();
  ASSERT_EQ(truncate(file.c_str(), 2), 0);
  fd = test_helper::ScopedFD(open(file.c_str(), O_RDONLY));
  char buffer[10];
  ASSERT_EQ(read(fd.get(), buffer, 10), 2);
}

TEST_F(FuseTest, Readdir) {
  // Create enough file to ensure more than one call to the fuse operation is
  // needed to read the full content of a directory. Experimentally, the libc
  // creates a buffer of 32k bytes when the user calls readdir.
  const size_t kFileCount = (32768 / (sizeof(struct fuse_dirent) + 6)) + 1;
  ASSERT_TRUE(Mount());
  std::string root = GetMountDir();
  for (size_t i = 0; i < kFileCount; ++i) {
    std::string value = std::to_string(i / 2);
    if (i % 2 == 0) {
      ASSERT_EQ(mkdir((root + "/dir_" + value).c_str(), 0777), 0);
    } else {
      test_helper::ScopedFD fd(open((root + "/file" + value).c_str(), O_WRONLY | O_CREAT));
      ASSERT_TRUE(fd.is_valid());
    }
  }

  std::map<std::string, struct dirent> files;
  DIR* dir = opendir(root.c_str());
  ASSERT_TRUE(dir);
  while (struct dirent* entry = readdir(dir)) {
    std::string name = entry->d_name;
    files[name] = *entry;
  }
  closedir(dir);
  ASSERT_EQ(files.size(), 2u + kFileCount);
  ASSERT_NE(files.find("witness"), files.end());
  ASSERT_NE(files.find("."), files.end());
  // fuse-overlayfs doesn't contain .. on root
  ASSERT_EQ(files.find(".."), files.end());

  files.clear();
  std::string dir1 = GetMountDir() + "/dir_0";
  dir = opendir(dir1.c_str());
  ASSERT_TRUE(dir);
  while (struct dirent* entry = readdir(dir)) {
    std::string name = entry->d_name;
    files[name] = *entry;
  }
  closedir(dir);
  ASSERT_EQ(files.size(), 2u);
  ASSERT_NE(files.find("."), files.end());
  ASSERT_NE(files.find(".."), files.end());
}

TEST_F(FuseTest, Getdents) {
  ASSERT_TRUE(Mount());
  std::string root = GetMountDir();
  test_helper::ScopedFD fd(open(root.c_str(), O_RDONLY));
  ASSERT_TRUE(fd.is_valid());
  char buffer[4096];
  ASSERT_GT(syscall(SYS_getdents64, fd.get(), buffer, 4096), 0);
}

TEST_F(FuseTest, XAttr) {
  const char attribute_name[] = "user.comment\0";
  ASSERT_TRUE(Mount());

  std::string filename = GetMountDir() + "/file";
  test_helper::ScopedFD fd(open(filename.c_str(), O_RDWR | O_CREAT));
  ASSERT_TRUE(fd.is_valid());

  ASSERT_EQ(fgetxattr(fd.get(), attribute_name, nullptr, 0), -1);
  ASSERT_EQ(errno, ENODATA);
  ASSERT_EQ(fsetxattr(fd.get(), attribute_name, "hello", 5, XATTR_CREATE), 0);
  ASSERT_EQ(fgetxattr(fd.get(), attribute_name, nullptr, 1), -1);
  ASSERT_EQ(errno, ERANGE);
  ASSERT_EQ(fgetxattr(fd.get(), attribute_name, nullptr, 0), 5);
  char buffer[5];
  ASSERT_EQ(fgetxattr(fd.get(), attribute_name, buffer, 5), 5);
  ASSERT_EQ(memcmp(buffer, "hello", 5), 0);

  ssize_t list_size = flistxattr(fd.get(), nullptr, 0);
  ASSERT_GE(list_size, 0);
  char list[list_size];
  ASSERT_EQ(flistxattr(fd.get(), list, list_size), list_size);
  ASSERT_EQ(list[list_size - 1], '\0');
  std::set<std::string> attributes;
  ssize_t index = 0;
  while (index < list_size) {
    std::string content = std::string(&list[index]);
    attributes.insert(content);
    index += content.size() + 1;
  }
  ASSERT_NE(attributes.find(attribute_name), attributes.end());

  ASSERT_EQ(fremovexattr(fd.get(), attribute_name), 0);
  ASSERT_EQ(fgetxattr(fd.get(), attribute_name, nullptr, 0), -1);
  ASSERT_EQ(errno, ENODATA);
}

TEST_F(FuseServerTest, NoReqsUntilInitResponse) {
  class NoReqsUntilInitResponseServer : public FuseServer {
   public:
    fuse_in_header WaitForInitAndReturnRequestHeader() {
      return WaitForInit(std::function<fuse_in_header()>([&]() { return init_hdr_; }));
    }

   protected:
    testing::AssertionResult HandleInit(const struct fuse_in_header* in_header,
                                        const struct fuse_init_in* init_in,
                                        const std::vector<std::byte>& message) {
      // Don't actually complete the request, just store the init request's header so
      // that we can respond to it later.
      NotifyInitWaiters([&]() { init_hdr_ = *in_header; });
      return testing::AssertionSuccess();
    }

   private:
    fuse_in_header init_hdr_;
  };

  std::shared_ptr<NoReqsUntilInitResponseServer> server(new NoReqsUntilInitResponseServer());
  ASSERT_TRUE(Mount(server));
  const fuse_in_header init_hdr = server->WaitForInitAndReturnRequestHeader();

  // Create a new thread to perform a request against the FUSE server.
  std::atomic_bool access_done = false;
  std::thread thrd([&]() {
    std::string filename = GetMountDir() + "/file";
    EXPECT_EQ(access(filename.c_str(), R_OK), 0) << strerror(errno);
    access_done = true;
  });
  // Make sure that the request is not completed.
  sleep(1);
  EXPECT_FALSE(access_done);

  // Send our (delayed) response to the FUSE_INIT request and make sure that the
  // access request is now completed.
  server->SendInitResponse(&init_hdr, 0);
  thrd.join();
  EXPECT_TRUE(access_done);
}

TEST_F(FuseServerTest, OpenAndClose) {
  ASSERT_TRUE(Mount(std::make_shared<FuseServer>()));

  std::string filename = GetMountDir() + "/file";
  test_helper::ScopedFD fd(open(filename.c_str(), O_RDWR | O_CREAT));
  ASSERT_TRUE(fd.is_valid());
  fd.reset();
}

TEST_F(FuseServerTest, HeaderLengthUnderflow) {
  class HeaderLengthUnderflowServer : public FuseServer {
    testing::AssertionResult HandleAccess(const struct fuse_in_header* in_header,
                                          const struct fuse_access_in* access_in,
                                          const std::vector<std::byte>& message) override {
      uint32_t response_len = sizeof(struct fuse_out_header);
      std::vector<std::byte> response;
      response.resize(response_len);
      struct fuse_out_header* out_header =
          reinterpret_cast<struct fuse_out_header*>(response.data());
      out_header->len = 0;
      out_header->unique = in_header->unique;
      return WriteResponse(response);
    }
  };

  ASSERT_TRUE(Mount(std::make_shared<HeaderLengthUnderflowServer>()));

  std::string filename = GetMountDir() + "/file";
  test_helper::ScopedFD fd(open(filename.c_str(), O_RDWR | O_CREAT));
  ASSERT_TRUE(fd.is_valid());
  fd.reset();
}

TEST_F(FuseServerTest, OverlongHeaderLength) {
  class OverlongHeaderLengthServer : public FuseServer {
    testing::AssertionResult HandleOpen(const struct fuse_in_header* in_header,
                                        const struct fuse_open_in* open_in,
                                        const std::vector<std::byte>& message) override {
      const uint32_t kBogusHeaderLengthAddition = 1024;

      struct fuse_open_out open_out = {};
      open_out.fh = GetNextFileHandle();

      struct fuse_out_header out_header = {};
      uint32_t payload_len = sizeof(open_out);
      uint32_t response_len = payload_len + sizeof(out_header);
      out_header.len = response_len + kBogusHeaderLengthAddition;
      out_header.unique = in_header->unique;
      std::vector<std::byte> response(response_len);
      memcpy(response.data(), &out_header, sizeof(out_header));
      memcpy(response.data() + sizeof(out_header), &open_out, sizeof(open_out));
      return WriteResponse(response);
    }
  };

  ASSERT_TRUE(Mount(std::make_shared<OverlongHeaderLengthServer>()));

  std::string filename = GetMountDir() + "/file";
  test_helper::ScopedFD fd(open(filename.c_str(), O_RDWR | O_CREAT));
  ASSERT_TRUE(fd.is_valid());
  fd.reset();
}

// Run the checks in a separate thread where we drop the |CAP_DAC_OVERRIDE|
// capability which bypasses file permission checks. See capabilities(7).
template <typename F>
void InThreadWithoutCapDacOverride(F f) {
  std::thread thrd([&]() {
    test_helper::UnsetCapability(CAP_DAC_OVERRIDE);

    f();
  });
  thrd.join();
}

struct BypassAccessTestCase {
  uint32_t want_init_flags;
  int access_reply;
  uint64_t max_access_count;
};

class CountingFuseServer : public FuseServer {
 public:
  CountingFuseServer(uint32_t want_init_flags, uint32_t file_type, int access_reply)
      : FuseServer(want_init_flags, file_type), access_reply_(access_reply) {}

  uint64_t LookupCount() { return calls_to_lookup_.load(std::memory_order_relaxed); }
  uint64_t AccessCount() { return calls_to_access_.load(std::memory_order_relaxed); }
  uint64_t NonRootGetAttrCount() {
    return calls_to_non_root_getattr_.load(std::memory_order_relaxed);
  }

 protected:
  testing::AssertionResult HandleLookup(const struct fuse_in_header* in_header,
                                        const std::vector<std::byte>& message) override {
    calls_to_lookup_.fetch_add(1, std::memory_order_relaxed);
    return FuseServer::HandleLookup(in_header, message);
  }

  testing::AssertionResult HandleAccess(const struct fuse_in_header* in_header,
                                        const struct fuse_access_in* access_in,
                                        const std::vector<std::byte>& message) override {
    calls_to_access_.fetch_add(1, std::memory_order_relaxed);
    return WriteDataFreeResponse(in_header, access_reply_);
  }

  testing::AssertionResult HandleGetAttr(const struct fuse_in_header* in_header,
                                         const struct fuse_getattr_in* getattr_in,
                                         const std::vector<std::byte>& message) override {
    if (in_header->nodeid != FUSE_ROOT_ID) {
      calls_to_non_root_getattr_.fetch_add(1, std::memory_order_relaxed);
    }
    return FuseServer::HandleGetAttr(in_header, getattr_in, message);
  }

 private:
  int access_reply_;
  std::atomic_uint64_t calls_to_lookup_ = 0;
  std::atomic_uint64_t calls_to_access_ = 0;
  std::atomic_uint64_t calls_to_non_root_getattr_ = 0;
};

struct PermissionCheckTestCase {
  std::optional<uint32_t> need_cap;
  uint32_t want_init_flags;
  uint32_t file_type;
  std::function<void(const std::string&)> fn;
  uint64_t expected_lookup_count;
  uint64_t expected_access_count;
  uint64_t expected_non_root_getattr_count;
};

class FuseServerPermissionCheck : public FuseServerTest,
                                  public ::testing::WithParamInterface<PermissionCheckTestCase> {};

TEST_P(FuseServerPermissionCheck, PermissionCheck) {
  const PermissionCheckTestCase& test_case = GetParam();

  if (test_case.need_cap && !test_helper::HasCapability(test_case.need_cap.value())) {
    GTEST_SKIP() << "Need extra capability " << test_case.need_cap.value();
  }

  std::shared_ptr<CountingFuseServer> server(
      new CountingFuseServer(test_case.want_init_flags, test_case.file_type, 0));
  ASSERT_TRUE(Mount(server));
  server->WaitForInit();
  EXPECT_EQ(server->LookupCount(), 0u);
  EXPECT_EQ(server->AccessCount(), 0u);
  EXPECT_EQ(server->NonRootGetAttrCount(), 0u);

  std::string path = GetMountDir() + "/node";
  ASSERT_NO_FATAL_FAILURE(test_case.fn(path));
  EXPECT_EQ(server->LookupCount(), test_case.expected_lookup_count);
  EXPECT_EQ(server->AccessCount(), test_case.expected_access_count);
  EXPECT_EQ(server->NonRootGetAttrCount(), test_case.expected_non_root_getattr_count);
}

void TestChdir(const std::string& path) {
  test_helper::ForkHelper fork_helper;
  // Run in a forked process to not modify the state of the current
  // process which may run other tests.
  fork_helper.RunInForkedProcess([&] { ASSERT_EQ(chdir(path.c_str()), 0) << strerror(errno); });
  ASSERT_TRUE(fork_helper.WaitForChildren());
}

void TestChroot(const std::string& path) {
  test_helper::ForkHelper fork_helper;
  // Run in a forked process to not modify the state of the current
  // process which may run other tests.
  fork_helper.RunInForkedProcess([&] { ASSERT_EQ(chroot(path.c_str()), 0) << strerror(errno); });
  ASSERT_TRUE(fork_helper.WaitForChildren());
}

void TestAccess(const std::string& path) {
  ASSERT_EQ(access(path.c_str(), R_OK), 0) << strerror(errno);
}

void TestStat(const std::string& path) {
  struct stat s;
  ASSERT_EQ(stat(path.c_str(), &s), 0) << strerror(errno);
}

void TestOpenWithFlags(const std::string& path, int flags) {
  test_helper::ScopedFD fd(open(path.c_str(), flags));
  ASSERT_TRUE(fd.is_valid());
}

INSTANTIATE_TEST_SUITE_P(FuseServerPermissionCheck, FuseServerPermissionCheck,
                         testing::Values(
                             // When performing a path walk, we should only use |FUSE_LOOKUP|
                             // for _initial_ permission/access checking.
                             PermissionCheckTestCase{
                                 .want_init_flags = 0,
                                 .file_type = S_IFREG,
                                 .fn =
                                     [](const std::string& path) {
                                       ASSERT_NO_FATAL_FAILURE(TestOpenWithFlags(path, O_RDWR));
                                     },
                                 .expected_lookup_count = 1,
                                 .expected_access_count = 0,
                                 .expected_non_root_getattr_count = 0,
                             },
                             PermissionCheckTestCase{
                                 .want_init_flags = 0,
                                 .file_type = S_IFDIR,
                                 .fn =
                                     [](const std::string& path) {
                                       ASSERT_NO_FATAL_FAILURE(TestOpenWithFlags(path, O_RDONLY));
                                     },
                                 .expected_lookup_count = 1,
                                 .expected_access_count = 0,
                                 .expected_non_root_getattr_count = 0,
                             },
                             PermissionCheckTestCase{
                                 .want_init_flags = 0,
                                 .file_type = S_IFREG,
                                 .fn = TestStat,
                                 .expected_lookup_count = 1,
                                 .expected_access_count = 0,
                                 .expected_non_root_getattr_count = 1,
                             },
                             PermissionCheckTestCase{
                                 .want_init_flags = 0,
                                 .file_type = S_IFDIR,
                                 .fn = TestStat,
                                 .expected_lookup_count = 1,
                                 .expected_access_count = 0,
                                 .expected_non_root_getattr_count = 1,
                             },
                             // These are the same as the above, but with the `FUSE_POSIX_ACL`
                             // init flag set.
                             PermissionCheckTestCase{
                                 .want_init_flags = FUSE_POSIX_ACL,
                                 .file_type = S_IFREG,
                                 .fn =
                                     [](const std::string& path) {
                                       ASSERT_NO_FATAL_FAILURE(TestOpenWithFlags(path, O_RDWR));
                                     },
                                 .expected_lookup_count = 1,
                                 .expected_access_count = 0,
                                 .expected_non_root_getattr_count = 1,
                             },
                             PermissionCheckTestCase{
                                 .want_init_flags = FUSE_POSIX_ACL,
                                 .file_type = S_IFDIR,
                                 .fn =
                                     [](const std::string& path) {
                                       ASSERT_NO_FATAL_FAILURE(TestOpenWithFlags(path, O_RDONLY));
                                     },
                                 .expected_lookup_count = 1,
                                 .expected_access_count = 0,
                                 .expected_non_root_getattr_count = 1,
                             },
                             PermissionCheckTestCase{
                                 .want_init_flags = FUSE_POSIX_ACL,
                                 .file_type = S_IFREG,
                                 .fn = TestStat,
                                 .expected_lookup_count = 1,
                                 .expected_access_count = 0,
                                 .expected_non_root_getattr_count = 1,
                             },
                             PermissionCheckTestCase{
                                 .want_init_flags = FUSE_POSIX_ACL,
                                 .file_type = S_IFDIR,
                                 .fn = TestStat,
                                 .expected_lookup_count = 1,
                                 .expected_access_count = 0,
                                 .expected_non_root_getattr_count = 1,
                             },

                             // Only the |access|, |chdir| and |chroot| family of
                             // syscalls may trigger |FUSE_ACCESS|.
                             PermissionCheckTestCase{
                                 .want_init_flags = 0,
                                 .file_type = S_IFREG,
                                 .fn = TestAccess,
                                 .expected_lookup_count = 1,
                                 .expected_access_count = 1,
                                 .expected_non_root_getattr_count = 0,
                             },
                             PermissionCheckTestCase{
                                 .want_init_flags = 0,
                                 .file_type = S_IFDIR,
                                 .fn = TestAccess,
                                 .expected_lookup_count = 1,
                                 .expected_access_count = 1,
                                 .expected_non_root_getattr_count = 0,
                             },
                             PermissionCheckTestCase{
                                 .want_init_flags = 0,
                                 .file_type = S_IFDIR,
                                 .fn = TestChdir,
                                 .expected_lookup_count = 1,
                                 .expected_access_count = 1,
                                 .expected_non_root_getattr_count = 0,
                             },
                             PermissionCheckTestCase{
                                 .need_cap = CAP_SYS_CHROOT,
                                 .want_init_flags = 0,
                                 .file_type = S_IFDIR,
                                 .fn = TestChroot,
                                 .expected_lookup_count = 1,
                                 .expected_access_count = 1,
                                 .expected_non_root_getattr_count = 0,
                             },
                             // These are the same as the above, but with the `FUSE_POSIX_ACL`
                             // init flag set.
                             PermissionCheckTestCase{
                                 .want_init_flags = FUSE_POSIX_ACL,
                                 .file_type = S_IFREG,
                                 .fn = TestAccess,
                                 .expected_lookup_count = 1,
                                 .expected_access_count = 0,
                                 .expected_non_root_getattr_count = 1,
                             },
                             PermissionCheckTestCase{
                                 .want_init_flags = FUSE_POSIX_ACL,
                                 .file_type = S_IFDIR,
                                 .fn = TestAccess,
                                 .expected_lookup_count = 1,
                                 .expected_access_count = 0,
                                 .expected_non_root_getattr_count = 1,
                             },
                             PermissionCheckTestCase{
                                 .want_init_flags = FUSE_POSIX_ACL,
                                 .file_type = S_IFDIR,
                                 .fn = TestChdir,
                                 .expected_lookup_count = 1,
                                 .expected_access_count = 0,
                                 .expected_non_root_getattr_count = 1,
                             },
                             PermissionCheckTestCase{
                                 .need_cap = CAP_SYS_CHROOT,
                                 .want_init_flags = FUSE_POSIX_ACL,
                                 .file_type = S_IFDIR,
                                 .fn = TestChroot,
                                 .expected_lookup_count = 1,
                                 .expected_access_count = 0,
                                 .expected_non_root_getattr_count = 1,
                             }));

class FuseServerBypassAccessTest : public FuseServerTest,
                                   public ::testing::WithParamInterface<BypassAccessTestCase> {};

TEST_P(FuseServerBypassAccessTest, BypassAccess) {
  const BypassAccessTestCase& test_case = GetParam();

  std::shared_ptr<CountingFuseServer> server(
      new CountingFuseServer(test_case.want_init_flags, S_IFREG, test_case.access_reply));
  ASSERT_TRUE(Mount(server));
  server->WaitForInit();
  EXPECT_EQ(server->AccessCount(), 0u);

  InThreadWithoutCapDacOverride([&]() {
    auto check_access = [&, max_access_count = test_case.max_access_count](const char* file) {
      const std::string filename = GetMountDir() + "/" + file;
      ASSERT_EQ(access(filename.c_str(), R_OK), 0) << strerror(errno);
      EXPECT_EQ(server->AccessCount(), max_access_count);
    };

    // No matter how many times we access a file, we should have only ever made
    // the |FUSE_ACCESS| request |params.max_access_count| times for the lifetime
    // of the connection/server.
    for (int i = 0; i < 3; ++i) {
      ASSERT_NO_FATAL_FAILURE(check_access("somefile1"));
    }

    // Accessing another file shouldn't change the access count since it is still
    // part of the same connection.
    ASSERT_NO_FATAL_FAILURE(check_access("somefile2"));
  });
}

INSTANTIATE_TEST_SUITE_P(
    FuseServerBypassAccessTest, FuseServerBypassAccessTest,
    testing::Values(
        // The kernel should stop sending |FUSE_ACCESS| requests once we send an
        // |ENOSYS| response.
        BypassAccessTestCase{.want_init_flags = 0, .access_reply = -ENOSYS, .max_access_count = 1},
        // The kernel should never send a |FUSE_ACCESS| request if we set the
        // |FUSE_POSIX_ACL| init flag.
        BypassAccessTestCase{
            .want_init_flags = FUSE_POSIX_ACL, .access_reply = 0, .max_access_count = 0}));

enum class ExpectedGetAttrBehaviour {
  kNone,
  kOncePerFile,
  kOncePerAccess,
};

uint64_t ExpectedGetAttrsValue(ExpectedGetAttrBehaviour behaviour, uint64_t access_count) {
  switch (behaviour) {
    case ExpectedGetAttrBehaviour::kNone:
      return 0;
    case ExpectedGetAttrBehaviour::kOncePerFile:
      return std::min(access_count, 1ul);
    case ExpectedGetAttrBehaviour::kOncePerAccess:
      return access_count;
  }
}

struct CacheAttributesTestCase {
  uint64_t lookup_attr_timeout;
  uint64_t getattr_attr_timeout;
  ExpectedGetAttrBehaviour expected_getattr_behaviour;
};

class FuseServerCacheAttributesTest
    : public FuseServerTest,
      public ::testing::WithParamInterface<CacheAttributesTestCase> {};

TEST_P(FuseServerCacheAttributesTest, CacheAttributes) {
  const CacheAttributesTestCase& test_case = GetParam();

  class CacheAttributesServer : public FuseServer {
   public:
    CacheAttributesServer(uint64_t lookup_attr_valid, uint64_t getattr_attr_valid)
        : FuseServer(FUSE_POSIX_ACL, S_IFREG),
          lookup_attr_valid_(lookup_attr_valid),
          getattr_attr_valid_(getattr_attr_valid) {}

    uint64_t NonRootGetAttrCount() {
      return calls_to_non_root_getattr_.load(std::memory_order_relaxed);
    }

   protected:
    testing::AssertionResult HandleLookup(const struct fuse_in_header* in_header,
                                          const std::vector<std::byte>& message) override {
      fuse_entry_out entry_out;
      PopulateDefaultEntry(entry_out);
      // Instruct the kernel to not immediately evict the |dcache| entry (|dentry|)
      // for this node by setting a really high entry value. This value is used to
      // determine when a FUSE-based |dentry| has gone stale. This test isn't focused
      // on the |dcache| or |dentry| so this is ok. For more details, see:
      //  - https://www.halolinux.us/kernel-reference/the-dentry-cache.html
      //  - https://www.kernel.org/doc/html/latest/filesystems/path-lookup.html
      //  - https://lwn.net/Articles/649115/
      //  -
      //  https://www.infradead.org/~mchehab/kernel_docs/filesystems/path-walking.html#dcache-name-lookup
      entry_out.entry_valid = std::numeric_limits<uint64_t>::max();
      entry_out.attr_valid = lookup_attr_valid_;
      return WriteStructResponse(in_header, entry_out);
    }

    testing::AssertionResult HandleGetAttr(const struct fuse_in_header* in_header,
                                           const struct fuse_getattr_in* getattr_in,
                                           const std::vector<std::byte>& message) override {
      if (in_header->nodeid != FUSE_ROOT_ID) {
        calls_to_non_root_getattr_.fetch_add(1, std::memory_order_relaxed);
      }
      fuse_attr_out attr_out = {
          .attr_valid = getattr_attr_valid_,
      };
      PopulateDefaultAttr(in_header->nodeid, attr_out.attr);
      return WriteStructResponse(in_header, attr_out);
    }

   private:
    uint64_t lookup_attr_valid_;
    uint64_t getattr_attr_valid_;
    std::atomic_uint64_t calls_to_non_root_getattr_ = 0;
  };

  std::shared_ptr<CacheAttributesServer> server(
      new CacheAttributesServer(test_case.lookup_attr_timeout, test_case.getattr_attr_timeout));
  ASSERT_TRUE(Mount(server));
  server->WaitForInit();
  EXPECT_EQ(server->NonRootGetAttrCount(), 0u);

  InThreadWithoutCapDacOverride([&]() {
    auto check_getattr = [&](const char* file, uint64_t expected_getattrs) {
      const std::string filename = GetMountDir() + "/" + file;
      ASSERT_EQ(access(filename.c_str(), R_OK), 0) << strerror(errno);
      EXPECT_EQ(server->NonRootGetAttrCount(), expected_getattrs);
    };

    uint64_t count_after_file1;
    for (uint64_t i = 1; i <= 3; ++i) {
      count_after_file1 = ExpectedGetAttrsValue(test_case.expected_getattr_behaviour, i);
      ASSERT_NO_FATAL_FAILURE(check_getattr("somefile1", count_after_file1));
    }

    for (uint64_t i = 1; i <= 5; ++i) {
      ASSERT_NO_FATAL_FAILURE(check_getattr(
          "somefile2",
          count_after_file1 + ExpectedGetAttrsValue(test_case.expected_getattr_behaviour, i)));
    }
  });
}

INSTANTIATE_TEST_SUITE_P(
    FuseServerCacheAttributesTest, FuseServerCacheAttributesTest,
    testing::Values(
        // When we don't cache the attributes, expect the kernel to refresh the
        // attributes each call.
        CacheAttributesTestCase{
            .lookup_attr_timeout = 0,
            .getattr_attr_timeout = 0,
            .expected_getattr_behaviour = ExpectedGetAttrBehaviour::kOncePerAccess},

        // When we respond to the lookup request with a cache timeout, it should
        // be respected.
        CacheAttributesTestCase{.lookup_attr_timeout = std::numeric_limits<uint64_t>::max(),
                                .getattr_attr_timeout = 0,
                                .expected_getattr_behaviour = ExpectedGetAttrBehaviour::kNone},
        // When don't provide a cache timeout with lookup, but do for getattr,
        // respect the cached attributes after the getattr request.
        CacheAttributesTestCase{
            .lookup_attr_timeout = 0,
            .getattr_attr_timeout = std::numeric_limits<uint64_t>::max(),
            .expected_getattr_behaviour = ExpectedGetAttrBehaviour::kOncePerFile}));
