// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <stdint.h>
#include <stdio.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <time.h>
#include <unistd.h>

#include <gtest/gtest.h>
#include <linux/perf_event.h>

#include "test_helper.h"

namespace {

// From https://man7.org/linux/man-pages/man2/perf_event_open.2.html
struct read_format_data {
  uint64_t value;        /* The value of the event */
  uint64_t time_enabled; /* if PERF_FORMAT_TOTAL_TIME_ENABLED */
  uint64_t time_running; /* if PERF_FORMAT_TOTAL_TIME_RUNNING */
  uint64_t id;           /* if PERF_FORMAT_ID */
  uint64_t lost;         /* if PERF_FORMAT_LOST */
};

// Valid example inputs to use for tests when we don't care about the value.
const int32_t example_pid = 40;
const int32_t example_cpu = 0;
const int example_group_fd = 0;
const long example_flags = 0;
perf_event_attr example_attr = {0, 0, 0, {}, 0, 0};
const int32_t from_nanos = 1000000000;

// Returns an example perf_event_attr where none of the values matter
// except for the read_format, which is passed in.
perf_event_attr attr_with_read_format(uint64_t read_format) {
  return {PERF_TYPE_SOFTWARE, PERF_ATTR_SIZE_VER1, PERF_COUNT_SW_CPU_CLOCK, {}, 0, read_format};
}

// Write wrapper because there isn't one per the man7 page.
long sys_perf_event_open(perf_event_attr* attr, int32_t pid, int32_t cpu, int group_fd,
                         unsigned long flags) {
  return syscall(__NR_perf_event_open, attr, pid, cpu, group_fd, flags);
}

TEST(PerfEventOpenTest, ValidInputsSucceed) {
  // The file descriptor value that is returned is not guaranteed to be a specific number.
  // Just check that's not -1 (error).
  // TODO(https://fxbug.dev/394960158): Change this test when we have something better
  // to test.
  if (test_helper::HasSysAdmin()) {
    EXPECT_NE(sys_perf_event_open(&example_attr, example_pid, example_cpu, example_group_fd,
                                  example_flags),
              -1);
  }
}

TEST(PerfEventOpenTest, InvalidPidAndCpuFails) {
  int32_t pid = -1;  // Invalid
  int32_t cpu = -1;  // Invalid

  if (test_helper::HasSysAdmin()) {
    EXPECT_THAT(sys_perf_event_open(&example_attr, pid, cpu, example_group_fd, example_flags),
                SyscallFailsWithErrno(EINVAL));
  }
}

TEST(PerfEventOpenTest, ReadEventWithTimeEnabledSucceeds) {
  uint64_t read_format = PERF_FORMAT_TOTAL_TIME_ENABLED;
  perf_event_attr attr = attr_with_read_format(read_format);

  if (test_helper::HasSysAdmin()) {
    long file_descriptor =
        sys_perf_event_open(&attr, example_pid, example_cpu, example_group_fd, example_flags);
    EXPECT_NE(file_descriptor, -1);

    // read() on the file descriptor should return the number of bytes written to buffer,
    // and the buffer should contain read_format_data information for that event.
    char buffer[sizeof(read_format_data)];
    uint64_t read_length = syscall(__NR_read, file_descriptor, buffer, sizeof(read_format_data));
    EXPECT_EQ(sizeof(buffer), read_length);

    read_format_data data;
    std::memcpy(&data, buffer, sizeof(buffer));

    // Check that the time_enabled param in secs is smaller than the current time.
    uint64_t read_format_time_secs = data.time_enabled / from_nanos;
    timespec current_time;
    clock_gettime(CLOCK_MONOTONIC, &current_time);
    uint64_t current_time_secs = current_time.tv_sec;

    EXPECT_LE(read_format_time_secs, current_time_secs);
  }
}

TEST(PerfEventOpenTest, ReadEventWithTimeRunningSucceeds) {
  uint64_t read_format = PERF_FORMAT_TOTAL_TIME_RUNNING;
  perf_event_attr attr = attr_with_read_format(read_format);

  if (test_helper::HasSysAdmin()) {
    long file_descriptor =
        sys_perf_event_open(&attr, example_pid, example_cpu, example_group_fd, example_flags);
    EXPECT_NE(file_descriptor, -1);

    // read() on the file descriptor should return the number of bytes written to buffer,
    // and the buffer should contain read_format_data information for that event.
    char buffer[sizeof(read_format_data)];
    uint64_t read_length = syscall(__NR_read, file_descriptor, buffer, sizeof(read_format_data));
    EXPECT_EQ(sizeof(buffer), read_length);

    read_format_data data;
    std::memcpy(&data, buffer, sizeof(buffer));

    // Check that the time_enabled param in secs is smaller than the current time.
    uint64_t read_format_time_secs = data.time_running / from_nanos;
    timespec current_time;
    clock_gettime(CLOCK_MONOTONIC, &current_time);
    uint64_t current_time_secs = current_time.tv_sec;

    EXPECT_LE(read_format_time_secs, current_time_secs);
  }
}

TEST(PerfEventOpenTest, ReadEventWithPerfFormatIdSucceeds) {
  uint64_t read_format = PERF_FORMAT_ID;
  perf_event_attr attr = attr_with_read_format(read_format);

  if (test_helper::HasSysAdmin()) {
    for (uint64_t i = 0; i < 3; i++) {
      long file_descriptor =
          sys_perf_event_open(&attr, example_pid, example_cpu, example_group_fd, example_flags);
      EXPECT_NE(file_descriptor, -1);

      // read() on the file descriptor should return the number of bytes written to buffer,
      // and the buffer should contain read_format_data information for that event.
      char buffer[sizeof(read_format_data)];
      uint64_t read_length = syscall(__NR_read, file_descriptor, buffer, sizeof(read_format_data));
      EXPECT_EQ(sizeof(buffer), read_length);

      read_format_data data;
      std::memcpy(&data, buffer, sizeof(buffer));

      EXPECT_EQ(data.id, i);
    }
  }
}

TEST(PerfEventOpenTest, ReadEventWithTimeEnabledAndRunningSucceeds) {
  uint64_t read_format = PERF_FORMAT_TOTAL_TIME_ENABLED | PERF_FORMAT_TOTAL_TIME_RUNNING;
  perf_event_attr attr = attr_with_read_format(read_format);

  if (test_helper::HasSysAdmin()) {
    long file_descriptor =
        sys_perf_event_open(&attr, example_pid, example_cpu, example_group_fd, example_flags);
    EXPECT_NE(file_descriptor, -1);

    // read() on the file descriptor should return the number of bytes written to buffer,
    // and the buffer should contain read_format_data information for that event.
    char buffer[sizeof(read_format_data)];
    uint64_t read_length = syscall(__NR_read, file_descriptor, buffer, sizeof(read_format_data));
    EXPECT_EQ(sizeof(buffer), read_length);

    read_format_data data;
    std::memcpy(&data, buffer, sizeof(buffer));

    // Check that the params are smaller than the current time.
    uint64_t time_enabled_secs = data.time_enabled / from_nanos;
    uint64_t time_running_secs = data.time_running / from_nanos;
    timespec current_time;
    clock_gettime(CLOCK_MONOTONIC, &current_time);
    uint64_t current_time_secs = current_time.tv_sec;

    EXPECT_LE(time_enabled_secs, time_running_secs);
    EXPECT_LE(time_enabled_secs, current_time_secs);
    EXPECT_LE(time_running_secs, current_time_secs);
  }
}

TEST(PerfEventOpenTest, ReadEventWithBufferTooSmallFails) {
  uint64_t read_format = PERF_FORMAT_TOTAL_TIME_ENABLED;
  perf_event_attr attr = attr_with_read_format(read_format);

  if (test_helper::HasSysAdmin()) {
    long file_descriptor =
        sys_perf_event_open(&attr, example_pid, example_cpu, example_group_fd, example_flags);
    EXPECT_NE(file_descriptor, -1);

    // Create buffer that is too small (<40) for the read() call to put stuff in. read() should
    // return ENOSPC.
    const int buffer_size = 10;
    char buffer[buffer_size];
    EXPECT_THAT(syscall(__NR_read, file_descriptor, buffer, buffer_size),
                SyscallFailsWithErrno(ENOSPC));
  }
}

}  // namespace
