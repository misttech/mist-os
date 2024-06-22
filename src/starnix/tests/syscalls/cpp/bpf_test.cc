// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fcntl.h>
#include <sys/file.h>
#include <syscall.h>
#include <unistd.h>

#include <algorithm>
#include <climits>
#include <vector>

#include <fbl/unique_fd.h>
#include <gtest/gtest.h>
#include <linux/bpf.h>

#include "src/starnix/tests/syscalls/cpp/test_helper.h"

namespace {

int bpf(int cmd, union bpf_attr attr) { return (int)syscall(__NR_bpf, cmd, &attr, sizeof(attr)); }

TEST(BpfTest, ArraySizeOverflow) {
  int result = SAFE_SYSCALL_SKIP_ON_EPERM(bpf(BPF_MAP_CREATE, (union bpf_attr){
                                                                  .map_type = BPF_MAP_TYPE_ARRAY,
                                                                  .key_size = sizeof(int),
                                                                  .value_size = 1024,
                                                                  .max_entries = INT_MAX / 8,
                                                              }));
  EXPECT_EQ(result, -1);
  EXPECT_EQ(errno, EINVAL);
}

class BpfMapTest : public testing::Test {
 protected:
  void SetUp() override {
    array_fd_ = SAFE_SYSCALL_SKIP_ON_EPERM(bpf(BPF_MAP_CREATE, (union bpf_attr){
                                                                   .map_type = BPF_MAP_TYPE_ARRAY,
                                                                   .key_size = sizeof(int),
                                                                   .value_size = 1024,
                                                                   .max_entries = 10,
                                                               }));
    map_fd_ = SAFE_SYSCALL_SKIP_ON_EPERM(bpf(BPF_MAP_CREATE, (union bpf_attr){
                                                                 .map_type = BPF_MAP_TYPE_HASH,
                                                                 .key_size = sizeof(int),
                                                                 .value_size = sizeof(int),
                                                                 .max_entries = 10,
                                                             }));

    CheckMapInfo();
  }

  void CheckMapInfo() { CheckMapInfo(map_fd_); }

  void CheckMapInfo(int map_fd) {
    struct bpf_map_info map_info;
    EXPECT_EQ(bpf(BPF_OBJ_GET_INFO_BY_FD,
                  (union bpf_attr){
                      .info =
                          {
                              .bpf_fd = (unsigned)map_fd_,
                              .info_len = sizeof(map_info),
                              .info = (uintptr_t)&map_info,
                          },
                  }),
              0)
        << strerror(errno);
    EXPECT_EQ(map_info.type, BPF_MAP_TYPE_HASH);
    EXPECT_EQ(map_info.key_size, sizeof(int));
    EXPECT_EQ(map_info.value_size, sizeof(int));
    EXPECT_EQ(map_info.max_entries, 10u);
    EXPECT_EQ(map_info.map_flags, 0u);
  }

  void Pin(int fd, const char* pin_path) {
    unlink(pin_path);
    ASSERT_EQ(bpf(BPF_OBJ_PIN,
                  (union bpf_attr){
                      .pathname = reinterpret_cast<uintptr_t>(pin_path),
                      .bpf_fd = static_cast<unsigned>(fd),
                  }),
              0)
        << strerror(errno);
  }

  int BpfGetFdMapId(const int fd) {
    struct bpf_map_info info = {};
    union bpf_attr attr = {.info = {
                               .bpf_fd = static_cast<uint32_t>(fd),
                               .info_len = sizeof(info),
                               .info = reinterpret_cast<uint64_t>(&info),
                           }};
    int rv = bpf(BPF_OBJ_GET_INFO_BY_FD, attr);
    if (rv)
      return rv;
    if (attr.info.info_len < offsetof(bpf_map_info, id) + sizeof(info.id)) {
      errno = EOPNOTSUPP;
      return -1;
    };
    return info.id;
  }

  int BpfLock(int fd, short type) {
    if (fd < 0)
      return fd;  // pass any errors straight through
    int mapId = BpfGetFdMapId(fd);
    if (mapId <= 0) {
      EXPECT_GT(mapId, 0);
      return -1;
    }

    struct flock64 fl = {
        .l_type = type,        // short: F_{RD,WR,UN}LCK
        .l_whence = SEEK_SET,  // short: SEEK_{SET,CUR,END}
        .l_start = mapId,      // off_t: start offset
        .l_len = 1,            // off_t: number of bytes
    };

    int ret = fcntl(fd, F_OFD_SETLK, &fl);
    if (!ret)
      return fd;  // success
    close(fd);
    return ret;  // most likely -1 with errno == EAGAIN, due to already held lock
  }

  int BpfFdGet(const char* pathname, uint32_t flag) {
    return bpf(BPF_OBJ_GET, {
                                .pathname = reinterpret_cast<uint64_t>(pathname),
                                .file_flags = flag,
                            });
  }

  int MapRetrieveLocklessRW(const char* pathname) { return BpfFdGet(pathname, 0); }

  int MapRetrieveExclusiveRW(const char* pathname) {
    return BpfLock(MapRetrieveLocklessRW(pathname), F_WRLCK);
  }

  int MapRetrieveRW(const char* pathname) {
    return BpfLock(MapRetrieveLocklessRW(pathname), F_RDLCK);
  }

  int MapRetrieveRO(const char* pathname) { return BpfFdGet(pathname, BPF_F_RDONLY); }

  // WARNING: it's impossible to grab a shared (ie. read) lock on a write-only fd,
  // so we instead choose to grab an exclusive (ie. write) lock.
  int MapRetrieveWO(const char* pathname) {
    return BpfLock(BpfFdGet(pathname, BPF_F_WRONLY), F_WRLCK);
  }

  int map_fd() const { return map_fd_; }
  int array_fd() const { return array_fd_; }

 private:
  int array_fd_ = -1;
  int map_fd_ = -1;
};

TEST_F(BpfMapTest, Map) {
  EXPECT_EQ(bpf(BPF_MAP_UPDATE_ELEM,
                (union bpf_attr){
                    .map_fd = (unsigned)map_fd(),
                    .key = (uintptr_t)(int[]){1},
                    .value = (uintptr_t)(int[]){2},
                }),
            0)
      << strerror(errno);
  EXPECT_EQ(bpf(BPF_MAP_UPDATE_ELEM,
                (union bpf_attr){
                    .map_fd = (unsigned)map_fd(),
                    .key = (uintptr_t)(int[]){2},
                    .value = (uintptr_t)(int[]){3},
                }),
            0)
      << strerror(errno);

  std::vector<int> keys;
  int next_key;
  int* last_key = nullptr;
  for (;;) {
    int err = bpf(BPF_MAP_GET_NEXT_KEY, (union bpf_attr){
                                            .map_fd = (unsigned)map_fd(),
                                            .key = (uintptr_t)last_key,
                                            .next_key = (uintptr_t)&next_key,
                                        });
    if (err < 0 && errno == ENOENT)
      break;
    ASSERT_GE(err, 0) << strerror(errno);
    keys.push_back(next_key);
    last_key = &next_key;
  }
  std::sort(keys.begin(), keys.end());
  EXPECT_EQ(keys.size(), 2u);
  EXPECT_EQ(keys[0], 1);
  EXPECT_EQ(keys[1], 2);

  // BPF_MAP_LOOKUP_ELEM is not yet implemented

  CheckMapInfo();
}

TEST_F(BpfMapTest, PinMap) {
  const char* pin_path = "/sys/fs/bpf/foo";

  unlink(pin_path);
  ASSERT_EQ(bpf(BPF_OBJ_PIN,
                (union bpf_attr){
                    .pathname = (uintptr_t)pin_path,
                    .bpf_fd = (unsigned)map_fd(),
                }),
            0)
      << strerror(errno);
  EXPECT_EQ(access(pin_path, F_OK), 0) << strerror(errno);

  EXPECT_EQ(close(map_fd()), 0);
  int map_fd = bpf(BPF_OBJ_GET, (union bpf_attr){.pathname = (uintptr_t)pin_path});
  ASSERT_GE(map_fd, 0) << strerror(errno);
  CheckMapInfo(map_fd);
}

TEST_F(BpfMapTest, LockTest) {
  const char* m1 = "/sys/fs/bpf/array";
  const char* m2 = "/sys/fs/bpf/map";

  Pin(array_fd(), m1);
  Pin(map_fd(), m2);

  fbl::unique_fd fd0(MapRetrieveExclusiveRW(m1));
  ASSERT_TRUE(fd0.is_valid());
  fbl::unique_fd fd1(MapRetrieveExclusiveRW(m2));
  ASSERT_TRUE(fd1.is_valid());  // no conflict with fd0
  fbl::unique_fd fd2(MapRetrieveExclusiveRW(m2));
  ASSERT_FALSE(fd2.is_valid());  // busy due to fd1
  fbl::unique_fd fd3(MapRetrieveRO(m2));
  ASSERT_TRUE(fd3.is_valid());  // no lock taken
  fbl::unique_fd fd4(MapRetrieveRW(m2));
  ASSERT_FALSE(fd4.is_valid());  // busy due to fd1
  fd1.reset();                   // releases exclusive lock
  fbl::unique_fd fd5(MapRetrieveRO(m2));
  ASSERT_TRUE(fd5.is_valid());  // no lock taken
  fbl::unique_fd fd6(MapRetrieveRW(m2));
  ASSERT_TRUE(fd6.is_valid());  // now ok
  fbl::unique_fd fd7(MapRetrieveRO(m2));
  ASSERT_TRUE(fd7.is_valid());  // no lock taken
  fbl::unique_fd fd8(MapRetrieveExclusiveRW(m2));
  ASSERT_FALSE(fd8.is_valid());  // busy due to fd6
}

}  // namespace
