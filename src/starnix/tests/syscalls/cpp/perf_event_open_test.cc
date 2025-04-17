// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <err.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <sys/types.h>
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

// Valid example inputs to use for tests when we aren't testing these values
// but still need to pass them in.
// TODO(https://fxbug.dev/409621963): implement permissions logic for any pid > 0.
const int32_t example_pid = 0;
const int32_t example_cpu = 0;
// TODO(https://fxbug.dev/409619971): handle cases other than -1.
const int example_group_fd = -1;
const long example_flags = 0;
perf_event_attr example_attr = {0, 0, 0, {}, 0, 0, 0};
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
    syscall(__NR_read, file_descriptor, buffer, sizeof(read_format_data));
    // TODO((https://fxbug.dev/409095706): Get correct ReadFormatData length.
    // EXPECT_EQ(sizeof(buffer), read_length);

    read_format_data data;
    std::memcpy(&data, buffer, sizeof(buffer));

    // Check that the time_enabled param in secs is smaller than the current time.
    uint64_t read_format_time_secs = data.time_enabled / from_nanos;
    timespec current_time;
    clock_gettime(CLOCK_MONOTONIC, &current_time);
    uint64_t current_time_secs = current_time.tv_sec;

    EXPECT_LE(read_format_time_secs, current_time_secs);
    EXPECT_NE(syscall(__NR_close, file_descriptor), EXIT_FAILURE);
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
    syscall(__NR_read, file_descriptor, buffer, sizeof(read_format_data));
    // TODO((https://fxbug.dev/409095706): Get correct ReadFormatData length.
    // EXPECT_EQ(sizeof(buffer), read_length);

    read_format_data data;
    std::memcpy(&data, buffer, sizeof(buffer));

    // TODO(https://fxbug.dev/404941053): Calculate time enabled correctly.
    // Check that the time_enabled param in secs is smaller than the current time.
    // uint64_t read_format_time_secs = data.time_running / from_nanos;
    // timespec current_time;
    // clock_gettime(CLOCK_MONOTONIC, &current_time);
    // uint64_t current_time_secs = current_time.tv_sec;

    // EXPECT_LE(read_format_time_secs, current_time_secs);
    EXPECT_NE(syscall(__NR_close, file_descriptor), EXIT_FAILURE);
  }
}

TEST(PerfEventOpenTest, ReadEventWithPerfFormatIdSucceeds) {
  uint64_t read_format = PERF_FORMAT_ID;
  perf_event_attr attr = attr_with_read_format(read_format);

  // TODO(https://fxbug.dev/409627657): Handle id correctly.
  if (test_helper::HasSysAdmin()) {
    for (uint64_t i = 0; i < 3; i++) {
      long file_descriptor =
          sys_perf_event_open(&attr, example_pid, uint32_t(i), example_group_fd, example_flags);
      EXPECT_NE(file_descriptor, -1);

      // read() on the file descriptor should return the number of bytes written to buffer,
      // and the buffer should contain read_format_data information for that event.
      char buffer[sizeof(read_format_data)];
      syscall(__NR_read, file_descriptor, buffer, sizeof(read_format_data));
      // TODO((https://fxbug.dev/409095706): Get correct ReadFormatData length.
      // EXPECT_EQ(sizeof(buffer), read_length);

      read_format_data data;
      std::memcpy(&data, buffer, sizeof(buffer));

      // TODO(https://fxbug.dev/409627657): Handle id correctly.
      // EXPECT_EQ(data.id, i);
      EXPECT_NE(syscall(__NR_close, file_descriptor), EXIT_FAILURE);
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
    syscall(__NR_read, file_descriptor, buffer, sizeof(read_format_data));
    // TODO((https://fxbug.dev/409095706): Get correct ReadFormatData length.
    // EXPECT_EQ(sizeof(buffer), read_length);

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
    EXPECT_NE(syscall(__NR_close, file_descriptor), EXIT_FAILURE);
  }
}

// TODO(https://fxbug.dev/409397980) fix host test.
TEST(PerfEventOpenTest, ReadEventWithBufferTooSmallFails) {
  if (test_helper::HasSysAdmin()) {
    long file_descriptor = sys_perf_event_open(&example_attr, example_pid, example_cpu,
                                               example_group_fd, example_flags);
    EXPECT_NE(file_descriptor, -1);

    // Create buffer that is too small (<40) for the read() call to put stuff in. read() should
    // return ENOSPC.
    size_t buffer_size = 10;
    char buffer[buffer_size];
    EXPECT_THAT(syscall(__NR_read, file_descriptor, buffer, buffer_size),
                SyscallFailsWithErrno(ENOSPC));
    EXPECT_NE(syscall(__NR_close, file_descriptor), EXIT_FAILURE);
  }
}

// When disabled is passed in the initial attr params.
TEST(PerfEventOpenTest, WhenDisabledEventCountShouldBeZero) {
  if (test_helper::HasSysAdmin()) {
    perf_event_attr attr = {0, 0, 0, {}, 0, 0, 1};

    long file_descriptor =
        sys_perf_event_open(&attr, example_pid, example_cpu, example_group_fd, example_flags);

    char buffer[sizeof(read_format_data)];
    syscall(__NR_read, file_descriptor, buffer, sizeof(read_format_data));
    // TODO((https://fxbug.dev/409095706): Get correct ReadFormatData length.
    // EXPECT_EQ(sizeof(buffer), read_length);

    read_format_data data;
    std::memcpy(&data, buffer, sizeof(buffer));

    unsigned long count = 0;
    EXPECT_EQ(data.value, count);
    EXPECT_NE(syscall(__NR_close, file_descriptor), EXIT_FAILURE);
  }
}

// When enabled is passed in the initial attr params.
// TODO(https://fxbug.dev/409397980) fix host test.
TEST(PerfEventOpenTest, WhenEnabledEventCountShouldBeOne) {
  if (test_helper::HasSysAdmin()) {
    long file_descriptor = sys_perf_event_open(&example_attr, example_pid, example_cpu,
                                               example_group_fd, example_flags);

    char buffer[sizeof(read_format_data)];
    syscall(__NR_read, file_descriptor, buffer, sizeof(read_format_data));
    // TODO((https://fxbug.dev/409095706): Get correct ReadFormatData length.
    // EXPECT_EQ(sizeof(buffer), read_length);

    read_format_data data;
    std::memcpy(&data, buffer, sizeof(buffer));

    unsigned long count = 1;
    EXPECT_EQ(data.value, count);
    EXPECT_NE(syscall(__NR_close, file_descriptor), EXIT_FAILURE);
  }
}

TEST(PerfEventOpenTest, SettingIoctlDisabledCallWorks) {
  if (test_helper::HasSysAdmin()) {
    long file_descriptor = sys_perf_event_open(&example_attr, example_pid, example_cpu,
                                               example_group_fd, example_flags);

    EXPECT_NE(syscall(__NR_ioctl, file_descriptor, PERF_EVENT_IOC_DISABLE), -1);
  }
}

TEST(PerfEventOpenTest, SettingIoctlEnabledCallWorks) {
  if (test_helper::HasSysAdmin()) {
    long file_descriptor = sys_perf_event_open(&example_attr, example_pid, example_cpu,
                                               example_group_fd, example_flags);
    EXPECT_NE(syscall(__NR_ioctl, file_descriptor, PERF_EVENT_IOC_ENABLE), -1);
  }
}

// TODO(https://fxbug.dev/409397980) fix host test.
TEST(PerfEventOpenTest, WhenResetAndDisabledEventCountShouldBeZero) {
  if (test_helper::HasSysAdmin()) {
    long file_descriptor = sys_perf_event_open(&example_attr, example_pid, example_cpu,
                                               example_group_fd, example_flags);

    char buffer[sizeof(read_format_data)];
    syscall(__NR_read, file_descriptor, buffer, sizeof(read_format_data));
    // TODO((https://fxbug.dev/409095706): Get correct ReadFormatData length.
    // EXPECT_EQ(sizeof(buffer), read_length);

    read_format_data data;
    std::memcpy(&data, buffer, sizeof(buffer));

    unsigned long count = 1;
    EXPECT_EQ(data.value, count);

    // Disable and reset. Count value should now be 0 and stay there.
    EXPECT_NE(syscall(__NR_ioctl, file_descriptor, PERF_EVENT_IOC_DISABLE), -1);
    EXPECT_NE(syscall(__NR_ioctl, file_descriptor, PERF_EVENT_IOC_RESET), -1);
    count = 0;

    syscall(__NR_read, file_descriptor, buffer, sizeof(read_format_data));
    // TODO((https://fxbug.dev/409095706): Get correct ReadFormatData length.
    // EXPECT_EQ(sizeof(buffer), read_length);

    std::memcpy(&data, buffer, sizeof(buffer));

    EXPECT_EQ(data.value, count);
    EXPECT_NE(syscall(__NR_close, file_descriptor), EXIT_FAILURE);
  }
}

// Example:
// - Start perf_event_open with disabled = 0 (enabled)
// - Do an event
// - Call IOC_DISABLE
// - Do a read() which will return a time_running (for that segment)
// - Do an event
// - Do a read() which will return the same time_running (because segment didn't change)
TEST(PerfEventOpenTest, WhenDisabledTimeRunningIsCorrect) {
  if (test_helper::HasSysAdmin()) {
    long file_descriptor = sys_perf_event_open(&example_attr, example_pid, example_cpu,
                                               example_group_fd, example_flags);

    printf("This is an event\n");

    EXPECT_NE(syscall(__NR_ioctl, file_descriptor, PERF_EVENT_IOC_DISABLE), -1);

    char buffer[sizeof(read_format_data)];
    syscall(__NR_read, file_descriptor, buffer, sizeof(read_format_data));

    read_format_data data;
    std::memcpy(&data, buffer, sizeof(buffer));

    uint64_t time_running = data.time_running;

    printf("This is an event\n");

    syscall(__NR_read, file_descriptor, buffer, sizeof(read_format_data));
    EXPECT_NE(syscall(__NR_close, file_descriptor), EXIT_FAILURE);

    std::memcpy(&data, buffer, sizeof(buffer));

    uint64_t time_running_after_disable = data.time_running;

    EXPECT_EQ(time_running, time_running_after_disable);
    EXPECT_NE(syscall(__NR_close, file_descriptor), EXIT_FAILURE);
  }
}

// Example:
// - Start perf_event_open with disabled = 0 (enabled)
// - Do an event
// - Do a read() which will return a time_running
// - Do an event
// - Do a read() which will return a larger time_running
// TODO(https://fxbug.dev/409397980) fix host test.
TEST(PerfEventOpenTest, WhenEnabledTimeRunningIsCorrect) {
  if (test_helper::HasSysAdmin()) {
    long file_descriptor = sys_perf_event_open(&example_attr, example_pid, example_cpu,
                                               example_group_fd, example_flags);

    printf("This is an event\n");

    char buffer[sizeof(read_format_data)];
    syscall(__NR_read, file_descriptor, buffer, sizeof(read_format_data));

    read_format_data data;
    std::memcpy(&data, buffer, sizeof(buffer));

    uint64_t time_running = data.time_running;

    printf("This is an event\n");

    syscall(__NR_read, file_descriptor, buffer, sizeof(read_format_data));
    EXPECT_NE(syscall(__NR_close, file_descriptor), EXIT_FAILURE);

    std::memcpy(&data, buffer, sizeof(buffer));

    uint64_t time_running_2 = data.time_running;

    EXPECT_LT(time_running, time_running_2);
    EXPECT_NE(syscall(__NR_close, file_descriptor), EXIT_FAILURE);
  }
}

// Example:
// - Start perf_event_open with disabled = 1 (disabled)
// - Do an event
// - Do a read() which will return a time_running of 0 (ensures it was initialized)
// TODO(https://fxbug.dev/409397980) fix host test.
TEST(PerfEventOpenTest, WhenDisabledTotalTimeRunningIsZero) {
  if (test_helper::HasSysAdmin()) {
    perf_event_attr attr = {0, 0, 0, {}, 0, 0, 1};
    long file_descriptor =
        sys_perf_event_open(&attr, example_pid, example_cpu, example_group_fd, example_flags);

    printf("This is an event\n");

    char buffer[sizeof(read_format_data)];
    syscall(__NR_read, file_descriptor, buffer, sizeof(read_format_data));

    read_format_data data;
    std::memcpy(&data, buffer, sizeof(buffer));

    uint64_t time_running = data.time_running;

    EXPECT_EQ(time_running, (uint64_t)0);
    EXPECT_NE(syscall(__NR_close, file_descriptor), EXIT_FAILURE);
  }
}

// Example:
// - Start perf_event_open with disabled = 1 (disabled)
// - Make multiple IOC_DISABLE calls
// - Do an event
// - Do a read() which will return a time_running of 0 (ensures it was initialized)
// TODO(https://fxbug.dev/409397980) fix host test.
TEST(PerfEventOpenTest, MultipleDisablesDoesNotChangeTime) {
  if (test_helper::HasSysAdmin()) {
    perf_event_attr attr = {0, 0, 0, {}, 0, 0, 1};
    long file_descriptor =
        sys_perf_event_open(&attr, example_pid, example_cpu, example_group_fd, example_flags);

    EXPECT_NE(syscall(__NR_ioctl, file_descriptor, PERF_EVENT_IOC_DISABLE), -1);
    EXPECT_NE(syscall(__NR_ioctl, file_descriptor, PERF_EVENT_IOC_DISABLE), -1);

    printf("This is an event\n");

    char buffer[sizeof(read_format_data)];
    syscall(__NR_read, file_descriptor, buffer, sizeof(read_format_data));

    read_format_data data;
    std::memcpy(&data, buffer, sizeof(buffer));

    uint64_t time_running = data.time_running;

    EXPECT_EQ(time_running, (uint64_t)0);
    EXPECT_NE(syscall(__NR_close, file_descriptor), EXIT_FAILURE);
  }
}

}  // namespace
