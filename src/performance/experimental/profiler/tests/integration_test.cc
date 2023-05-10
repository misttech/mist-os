// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.cpu.profiler/cpp/fidl.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/zx/job.h>
#include <lib/zx/process.h>
#include <lib/zx/socket.h>
#include <lib/zx/thread.h>
#include <lib/zx/vmar.h>
#include <lib/zx/vmo.h>

#include <gtest/gtest.h>

void MakeWork() {
  for (;;) {
  }
  zx_thread_exit();
}

TEST(ProfilerIntegrationTest, EndToEnd) {
  zx::result client_end = component::Connect<fuchsia_cpu_profiler::Session>();
  ASSERT_TRUE(client_end.is_ok());
  const fidl::SyncClient client{std::move(*client_end)};

  zx::socket in_socket;
  zx::socket outgoing_socket;

  ASSERT_EQ(zx::socket::create(0u, &in_socket, &outgoing_socket), ZX_OK);
  zx::process self;
  zx::process::self()->duplicate(ZX_RIGHT_SAME_RIGHTS, &self);

  zx::thread child;
  zx_status_t res = zx::thread::create(self, "TestChild", 9, 0, &child);

  static uint8_t stack[1024] __ALIGNED(16);
  res = child.start(reinterpret_cast<uintptr_t>(MakeWork),
                    reinterpret_cast<uintptr_t>(stack + sizeof(stack)), 0, 0);
  ASSERT_EQ(ZX_OK, res);

  res = child.wait_one(ZX_THREAD_RUNNING, zx::deadline_after(zx::sec(1)), nullptr);
  ASSERT_EQ(ZX_OK, res);

  zx_info_handle_basic_t info;
  res = child.get_info(ZX_INFO_HANDLE_BASIC, &info, sizeof(info), nullptr, nullptr);
  ASSERT_EQ(ZX_OK, res);

  fuchsia_cpu_profiler::SamplingConfig sampling_config{{
      .period = 1000000,
      .timebase = fuchsia_cpu_profiler::Counter::WithPlatformIndependent(
          fuchsia_cpu_profiler::CounterId::kNanoseconds),
      .sample = fuchsia_cpu_profiler::Sample{{
          .callgraph =
              fuchsia_cpu_profiler::CallgraphConfig{
                  {.strategy = fuchsia_cpu_profiler::CallgraphStrategy::kFramePointer}},
          .counters = std::vector<fuchsia_cpu_profiler::Counter>{},
      }},
  }};

  fuchsia_cpu_profiler::TargetConfig target_config{{
      .task = fuchsia_cpu_profiler::Task::WithThread(info.koid),
  }};

  fuchsia_cpu_profiler::Config config{{
      .configs = std::vector{sampling_config},
      .targets = std::vector{target_config},
  }};

  auto config_response = client->Configure({{
      .output = std::move(outgoing_socket),
      .config = config,
  }});
  ASSERT_TRUE(config_response.is_ok());

  auto start_response = client->Start({{.buffer_results = true}});
  ASSERT_TRUE(start_response.is_ok());
  // Get Some samples
  sleep(1);

  auto stop_response = client->Stop();
  ASSERT_TRUE(stop_response.is_ok());
  ASSERT_TRUE(stop_response.value().samples_collected().has_value());
  EXPECT_GT(stop_response.value().samples_collected().value(), size_t{10});

  auto reset_response = client->Reset();
  ASSERT_TRUE(reset_response.is_ok());
}
