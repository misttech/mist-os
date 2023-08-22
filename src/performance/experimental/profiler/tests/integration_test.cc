// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.cpu.profiler/cpp/fidl.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/fdio/spawn.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/zx/job.h>
#include <lib/zx/process.h>
#include <lib/zx/result.h>
#include <lib/zx/socket.h>
#include <lib/zx/thread.h>
#include <lib/zx/time.h>
#include <unistd.h>
#include <zircon/errors.h>
#include <zircon/rights.h>
#include <zircon/syscalls.h>
#include <zircon/threads.h>
#include <zircon/types.h>

#include <cctype>
#include <cstddef>
#include <cstdlib>
#include <set>
#include <sstream>
#include <string>
#include <thread>
#include <utility>
#include <vector>

#include <gtest/gtest.h>
#include <src/lib/fsl/socket/strings.h>

void MakeWork() {
  for (;;) {
    // We need to have at least some side effect producing code or a release build will elide the
    // entire function
    FX_LOGS(TRACE) << "Working!";
  }
  zx_thread_exit();
}

std::pair<std::set<zx_koid_t>, std::set<zx_koid_t>> GetOutputKoids(zx::socket sock) {
  std::string contents;
  if (!fsl::BlockingCopyToString(std::move(sock), &contents)) {
    return std::make_pair(std::set<zx_koid_t>(), std::set<zx_koid_t>());
  }

  std::stringstream ss;
  ss << contents;
  std::set<zx_koid_t> pids;
  std::set<zx_koid_t> tids;
  // The socket data looks like:
  // <pid>\n
  // <tid>\n
  // {{{bt1}}}\n
  // {{{bt2}}}\n
  // ...
  // <pid>\n
  // <tid>\n
  // {{{bt1}}}\n
  // {{{bt2}}}\n
  // ...
  for (std::string pid_string; std::getline(ss, pid_string);) {
    if (pid_string.empty() || !isdigit(pid_string[0])) {
      continue;
    }
    std::string tid_string;
    std::getline(ss, tid_string);
    pids.insert(strtoll(pid_string.data(), nullptr, 0));
    tids.insert(strtoll(tid_string.data(), nullptr, 0));
  }
  return std::make_pair(std::move(pids), std::move(tids));
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

  std::thread child(MakeWork);
  const zx::unowned_thread child_handle{native_thread_get_zx_handle(child.native_handle())};
  child.detach();

  zx_status_t res =
      child_handle->wait_one(ZX_THREAD_RUNNING, zx::deadline_after(zx::sec(1)), nullptr);
  ASSERT_EQ(ZX_OK, res);

  zx_info_handle_basic_t info;
  res = child_handle->get_info(ZX_INFO_HANDLE_BASIC, &info, sizeof(info), nullptr, nullptr);
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

  fuchsia_cpu_profiler::TargetConfig target_config = fuchsia_cpu_profiler::TargetConfig::WithTasks(
      std::vector{fuchsia_cpu_profiler::Task::WithThread(info.koid)});
  fuchsia_cpu_profiler::Config config{{
      .configs = std::vector{sampling_config},
      .target = target_config,
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

// Monitor ourself and check that if we start new threads after the profiling session starts, that
// one or more of them show up in the samples we take.
TEST(ProfilerIntegrationTest, NewThreads) {
  zx::result client_end = component::Connect<fuchsia_cpu_profiler::Session>();
  ASSERT_TRUE(client_end.is_ok());
  const fidl::SyncClient client{std::move(*client_end)};

  zx::socket in_socket;
  zx::socket outgoing_socket;

  ASSERT_EQ(zx::socket::create(0u, &in_socket, &outgoing_socket), ZX_OK);
  zx::unowned_process self = zx::process::self();

  zx_info_handle_basic_t info;
  zx_status_t info_result =
      self->get_info(ZX_INFO_HANDLE_BASIC, &info, sizeof(info), nullptr, nullptr);
  ASSERT_EQ(ZX_OK, info_result);

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

  // We'll sample ourself.
  fuchsia_cpu_profiler::TargetConfig target_config = fuchsia_cpu_profiler::TargetConfig::WithTasks(
      std::vector{fuchsia_cpu_profiler::Task::WithProcess(info.koid)});

  fuchsia_cpu_profiler::Config config{{
      .configs = std::vector{sampling_config},
      .target = target_config,
  }};

  auto config_response = client->Configure({{
      .output = std::move(outgoing_socket),
      .config = config,
  }});
  ASSERT_TRUE(config_response.is_ok());

  auto start_response = client->Start({{.buffer_results = true}});
  ASSERT_TRUE(start_response.is_ok());
  // Start some threads;
  std::thread t1{MakeWork};
  std::thread t2{MakeWork};
  std::thread t3{MakeWork};
  t1.detach();
  t2.detach();
  t3.detach();
  // Get Some samples
  sleep(1);

  auto stop_response = client->Stop();
  ASSERT_TRUE(stop_response.is_ok());
  ASSERT_TRUE(stop_response.value().samples_collected().has_value());
  EXPECT_GT(stop_response.value().samples_collected().value(), size_t{10});
  auto [pids, tids] = GetOutputKoids(std::move(in_socket));

  auto reset_response = client->Reset();
  ASSERT_TRUE(reset_response.is_ok());

  // We should only have one pid
  EXPECT_EQ(size_t{1}, pids.size());

  // We should only have more than one thread
  EXPECT_GT(tids.size(), size_t{1});
}

// Monitor ourself via our job id
TEST(ProfilerIntegrationTest, OwnJobId) {
  zx::result client_end = component::Connect<fuchsia_cpu_profiler::Session>();
  ASSERT_TRUE(client_end.is_ok());
  const fidl::SyncClient client{std::move(*client_end)};

  zx::socket in_socket;
  zx::socket outgoing_socket;

  ASSERT_EQ(zx::socket::create(0u, &in_socket, &outgoing_socket), ZX_OK);
  zx::unowned_job self = zx::job::default_job();

  zx_info_handle_basic_t info;
  zx_status_t info_result =
      self->get_info(ZX_INFO_HANDLE_BASIC, &info, sizeof(info), nullptr, nullptr);
  ASSERT_EQ(ZX_OK, info_result);

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

  // We'll sample ourself by our job id
  fuchsia_cpu_profiler::TargetConfig target_config = fuchsia_cpu_profiler::TargetConfig::WithTasks(
      std::vector{fuchsia_cpu_profiler::Task::WithJob(info.koid)});

  fuchsia_cpu_profiler::Config config{{
      .configs = std::vector{sampling_config},
      .target = target_config,
  }};

  auto config_response = client->Configure({{
      .output = std::move(outgoing_socket),
      .config = config,
  }});
  ASSERT_TRUE(config_response.is_ok());

  auto start_response = client->Start({{.buffer_results = true}});

  std::thread t1{MakeWork};
  std::thread t2{MakeWork};
  std::thread t3{MakeWork};
  t1.detach();
  t2.detach();
  t3.detach();
  ASSERT_TRUE(start_response.is_ok());
  // Get Some samples
  sleep(1);

  auto stop_response = client->Stop();
  ASSERT_TRUE(stop_response.is_ok());
  ASSERT_TRUE(stop_response.value().samples_collected().has_value());
  ASSERT_GT(stop_response.value().samples_collected().value(), size_t{10});
  auto [pids, tids] = GetOutputKoids(std::move(in_socket));
  auto reset_response = client->Reset();
  ASSERT_TRUE(reset_response.is_ok());

  // We should only have one pid
  EXPECT_EQ(size_t{1}, pids.size());

  // And that pid should be us
  zx::unowned_process process_self = zx::process::self();
  zx_info_handle_basic_t process_info;
  ASSERT_EQ(ZX_OK, process_self->get_info(ZX_INFO_HANDLE_BASIC, &process_info, sizeof(process_info),
                                          nullptr, nullptr));
  EXPECT_EQ(*pids.begin(), process_info.koid);
}

// Monitor ourself via our job id and then launch a process as part of our job and check that it
// gets added to the profiling set
TEST(ProfilerIntegrationTest, LaunchedProcess) {
  zx::result client_end = component::Connect<fuchsia_cpu_profiler::Session>();
  ASSERT_TRUE(client_end.is_ok());
  const fidl::SyncClient client{std::move(*client_end)};

  zx::socket in_socket;
  zx::socket outgoing_socket;

  ASSERT_EQ(zx::socket::create(0u, &in_socket, &outgoing_socket), ZX_OK);
  zx::unowned_job self = zx::job::default_job();

  zx_info_handle_basic_t info;
  zx_status_t info_result =
      self->get_info(ZX_INFO_HANDLE_BASIC, &info, sizeof(info), nullptr, nullptr);
  ASSERT_EQ(ZX_OK, info_result);

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

  // We'll sample ourself by our job id
  fuchsia_cpu_profiler::TargetConfig target_config = fuchsia_cpu_profiler::TargetConfig::WithTasks(
      std::vector{fuchsia_cpu_profiler::Task::WithJob(info.koid)});

  fuchsia_cpu_profiler::Config config{{
      .configs = std::vector{sampling_config},
      .target = target_config,
  }};

  // Launch an additional process before starting
  zx::process process1;
  const char* kArgs[] = {"/pkg/bin/demo_target", nullptr};
  ASSERT_EQ(ZX_OK, fdio_spawn(self->get(), FDIO_SPAWN_CLONE_ALL, "/pkg/bin/demo_target", kArgs,
                              process1.reset_and_get_address()));

  size_t num_processes;
  self->get_info(ZX_INFO_JOB_PROCESSES, nullptr, 0, nullptr, &num_processes);
  ASSERT_EQ(num_processes, size_t{2});

  auto config_response = client->Configure({{
      .output = std::move(outgoing_socket),
      .config = config,
  }});
  ASSERT_TRUE(config_response.is_ok());

  auto start_response = client->Start({{.buffer_results = true}});
  ASSERT_TRUE(start_response.is_ok());

  // Launch a thread in our process to ensure we get samples that aren't
  // just this process sleeping
  std::thread t1{MakeWork};
  t1.detach();

  // Then launch another process after starting
  zx::process process2;
  ASSERT_EQ(ZX_OK, fdio_spawn(self->get(), FDIO_SPAWN_CLONE_ALL, "/pkg/bin/demo_target", kArgs,
                              process2.reset_and_get_address()));

  self->get_info(ZX_INFO_JOB_PROCESSES, nullptr, 0, nullptr, &num_processes);
  ASSERT_EQ(num_processes, size_t{3});
  // Get Some samples
  sleep(1);

  auto stop_response = client->Stop();
  ASSERT_TRUE(stop_response.is_ok());
  ASSERT_TRUE(stop_response.value().samples_collected().has_value());
  ASSERT_GT(stop_response.value().samples_collected().value(), size_t{10});
  auto [pids, tids] = GetOutputKoids(std::move(in_socket));
  auto reset_response = client->Reset();
  ASSERT_TRUE(reset_response.is_ok());

  // We should three pids, our pid, the pid of process1, and the pid of process2
  zx_info_handle_basic_t pid_info;
  ASSERT_EQ(ZX_OK, zx::process::self()->get_info(ZX_INFO_HANDLE_BASIC, &pid_info, sizeof(pid_info),
                                                 nullptr, nullptr));
  zx_koid_t our_pid = pid_info.koid;
  ASSERT_EQ(ZX_OK,
            process1.get_info(ZX_INFO_HANDLE_BASIC, &pid_info, sizeof(pid_info), nullptr, nullptr));
  zx_koid_t process1_pid = pid_info.koid;
  ASSERT_EQ(ZX_OK,
            process2.get_info(ZX_INFO_HANDLE_BASIC, &pid_info, sizeof(pid_info), nullptr, nullptr));
  zx_koid_t process2_pid = pid_info.koid;
  EXPECT_EQ(size_t{3}, pids.size());
  EXPECT_TRUE(pids.find(our_pid) != pids.end());
  EXPECT_TRUE(pids.find(process1_pid) != pids.end());
  EXPECT_TRUE(pids.find(process2_pid) != pids.end());
  process1.kill();
  process2.kill();
}

// Monitor ourself via our job id and then launch a process as part of our job and check that it we
// see the threads it spawns
TEST(ProfilerIntegrationTest, LaunchedProcessThreadSpawner) {
  zx::result client_end = component::Connect<fuchsia_cpu_profiler::Session>();
  ASSERT_TRUE(client_end.is_ok());
  const fidl::SyncClient client{std::move(*client_end)};

  zx::socket in_socket;
  zx::socket outgoing_socket;

  ASSERT_EQ(zx::socket::create(0u, &in_socket, &outgoing_socket), ZX_OK);
  zx::unowned_job self = zx::job::default_job();

  zx_info_handle_basic_t info;
  zx_status_t info_result =
      self->get_info(ZX_INFO_HANDLE_BASIC, &info, sizeof(info), nullptr, nullptr);
  ASSERT_EQ(ZX_OK, info_result);

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

  // We'll sample ourself by our job id
  fuchsia_cpu_profiler::TargetConfig target_config = fuchsia_cpu_profiler::TargetConfig::WithTasks(
      std::vector{fuchsia_cpu_profiler::Task::WithJob(info.koid)});

  fuchsia_cpu_profiler::Config config{{
      .configs = std::vector{sampling_config},
      .target = target_config,
  }};

  auto config_response = client->Configure({{
      .output = std::move(outgoing_socket),
      .config = config,
  }});
  ASSERT_TRUE(config_response.is_ok());

  auto start_response = client->Start({{.buffer_results = true}});
  ASSERT_TRUE(start_response.is_ok());

  // Launch the thread spawner process after starting
  zx::process process;
  const char* kArgs[] = {"/pkg/bin/thread_spawner", nullptr};

  ASSERT_EQ(ZX_OK, fdio_spawn(self->get(), FDIO_SPAWN_CLONE_ALL, "/pkg/bin/thread_spawner", kArgs,
                              process.reset_and_get_address()));
  // Get Some samples
  sleep(2);

  auto stop_response = client->Stop();
  ASSERT_TRUE(stop_response.is_ok());
  ASSERT_TRUE(stop_response.value().samples_collected().has_value());
  ASSERT_GT(stop_response.value().samples_collected().value(), size_t{10});
  auto [pids, tids] = GetOutputKoids(std::move(in_socket));
  auto reset_response = client->Reset();
  ASSERT_TRUE(reset_response.is_ok());

  // We should have many sampled threads
  EXPECT_GT(tids.size(), size_t{10});

  process.kill();
}

// Monitor a component via moniker. Since we're running in the test realm, we only have access to
// our children components.
TEST(ProfilerIntegrationTest, ComponentByMoniker) {
  zx::result client_end = component::Connect<fuchsia_cpu_profiler::Session>();
  ASSERT_TRUE(client_end.is_ok());
  const fidl::SyncClient client{std::move(*client_end)};

  zx::socket in_socket;
  zx::socket outgoing_socket;

  ASSERT_EQ(zx::socket::create(0u, &in_socket, &outgoing_socket), ZX_OK);
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

  fuchsia_cpu_profiler::TargetConfig target_config =
      fuchsia_cpu_profiler::TargetConfig::WithComponent(
          fuchsia_cpu_profiler::ComponentConfig{{.moniker = "demo_target"}});

  fuchsia_cpu_profiler::TargetConfig no_such_target_config =
      fuchsia_cpu_profiler::TargetConfig::WithComponent(
          fuchsia_cpu_profiler::ComponentConfig{{.moniker = "doesntexist"}});

  fuchsia_cpu_profiler::Config no_such_moniker_config{{
      .configs = std::vector{sampling_config},
      .target = no_such_target_config,
  }};

  fuchsia_cpu_profiler::Config demo_target_config{{
      .configs = std::vector{sampling_config},
      .target = target_config,
  }};

  zx::socket outgoing_socket2;
  zx_status_t duplicate_result = outgoing_socket.duplicate(ZX_RIGHT_SAME_RIGHTS, &outgoing_socket2);
  ASSERT_EQ(ZX_OK, duplicate_result);

  auto bad_config_response = client->Configure({{
      .output = std::move(outgoing_socket2),
      .config = no_such_moniker_config,
  }});
  EXPECT_TRUE(bad_config_response.is_error());
  EXPECT_TRUE(bad_config_response.error_value().is_domain_error());
  EXPECT_EQ(bad_config_response.error_value().domain_error(),
            fuchsia_cpu_profiler::SessionConfigureError::kInvalidConfiguration);

  auto config_response = client->Configure({{
      .output = std::move(outgoing_socket),
      .config = demo_target_config,
  }});

  ASSERT_TRUE(config_response.is_ok());

  auto start_response = client->Start({{.buffer_results = true}});
  ASSERT_TRUE(start_response.is_ok());

  // Get Some samples
  sleep(1);

  auto stop_response = client->Stop();
  ASSERT_TRUE(stop_response.is_ok());
  ASSERT_TRUE(stop_response.value().samples_collected().has_value());
  ASSERT_GT(stop_response.value().samples_collected().value(), size_t{10});
  auto [pids, tids] = GetOutputKoids(std::move(in_socket));
  auto reset_response = client->Reset();
  ASSERT_TRUE(reset_response.is_ok());

  // We should have only one thread and one process
  EXPECT_EQ(tids.size(), size_t{1});
  EXPECT_EQ(pids.size(), size_t{1});
}

TEST(ProfilerIntegrationTest, LaunchedComponent) {
  zx::result client_end = component::Connect<fuchsia_cpu_profiler::Session>();
  ASSERT_TRUE(client_end.is_ok());
  const fidl::SyncClient client{std::move(*client_end)};

  zx::socket in_socket, outgoing_socket;
  ASSERT_EQ(zx::socket::create(0u, &in_socket, &outgoing_socket), ZX_OK);

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

  fuchsia_cpu_profiler::TargetConfig target_config =
      fuchsia_cpu_profiler::TargetConfig::WithComponent(fuchsia_cpu_profiler::ComponentConfig{{
          .url = "demo_target#meta/demo_target.cm",
          .moniker = "./launchpad:demo_target",
      }});

  fuchsia_cpu_profiler::Config config{{
      .configs = std::vector{sampling_config},
      .target = target_config,
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
