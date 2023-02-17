// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/bringup/bin/netsvc/args.h"

#include <fidl/fuchsia.boot/cpp/wire.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/loop.h>
#include <lib/async/dispatcher.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/fdio/spawn.h>
#include <lib/fit/defer.h>
#include <lib/fit/function.h>
#include <lib/sync/completion.h>
#include <lib/zx/process.h>

#include <mock-boot-arguments/server.h>
#include <zxtest/zxtest.h>

namespace {
constexpr char kInterface[] = "/dev/whatever/whatever";

TEST(ArgsTest, NetsvcNodenamePrintsAndExits) {
  const std::string path = "/pkg/bin/netsvc";
  const char* argv[] = {path.c_str(), "--nodename", nullptr};
  zx::process process;
  char err_msg[FDIO_SPAWN_ERR_MSG_MAX_LENGTH];

  ASSERT_OK(fdio_spawn_etc(ZX_HANDLE_INVALID, FDIO_SPAWN_CLONE_ALL, argv[0], argv, nullptr, 0,
                           nullptr, process.reset_and_get_address(), err_msg),
            "%s", err_msg);

  ASSERT_OK(process.wait_one(ZX_TASK_TERMINATED, zx::time::infinite(), nullptr));
  zx_info_process_t proc_info;
  ASSERT_OK(process.get_info(ZX_INFO_PROCESS, &proc_info, sizeof(proc_info), nullptr, nullptr));
  ASSERT_EQ(proc_info.return_code, 0);
}

class FakeSvc {
 public:
  explicit FakeSvc(async_dispatcher_t* dispatcher) : outgoing_(dispatcher) {
    ASSERT_OK(outgoing_.AddUnmanagedProtocol<fuchsia_boot::Arguments>(
        [this, dispatcher](fidl::ServerEnd<fuchsia_boot::Arguments> server_end) {
          fidl::BindServer(dispatcher, std::move(server_end), &mock_boot_);
        }));

    zx::result endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
    ASSERT_OK(endpoints);
    auto& [client_end, server_end] = endpoints.value();
    zx::result result = outgoing_.Serve(std::move(server_end));
    ASSERT_OK(result);
    root_.Bind(std::move(client_end));
  }

  mock_boot_arguments::Server& mock_boot() { return mock_boot_; }

  zx::result<fidl::ClientEnd<fuchsia_io::Directory>> svc() {
    zx::result endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
    if (endpoints.is_error()) {
      return endpoints.take_error();
    }
    auto& [client_end, server_end] = endpoints.value();
    const fidl::OneWayStatus status =
        root_->Open({}, {}, component::OutgoingDirectory::kServiceDirectory,
                    fidl::ServerEnd<fuchsia_io::Node>{server_end.TakeChannel()});
    return zx::make_result(status.status(), std::move(client_end));
  }

 private:
  component::OutgoingDirectory outgoing_;
  mock_boot_arguments::Server mock_boot_;
  fidl::WireSyncClient<fuchsia_io::Directory> root_;
};

class ArgsTest : public zxtest::Test {
 public:
  ArgsTest() : loop_(&kAsyncLoopConfigNeverAttachToThread), fake_svc_(loop_.dispatcher()) {}

  FakeSvc& fake_svc() { return fake_svc_; }
  zx::result<fidl::ClientEnd<fuchsia_io::Directory>> svc_root() { return fake_svc_.svc(); }
  async::Loop& loop() { return loop_; }

 private:
  async::Loop loop_;
  FakeSvc fake_svc_;
};

TEST_F(ArgsTest, NetsvcNoneProvided) {
  int argc = 1;
  const char* argv[] = {"netsvc"};
  const char* error = nullptr;
  NetsvcArgs args;
  zx::result svc = svc_root();
  ASSERT_OK(svc);
  {
    ASSERT_OK(loop().StartThread());
    auto cleanup = fit::defer(fit::bind_member<&async::Loop::Shutdown>(&loop()));
    ASSERT_EQ(ParseArgs(argc, const_cast<char**>(argv), svc.value(), &error, &args), 0, "%s",
              error);
  }
  ASSERT_FALSE(args.netboot);
  ASSERT_FALSE(args.print_nodename_and_exit);
  ASSERT_TRUE(args.advertise);
  ASSERT_FALSE(args.all_features);
  ASSERT_TRUE(args.interface.empty());
  ASSERT_EQ(error, nullptr);
}

TEST_F(ArgsTest, NetsvcAllProvided) {
  int argc = 7;
  const char* argv[] = {
      "netsvc",         "--netboot",   "--nodename", "--advertise",
      "--all-features", "--interface", kInterface,
  };
  const char* error = nullptr;
  NetsvcArgs args;
  zx::result svc = svc_root();
  ASSERT_OK(svc);
  {
    ASSERT_OK(loop().StartThread());
    auto cleanup = fit::defer(fit::bind_member<&async::Loop::Shutdown>(&loop()));
    ASSERT_EQ(ParseArgs(argc, const_cast<char**>(argv), svc.value(), &error, &args), 0, "%s",
              error);
  }
  ASSERT_TRUE(args.netboot);
  ASSERT_TRUE(args.print_nodename_and_exit);
  ASSERT_TRUE(args.advertise);
  ASSERT_TRUE(args.all_features);
  ASSERT_EQ(args.interface, std::string(kInterface));
  ASSERT_EQ(error, nullptr);
}

TEST_F(ArgsTest, NetsvcValidation) {
  int argc = 2;
  const char* argv[] = {
      "netsvc",
      "--interface",
  };
  const char* error = nullptr;
  NetsvcArgs args;
  zx::result svc = svc_root();
  ASSERT_OK(svc);
  {
    ASSERT_OK(loop().StartThread());
    auto cleanup = fit::defer(fit::bind_member<&async::Loop::Shutdown>(&loop()));
    ASSERT_LT(ParseArgs(argc, const_cast<char**>(argv), svc.value(), &error, &args), 0);
  }
  ASSERT_TRUE(args.interface.empty());
  ASSERT_TRUE(strstr(error, "interface"));
}

TEST_F(ArgsTest, LogPackets) {
  int argc = 2;
  const char* argv[] = {
      "netsvc",
      "--log-packets",
  };
  NetsvcArgs args;
  EXPECT_FALSE(args.log_packets);
  const char* error = nullptr;
  zx::result svc = svc_root();
  ASSERT_OK(svc);
  {
    ASSERT_OK(loop().StartThread());
    auto cleanup = fit::defer(fit::bind_member<&async::Loop::Shutdown>(&loop()));
    ASSERT_EQ(ParseArgs(argc, const_cast<char**>(argv), svc.value(), &error, &args), 0, "%s",
              error);
  }
  EXPECT_TRUE(args.log_packets);
}

}  // namespace
