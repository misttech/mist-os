// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "third_party/android/platform/bionic/libc/kernel/uapi/linux/bpf.h"

#include <fcntl.h>
#include <netinet/in.h>
#include <sys/epoll.h>
#include <sys/file.h>
#include <sys/mman.h>
#include <sys/mount.h>
#include <sys/socket.h>
#include <sys/types.h>
#include <syscall.h>
#include <unistd.h>

#include <algorithm>
#include <atomic>
#include <climits>
#include <vector>

#include <fbl/unique_fd.h>
#include <gtest/gtest.h>

#include "src/starnix/tests/syscalls/cpp/test_helper.h"

#define BPF_LOAD_MAP(reg, value)                               \
  bpf_insn{                                                    \
      .code = BPF_LD | BPF_DW,                                 \
      .dst_reg = reg,                                          \
      .src_reg = 1,                                            \
      .off = 0,                                                \
      .imm = static_cast<int32_t>(value),                      \
  },                                                           \
      bpf_insn {                                               \
    .code = 0, .dst_reg = 0, .src_reg = 0, .off = 0, .imm = 0, \
  }

#define BPF_LOAD_OFFSET(dst, src, offset)                                                       \
  bpf_insn {                                                                                    \
    .code = BPF_LDX | BPF_MEM | BPF_W, .dst_reg = dst, .src_reg = src, .off = offset, .imm = 0, \
  }

#define BPF_MOV_IMM(reg, value)                                                    \
  bpf_insn {                                                                       \
    .code = BPF_ALU64 | BPF_MOV | BPF_IMM, .dst_reg = reg, .src_reg = 0, .off = 0, \
    .imm = static_cast<int32_t>(value),                                            \
  }

#define BPF_MOV_REG(dst, src)                                                                \
  bpf_insn {                                                                                 \
    .code = BPF_ALU64 | BPF_MOV | BPF_X, .dst_reg = dst, .src_reg = src, .off = 0, .imm = 0, \
  }

#define BPF_ADD_IMM(reg, value)                                                  \
  bpf_insn {                                                                     \
    .code = BPF_ALU64 | BPF_ADD | BPF_K, .dst_reg = reg, .src_reg = 0, .off = 0, \
    .imm = static_cast<int32_t>(value),                                          \
  }

#define BPF_CALL_EXTERNAL(function)                                   \
  bpf_insn {                                                          \
    .code = BPF_JMP | BPF_CALL, .dst_reg = 0, .src_reg = 0, .off = 0, \
    .imm = static_cast<int32_t>(function),                            \
  }

#define BPF_STORE_B(ptr, value)                                               \
  bpf_insn {                                                                  \
    .code = BPF_ST | BPF_B | BPF_MEM, .dst_reg = ptr, .src_reg = 0, .off = 0, \
    .imm = static_cast<int32_t>(value),                                       \
  }

#define BPF_STORE_W(ptr, value)                                               \
  bpf_insn {                                                                  \
    .code = BPF_ST | BPF_W | BPF_MEM, .dst_reg = ptr, .src_reg = 0, .off = 0, \
    .imm = static_cast<int32_t>(value),                                       \
  }

#define BPF_RETURN() \
  bpf_insn { .code = BPF_JMP | BPF_EXIT, .dst_reg = 0, .src_reg = 0, .off = 0, .imm = 0, }

#define BPF_JNE_IMM(dst, value, offset)                                             \
  bpf_insn {                                                                        \
    .code = BPF_JMP | BPF_JNE | BPF_K, .dst_reg = dst, .src_reg = 0, .off = offset, \
    .imm = static_cast<int32_t>(value),                                             \
  }

#define BPF_JLT_REG(dst, src, offset)                                                           \
  bpf_insn {                                                                                    \
    .code = BPF_JMP | BPF_JLT | BPF_X, .dst_reg = dst, .src_reg = src, .off = offset, .imm = 0, \
  }

namespace {

int bpf(int cmd, union bpf_attr attr) { return (int)syscall(__NR_bpf, cmd, &attr, sizeof(attr)); }

void TestMapCreationFail(uint32_t type, uint32_t key_size, uint32_t value_size,
                         uint32_t max_entries, int expected_errno) {
  int result = bpf(BPF_MAP_CREATE, (union bpf_attr){
                                       .map_type = type,
                                       .key_size = key_size,
                                       .value_size = value_size,
                                       .max_entries = max_entries,
                                   });
  EXPECT_EQ(result, -1);
  // TODO(https://fxbug.dev/317285180) don't skip on baseline
  if (errno == EPERM) {
    GTEST_SKIP() << "Permission denied.";
  }
  EXPECT_EQ(errno, expected_errno);
}

TEST(BpfTest, ArraySizeOverflow) {
  TestMapCreationFail(BPF_MAP_TYPE_ARRAY, sizeof(int), 1024, INT_MAX / 8, ENOMEM);
}

TEST(BpfTest, ArraySizeZero) {
  TestMapCreationFail(BPF_MAP_TYPE_ARRAY, sizeof(int), 1024, 0, EINVAL);
}

TEST(BpfTest, HashMapSizeZero) { TestMapCreationFail(BPF_MAP_TYPE_HASH, 1, 1024, 0, EINVAL); }

TEST(BpfTest, HashMapZeroKeySize) { TestMapCreationFail(BPF_MAP_TYPE_HASH, 0, 1024, 10, EINVAL); }

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
    ringbuf_fd_ = SAFE_SYSCALL_SKIP_ON_EPERM(
        bpf(BPF_MAP_CREATE, (union bpf_attr){
                                .map_type = BPF_MAP_TYPE_RINGBUF,
                                .key_size = 0,
                                .value_size = 0,
                                .max_entries = static_cast<uint32_t>(getpagesize()),
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

  // Run the given bpf program.
  void Run(const bpf_insn* program, size_t len) {
    char buffer[4096];
    union bpf_attr attr;
    memset(&attr, 0, sizeof(attr));
    attr.prog_type = BPF_PROG_TYPE_SOCKET_FILTER;
    attr.insns = reinterpret_cast<uint64_t>(program);
    attr.insn_cnt = static_cast<uint32_t>(len);
    attr.license = reinterpret_cast<uint64_t>("N/A");
    attr.log_buf = reinterpret_cast<uint64_t>(buffer);
    attr.log_size = 4096;
    attr.log_level = 1;

    fbl::unique_fd prog_fd(SAFE_SYSCALL(bpf(BPF_PROG_LOAD, attr)));
    int sk[2];
    SAFE_SYSCALL(socketpair(AF_UNIX, SOCK_DGRAM, 0, sk));
    fbl::unique_fd sk0(sk[0]);
    fbl::unique_fd sk1(sk[1]);
    int fd = prog_fd.get();
    SAFE_SYSCALL(setsockopt(sk1.get(), SOL_SOCKET, SO_ATTACH_BPF, &fd, sizeof(int)));
    SAFE_SYSCALL(write(sk0.get(), "", 1));
  }

  void WriteToRingBuffer(char v, uint32_t submit_flag = 0) {
    // A bpf program that write a record in the ringbuffer.
    bpf_insn program[] = {
        // r1 <- ringbuf_fd_
        BPF_LOAD_MAP(1, ringbuf_fd()),
        // r2 <- 1
        BPF_MOV_IMM(2, 1),
        // r3 <- 0
        BPF_MOV_IMM(3, 0),
        // Call bpf_ringbuf_reserve
        BPF_CALL_EXTERNAL(BPF_FUNC_ringbuf_reserve),
        // r0 != 0 -> JMP 1
        BPF_JNE_IMM(0, 0, 1),
        // exit
        BPF_RETURN(),
        // *r0 = `v`
        BPF_STORE_B(0, v),
        // r1 <- r0,
        BPF_MOV_REG(1, 0),
        // r2 <- `submit_flag`,
        BPF_MOV_IMM(2, submit_flag),
        // Call bpf_ringbuf_submit
        BPF_CALL_EXTERNAL(BPF_FUNC_ringbuf_submit),
        // r0 <- 0,
        BPF_MOV_IMM(0, 0),
        // exit
        BPF_RETURN(),
    };
    Run(program, sizeof(program) / sizeof(program[0]));
  }

  // Writes a new message with the specified `size`, which must be a multiple
  // of 4. `v` is written throughout the message.
  void WriteToRingBufferLarge(uint32_t v, size_t size, uint32_t submit_flag = 0) {
    ASSERT_TRUE(size % 4 == 0);

    // A bpf program that write a record in the ringbuffer.
    bpf_insn program[] = {
        // r1 <- ringbuf_fd_
        BPF_LOAD_MAP(1, ringbuf_fd()),
        // r2 <- `size`
        BPF_MOV_IMM(2, size),
        // r3 <- 0
        BPF_MOV_IMM(3, 0),
        // Call bpf_ringbuf_reserve
        BPF_CALL_EXTERNAL(BPF_FUNC_ringbuf_reserve),
        // r0 != 0 -> JMP 1
        BPF_JNE_IMM(0, 0, 1),
        // exit
        BPF_RETURN(),
        // r1 <- r0,
        BPF_MOV_REG(1, 0),
        // r2 <- r0,
        BPF_MOV_REG(2, 0),
        // r2 <- r2 + `size`
        BPF_ADD_IMM(2, size),
        // *r0 = `v`
        BPF_STORE_W(0, v),
        // r0 <- r0 + 4
        BPF_ADD_IMM(0, 4),
        // r0 < r2 -> JMP -3
        BPF_JLT_REG(0, 2, -3),
        // r2 <- `submit_flag`,
        BPF_MOV_IMM(2, submit_flag),
        // Call bpf_ringbuf_submit
        BPF_CALL_EXTERNAL(BPF_FUNC_ringbuf_submit),
        // r0 <- 0,
        BPF_MOV_IMM(0, 0),
        // exit
        BPF_RETURN(),
    };
    Run(program, sizeof(program) / sizeof(program[0]));
  }

  void DiscardWriteToRingBuffer() {
    // A bpf program that cancel a write the ringbuffer.
    bpf_insn program[] = {
        // r1 <- ringbuf_fd_
        BPF_LOAD_MAP(1, ringbuf_fd()),
        // r2 <- 1
        BPF_MOV_IMM(2, 1),
        // r3 <- 0
        BPF_MOV_IMM(3, 0),
        // Call bpf_ringbuf_reserve
        BPF_CALL_EXTERNAL(BPF_FUNC_ringbuf_reserve),
        // r0 != 0 -> JMP 1
        BPF_JNE_IMM(0, 0, 1),
        // exit
        BPF_RETURN(),
        // r1 <- r0,
        BPF_MOV_REG(1, 0),
        // r2 <- 0,
        BPF_MOV_IMM(2, 0),
        // Call bpf_ringbuf_discard
        BPF_CALL_EXTERNAL(BPF_FUNC_ringbuf_discard),
        // r0 <- 0,
        BPF_MOV_IMM(0, 0),
        // exit
        BPF_RETURN(),
    };
    Run(program, sizeof(program) / sizeof(program[0]));
  }

  int array_fd() const { return array_fd_; }
  int map_fd() const { return map_fd_; }
  int ringbuf_fd() const { return ringbuf_fd_; }

 private:
  int array_fd_ = -1;
  int map_fd_ = -1;
  int ringbuf_fd_ = -1;
};  // namespace

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

TEST_F(BpfMapTest, ArrayEpoll) {
  fbl::unique_fd epollfd(SAFE_SYSCALL(epoll_create(1)));

  struct epoll_event ev;
  ev.events = EPOLLIN | EPOLLET;

  SAFE_SYSCALL(epoll_ctl(epollfd.get(), EPOLL_CTL_ADD, array_fd(), &ev));
  ASSERT_EQ(epoll_wait(epollfd.get(), &ev, 1, 0), 1);
  ASSERT_EQ(ev.events, EPOLLERR);
}

TEST_F(BpfMapTest, ArraySelect) {
  {
    fd_set readfds = {};
    fd_set writefds = {};
    FD_SET(array_fd(), &readfds);
    ASSERT_EQ(select(FD_SETSIZE, &readfds, &writefds, nullptr, nullptr), 1);
    ASSERT_TRUE(FD_ISSET(array_fd(), &readfds));
    ASSERT_FALSE(FD_ISSET(array_fd(), &writefds));
  }

  {
    fd_set readfds = {};
    fd_set writefds = {};
    FD_SET(array_fd(), &readfds);
    FD_SET(array_fd(), &writefds);
    ASSERT_EQ(select(FD_SETSIZE, &readfds, &writefds, nullptr, nullptr), 2);
    ASSERT_TRUE(FD_ISSET(array_fd(), &readfds));
    ASSERT_TRUE(FD_ISSET(array_fd(), &writefds));
  }
}

TEST_F(BpfMapTest, HashMapEpoll) {
  fbl::unique_fd epollfd(SAFE_SYSCALL(epoll_create(1)));

  struct epoll_event ev;
  ev.events = EPOLLIN | EPOLLET;

  SAFE_SYSCALL(epoll_ctl(epollfd.get(), EPOLL_CTL_ADD, map_fd(), &ev));
  ASSERT_EQ(epoll_wait(epollfd.get(), &ev, 1, 0), 1);
  ASSERT_EQ(ev.events, EPOLLERR);
}

TEST_F(BpfMapTest, MMapRingBufTest) {
  // Can map the first page of the ringbuffer R/W
  ASSERT_TRUE(test_helper::ScopedMMap::MMap(nullptr, getpagesize(), PROT_READ | PROT_WRITE,
                                            MAP_SHARED, ringbuf_fd(), 0)
                  .is_ok());
  // Cannot mmap the second page of the ringbuffer R/W
  ASSERT_EQ(test_helper::ScopedMMap::MMap(nullptr, getpagesize(), PROT_READ | PROT_WRITE,
                                          MAP_SHARED, ringbuf_fd(), getpagesize())
                .error_value(),
            EPERM);
  // Cannot mmap the second page, 3rd and 4th page RO
  for (int i = 0; i < 3; ++i) {
    ASSERT_TRUE(test_helper::ScopedMMap::MMap(nullptr, getpagesize(), PROT_READ, MAP_SHARED,
                                              ringbuf_fd(), (i + 1) * getpagesize())
                    .is_ok());
  }
  // Can mmap the 4 pages in a single mapping.
  ASSERT_TRUE(test_helper::ScopedMMap::MMap(nullptr, 4 * getpagesize(), PROT_READ, MAP_SHARED,
                                            ringbuf_fd(), 0)
                  .is_ok());
  // Cannot mmap 5 pages.
  ASSERT_EQ(test_helper::ScopedMMap::MMap(nullptr, 5 * getpagesize(), PROT_READ, MAP_SHARED,
                                          ringbuf_fd(), 0)
                .error_value(),
            EINVAL);
}

TEST_F(BpfMapTest, WriteRingBufTest) {
  auto pagewr = ASSERT_RESULT_SUCCESS_AND_RETURN(test_helper::ScopedMMap::MMap(
      nullptr, getpagesize(), PROT_READ | PROT_WRITE, MAP_SHARED, ringbuf_fd(), 0));
  auto pagero = ASSERT_RESULT_SUCCESS_AND_RETURN(test_helper::ScopedMMap::MMap(
      nullptr, 3 * getpagesize(), PROT_READ, MAP_SHARED, ringbuf_fd(), getpagesize()));
  std::atomic<unsigned long>* consumer_pos =
      static_cast<std::atomic<unsigned long>*>(pagewr.mapping());
  std::atomic<unsigned long>* producer_pos =
      static_cast<std::atomic<unsigned long>*>(pagero.mapping());
  uint8_t* data = static_cast<uint8_t*>(pagero.mapping()) + getpagesize();
  ASSERT_EQ(0u, consumer_pos->load(std::memory_order_acquire));
  ASSERT_EQ(0u, producer_pos->load(std::memory_order_acquire));

  WriteToRingBuffer(42);

  ASSERT_EQ(0u, consumer_pos->load(std::memory_order_acquire));
  ASSERT_EQ(16u, producer_pos->load(std::memory_order_acquire));

  uint32_t record_length = *reinterpret_cast<uint32_t*>(data);
  ASSERT_EQ(0u, record_length & BPF_RINGBUF_BUSY_BIT);
  ASSERT_EQ(0u, record_length & BPF_RINGBUF_DISCARD_BIT);
  ASSERT_EQ(1u, record_length);

  uint32_t page_offset = *reinterpret_cast<uint32_t*>(data + 4);
  ASSERT_EQ(3u, page_offset);

  uint8_t record_value = *(data + 8);
  ASSERT_EQ(42u, record_value);

  DiscardWriteToRingBuffer();

  ASSERT_EQ(0u, consumer_pos->load(std::memory_order_acquire));
  ASSERT_EQ(32u, producer_pos->load(std::memory_order_acquire));

  record_length = *reinterpret_cast<uint32_t*>(data + 16);
  ASSERT_EQ(0u, record_length & BPF_RINGBUF_BUSY_BIT);
  ASSERT_EQ(BPF_RINGBUF_DISCARD_BIT, record_length & BPF_RINGBUF_DISCARD_BIT);
}

TEST_F(BpfMapTest, IdenticalPagesRingBufTest) {
  // Map the last 2 pages, and check that they are the same after some operations on the buffer.
  auto pages = ASSERT_RESULT_SUCCESS_AND_RETURN(test_helper::ScopedMMap::MMap(
      nullptr, 2 * getpagesize(), PROT_READ, MAP_SHARED, ringbuf_fd(), 2 * getpagesize()));

  uint8_t* page1 = static_cast<uint8_t*>(pages.mapping());
  uint8_t* page2 = page1 + getpagesize();

  // Check that the pages are equal after creating the buffer.
  ASSERT_EQ(memcmp(page1, page2, getpagesize()), 0);

  for (size_t i = 0; i < 256; ++i) {
    WriteToRingBuffer(static_cast<uint8_t>(i));
  }

  // Check that they are still equals after some operations.
  ASSERT_EQ(memcmp(page1, page2, getpagesize()), 0);
}

TEST_F(BpfMapTest, RingBufferWrapAround) {
  auto pagewr = ASSERT_RESULT_SUCCESS_AND_RETURN(test_helper::ScopedMMap::MMap(
      nullptr, getpagesize(), PROT_READ | PROT_WRITE, MAP_SHARED, ringbuf_fd(), 0));
  auto pagero = ASSERT_RESULT_SUCCESS_AND_RETURN(test_helper::ScopedMMap::MMap(
      nullptr, 3 * getpagesize(), PROT_READ, MAP_SHARED, ringbuf_fd(), getpagesize()));
  std::atomic<uint32_t>* consumer_pos = static_cast<std::atomic<uint32_t>*>(pagewr.mapping());
  std::atomic<uint32_t>* producer_pos = static_cast<std::atomic<uint32_t>*>(pagero.mapping());
  uint8_t* data = static_cast<uint8_t*>(pagero.mapping()) + getpagesize();
  ASSERT_EQ(0u, consumer_pos->load(std::memory_order_acquire));
  ASSERT_EQ(0u, producer_pos->load(std::memory_order_acquire));

  // Write first message that's 3016 bytes long.
  size_t msg_size = 3016;
  uint32_t value = 0x6143abcd;
  WriteToRingBufferLarge(value, msg_size);
  ASSERT_EQ(3024u, producer_pos->load(std::memory_order_acquire));
  uint32_t* msg_ptr = reinterpret_cast<uint32_t*>(data + 8);
  for (size_t i = 0; i < msg_size / 4; ++i) {
    ASSERT_EQ(msg_ptr[i], value);
  }

  // Consume the message.
  uint32_t pos = producer_pos->load(std::memory_order_acquire);
  consumer_pos->store(pos, std::memory_order_release);

  // Write a second message that wraps around to the head of the buffer.
  uint32_t value2 = 0xc23414f2;
  size_t msg_size2 = 2072;
  WriteToRingBufferLarge(value2, msg_size2);

  EXPECT_GT(producer_pos->load(std::memory_order_acquire), static_cast<uint32_t>(getpagesize()));
  msg_ptr = reinterpret_cast<uint32_t*>(data + pos + 8);
  for (size_t i = 0; i < msg_size2 / 4; ++i) {
    ASSERT_EQ(msg_ptr[i], value2);
  }
}

TEST_F(BpfMapTest, NotificationsRingBufTest) {
  auto pagewr = ASSERT_RESULT_SUCCESS_AND_RETURN(test_helper::ScopedMMap::MMap(
      nullptr, getpagesize(), PROT_READ | PROT_WRITE, MAP_SHARED, ringbuf_fd(), 0));
  auto pagero = ASSERT_RESULT_SUCCESS_AND_RETURN(test_helper::ScopedMMap::MMap(
      nullptr, 3 * getpagesize(), PROT_READ, MAP_SHARED, ringbuf_fd(), getpagesize()));
  std::atomic<unsigned long>* consumer_pos =
      static_cast<std::atomic<unsigned long>*>(pagewr.mapping());
  std::atomic<unsigned long>* producer_pos =
      static_cast<std::atomic<unsigned long>*>(pagero.mapping());

  fbl::unique_fd epollfd(SAFE_SYSCALL(epoll_create(1)));

  struct epoll_event ev;
  ev.events = EPOLLIN | EPOLLET;

  SAFE_SYSCALL(epoll_ctl(epollfd.get(), EPOLL_CTL_ADD, ringbuf_fd(), &ev));

  // First wait should return no result.
  ASSERT_EQ(0, epoll_wait(epollfd.get(), &ev, 1, 0));

  // After a normal write, epoll should return an event.
  WriteToRingBuffer(42);
  ASSERT_EQ(1, epoll_wait(epollfd.get(), &ev, 1, 0));

  // But only once, as this is edge triggered.
  ASSERT_EQ(0, epoll_wait(epollfd.get(), &ev, 1, 0));

  // A new write will not trigger an event, as the reader is late.
  WriteToRingBuffer(42);
  ASSERT_EQ(0, epoll_wait(epollfd.get(), &ev, 1, 0));

  // Unless the event is forced through flags
  WriteToRingBuffer(42, BPF_RB_FORCE_WAKEUP);
  ASSERT_EQ(1, epoll_wait(epollfd.get(), &ev, 1, 0));

  // Let's catch up.
  consumer_pos->store(producer_pos->load(std::memory_order_acquire), std::memory_order_release);

  // Execute a write, preventing an event to run.
  WriteToRingBuffer(42, BPF_RB_NO_WAKEUP);
  ASSERT_EQ(0, epoll_wait(epollfd.get(), &ev, 1, 0));

  // A normal write will now not send an event because the client has not caught
  // up.
  WriteToRingBuffer(42);
  ASSERT_EQ(0, epoll_wait(epollfd.get(), &ev, 1, 0));

  // Let's catch up again.
  consumer_pos->store(producer_pos->load(std::memory_order_acquire), std::memory_order_release);

  // A normal write will now send an event.
  WriteToRingBuffer(42);
  EXPECT_EQ(1, epoll_wait(epollfd.get(), &ev, 1, 0));
}

class BpfCgroupTest : public testing::Test {
 protected:
  const uint16_t BLOCKED_PORT = 1236;

  void SetUp() override {
    ASSERT_FALSE(temp_dir_.path().empty());
    int mount_result = mount(nullptr, temp_dir_.path().c_str(), "cgroup2", 0, nullptr);
    if (mount_result == -1 && errno == EPERM) {
      GTEST_SKIP() << "Can't mount cgroup2.";
    }
    ASSERT_EQ(mount_result, 0);

    root_cgroup_.reset(open(temp_dir_.path().c_str(), O_RDONLY));
    assert(root_cgroup_);
  }

  fbl::unique_fd LoadProgram(const bpf_insn* program, size_t len, uint32_t expected_attach_type) {
    char buffer[4096];
    union bpf_attr attr;
    memset(&attr, 0, sizeof(attr));
    attr.prog_type = BPF_PROG_TYPE_CGROUP_SOCK_ADDR;
    attr.expected_attach_type = expected_attach_type;
    attr.insns = reinterpret_cast<uint64_t>(program);
    attr.insn_cnt = static_cast<uint32_t>(len);
    attr.license = reinterpret_cast<uint64_t>("N/A");
    attr.log_buf = reinterpret_cast<uint64_t>(buffer);
    attr.log_size = 4096;
    attr.log_level = 1;

    return fbl::unique_fd(bpf(BPF_PROG_LOAD, attr));
  }

  fbl::unique_fd LoadBlockPortProgram(uint32_t expected_attach_type) {
    // A bpf program that blocks bind on 42.
    bpf_insn program[] = {
        // r0 <- [r1+24] (bpf_sock_addr.user_port)
        BPF_LOAD_OFFSET(0, 1, offsetof(bpf_sock_addr, user_port)),
        // r0 != BLOCKED_PORT -> JMP 2
        BPF_JNE_IMM(0, htons(BLOCKED_PORT), 2),

        // r0 <- 0
        BPF_MOV_IMM(0, 0),
        // exit
        BPF_RETURN(),

        // r0 <- 1
        BPF_MOV_IMM(0, 1),
        // exit
        BPF_RETURN(),
    };

    return LoadProgram(program, sizeof(program) / sizeof(program[0]), expected_attach_type);
  }

  void AttachToRootCgroup(uint32_t attach_type, int prog_fd) {
    bpf_attr attr;
    memset(&attr, 0, sizeof(attr));
    attr.target_fd = root_cgroup_.get();
    attr.attach_bpf_fd = prog_fd;
    attr.attach_type = attach_type;
    ASSERT_EQ(bpf(BPF_PROG_ATTACH, attr), 0) << " errno: " << errno;
  }

  int TryDetachFromRootCgroup(uint32_t attach_type) {
    bpf_attr attr;
    memset(&attr, 0, sizeof(attr));
    attr.target_fd = root_cgroup_.get();
    attr.attach_type = attach_type;
    return bpf(BPF_PROG_DETACH, attr);
  }

  void DetachFromRootCgroup(uint32_t attach_type) {
    ASSERT_EQ(TryDetachFromRootCgroup(attach_type), 0) << " errno: " << errno;
  }

  testing::AssertionResult TryBind(uint16_t port, int expected_errno) {
    fbl::unique_fd sock(socket(AF_INET, SOCK_DGRAM, IPPROTO_UDP));
    if (!sock) {
      return testing::AssertionFailure() << "socket failed: " << strerror(errno);
    }
    sockaddr_in addr = {
        .sin_family = AF_INET,
        .sin_port = htons(port),
        .sin_addr =
            {
                .s_addr = htonl(INADDR_LOOPBACK),
            },
    };
    int r = bind(sock.get(), reinterpret_cast<sockaddr*>(&addr), sizeof(addr));
    if (expected_errno) {
      if (r != -1) {
        return testing::AssertionFailure() << "bind succeeded when it expected to fail";
      }
      if (errno != expected_errno) {
        return testing::AssertionFailure() << "bind failed with an invalid errno=" << errno
                                           << ", expected errno=" << expected_errno;
      }
    } else if (r != 0) {
      return testing::AssertionFailure() << "bind failed: " << strerror(errno);
    }
    return testing::AssertionSuccess();
  }

  testing::AssertionResult TryConnect(uint16_t port, int expected_errno) {
    fbl::unique_fd sock(socket(AF_INET, SOCK_DGRAM, IPPROTO_UDP));
    if (!sock) {
      return testing::AssertionFailure() << "socket failed: " << strerror(errno);
    }

    sockaddr_in addr = {
        .sin_family = AF_INET,
        .sin_port = htons(port),
        .sin_addr =
            {
                .s_addr = htonl(INADDR_LOOPBACK),
            },
    };
    int r = connect(sock.get(), reinterpret_cast<sockaddr*>(&addr), sizeof(addr));
    if (expected_errno) {
      if (r != -1) {
        return testing::AssertionFailure() << "connect succeeded when it expected to fail";
      }
      if (errno != expected_errno) {
        return testing::AssertionFailure() << "connect failed with an invalid errno=" << errno
                                           << ", expected errno=" << expected_errno;
      }
    } else if (r != 0) {
      return testing::AssertionFailure() << "connect failed: " << strerror(errno);
    }
    return testing::AssertionSuccess();
  }

 protected:
  test_helper::ScopedTempDir temp_dir_;
  fbl::unique_fd root_cgroup_;
};

TEST_F(BpfCgroupTest, BlockBind) {
  ASSERT_TRUE(TryBind(BLOCKED_PORT, 0));

  auto prog = LoadBlockPortProgram(BPF_CGROUP_INET4_BIND);

  AttachToRootCgroup(BPF_CGROUP_INET4_BIND, prog.get());

  // The port should be blocked now.
  ASSERT_TRUE(TryBind(BLOCKED_PORT, EPERM));

  // Other ports are not blocked.
  ASSERT_TRUE(TryBind(BLOCKED_PORT + 1, 0));

  DetachFromRootCgroup(BPF_CGROUP_INET4_BIND);

  // Repeated attempt to detach the program should fail.
  EXPECT_EQ(TryDetachFromRootCgroup(BPF_CGROUP_INET4_BIND), -1);
  EXPECT_EQ(errno, ENOENT);

  // Should be unblocked now.
  ASSERT_TRUE(TryBind(BLOCKED_PORT, 0));
}

TEST_F(BpfCgroupTest, BlockConnect) {
  ASSERT_TRUE(TryConnect(BLOCKED_PORT, 0));

  auto prog = LoadBlockPortProgram(BPF_CGROUP_INET4_CONNECT);

  AttachToRootCgroup(BPF_CGROUP_INET4_CONNECT, prog.get());

  // The port should be blocked now.
  ASSERT_TRUE(TryConnect(BLOCKED_PORT, EPERM));

  // Other ports are not blocked.
  ASSERT_TRUE(TryConnect(BLOCKED_PORT + 1, 0));

  DetachFromRootCgroup(BPF_CGROUP_INET4_CONNECT);

  // Should be unblocked now.
  ASSERT_TRUE(TryConnect(BLOCKED_PORT, 0));
}

// Checks that epoll is handled properly for program FDs.
TEST_F(BpfCgroupTest, ProgFdEpoll) {
  auto prog = LoadBlockPortProgram(BPF_CGROUP_INET4_CONNECT);

  fbl::unique_fd epollfd(SAFE_SYSCALL(epoll_create(1)));

  struct epoll_event ev;
  ev.events = EPOLLIN;

  ASSERT_EQ(epoll_ctl(epollfd.get(), EPOLL_CTL_ADD, prog.get(), &ev), -1);
  ASSERT_EQ(errno, EPERM);
}

}  // namespace
