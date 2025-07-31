// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <sys/mman.h>
#include <sys/syscall.h>
#include <unistd.h>

#include <fbl/unique_fd.h>
#include <gtest/gtest.h>
#include <linux/io_uring.h>

#include "src/starnix/tests/syscalls/cpp/test_helper.h"

#ifndef IORING_SETUP_COOP_TASKRUN
#define IORING_SETUP_COOP_TASKRUN (1U << 8)
#endif

#ifndef IORING_SETUP_SINGLE_ISSUER
#define IORING_SETUP_SINGLE_ISSUER (1U << 12)
#endif

#ifndef IORING_SETUP_DEFER_TASKRUN
#define IORING_SETUP_DEFER_TASKRUN (1U << 13)
#endif

namespace {

int io_uring_setup(uint32_t entries, io_uring_params* params) {
  return static_cast<int>(syscall(__NR_io_uring_setup, entries, params));
}

int io_uring_enter(int fd, int to_submit, int min_complete, int flags, sigset_t* sigset) {
  return static_cast<int>(syscall(__NR_io_uring_enter, fd, to_submit, min_complete, flags, sigset));
}

TEST(IoUringTest, IoUringReadWrite) {
  test_helper::ScopedTempFD temp_fd;
  ASSERT_TRUE(temp_fd.fd() >= 0);

  struct io_uring_params params = {};
  fbl::unique_fd ring_fd(io_uring_setup(2, &params));
  ASSERT_TRUE(ring_fd.is_valid()) << strerror(errno);

  auto sq_mapping = ASSERT_RESULT_SUCCESS_AND_RETURN(test_helper::ScopedMMap::MMap(
      NULL, params.sq_off.array + params.sq_entries * sizeof(__u32), PROT_READ | PROT_WRITE,
      MAP_SHARED | MAP_POPULATE, ring_fd.get(), IORING_OFF_SQ_RING));
  char* sq_ptr = static_cast<char*>(sq_mapping.mapping());

  auto cqe_mapping = ASSERT_RESULT_SUCCESS_AND_RETURN(test_helper::ScopedMMap::MMap(
      NULL, params.cq_off.cqes + params.cq_entries * sizeof(io_uring_cqe), PROT_READ | PROT_WRITE,
      MAP_SHARED | MAP_POPULATE, ring_fd.get(), IORING_OFF_CQ_RING));
  char* cqe_ptr = static_cast<char*>(cqe_mapping.mapping());

  auto sqe_mapping = ASSERT_RESULT_SUCCESS_AND_RETURN(test_helper::ScopedMMap::MMap(
      NULL, params.sq_entries * sizeof(io_uring_sqe), PROT_READ | PROT_WRITE,
      MAP_SHARED | MAP_POPULATE, ring_fd.get(), IORING_OFF_SQES));
  io_uring_sqe* sqes = reinterpret_cast<io_uring_sqe*>(sqe_mapping.mapping());

  uint32_t* sq_array = reinterpret_cast<uint32_t*>(sq_ptr + params.sq_off.array);
  std::atomic<uint32_t>* sq_tail_ptr =
      reinterpret_cast<std::atomic<uint32_t>*>(sq_ptr + params.sq_off.tail);

  // Write to the file
  char write_data[] = "hello";
  struct iovec write_iov = {.iov_base = write_data, .iov_len = sizeof(write_data)};
  uint32_t tail = sq_tail_ptr->load(std::memory_order_acquire);
  uint32_t write_index = tail & (params.sq_entries - 1);
  sqes[write_index].opcode = IORING_OP_WRITEV;
  sqes[write_index].fd = temp_fd.fd();
  sqes[write_index].addr = (uint64_t)&write_iov;
  sqes[write_index].len = 1;
  sqes[write_index].off = 0;
  sqes[write_index].user_data = 1;
  sq_array[write_index] = write_index;
  sq_tail_ptr->store(tail + 1, std::memory_order_release);

  // Read from the file
  char read_data[sizeof(write_data)];
  struct iovec read_iov = {.iov_base = read_data, .iov_len = sizeof(read_data)};
  tail = sq_tail_ptr->load(std::memory_order_acquire);
  uint32_t read_index = tail & (params.sq_entries - 1);
  sqes[read_index].opcode = IORING_OP_READV;
  sqes[read_index].fd = temp_fd.fd();
  sqes[read_index].addr = (uint64_t)&read_iov;
  sqes[read_index].len = 1;
  sqes[read_index].off = 0;
  sqes[read_index].user_data = 2;
  sq_array[read_index] = read_index;
  sq_tail_ptr->store(tail + 1, std::memory_order_release);

  // Submit and wait for both operations
  ASSERT_EQ(io_uring_enter(ring_fd.get(), 2, 2, IORING_ENTER_GETEVENTS, NULL), 2);

  // Process completions
  std::atomic<uint32_t>* cq_head_ptr =
      reinterpret_cast<std::atomic<uint32_t>*>(cqe_ptr + params.cq_off.head);
  std::atomic<uint32_t>* cq_tail_ptr =
      reinterpret_cast<std::atomic<uint32_t>*>(cqe_ptr + params.cq_off.tail);
  io_uring_cqe* cqes = reinterpret_cast<io_uring_cqe*>(cqe_ptr + params.cq_off.cqes);

  uint32_t head = cq_head_ptr->load(std::memory_order_acquire);
  ASSERT_EQ(cq_tail_ptr->load(std::memory_order_acquire) - head, 2U);

  bool write_done = false;
  bool read_done = false;
  for (int i = 0; i < 2; i++) {
    io_uring_cqe* cqe = &cqes[head & (params.cq_entries - 1)];
    if (cqe->user_data == 1) {
      ASSERT_EQ(cqe->res, static_cast<ssize_t>(sizeof(write_data)));
      write_done = true;
    } else if (cqe->user_data == 2) {
      ASSERT_EQ(cqe->res, static_cast<ssize_t>(sizeof(read_data)));
      read_done = true;
    }
    head++;
  }
  ASSERT_TRUE(write_done);
  ASSERT_TRUE(read_done);
  cq_head_ptr->store(head, std::memory_order_release);

  // Verify
  ASSERT_STREQ(write_data, read_data);
}

TEST(IoUringTest, IoUringSetupCoopTaskrun) {
  if (!test_helper::IsStarnix() && !test_helper::IsKernelVersionAtLeast(5, 19)) {
    GTEST_SKIP() << "Skip test for unsupported feature";
  }
  struct io_uring_params params = {};
  params.flags = IORING_SETUP_COOP_TASKRUN;
  fbl::unique_fd fd(io_uring_setup(1, &params));
  ASSERT_TRUE(fd.is_valid()) << strerror(errno);
}

TEST(IoUringTest, IoUringSetupSingleIssuer) {
  if (!test_helper::IsStarnix() && !test_helper::IsKernelVersionAtLeast(6, 0)) {
    GTEST_SKIP() << "Skip test for unsupported feature";
  }
  struct io_uring_params params = {};
  params.flags = IORING_SETUP_SINGLE_ISSUER;
  fbl::unique_fd fd(io_uring_setup(1, &params));
  ASSERT_TRUE(fd.is_valid()) << strerror(errno);
}

TEST(IoUringTest, IoUringSetupDeferTaskrun) {
  if (!test_helper::IsStarnix() && !test_helper::IsKernelVersionAtLeast(6, 1)) {
    GTEST_SKIP() << "Skip test for unsupported feature";
  }
  struct io_uring_params params = {};
  params.flags = IORING_SETUP_DEFER_TASKRUN;
  fbl::unique_fd fd(io_uring_setup(1, &params));
  ASSERT_FALSE(fd.is_valid());
  ASSERT_EQ(EINVAL, errno);

  params.flags = IORING_SETUP_SINGLE_ISSUER | IORING_SETUP_DEFER_TASKRUN;
  fd = fbl::unique_fd(io_uring_setup(1, &params));
  ASSERT_TRUE(fd.is_valid()) << strerror(errno);
}

}  // namespace
