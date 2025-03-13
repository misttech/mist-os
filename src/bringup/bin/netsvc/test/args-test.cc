// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/bringup/bin/netsvc/args.h"

#include <fidl/fuchsia.boot/cpp/wire.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/loop.h>
#include <lib/async/cpp/task.h>
#include <lib/async/dispatcher.h>
#include <lib/component/incoming/cpp/directory.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/fdio/spawn.h>
#include <lib/fit/defer.h>
#include <lib/fit/function.h>
#include <lib/sync/completion.h>
#include <lib/zx/process.h>

#include <mock-boot-arguments/server.h>
#include <zxtest/zxtest.h>

#include "src/bringup/bin/netsvc/netsvc_structured_config.h"

namespace {
constexpr char kInterface[] = "/dev/whatever/whatever";

class FakeSvc {
 public:
  explicit FakeSvc(async_dispatcher_t* dispatcher) {
    zx::result server_end = fidl::CreateEndpoints(&root_);
    ASSERT_OK(server_end);
    async::PostTask(dispatcher, [dispatcher, &mock_boot = mock_boot_,
                                 server_end = std::move(server_end.value())]() mutable {
      component::OutgoingDirectory outgoing{dispatcher};
      ASSERT_OK(outgoing.AddUnmanagedProtocol<fuchsia_boot::Arguments>(
          [&mock_boot, dispatcher](fidl::ServerEnd<fuchsia_boot::Arguments> server_end) {
            fidl::BindServer(dispatcher, std::move(server_end), &mock_boot);
          }));
      zx::result result = outgoing.Serve(std::move(server_end));
      ASSERT_OK(result);
      // Stash the outgoing directory on the dispatcher so that the dtor runs on the dispatcher
      // thread.
      async::PostDelayedTask(
          dispatcher, [outgoing = std::move(outgoing)]() {}, zx::duration::infinite());
    });
  }

  mock_boot_arguments::Server& mock_boot() { return mock_boot_; }

  zx::result<fidl::ClientEnd<fuchsia_io::Directory>> svc() {
    return component::OpenDirectoryAt(root_, component::OutgoingDirectory::kServiceDirectory);
  }

 private:
  mock_boot_arguments::Server mock_boot_;
  fidl::ClientEnd<fuchsia_io::Directory> root_;
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
  auto config = netsvc_structured_config::Config();
  NetsvcArgs args;
  zx::result svc = svc_root();
  ASSERT_OK(svc);
  {
    ASSERT_OK(loop().StartThread());
    auto cleanup = fit::defer(fit::bind_member<&async::Loop::Shutdown>(&loop()));
    ASSERT_EQ(ParseArgs(argc, const_cast<char**>(argv), config, svc.value(), &error, &args), 0,
              "%s", error);
  }
  ASSERT_FALSE(args.netboot);
  ASSERT_FALSE(args.print_nodename_and_exit);
  ASSERT_TRUE(args.advertise);
  ASSERT_FALSE(args.all_features);
  ASSERT_TRUE(args.interface.empty());
  ASSERT_EQ(error, nullptr);
}

TEST_F(ArgsTest, NetsvcStructuredConfigProvided) {
  int argc = 1;
  const char* argv[] = {"netsvc"};
  const char* error = nullptr;
  auto config = netsvc_structured_config::Config();
  config.primary_interface() = kInterface;
  NetsvcArgs args;
  zx::result svc = svc_root();
  ASSERT_OK(svc);
  {
    ASSERT_OK(loop().StartThread());
    auto cleanup = fit::defer(fit::bind_member<&async::Loop::Shutdown>(&loop()));
    ASSERT_EQ(ParseArgs(argc, const_cast<char**>(argv), config, svc.value(), &error, &args), 0,
              "%s", error);
  }
  ASSERT_FALSE(args.netboot);
  ASSERT_FALSE(args.print_nodename_and_exit);
  ASSERT_TRUE(args.advertise);
  ASSERT_FALSE(args.all_features);
  ASSERT_EQ(args.interface, kInterface);
  ASSERT_EQ(error, nullptr);
}

TEST_F(ArgsTest, NetsvcAllProvided) {
  int argc = 7;
  const char* argv[] = {
      "netsvc",         "--netboot",   "--nodename", "--advertise",
      "--all-features", "--interface", kInterface,
  };
  auto config = netsvc_structured_config::Config();
  const char* error = nullptr;
  NetsvcArgs args;
  zx::result svc = svc_root();
  ASSERT_OK(svc);
  {
    ASSERT_OK(loop().StartThread());
    auto cleanup = fit::defer(fit::bind_member<&async::Loop::Shutdown>(&loop()));
    ASSERT_EQ(ParseArgs(argc, const_cast<char**>(argv), config, svc.value(), &error, &args), 0,
              "%s", error);
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
  auto config = netsvc_structured_config::Config();
  const char* error = nullptr;
  NetsvcArgs args;
  zx::result svc = svc_root();
  ASSERT_OK(svc);
  {
    ASSERT_OK(loop().StartThread());
    auto cleanup = fit::defer(fit::bind_member<&async::Loop::Shutdown>(&loop()));
    ASSERT_LT(ParseArgs(argc, const_cast<char**>(argv), config, svc.value(), &error, &args), 0);
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
  auto config = netsvc_structured_config::Config();
  NetsvcArgs args;
  EXPECT_FALSE(args.log_packets);
  const char* error = nullptr;
  zx::result svc = svc_root();
  ASSERT_OK(svc);
  {
    ASSERT_OK(loop().StartThread());
    auto cleanup = fit::defer(fit::bind_member<&async::Loop::Shutdown>(&loop()));
    ASSERT_EQ(ParseArgs(argc, const_cast<char**>(argv), config, svc.value(), &error, &args), 0,
              "%s", error);
  }
  EXPECT_TRUE(args.log_packets);
}

}  // namespace
