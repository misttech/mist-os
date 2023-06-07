// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "server.h"

#include <lib/fit/defer.h>
#include <lib/sync/completion.h>
#include <unistd.h>

#include <thread>

#include <zxtest/zxtest.h>

#include "test/stub-block-device.h"

namespace {

class ServerTestFixture : public zxtest::Test {
 public:
  ServerTestFixture() : client_(blkdev_.proto()) {}

 protected:
  void SetUp() override;
  void TearDown() override;

  void CreateThread();
  void WaitForThreadStart();
  void WaitForThreadExit();
  void JoinThread();

  StubBlockDevice blkdev_;
  ddk::BlockProtocolClient client_;
  std::unique_ptr<Server> server_;

 private:
  sync_completion_t thread_started_;
  sync_completion_t thread_exited_;
  std::thread thread_;
};

void ServerTestFixture::SetUp() {
  zx::result server = Server::Create(&client_);
  ASSERT_OK(server);
  server_ = std::move(server.value());
}

void ServerTestFixture::TearDown() { ASSERT_FALSE(thread_.joinable()); }

void ServerTestFixture::CreateThread() {
  thread_ = std::thread([this]() {
    sync_completion_signal(&thread_started_);
    [[maybe_unused]] zx_status_t status = server_->Serve();
    sync_completion_signal(&thread_exited_);
    return 0;
  });
}

void ServerTestFixture::WaitForThreadStart() {
  ASSERT_OK(sync_completion_wait(&thread_started_, ZX_SEC(5)));
}

void ServerTestFixture::WaitForThreadExit() {
  ASSERT_OK(sync_completion_wait(&thread_exited_, ZX_SEC(5)));
}

void ServerTestFixture::JoinThread() { thread_.join(); }

TEST_F(ServerTestFixture, CreateServer) {}

TEST_F(ServerTestFixture, StartServer) {
  CreateThread();
  WaitForThreadStart();

  // This code is racy with Serve() being called. This is expected.
  // The server should handle shutdown commands at any time.
  server_->Shutdown();

  WaitForThreadExit();
  JoinThread();
}

TEST_F(ServerTestFixture, SplitRequestAfterFailedRequestReturnsFailure) {
  zx::result fifo_result = server_->GetFifo();
  ASSERT_OK(fifo_result);
  fzl::fifo<block_fifo_request_t, block_fifo_response_t> fifo(std::move(fifo_result.value()));
  CreateThread();
  auto cleanup = fit::defer([&] {
    server_->Shutdown();
    JoinThread();
  });
  zx::vmo vmo;
  constexpr int kTestBlockCount = 257;
  ASSERT_OK(zx::vmo::create(kTestBlockCount * kBlockSize, 0, &vmo));
  zx::result vmoid = server_->AttachVmo(std::move(vmo));
  ASSERT_OK(vmoid);

  block_fifo_request_t request = {
      .command = {.opcode = BLOCK_OPCODE_WRITE, .flags = BLOCK_IO_FLAG_GROUP_ITEM},
      .reqid = 100,
      .group = 5,
      .vmoid = vmoid.value(),
      .length = 4,
      .vmo_offset = 0,
      .dev_offset = 0,
  };

  blkdev_.set_callback([&](const block_op_t&) { return ZX_ERR_IO; });

  size_t actual_count = 0;
  ASSERT_OK(fifo.write(&request, 1, &actual_count));
  ASSERT_EQ(actual_count, 1);

  request = {
      .command = {.opcode = BLOCK_OPCODE_READ, .flags = 0},
      .reqid = 101,
      .vmoid = vmoid.value(),
  };
  ASSERT_OK(fifo.write(&request, 1, &actual_count));
  ASSERT_EQ(actual_count, 1);

  block_fifo_response_t response;
  zx_signals_t seen;
  ASSERT_OK(fifo.wait_one(ZX_FIFO_READABLE | ZX_FIFO_PEER_CLOSED, zx::time::infinite(), &seen));
  ASSERT_OK(fifo.read_one(&response));

  // Should get the response for the read.
  EXPECT_EQ(response.reqid, 101);

  request = {
      .command = {.opcode = BLOCK_OPCODE_WRITE,
                  .flags = BLOCK_IO_FLAG_GROUP_ITEM | BLOCK_IO_FLAG_GROUP_LAST},
      .reqid = 102,
      .group = 5,
      .vmoid = vmoid.value(),
      .length = kTestBlockCount,
      .vmo_offset = 0,
      .dev_offset = 0,
  };
  ASSERT_OK(fifo.write(&request, 1, &actual_count));
  ASSERT_EQ(actual_count, 1);

  // It's the last one so it should trigger a response.
  ASSERT_OK(fifo.wait_one(ZX_FIFO_READABLE | ZX_FIFO_PEER_CLOSED, zx::time::infinite(), &seen));
  ASSERT_OK(fifo.read_one(&response));

  EXPECT_EQ(response.reqid, 102);

  // Make sure the group is correctly cleaned up and able to be used for another request.
  blkdev_.set_callback({});

  block_fifo_request_t requests[] = {
      {
          .command = {.opcode = BLOCK_OPCODE_WRITE, .flags = BLOCK_IO_FLAG_GROUP_ITEM},
          .reqid = 103,
          .group = 5,
          .vmoid = vmoid.value(),
          .length = 257,
          .vmo_offset = 0,
          .dev_offset = 0,
      },
      {
          .command = {.opcode = BLOCK_OPCODE_WRITE,
                      .flags = BLOCK_IO_FLAG_GROUP_ITEM | BLOCK_IO_FLAG_GROUP_LAST},
          .reqid = 104,
          .group = 5,
          .vmoid = vmoid.value(),
          .length = 257,
          .vmo_offset = 0,
          .dev_offset = 0,
      },
  };
  ASSERT_OK(fifo.write(requests, 2, &actual_count));
  ASSERT_EQ(actual_count, 2);

  ASSERT_OK(fifo.wait_one(ZX_FIFO_READABLE | ZX_FIFO_PEER_CLOSED, zx::time::infinite(), &seen));
  ASSERT_OK(fifo.read_one(&response));

  EXPECT_EQ(response.reqid, 104);
}

}  // namespace
