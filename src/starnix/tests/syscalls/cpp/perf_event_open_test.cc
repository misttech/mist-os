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

// We use this struct when we only request `time_running` in the `read_format`.
// Because the struct is order-dependent we don't want the `time_running` param to
// get written to `time_enabled`.
struct read_format_data_time_running {
  uint64_t value;
  uint64_t time_running;
};

struct read_format_data_id {
  uint64_t value;
  uint64_t id;
};

// Valid example inputs to use for tests when we aren't testing these values
// but still need to pass them in.
// TODO(https://fxbug.dev/409621963): implement permissions logic for any pid > 0.
const int32_t example_pid = 0;
const int32_t example_cpu = -1;  // Keep this as -1 for now so that it includes events on ANY CPU.
// TODO(https://fxbug.dev/409619971): handle cases other than -1.
const int example_group_fd = -1;
const long example_flags = 0;
perf_event_attr example_attr = {0, 0, 0, {}, 0, 0, 0};
const int32_t from_nanos = 1000000000;

// Returns an example perf_event_attr where none of the values matter
// except for the read_format, which is passed in.
perf_event_attr attr_with_read_format(uint64_t read_format) {
  return {PERF_TYPE_SOFTWARE, PERF_ATTR_SIZE_VER1, PERF_COUNT_SW_CPU_CLOCK, {}, 0, read_format, 0};
}

perf_event_attr attr_with_read_format(uint64_t read_format, uint64_t disabled) {
  return {PERF_TYPE_SOFTWARE,
          PERF_ATTR_SIZE_VER1,
          PERF_COUNT_SW_CPU_CLOCK,
          {},
          0,
          read_format,
          disabled};
}

// Write wrapper because there isn't one per the man7 page.
int32_t sys_perf_event_open(perf_event_attr* attr, int32_t pid, int32_t cpu, int group_fd,
                            unsigned long flags) {
  // Explicitly cast to int32_t because perf_event_open() returns `int`. Also, `FdNumber` is i32.
  return static_cast<int32_t>(syscall(__NR_perf_event_open, attr, pid, cpu, group_fd, flags));
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
    int32_t file_descriptor =
        sys_perf_event_open(&attr, example_pid, example_cpu, example_group_fd, example_flags);
    EXPECT_NE(file_descriptor, -1);

    // read() on the file descriptor should return the number of bytes written to buffer,
    // and the buffer should contain read_format_data information for that event.
    char buffer[16];
    uint64_t read_length = syscall(__NR_read, file_descriptor, buffer, sizeof(buffer));
    EXPECT_EQ(read_length, sizeof(buffer));

    read_format_data data;
    std::memcpy(&data, buffer, sizeof(buffer));

    // Check that the time_enabled param in secs is smaller than the current time.
    uint64_t time_enabled_secs = data.time_enabled / from_nanos;
    timespec current_time;
    clock_gettime(CLOCK_MONOTONIC, &current_time);
    uint64_t current_time_secs = current_time.tv_sec;

    EXPECT_LE(time_enabled_secs, current_time_secs);
    EXPECT_NE(syscall(__NR_close, file_descriptor), EXIT_FAILURE);
  }
}

TEST(PerfEventOpenTest, ReadEventWithTimeRunningSucceeds) {
  uint64_t read_format = PERF_FORMAT_TOTAL_TIME_RUNNING;
  perf_event_attr attr = attr_with_read_format(read_format);

  if (test_helper::HasSysAdmin()) {
    int32_t file_descriptor =
        sys_perf_event_open(&attr, example_pid, example_cpu, example_group_fd, example_flags);
    EXPECT_NE(file_descriptor, -1);

    // read() on the file descriptor should return the number of bytes written to buffer,
    // and the buffer should contain read_format_data information for that event.
    char buffer[16];
    uint64_t read_length = syscall(__NR_read, file_descriptor, buffer, sizeof(buffer));
    EXPECT_EQ(read_length, sizeof(buffer));

    read_format_data_time_running data;
    std::memcpy(&data, buffer, sizeof(buffer));

    uint64_t time_running_secs = data.time_running / from_nanos;
    timespec current_time;
    clock_gettime(CLOCK_MONOTONIC, &current_time);
    uint64_t current_time_secs = current_time.tv_sec;

    EXPECT_LE(time_running_secs, current_time_secs);
    EXPECT_NE(syscall(__NR_close, file_descriptor), EXIT_FAILURE);
  }
}

TEST(PerfEventOpenTest, ReadEventWithPerfFormatIdSucceeds) {
  if (test_helper::HasSysAdmin()) {
    uint64_t read_format = PERF_FORMAT_ID;
    perf_event_attr attr = attr_with_read_format(read_format);
    uint64_t ids[3];

    for (uint64_t i = 0; i < 3; i++) {
      int32_t file_descriptor =
          sys_perf_event_open(&attr, example_pid, example_cpu, example_group_fd, example_flags);
      EXPECT_NE(file_descriptor, -1);

      char buffer[16];
      read_format_data_id data;
      syscall(__NR_read, file_descriptor, buffer, sizeof(buffer));
      std::memcpy(&data, buffer, sizeof(buffer));

      ids[i] = data.id;
      EXPECT_NE(syscall(__NR_close, file_descriptor), EXIT_FAILURE);
    }

    EXPECT_LT(ids[0], ids[1]);
    EXPECT_LT(ids[1], ids[2]);
  }
}

TEST(PerfEventOpenTest, ReadEventWithTimeEnabledAndRunningSucceeds) {
  uint64_t read_format = PERF_FORMAT_TOTAL_TIME_ENABLED | PERF_FORMAT_TOTAL_TIME_RUNNING;
  perf_event_attr attr = attr_with_read_format(read_format);

  if (test_helper::HasSysAdmin()) {
    int32_t file_descriptor =
        sys_perf_event_open(&attr, example_pid, example_cpu, example_group_fd, example_flags);
    EXPECT_NE(file_descriptor, -1);

    char buffer[24];
    uint64_t read_length = syscall(__NR_read, file_descriptor, buffer, sizeof(buffer));
    EXPECT_EQ(read_length, sizeof(buffer));

    read_format_data data;
    std::memcpy(&data, buffer, sizeof(buffer));

    // Check that the params are smaller than the current time.
    uint64_t time_enabled_secs = data.time_enabled / from_nanos;
    uint64_t time_running_secs = data.time_running / from_nanos;
    timespec current_time;
    clock_gettime(CLOCK_MONOTONIC, &current_time);
    uint64_t current_time_secs = current_time.tv_sec;

    EXPECT_EQ(time_enabled_secs, time_running_secs);
    EXPECT_LE(time_enabled_secs, current_time_secs);
    EXPECT_LE(time_running_secs, current_time_secs);
    EXPECT_NE(syscall(__NR_close, file_descriptor), EXIT_FAILURE);
  }
}

TEST(PerfEventOpenTest, ReadEventWithBufferTooSmallFails) {
  if (test_helper::HasSysAdmin()) {
    int32_t file_descriptor = sys_perf_event_open(&example_attr, example_pid, example_cpu,
                                                  example_group_fd, example_flags);
    EXPECT_NE(file_descriptor, -1);

    // Create buffer that is too small for the read() call to put stuff in. read() should
    // return ENOSPC.
    char buffer[7];
    EXPECT_THAT(syscall(__NR_read, file_descriptor, buffer, sizeof(buffer)),
                SyscallFailsWithErrno(ENOSPC));
    EXPECT_NE(syscall(__NR_close, file_descriptor), EXIT_FAILURE);
  }
}

// When disabled is passed in the initial attr params.
TEST(PerfEventOpenTest, WhenDisabledEventCountShouldBeZero) {
  if (test_helper::HasSysAdmin()) {
    perf_event_attr attr = {0, 0, 0, {}, 0, 0, 1};

    int32_t file_descriptor =
        sys_perf_event_open(&attr, example_pid, example_cpu, example_group_fd, example_flags);

    char buffer[8];
    uint64_t read_length = syscall(__NR_read, file_descriptor, buffer, sizeof(buffer));
    EXPECT_EQ(read_length, sizeof(buffer));

    read_format_data data;
    std::memcpy(&data, buffer, sizeof(buffer));

    unsigned long count = 0;
    EXPECT_EQ(data.value, count);
    EXPECT_NE(syscall(__NR_close, file_descriptor), EXIT_FAILURE);
  }
}

// When enabled is passed in the initial attr params.
TEST(PerfEventOpenTest, WhenEnabledEventCountShouldBeOne) {
  if (test_helper::HasSysAdmin()) {
    int32_t file_descriptor = sys_perf_event_open(&example_attr, example_pid, example_cpu,
                                                  example_group_fd, example_flags);

    char buffer[8];
    uint64_t read_length = syscall(__NR_read, file_descriptor, buffer, sizeof(buffer));
    EXPECT_EQ(read_length, sizeof(buffer));

    read_format_data data;
    std::memcpy(&data, buffer, sizeof(buffer));

    EXPECT_GT(data.value, (uint64_t)0);
    EXPECT_NE(syscall(__NR_close, file_descriptor), EXIT_FAILURE);
  }
}

TEST(PerfEventOpenTest, SettingIoctlDisabledCallWorks) {
  if (test_helper::HasSysAdmin()) {
    int32_t file_descriptor = sys_perf_event_open(&example_attr, example_pid, example_cpu,
                                                  example_group_fd, example_flags);

    EXPECT_NE(syscall(__NR_ioctl, file_descriptor, PERF_EVENT_IOC_DISABLE), -1);
  }
}

TEST(PerfEventOpenTest, SettingIoctlEnabledCallWorks) {
  if (test_helper::HasSysAdmin()) {
    int32_t file_descriptor = sys_perf_event_open(&example_attr, example_pid, example_cpu,
                                                  example_group_fd, example_flags);
    EXPECT_NE(syscall(__NR_ioctl, file_descriptor, PERF_EVENT_IOC_ENABLE), -1);
  }
}

TEST(PerfEventOpenTest, WhenResetAndDisabledEventCountShouldBeZero) {
  if (test_helper::HasSysAdmin()) {
    int32_t file_descriptor = sys_perf_event_open(&example_attr, example_pid, example_cpu,
                                                  example_group_fd, example_flags);

    char buffer[8];
    syscall(__NR_read, file_descriptor, buffer,
            sizeof(buffer));  // Read once: the test rf_value = 1
    uint64_t read_length =
        syscall(__NR_read, file_descriptor, buffer, sizeof(buffer));  // Read twice: rf_value = 2
    EXPECT_EQ(read_length, sizeof(buffer));

    read_format_data data;
    std::memcpy(&data, buffer, sizeof(buffer));

    // Host test will return a real value, which is bigger.
    unsigned long count = 2;
    EXPECT_GE(data.value, count);

    // Disable and reset. Count value should now be 0 and stay there.
    EXPECT_NE(syscall(__NR_ioctl, file_descriptor, PERF_EVENT_IOC_DISABLE), -1);
    EXPECT_NE(syscall(__NR_ioctl, file_descriptor, PERF_EVENT_IOC_RESET), -1);
    count = 0;

    read_length = syscall(__NR_read, file_descriptor, buffer, sizeof(buffer));
    EXPECT_EQ(read_length, sizeof(buffer));

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
TEST(PerfEventOpenTest, WhenDisabledTimeRunningAndTimeEnabledAreCorrect) {
  uint64_t read_format = PERF_FORMAT_TOTAL_TIME_RUNNING | PERF_FORMAT_TOTAL_TIME_ENABLED;
  perf_event_attr attr = attr_with_read_format(read_format);

  if (test_helper::HasSysAdmin()) {
    int32_t file_descriptor =
        sys_perf_event_open(&attr, example_pid, example_cpu, example_group_fd, example_flags);

    char buffer[24];
    read_format_data data;

    printf("This is an event\n");
    EXPECT_NE(syscall(__NR_ioctl, file_descriptor, PERF_EVENT_IOC_DISABLE), -1);
    syscall(__NR_read, file_descriptor, buffer, sizeof(buffer));
    std::memcpy(&data, buffer, sizeof(buffer));

    uint64_t time_running = data.time_running;
    uint64_t time_enabled = data.time_enabled;

    printf("This is an event\n");
    syscall(__NR_read, file_descriptor, buffer, sizeof(buffer));
    std::memcpy(&data, buffer, sizeof(buffer));

    uint64_t time_running_after_disable = data.time_running;
    uint64_t time_enabled_after_disable = data.time_enabled;

    EXPECT_EQ(time_running, time_running_after_disable);
    EXPECT_EQ(time_enabled, time_enabled_after_disable);
    EXPECT_NE(syscall(__NR_close, file_descriptor), EXIT_FAILURE);
  }
}

// Example:
// - Start perf_event_open with disabled = 0 (enabled)
// - Do an event
// - Do a read() which will return a time_running
// - Do an event
// - Do a read() which will return a larger time_running
TEST(PerfEventOpenTest, WhenEnabledTimeRunningIsCorrect) {
  uint64_t read_format = PERF_FORMAT_TOTAL_TIME_RUNNING;
  perf_event_attr attr = attr_with_read_format(read_format);

  if (test_helper::HasSysAdmin()) {
    int32_t file_descriptor =
        sys_perf_event_open(&attr, example_pid, example_cpu, example_group_fd, example_flags);
    char buffer[16];
    read_format_data_time_running data;

    EXPECT_NE(syscall(__NR_ioctl, file_descriptor, PERF_EVENT_IOC_ENABLE), -1);
    printf("This is an event\n");
    syscall(__NR_read, file_descriptor, buffer, sizeof(buffer));
    std::memcpy(&data, buffer, sizeof(buffer));
    uint64_t time_running = data.time_running;

    // Check that time later is bigger.
    printf("This is an event\n");
    syscall(__NR_read, file_descriptor, buffer, sizeof(buffer));
    std::memcpy(&data, buffer, sizeof(buffer));
    uint64_t time_running_later = data.time_running;

    // This fails for the host test because time_running is always 0.
    // TODO(https://fxbug.dev/413146816): figure out what the real time_running is.
    EXPECT_LT(time_running, time_running_later);
    EXPECT_NE(syscall(__NR_close, file_descriptor), EXIT_FAILURE);
  }
}

// Example:
// - Start perf_event_open with disabled = 0 (enabled)
// - Do an event
// - Do a read() which will return a time_running
// - Do an event
// - Do a read() which will return a larger time_running
TEST(PerfEventOpenTest, WhenEnabledTimeEnabledIsCorrect) {
  uint64_t read_format = PERF_FORMAT_TOTAL_TIME_ENABLED;
  perf_event_attr attr = attr_with_read_format(read_format);

  if (test_helper::HasSysAdmin()) {
    int32_t file_descriptor =
        sys_perf_event_open(&attr, example_pid, example_cpu, example_group_fd, example_flags);
    char buffer[16];
    read_format_data data;

    EXPECT_NE(syscall(__NR_ioctl, file_descriptor, PERF_EVENT_IOC_ENABLE), -1);
    printf("This is an event\n");
    syscall(__NR_read, file_descriptor, buffer, sizeof(buffer));
    std::memcpy(&data, buffer, sizeof(buffer));
    uint64_t time_enabled = data.time_enabled;

    // Check that time later is bigger.
    printf("This is an event\n");
    syscall(__NR_read, file_descriptor, buffer, sizeof(buffer));
    std::memcpy(&data, buffer, sizeof(buffer));
    uint64_t time_enabled_later = data.time_enabled;

    EXPECT_LT(time_enabled, time_enabled_later);
    EXPECT_NE(syscall(__NR_close, file_descriptor), EXIT_FAILURE);
  }
}

// Example:
// - Start perf_event_open with disabled = 1 (disabled)
// - Do an event
// - Do a read() which will return a time_running and time_enabled of 0 (ensures initialization)
TEST(PerfEventOpenTest, WhenDisabledTotalTimeRunningAndEnabledAreZero) {
  if (test_helper::HasSysAdmin()) {
    uint64_t read_format = PERF_FORMAT_TOTAL_TIME_RUNNING | PERF_FORMAT_TOTAL_TIME_ENABLED;
    perf_event_attr attr = attr_with_read_format(read_format, 1);
    int32_t file_descriptor =
        sys_perf_event_open(&attr, example_pid, example_cpu, example_group_fd, example_flags);

    printf("This is an event\n");

    char buffer[24];
    syscall(__NR_read, file_descriptor, buffer, sizeof(buffer));

    read_format_data data;
    std::memcpy(&data, buffer, sizeof(buffer));

    EXPECT_EQ(data.time_running, (uint64_t)0);
    EXPECT_EQ(data.time_enabled, (uint64_t)0);
  }
}

// Example:
// - Start perf_event_open with disabled = 1 (disabled)
// - Make multiple IOC_DISABLE calls
// - Do an event
// - Do a read() which will return a time_running of 0 (ensures it was initialized)
TEST(PerfEventOpenTest, MultipleDisablesDoesNotChangeTime) {
  if (test_helper::HasSysAdmin()) {
    uint64_t read_format = PERF_FORMAT_TOTAL_TIME_RUNNING;
    perf_event_attr attr = attr_with_read_format(read_format, 1);
    int32_t file_descriptor =
        sys_perf_event_open(&attr, example_pid, example_cpu, example_group_fd, example_flags);

    EXPECT_NE(syscall(__NR_ioctl, file_descriptor, PERF_EVENT_IOC_DISABLE), -1);
    EXPECT_NE(syscall(__NR_ioctl, file_descriptor, PERF_EVENT_IOC_DISABLE), -1);

    printf("This is an event\n");

    char buffer[16];
    syscall(__NR_read, file_descriptor, buffer, sizeof(buffer));

    read_format_data_time_running data;
    std::memcpy(&data, buffer, sizeof(buffer));

    EXPECT_EQ(data.time_running, (uint64_t)0);
    EXPECT_NE(syscall(__NR_close, file_descriptor), EXIT_FAILURE);
  }
}

TEST(PerfEventOpenTest, ReadingFirstEightBytesCanReturnCountAsALong) {
  if (test_helper::HasSysAdmin()) {
    int32_t file_descriptor = sys_perf_event_open(&example_attr, example_pid, example_cpu,
                                                  example_group_fd, example_flags);

    // Both char buffer[8] (tested in previous tests) and long long (8 bytes) should work.
    long long count;
    syscall(__NR_read, file_descriptor, &count, sizeof(count));

    EXPECT_GE(count, 1);
    EXPECT_NE(syscall(__NR_close, file_descriptor), EXIT_FAILURE);
  }
}

// Here is a full example of a counting case.
TEST(PerfEventOpenTest, CountingCPUClockSucceeds) {
  if (test_helper::HasSysAdmin()) {
    perf_event_attr attr;
    memset(&attr, 0, sizeof(attr));
    attr.type = PERF_TYPE_SOFTWARE;
    attr.size = sizeof(attr);
    attr.config = PERF_COUNT_SW_CPU_CLOCK;
    attr.disabled = 1;

    int32_t file_descriptor =
        sys_perf_event_open(&attr, example_pid, -1, example_group_fd, example_flags);
    EXPECT_NE(file_descriptor, -1);

    EXPECT_NE(syscall(__NR_ioctl, file_descriptor, PERF_EVENT_IOC_RESET), -1);
    EXPECT_NE(syscall(__NR_ioctl, file_descriptor, PERF_EVENT_IOC_ENABLE), -1);

    printf("This is an event\n");

    EXPECT_NE(syscall(__NR_ioctl, file_descriptor, PERF_EVENT_IOC_DISABLE), -1);

    long long count;
    syscall(__NR_read, file_descriptor, &count, sizeof(count));

    // TODO(https://fxbug.dev/402938671): this is expected to be 0 right now
    // because real value hasn't been implemented. When running on host, it will give
    // a real number (~6000-10000). Update this when we get a real instruction count.
    EXPECT_GT(count, -1);
    EXPECT_NE(syscall(__NR_close, file_descriptor), EXIT_FAILURE);
  }
}

/* Below are sampling tests */

// Valid attributes for sampling. On Linux for the first sampling event you'll get something like:
// perf_event_header { type = 9, size = 16, misc = 2 }.
perf_event_attr example_sampling_attr() {
  perf_event_attr attr;
  memset(&attr, 0, sizeof(attr));
  attr.type = PERF_TYPE_HARDWARE;
  attr.size = sizeof(attr);
  attr.config = PERF_COUNT_HW_INSTRUCTIONS;
  attr.sample_period = 10;  // Elects sampling instead of counting. "1 sample per 10 events".
  attr.sample_type = PERF_SAMPLE_TIME;
  attr.disabled = 0;      // Initiate as enabled.
  attr.exclude_user = 0;  // Necessary otherwise we get zeros for sampling.
  attr.exclude_kernel = 1;
  attr.sample_id_all = 1;

  return attr;
}

TEST(PerfEventOpenTest, MmapMetadataPageIsValid) {
  if (test_helper::HasSysAdmin()) {
    perf_event_attr attr = example_sampling_attr();
    int32_t file_descriptor =
        sys_perf_event_open(&attr, example_pid, example_cpu, example_group_fd, example_flags);

    int num_pages = 2;
    size_t data_size = num_pages * getpagesize();
    // Ring buffer size, defined to be 1 + 2^n pages per the docs.
    size_t buffer_size = getpagesize() + data_size;

    // mmap() returns the address of the mapping. Note you MUST use MAP_SHARED because you're doing
    // kernel and user stuff. The offset has to be 0 to access the metadata page (first page).
    void* address = mmap(NULL, buffer_size, PROT_READ, MAP_SHARED, file_descriptor, 0);

    // Address should not be 0xffffffffffffffff.
    EXPECT_NE(address, MAP_FAILED);

    // Docs say you can close here before reading anything.
    EXPECT_NE(syscall(__NR_close, file_descriptor), EXIT_FAILURE);

    char buffer[buffer_size];
    EXPECT_EQ(buffer_size, sizeof(buffer));

    // Verify metadata page has valid/reasonable info.
    // Don't need to memcopy. Just read directly.
    perf_event_mmap_page* metadata = (perf_event_mmap_page*)address;
    EXPECT_LT(metadata->version, (uint32_t)10);
    EXPECT_LT(metadata->compat_version, (uint32_t)10);
    EXPECT_EQ(metadata->lock % 2, (uint32_t)0);
    EXPECT_NE(metadata->index, (uint32_t)-1);
    EXPECT_NE(metadata->offset, (int64_t)-1);
    EXPECT_EQ(metadata->capabilities, (uint64_t)30);
    EXPECT_EQ(metadata->cap_user_time, (uint64_t)1);
    EXPECT_GT(metadata->time_enabled, (uint64_t)0);
    EXPECT_GT(metadata->time_running, (uint64_t)0);
    // Verify that there is a sample to read, this must be > 0.
    EXPECT_GE(metadata->data_head, (uint64_t)0);
    EXPECT_EQ(metadata->data_tail, (uint64_t)0);
    EXPECT_EQ(metadata->data_offset, (uint64_t)getpagesize());
    EXPECT_EQ(metadata->data_size, (uint64_t)data_size);
  }
}

// TODO(https://fxbug.dev/398914921): The Linux version of this test will fail because
// we are currently testing against hardcoded values in the Starnix implementation.
// Use better EXPECT statements when we grab real values.
TEST(PerfEventOpenTest, MmapFirstRecordPageIsValid) {
  if (test_helper::HasSysAdmin()) {
    perf_event_attr attr = example_sampling_attr();
    int32_t file_descriptor =
        sys_perf_event_open(&attr, example_pid, example_cpu, example_group_fd, example_flags);

    int num_pages = 2;
    size_t data_size = num_pages * getpagesize();
    // Ring buffer size, defined to be 1 + 2^n pages per the docs.
    size_t buffer_size = getpagesize() + data_size;

    // mmap() returns the address of the mapping. Note you MUST use MAP_SHARED because you're doing
    // kernel and user stuff. The offset has to be 0 to access the metadata page (first page).
    void* address = mmap(NULL, buffer_size, PROT_READ, MAP_SHARED, file_descriptor, 0);

    // Address should not be 0xffffffffffffffff.
    EXPECT_NE(address, MAP_FAILED);

    // Docs say you can close here before reading anything.
    EXPECT_NE(syscall(__NR_close, file_descriptor), EXIT_FAILURE);

    char buffer[buffer_size];
    EXPECT_EQ(buffer_size, sizeof(buffer));

    // Verify metadata page has valid/reasonable info.
    // Don't need to memcopy. Just read directly.
    perf_event_mmap_page* metadata = (perf_event_mmap_page*)address;
    EXPECT_LT(metadata->version, (uint32_t)10);
    EXPECT_LT(metadata->compat_version, (uint32_t)10);
    EXPECT_EQ(metadata->lock % 2, (uint32_t)0);
    EXPECT_NE(metadata->index, (uint32_t)-1);
    EXPECT_NE(metadata->offset, (int64_t)-1);
    EXPECT_EQ(metadata->capabilities, (uint64_t)30);
    EXPECT_EQ(metadata->cap_user_time, (uint64_t)1);
    EXPECT_GT(metadata->time_enabled, (uint64_t)0);
    EXPECT_GT(metadata->time_running, (uint64_t)0);
    // Verify that there is a sample to read, this must be > 0.
    EXPECT_GT(metadata->data_head, (uint64_t)0);
    EXPECT_EQ(metadata->data_tail, (uint64_t)0);
    EXPECT_EQ(metadata->data_offset, (uint64_t)getpagesize());
    EXPECT_EQ(metadata->data_size, data_size);

    // Start reading next page, which is the first sampling data page. From there
    // you can keep iterating to read each sample. Layout:
    //
    // The whole object is a Record, comprised of a Header and a RecordDetails:
    // ------           <-- sample_start, start of the header and the sample.
    // |  |
    // |  |                 perf_event_header size
    // |  |
    // |  ---           <-- record_details_start, start of the record_details.
    // |  |
    // |  |                 varying size based on `perf_event_header->type`
    // |  |
    // |  |
    // ------
    uint64_t curr_pointer = metadata->data_tail;
    while (curr_pointer < metadata->data_head) {
      char* record_start =
          static_cast<char*>(address) + metadata->data_offset + metadata->data_tail + curr_pointer;
      perf_event_header* header = (perf_event_header*)record_start;

      // Increment by sample size, so that we can read the next sample in the next iteration.
      curr_pointer += header->size;

      EXPECT_EQ(header->type, PERF_RECORD_SAMPLE /* 9 */);
      EXPECT_THAT(header->misc, testing::AnyOf(testing::Eq(PERF_RECORD_MISC_KERNEL) /* 1 */,
                                               testing::Eq(PERF_RECORD_MISC_USER) /* 2 */));
      EXPECT_GE(header->size, (uint16_t)8);  // Size of the whole sample, INCLUDING THIS HEADER.

      // Now that we know the type, we can roll past the perf_event_header
      // and read the rest of the struct, which is different for each type.
      char* record_details_start = record_start + sizeof(perf_event_header);
      // This is a subset of the real perf_record_sample which we will implement later.
      struct perf_record_sample {
        uint64_t sample_id;
        uint64_t ip;
        uint32_t pid;
        uint32_t tid;
        uint64_t time;
      };
      struct perf_record_sample* record_details = (struct perf_record_sample*)record_details_start;
      EXPECT_GE(record_details->sample_id, (uint64_t)12);
      EXPECT_GE(record_details->ip, (uint64_t)123);
      EXPECT_GE(record_details->pid, (uint64_t)1234);
      EXPECT_GE(record_details->tid, (uint64_t)12345);
      EXPECT_GE(record_details->time, (uint64_t)123456);
    }
  }
}

}  // namespace
