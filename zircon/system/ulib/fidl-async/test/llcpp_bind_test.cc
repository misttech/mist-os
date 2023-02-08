// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fidl.test.simple/cpp/wire.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/fidl-async/cpp/bind.h>
#include <lib/sync/completion.h>

#include <zxtest/zxtest.h>

namespace {

class Server : public fidl::WireServer<fidl_test_simple::Simple> {
 public:
  explicit Server(sync_completion_t* destroyed) : destroyed_(destroyed) {}
  Server(Server&& other) = delete;
  Server(const Server& other) = delete;
  Server& operator=(Server&& other) = delete;
  Server& operator=(const Server& other) = delete;

  ~Server() override { sync_completion_signal(destroyed_); }

  void Echo(EchoRequestView request, EchoCompleter::Sync& completer) override {
    completer.Reply(request->request);
  }
  void Close(CloseCompleter::Sync& completer) override { completer.Close(ZX_OK); }

 private:
  sync_completion_t* destroyed_;
};

TEST(BindTestCase, UniquePtrDestroyOnClientClose) {
  sync_completion_t destroyed;
  async::Loop loop(&kAsyncLoopConfigNoAttachToCurrentThread);

  zx::result endpoints = fidl::CreateEndpoints<fidl_test_simple::Simple>();
  ASSERT_OK(endpoints);
  auto& [client_end, server_end] = endpoints.value();

  ASSERT_OK(fidl::BindSingleInFlightOnly(loop.dispatcher(), std::move(server_end),
                                         std::make_unique<Server>(&destroyed)));
  loop.RunUntilIdle();
  ASSERT_FALSE(sync_completion_signaled(&destroyed));

  client_end.reset();
  loop.RunUntilIdle();
  ASSERT_OK(sync_completion_wait(&destroyed, ZX_TIME_INFINITE));
}

TEST(BindTestCase, UniquePtrDestroyOnServerClose) {
  sync_completion_t destroyed;
  async::Loop loop(&kAsyncLoopConfigNoAttachToCurrentThread);

  zx::result endpoints = fidl::CreateEndpoints<fidl_test_simple::Simple>();
  ASSERT_OK(endpoints);
  auto& [client_end, server_end] = endpoints.value();

  ASSERT_OK(fidl::BindSingleInFlightOnly(loop.dispatcher(), std::move(server_end),
                                         std::make_unique<Server>(&destroyed)));
  ASSERT_FALSE(sync_completion_signaled(&destroyed));

  // Launch a thread so we can make a blocking client call
  ASSERT_OK(loop.StartThread());

  const fidl::WireResult result = fidl::WireCall(client_end)->Close();
  ASSERT_STATUS(result.status(), ZX_ERR_PEER_CLOSED);
  // Make sure the other end closed
  ASSERT_OK(client_end.channel().wait_one(ZX_CHANNEL_PEER_CLOSED, zx::time{}, nullptr));
  ASSERT_OK(sync_completion_wait(&destroyed, ZX_TIME_INFINITE));
}

TEST(BindTestCase, CallbackDestroyOnClientClose) {
  sync_completion_t destroyed;
  auto server = std::make_unique<Server>(&destroyed);
  async::Loop loop(&kAsyncLoopConfigNoAttachToCurrentThread);

  zx::result endpoints = fidl::CreateEndpoints<fidl_test_simple::Simple>();
  ASSERT_OK(endpoints);
  auto& [client_end, server_end] = endpoints.value();

  ASSERT_OK(fidl::BindSingleInFlightOnly(
      loop.dispatcher(), std::move(server_end), server.release(),
      fidl::OnChannelClosedFn<Server>{[](Server* server) { delete server; }}));

  loop.RunUntilIdle();
  ASSERT_FALSE(sync_completion_signaled(&destroyed));

  client_end.reset();
  loop.RunUntilIdle();
  ASSERT_OK(sync_completion_wait(&destroyed, ZX_TIME_INFINITE));
}

TEST(BindTestCase, CallbackDestroyOnServerClose) {
  sync_completion_t destroyed;
  auto server = std::make_unique<Server>(&destroyed);
  async::Loop loop(&kAsyncLoopConfigNoAttachToCurrentThread);

  zx::result endpoints = fidl::CreateEndpoints<fidl_test_simple::Simple>();
  ASSERT_OK(endpoints);
  auto& [client_end, server_end] = endpoints.value();

  ASSERT_OK(fidl::BindSingleInFlightOnly(
      loop.dispatcher(), std::move(server_end), server.release(),
      fidl::OnChannelClosedFn<Server>{[](Server* server) { delete server; }}));

  ASSERT_FALSE(sync_completion_signaled(&destroyed));

  // Launch a thread so we can make a blocking client call
  ASSERT_OK(loop.StartThread());

  const fidl::WireResult result = fidl::WireCall(client_end)->Close();
  ASSERT_STATUS(result.status(), ZX_ERR_PEER_CLOSED);

  ASSERT_OK(sync_completion_wait(&destroyed, ZX_TIME_INFINITE));
  // Make sure the other end closed
  ASSERT_OK(client_end.channel().wait_one(ZX_CHANNEL_PEER_CLOSED, zx::time{}, nullptr));
}

TEST(BindTestCase, UnknownMethod) {
  sync_completion_t destroyed;
  async::Loop loop(&kAsyncLoopConfigNoAttachToCurrentThread);

  zx::result endpoints = fidl::CreateEndpoints<fidl_test_simple::Simple>();
  ASSERT_OK(endpoints);
  auto& [client_end, server_end] = endpoints.value();

  ASSERT_OK(fidl::BindSingleInFlightOnly(loop.dispatcher(), std::move(server_end),
                                         std::make_unique<Server>(&destroyed)));
  loop.RunUntilIdle();
  ASSERT_FALSE(sync_completion_signaled(&destroyed));

  // An epitaph is never a valid message to a server.
  fidl_epitaph_write(client_end.channel().get(), ZX_OK);

  loop.RunUntilIdle();
  ASSERT_OK(sync_completion_wait(&destroyed, ZX_TIME_INFINITE));
}

// These classes are used to create a server implementation with multiple
// inheritance.
class PlaceholderBase1 {
 public:
  virtual void Foo() = 0;
  int a;
};

class PlaceholderBase2 {
 public:
  virtual void Bar() = 0;
  int b;
};

class MultiInheritanceServer : public PlaceholderBase1,
                               public fidl::WireServer<fidl_test_simple::Simple>,
                               public PlaceholderBase2 {
 public:
  explicit MultiInheritanceServer(sync_completion_t* destroyed) : destroyed_(destroyed) {}
  MultiInheritanceServer(MultiInheritanceServer&& other) = delete;
  MultiInheritanceServer(const MultiInheritanceServer& other) = delete;
  MultiInheritanceServer& operator=(MultiInheritanceServer&& other) = delete;
  MultiInheritanceServer& operator=(const MultiInheritanceServer& other) = delete;

  ~MultiInheritanceServer() override { sync_completion_signal(destroyed_); }

  void Echo(EchoRequestView request, EchoCompleter::Sync& completer) override {
    completer.Reply(request->request);
  }
  void Close(CloseCompleter::Sync& completer) override { completer.Close(ZX_OK); }

  void Foo() override {}
  void Bar() override {}

 private:
  sync_completion_t* destroyed_;
};

TEST(BindTestCase, MultipleInheritanceServer) {
  sync_completion_t destroyed;
  async::Loop loop(&kAsyncLoopConfigNoAttachToCurrentThread);

  zx::result endpoints = fidl::CreateEndpoints<fidl_test_simple::Simple>();
  ASSERT_OK(endpoints);
  auto& [client_end, server_end] = endpoints.value();

  ASSERT_OK(fidl::BindSingleInFlightOnly(loop.dispatcher(), std::move(server_end),
                                         std::make_unique<Server>(&destroyed)));
  ASSERT_FALSE(sync_completion_signaled(&destroyed));

  // Launch a thread so we can make a blocking client call
  ASSERT_OK(loop.StartThread());

  const fidl::WireResult result = fidl::WireCall(client_end)->Close();
  ASSERT_STATUS(result.status(), ZX_ERR_PEER_CLOSED);
  // Make sure the other end closed
  ASSERT_OK(client_end.channel().wait_one(ZX_CHANNEL_PEER_CLOSED, zx::time{}, nullptr));
  ASSERT_OK(sync_completion_wait(&destroyed, ZX_TIME_INFINITE));
}

}  // namespace
