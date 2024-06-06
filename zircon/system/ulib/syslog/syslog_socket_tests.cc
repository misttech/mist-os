// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <errno.h>
#include <fcntl.h>
#include <lib/syslog/global.h>
#include <lib/syslog/wire_format.h>
#include <lib/zx/socket.h>
#include <poll.h>
#include <string.h>
#include <unistd.h>

#include <utility>

#include <fbl/string.h>
#include <fbl/string_printf.h>
#include <fbl/unique_fd.h>
#include <src/diagnostics/lib/cpp-log-tester/log_tester.h>
#include <zxtest/zxtest.h>

#include "lib/syslog/cpp/log_settings.h"
#include "lib/syslog/cpp/logging_backend.h"
#include "lib/syslog/logger.h"
#include "zircon/system/ulib/syslog/helpers.h"

namespace {

const char* kFileName = syslog::internal::StripPath(__FILE__);
const char* kFilePath = syslog::internal::StripDots(__FILE__);

inline zx_status_t init_helper(zx_handle_t handle, const char** tags, size_t num_tags,
                               fx_log_severity_t severity = FX_LOG_INFO) {
  fx_logger_config_t config = {
      .min_severity = severity,
      .log_sink_socket = handle,
      .tags = tags,
      .num_tags = num_tags,
  };
  fuchsia_logging::LogSettingsBuilder builder;
  builder.WithMinLogSeverity(severity).DisableInterestListener().BuildAndInitialize();
  return fx_log_reconfigure(&config);
}

inline zx_status_t init_helper(zx_handle_t handle) { return init_helper(handle, nullptr, 0); }

inline zx_status_t init_helper(zx_handle_t handle, fx_log_severity_t severity) {
  return init_helper(handle, nullptr, 0, severity);
}

template <size_t N>
inline zx_status_t init_helper(zx_handle_t handle, const char* (&tags)[N]) {
  return init_helper(handle, tags, std::size(tags));
}

bool ends_with(const char* str, const fbl::String& suffix) {
  size_t str_len = strlen(str);
  size_t suffix_len = suffix.size();
  if (str_len < suffix_len) {
    return false;
  }
  str += str_len - suffix_len;
  return strcmp(str, suffix.c_str()) == 0;
}

void output_compare_helper(const zx::socket& local, fx_log_severity_t severity, const char* msg,
                           const char** tags, size_t num_tags, int line) {
  auto messages = log_tester::RetrieveLogsAsLogMessage(local);
  ASSERT_EQ(messages.size(), 1);
  ASSERT_EQ(messages[0].metadata().tags.size(), num_tags);
  const char* file = severity > FX_LOG_INFO ? kFilePath : kFileName;
  for (size_t i = 0; i < num_tags; i++) {
    ASSERT_EQ(messages[0].metadata().tags[i], tags[i]);
  }
  EXPECT_EQ(messages[0].message(), std::string(msg));
  EXPECT_EQ(messages[0].metadata().file, std::string(file));
  EXPECT_EQ(messages[0].metadata().line, line);
}

void output_compare_helper(const zx::socket& local, fx_log_severity_t severity, const char* msg,
                           int line) {
  output_compare_helper(local, severity, msg, nullptr, 0, line);
}

template <size_t N>
void output_compare_helper(const zx::socket& local, fx_log_severity_t severity, const char* msg,
                           const char* (&tags)[N], int line) {
  output_compare_helper(local, severity, msg, tags, N, line);
}

}  // namespace

TEST(SyslogSocketTests, TestLogSimpleWrite) {
  zx::socket local, remote;
  EXPECT_OK(zx::socket::create(ZX_SOCKET_DATAGRAM, &local, &remote));
  ASSERT_OK(init_helper(remote.release()));
  const char* msg = "test message";
  int line = __LINE__ + 1;
  FX_LOG(INFO, nullptr, msg);
  output_compare_helper(local, FX_LOG_INFO, msg, line);
}

TEST(SyslogSocketTests, TestLogWrite) {
  zx::socket local, remote;
  EXPECT_OK(zx::socket::create(ZX_SOCKET_DATAGRAM, &local, &remote));
  ASSERT_OK(init_helper(remote.release()));
  int line = __LINE__ + 1;
  FX_LOGF(INFO, nullptr, "%d, %s", 10, "just some number");
  output_compare_helper(local, FX_LOG_INFO, "10, just some number", line);
}

TEST(SyslogSocketTests, TestLogPreprocessedMessage) {
  zx::socket local, remote;
  EXPECT_OK(zx::socket::create(ZX_SOCKET_DATAGRAM, &local, &remote));
  ASSERT_OK(init_helper(remote.release()));
  int line = __LINE__ + 1;
  FX_LOG(INFO, nullptr, "%d, %s");
  output_compare_helper(local, FX_LOG_INFO, "%d, %s", line);
}

static zx_status_t GetAvailableBytes(const zx::socket& socket, size_t* out_available) {
  zx_info_socket_t info = {};
  zx_status_t status = socket.get_info(ZX_INFO_SOCKET, &info, sizeof(info), nullptr, nullptr);
  if (status != ZX_OK) {
    return status;
  }
  *out_available = info.rx_buf_available;
  return ZX_OK;
}

TEST(SyslogSocketTests, TestLogSeverity) {
  zx::socket local, remote;
  EXPECT_OK(zx::socket::create(ZX_SOCKET_DATAGRAM, &local, &remote));
  ASSERT_OK(init_helper(remote.release()));

  FX_LOG_SET_SEVERITY(WARNING);
  FX_LOGF(INFO, nullptr, "%d, %s", 10, "just some number");
  size_t outstanding_bytes = 10u;  // init to non zero value.
  ASSERT_OK(GetAvailableBytes(local, &outstanding_bytes));
  EXPECT_EQ(0u, outstanding_bytes);

  int line = __LINE__ + 1;
  FX_LOGF(WARNING, nullptr, "%d, %s", 10, "just some number");
  output_compare_helper(local, FX_LOG_WARNING, "10, just some number", line);
}

TEST(SyslogSocketTests, TestLogWriteWithTag) {
  zx::socket local, remote;
  EXPECT_OK(zx::socket::create(ZX_SOCKET_DATAGRAM, &local, &remote));
  ASSERT_OK(init_helper(remote.release()));
  int line = __LINE__ + 1;
  FX_LOGF(INFO, "tag", "%d, %s", 10, "just some string");
  const char* tags[] = {"tag"};
  output_compare_helper(local, FX_LOG_INFO, "10, just some string", tags, line);
}

TEST(SyslogSocketTests, TestLogWriteWithGlobalTag) {
  zx::socket local, remote;
  EXPECT_OK(zx::socket::create(ZX_SOCKET_DATAGRAM, &local, &remote));
  const char* gtags[] = {"gtag"};
  ASSERT_OK(init_helper(remote.release(), gtags));
  int line = __LINE__ + 1;
  FX_LOGF(INFO, "tag", "%d, %s", 10, "just some string");
  const char* tags[] = {"gtag", "tag"};
  output_compare_helper(local, FX_LOG_INFO, "10, just some string", tags, line);
}

TEST(SyslogSocketTests, TestLogWriteWithMultiGlobalTag) {
  zx::socket local, remote;
  EXPECT_OK(zx::socket::create(ZX_SOCKET_DATAGRAM, &local, &remote));
  const char* gtags[] = {"gtag", "gtag2"};
  ASSERT_OK(init_helper(remote.release(), gtags));
  int line = __LINE__ + 1;
  FX_LOGF(INFO, "tag", "%d, %s", 10, "just some string");
  const char* tags[] = {"gtag", "gtag2", "tag"};
  output_compare_helper(local, FX_LOG_INFO, "10, just some string", tags, line);
}

TEST(SyslogSocketTests, TestGetTags) {
  zx::socket local, remote;
  EXPECT_OK(zx::socket::create(ZX_SOCKET_DATAGRAM, &local, &remote));
  const char* tags[] = {"gtag", "gTag"};
  ASSERT_OK(init_helper(remote.release(), tags));
  std::vector<std::string_view> logger_tags;
  fx_logger_get_tags(
      fx_log_get_logger(),
      [](void* context, const char* tag) {
        static_cast<decltype(&logger_tags)>(context)->push_back(tag);
      },
      &logger_tags);
  EXPECT_EQ(logger_tags.size(), std::size(tags));
  EXPECT_EQ(logger_tags[0], tags[0]);
  EXPECT_EQ(logger_tags[1], tags[1]);
}

TEST(SyslogSocketTests, TestLogFallback) {
  zx::socket local, remote;
  EXPECT_OK(zx::socket::create(ZX_SOCKET_DATAGRAM, &local, &remote));
  const char* gtags[] = {"gtag", "gtag2"};
  ASSERT_OK(init_helper(remote.release(), gtags));

  int pipefd[2];
  EXPECT_EQ(pipe2(pipefd, O_NONBLOCK), 0);
  fbl::unique_fd fd_to_close1(pipefd[0]);
  fbl::unique_fd fd_to_close2(pipefd[1]);
  fx_logger_activate_fallback(fx_log_get_logger(), pipefd[0]);

  int line = __LINE__ + 1;
  FX_LOGF(INFO, "tag", "%d, %s", 10, "just some string");

  char buf[256];
  size_t n = read(pipefd[1], buf, sizeof(buf));
  EXPECT_GT(n, 0u);
  buf[n] = 0;
  EXPECT_TRUE(
      ends_with(buf, fbl::StringPrintf("[gtag, gtag2, tag] INFO: [%s(%d)] 10, just some string\n",
                                       kFileName, line)),
      "%s", buf);
}

TEST(SyslogSocketTests, TestVlogSimpleWrite) {
  zx::socket local, remote;
  EXPECT_OK(zx::socket::create(ZX_SOCKET_DATAGRAM, &local, &remote));
  ASSERT_OK(init_helper(remote.release(), 1));  // 1 is INFO-1
  const char* msg = "test message";
  int line = __LINE__ + 1;
  FX_VLOG(1, nullptr, msg);
  output_compare_helper(local, (FX_LOG_INFO - 1), msg, line);
}

TEST(SyslogSocketTests, TestVlogWrite) {
  zx::socket local, remote;
  EXPECT_OK(zx::socket::create(ZX_SOCKET_DATAGRAM, &local, &remote));
  ASSERT_OK(init_helper(remote.release(), 1));  // 1 is INFO-1
  int line = __LINE__ + 1;
  FX_VLOGF(1, nullptr, "%d, %s", 10, "just some number");
  output_compare_helper(local, (FX_LOG_INFO - 1), "10, just some number", line);
}

TEST(SyslogSocketTests, TestVlogWriteWithTag) {
  zx::socket local, remote;
  EXPECT_OK(zx::socket::create(ZX_SOCKET_DATAGRAM, &local, &remote));
  ASSERT_OK(init_helper(remote.release(), 5));  // INFO-5
  int line = __LINE__ + 1;
  FX_VLOGF(5, "tag", "%d, %s", 10, "just some string");
  const char* tags[] = {"tag"};
  output_compare_helper(local, (FX_LOG_INFO - 5), "10, just some string", tags, 1, line);
}

TEST(SyslogSocketTests, TestLogVerbosity) {
  zx::socket local, remote;
  EXPECT_OK(zx::socket::create(ZX_SOCKET_DATAGRAM, &local, &remote));
  ASSERT_OK(init_helper(remote.release()));

  FX_VLOGF(10, nullptr, "%d, %s", 10, "just some number");
  size_t outstanding_bytes = 10u;  // init to non zero value.
  ASSERT_OK(GetAvailableBytes(local, &outstanding_bytes));
  EXPECT_EQ(0u, outstanding_bytes);

  FX_VLOGF(1, nullptr, "%d, %s", 10, "just some number");
  outstanding_bytes = 10u;  // init to non zero value.
  ASSERT_OK(GetAvailableBytes(local, &outstanding_bytes));
  EXPECT_EQ(0u, outstanding_bytes);

  FX_LOG_SET_VERBOSITY(1);  // INFO - 1
  fuchsia_logging::LogSettingsBuilder builder;
  builder.WithMinLogSeverity(1).DisableInterestListener().BuildAndInitialize();
  int line = __LINE__ + 1;
  FX_VLOGF(1, nullptr, "%d, %s", 10, "just some number");
  output_compare_helper(local, (FX_LOG_INFO - 1), "10, just some number", line);
}

TEST(SyslogSocketTests, TestLogReconfiguration) {
  // Initialize with no tags.
  zx::socket local, remote;
  EXPECT_OK(zx::socket::create(ZX_SOCKET_DATAGRAM, &local, &remote));
  ASSERT_OK(init_helper(remote.release()));
  int line = __LINE__ + 1;
  FX_LOG(INFO, NULL, "Hi");
  output_compare_helper(local, FX_LOG_INFO, "Hi", line);

  // Now reconfigure the logger and add tags.
  const char* tags[] = {"tag1", "tag2"};
  ASSERT_OK(init_helper(ZX_HANDLE_INVALID, tags));
  line = __LINE__ + 1;
  FX_LOG(INFO, NULL, "Hi");
  output_compare_helper(local, FX_LOG_INFO, "Hi", tags, line);
}
