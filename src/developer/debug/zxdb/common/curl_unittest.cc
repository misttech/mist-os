// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/debug/zxdb/common/curl.h"

#include <arpa/inet.h>
#include <lib/syslog/cpp/macros.h>

#include <condition_variable>
#include <cstddef>
#include <mutex>
#include <string>
#include <thread>

#include <fbl/unique_fd.h>
#include <gtest/gtest.h>

#include "src/developer/debug/shared/message_loop_poll.h"
#include "src/lib/fxl/memory/ref_ptr.h"

namespace zxdb {

namespace {

// This is a simple HTTP server that accepts the connection, reads once, sends reply and closes.
class SimpleHttpServer {
 public:
  // Initialize a dummy server that never replies.
  SimpleHttpServer() = default;

  // Initialize with a reply.
  explicit SimpleHttpServer(std::string reply) : reply_(std::move(reply)) {}

  ~SimpleHttpServer() {
    if (server_fd_.is_valid()) {
      shutdown(server_fd_.get(), SHUT_RDWR);
      // On macOS, shutdown alone won't interrupt the accept call.
      server_fd_.reset();
      thread_.join();
    }
  }

  // Port is randomly assigned and is available after Serve() is called.
  uint16_t port() const { return port_; }

  void Serve() {
    FX_CHECK(!server_fd_.is_valid());

    server_fd_ = fbl::unique_fd(socket(AF_INET, SOCK_STREAM, 0));
    ASSERT_TRUE(server_fd_.is_valid());

    sockaddr_in addr{};
    addr.sin_family = AF_INET;
    addr.sin_addr.s_addr = INADDR_ANY;
    addr.sin_port = 0;
    socklen_t addrlen = sizeof(addr);
    ASSERT_EQ(0, bind(server_fd_.get(), reinterpret_cast<sockaddr*>(&addr), addrlen));
    ASSERT_EQ(0, getsockname(server_fd_.get(), reinterpret_cast<sockaddr*>(&addr), &addrlen));
    ASSERT_EQ(0, listen(server_fd_.get(), 1));

    port_ = ntohs(addr.sin_port);
    thread_ = std::thread(&SimpleHttpServer::Run, this);
  }

 private:
  void Run() {
    while (true) {
      sockaddr_in addr{};
      socklen_t addrlen = sizeof(addr);
      fbl::unique_fd conn(accept(server_fd_.get(), reinterpret_cast<sockaddr*>(&addr), &addrlen));
      if (!conn.is_valid()) {
        break;
      }
      std::byte buf[1024];
      FX_CHECK(read(conn.get(), buf, 1024) >= 0);
      if (reply_.empty()) {
        // Use the accept call to block this thread, which should only be interrupted by shutdown.
        FX_CHECK(accept(server_fd_.get(), reinterpret_cast<sockaddr*>(&addr), &addrlen) < 0);
        break;
      }
      std::string response =
          "HTTP/1.1 200 OK\r\n"
          "Content-Length: " +
          std::to_string(reply_.size()) + "\r\n\r\n" + reply_;
      write(conn.get(), response.data(), response.size());
    }
  }

  std::string reply_;
  uint16_t port_ = 0;
  fbl::unique_fd server_fd_;
  std::thread thread_;
};

// Perform against a hello_world server.
TEST(Curl, Perform) {
  const std::string message = "Hello, World!";
  SimpleHttpServer server(message);
  server.Serve();

  debug::MessageLoopPoll loop;
  loop.Init(nullptr);
  Curl::GlobalInit();

  auto curl = fxl::MakeRefCounted<zxdb::Curl>();
  std::string reply;
  curl->SetURL("http://127.0.0.1:" + std::to_string(server.port()));
  curl->set_data_callback([&](const std::string& received) {
    reply = received;
    return received.size();
  });
  curl->Perform([&](Curl* curl, Curl::Error err) {
    loop.QuitNow();
    ASSERT_FALSE(err) << err.ToString();
  });

  loop.Run();

  ASSERT_EQ(reply, message);
  Curl::GlobalCleanup();
  loop.Cleanup();
}

// Perform against a dummy server which hangs the connection forever.
// This tests the behavior of terminating the message loop when a transfer is in progress.
TEST(Curl, PerformDummy) {
  SimpleHttpServer dummy_server;
  dummy_server.Serve();

  debug::MessageLoopPoll loop;
  loop.Init(nullptr);
  Curl::GlobalInit();

  {
    auto curl = fxl::MakeRefCounted<zxdb::Curl>();
    std::string reply;
    curl->SetURL("http://127.0.0.1:" + std::to_string(dummy_server.port()));
    curl->Perform([&](Curl* curl, Curl::Error err) { FX_NOTREACHED(); });
  }

  loop.PostTimer(FROM_HERE, 10, [&]() { loop.QuitNow(); });
  loop.Run();

  Curl::GlobalCleanup();
  loop.Cleanup();
}

namespace {

std::condition_variable cond;
std::mutex mutex;

// Stops the given message loop when |remaining_threads| is 0.
void WaitForCondVar(debug::MessageLoop* loop, size_t& remaining_threads) {
  std::unique_lock lock(mutex);

  // Block until there are no more running threads. This will block |loop| until the condition
  // variable is signaled..
  cond.wait(lock, [&remaining_threads]() { return remaining_threads == 0; });

  loop->QuitNow();
}
}  // namespace

TEST(Curl, MultiThreadedPerform) {
  constexpr std::string message = "Hello world!";
  SimpleHttpServer server(message);
  server.Serve();

  debug::MessageLoopPoll loop;
  ASSERT_TRUE(loop.Init(nullptr));

  Curl::GlobalInit();

  constexpr size_t kNumThreads = 2;
  std::vector<std::thread> threads(kNumThreads);
  std::vector<std::string> replies;

  // Protected by |mutex| above.
  size_t remaining_threads = kNumThreads;

  // Each of these will have a unique thread_local MultiHandle, and explicitly cannot capture the
  // outer MessageLoop, which will conflict
  auto fn = [&remaining_threads, &replies, port = server.port()](int id) {
    // Each thread needs a message loop to service the curl objects.
    debug::MessageLoopPoll local_loop;
    ASSERT_TRUE(local_loop.Init(nullptr));

    auto curl = fxl::MakeRefCounted<Curl>();
    curl->SetURL("http://127.0.0.1:" + std::to_string(port));
    curl->set_data_callback([&](const std::string& data) -> size_t {
      std::lock_guard l(mutex);
      replies.push_back(data);
      return data.size();
    });
    curl->Perform([&](Curl* curl, Curl::Error err) {
      ASSERT_FALSE(err) << err.ToString();
      {
        std::lock_guard lock(mutex);
        --remaining_threads;
      }

      cond.notify_one();

      local_loop.QuitNow();
    });

    local_loop.Run();
    local_loop.Cleanup();
  };

  size_t c = 0;
  for (auto& thread : threads) {
    thread = std::thread(fn, c++);
  }

  loop.PostTask(FROM_HERE,
                [&loop, &remaining_threads]() { WaitForCondVar(&loop, remaining_threads); });

  loop.Run();

  // Once the loop returns, all threads should be finished and join immediately.
  for (auto& thread : threads) {
    thread.join();
  }

  for (const auto& reply : replies) {
    ASSERT_EQ(reply, message);
  }

  Curl::GlobalCleanup();
  loop.Cleanup();
}

}  // namespace

}  // namespace zxdb
