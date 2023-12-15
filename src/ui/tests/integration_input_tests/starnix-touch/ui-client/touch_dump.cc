// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <errno.h>
#include <fcntl.h>
#include <stdlib.h>
#include <string.h>
#include <sys/epoll.h>
#include <unistd.h>

#include <array>
#include <iostream>

#include "src/ui/tests/integration_input_tests/starnix-touch/relay-api.h"
#include "third_party/android/platform/bionic/libc/kernel/uapi/linux/input.h"

// Note: This program uses `fprintf()` instead of `std::cerr`, because the latter flushes the writes
// after each token. That flushing causing a single error message to be split over multiple log
// lines.

void fail() {
  std::string packet(relay_api::kFailedMessage);
  write(STDOUT_FILENO, packet.data(), packet.size());
  abort();
}

template <auto F, typename... Args>
auto ensure(size_t caller_lineno, const std::string& callee, Args... args) {
  auto res = F(args...);
  if (errno != 0) {
    fprintf(stderr, "`%s` failed: %s (called from line %zu)", callee.c_str(), strerror(errno),
            caller_lineno);
    fail();
  }
  return res;
}

// Invoke `function`, then `abort()` if `errno` is non-zero.
// In case of failure, logs the caller line number and callee name.
#define ENSURE(function, ...) ensure<function>(__LINE__, #function, __VA_ARGS__)

// Asserts that `val1` equals `val2`.
//
// Written as a macro so that the compiler can verify that
// `format` matches the types of `val1` and `val2`.
//
// Evaluates macro parameters into local variables to ensure
// that any expressions are only evaluated once.
#define ASSERT_EQ(val1, val2, format) \
  do {                                \
    auto v1 = val1;                   \
    auto v2 = val2;                   \
    auto f = format;                  \
    if (v1 != v2) {                   \
      fprintf(stderr, f, v1, v2);     \
      fail();                         \
    }                                 \
  } while (0)

void relay_events(int epoll_fd) {
  constexpr size_t kExpectedEventLen = sizeof(input_event);
  constexpr int kMaxEvents = 1;
  constexpr int kInfiniteTimeout = -1;
  while (true) {
    // Wait for data.
    std::array<epoll_event, kMaxEvents> event_buf{};
    int n_ready = ENSURE(epoll_wait, epoll_fd, event_buf.data(), kMaxEvents, kInfiniteTimeout);
    ASSERT_EQ(kMaxEvents, n_ready, "expected n_ready=%d, but got %d");
    ASSERT_EQ(EPOLLIN, event_buf[0].events, "expected events_buf[0].events=%u, but got %u");

    // Read the raw data into an `input_event`.
    std::array<unsigned char, 64 * 1024> data_buf{};
    struct input_event event {};
    ssize_t n_read = ENSURE(read, event_buf[0].data.fd, data_buf.data(), data_buf.size());
    ASSERT_EQ(kExpectedEventLen, static_cast<size_t>(n_read), "expected n_read=%zu but got %zu");
    memcpy(&event, data_buf.data(), kExpectedEventLen);

    // Format the event as a string.
    std::array<char, relay_api::kMaxPacketLen> text_event_buf{};
    size_t formatted_len =
        snprintf(text_event_buf.data(), text_event_buf.size(), relay_api::kEventFormat,
                 event.time.tv_sec, event.time.tv_usec, event.type, event.code, event.value);
    if (formatted_len > text_event_buf.size()) {
      fprintf(stderr, "expected formatted_len < %zu, but got %zu", text_event_buf.size(),
              formatted_len);
      abort();
    }

    // Write the string to `stdout`.
    ssize_t n_written = ENSURE(write, STDOUT_FILENO, text_event_buf.data(), formatted_len);
    ASSERT_EQ(formatted_len, static_cast<size_t>(n_written), "expected n_written=%zu, but got %zd");
  }
}

int main() {
  // Get ready to read input events.
  const int touch_fd = ENSURE(open, "/dev/input/event0", O_RDONLY);
  int epoll_fd = ENSURE(epoll_create, 1);  // Per manual page, must be >0.
  epoll_event epoll_params = {.events = EPOLLIN, .data = {.fd = touch_fd}};
  ENSURE(epoll_ctl, epoll_fd, EPOLL_CTL_ADD, touch_fd, &epoll_params);

  // Let `starnix-touch-test.cc` know that we're ready for it to inject
  // touch events.
  std::string packet(relay_api::kReadyMessage);
  auto n_written = ENSURE(write, STDOUT_FILENO, packet.data(), packet.size());
  ASSERT_EQ(packet.size(), static_cast<size_t>(n_written), "expected n_written=%zu, but got %zd");

  // Now just copy events from `evdev` to stdout.
  relay_events(epoll_fd);
}
