// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.block.volume/cpp/wire.h>

#include <span>
#include <unordered_set>

#include <gtest/gtest.h>

#include "src/storage/lib/block_client/cpp/remote_block_device.h"
#include "src/storage/lib/block_server/block_server.h"
#include "storage/buffer/owned_vmoid.h"

namespace block_server {
namespace {

namespace fvolume = ::fuchsia_hardware_block_volume;

constexpr uint64_t kBlocks = 1024;
constexpr uint64_t kBlockSize = 512;

class TestInterface : public Interface {
 public:
  int threads_running() const { return threads_running_; }

  void StartThread(Thread thread) override {
    std::thread([this, thread = std::move(thread)]() mutable {
      ++threads_running_;
      thread.Run();

      // Deliberately add a delay to increase the chances of catching regressions where the server
      // does not wait for the thread to terminate.
      usleep(1000);

      --threads_running_;
    }).detach();
  }

  void OnNewSession(Session session) override {
    std::thread([this, session = std::move(session)]() mutable {
      ++threads_running_;
      session.Run();

      // Deliberately add a delay to increase the chances of catching regressions where the server
      // does not wait for the thread to terminate.
      usleep(1000);

      --threads_running_;
    }).detach();
  }

  void OnRequests(const Session& session, std::span<Request> requests) override {
    for (const Request& request : requests) {
      switch (request.operation.tag) {
        case Operation::Tag::Read:
          EXPECT_EQ(
              request.vmo->write(&data_[request.operation.read.device_block_offset * kBlockSize],
                                 request.operation.read.vmo_offset,
                                 request.operation.read.block_count * kBlockSize),
              ZX_OK);
          break;

        case Operation::Tag::Write:
          EXPECT_EQ(
              request.vmo->read(&data_[request.operation.write.device_block_offset * kBlockSize],
                                request.operation.write.vmo_offset,
                                request.operation.write.block_count * kBlockSize),
              ZX_OK);
          break;

        default:
          ZX_PANIC("Unexpected operation");
      }
      session.SendReply(request.request_id, zx::ok());
    }
  }

 private:
  std::atomic<int> threads_running_ = 0;
  std::unique_ptr<uint8_t[]> data_ = std::make_unique<uint8_t[]>(kBlockSize * kBlocks);
};

TEST(BlockServer, Basic) {
  fidl::ServerEnd<fvolume::Volume> server_end;
  zx::result client_end = fidl::CreateEndpoints(&server_end);
  ASSERT_EQ(client_end.status_value(), ZX_OK);

  TestInterface test_interface;
  BlockServer block_server(
      PartitionInfo{
          .start_block = 0,
          .block_count = kBlocks,
          .block_size = kBlockSize,
          .type_guid = {1, 2, 3, 4},
          .instance_guid = {5, 6, 7, 8},
          .name = "partition",
      },
      &test_interface);

  block_server.Serve(std::move(server_end));

  auto client = block_client::RemoteBlockDevice::Create(*std::move(client_end));
  ASSERT_EQ(client.status_value(), ZX_OK);

  zx::vmo vmo;
  ASSERT_EQ(zx::vmo::create(4096, 0, &vmo), ZX_OK);
  ASSERT_EQ(vmo.write("hello", kBlockSize, 5), ZX_OK);
  storage::Vmoid vmoid;
  ASSERT_EQ(client->BlockAttachVmo(vmo, &vmoid), ZX_OK);
  storage::OwnedVmoid owned_vmoid(std::move(vmoid), (*client).get());

  block_fifo_request_t request = {
      .command =
          {
              .opcode = BLOCK_OPCODE_WRITE,
          },
      .vmoid = owned_vmoid.get(),
      .length = 1,
      .vmo_offset = 1,
      .dev_offset = 3,
  };

  ASSERT_EQ(client->FifoTransaction(&request, 1), ZX_OK);

  request.command.opcode = BLOCK_OPCODE_READ;
  request.vmo_offset = 2;
  ASSERT_EQ(client->FifoTransaction(&request, 1), ZX_OK);

  char buffer[6] = {};
  ASSERT_EQ(vmo.read(buffer, 2 * kBlockSize, sizeof(buffer)), ZX_OK);

  ASSERT_EQ(memcmp(buffer, "hello", 6), 0);
}

TEST(BlockServer, Termination) {
  fidl::ServerEnd<fvolume::Volume> server_end;
  zx::result client_end = fidl::CreateEndpoints(&server_end);
  ASSERT_EQ(client_end.status_value(), ZX_OK);

  TestInterface test_interface;

  std::unique_ptr<block_client::RemoteBlockDevice> client;

  {
    BlockServer block_server(
        PartitionInfo{
            .start_block = 0,
            .block_count = kBlocks,
            .block_size = kBlockSize,
            .type_guid = {1, 2, 3, 4},
            .instance_guid = {5, 6, 7, 8},
            .name = "partition",
        },
        &test_interface);

    block_server.Serve(std::move(server_end));

    auto client_result = block_client::RemoteBlockDevice::Create(*std::move(client_end));
    ASSERT_EQ(client_result.status_value(), ZX_OK);
    client = *std::move(client_result);
  }

  EXPECT_EQ(test_interface.threads_running(), 0);
}

TEST(BlockServer, AsyncTermination) {
  fidl::ServerEnd<fvolume::Volume> server_end;
  zx::result client_end = fidl::CreateEndpoints(&server_end);
  ASSERT_EQ(client_end.status_value(), ZX_OK);

  TestInterface test_interface;

  std::unique_ptr<block_client::RemoteBlockDevice> client;

  BlockServer block_server(
      PartitionInfo{
          .start_block = 0,
          .block_count = kBlocks,
          .block_size = kBlockSize,
          .type_guid = {1, 2, 3, 4},
          .instance_guid = {5, 6, 7, 8},
          .name = "partition",
      },
      &test_interface);

  block_server.Serve(std::move(server_end));

  auto client_result = block_client::RemoteBlockDevice::Create(*std::move(client_end));
  ASSERT_EQ(client_result.status_value(), ZX_OK);
  client = *std::move(client_result);

  sync_completion_t completion;
  std::move(block_server).DestroyAsync([&] {
    EXPECT_EQ(test_interface.threads_running(), 0);
    sync_completion_signal(&completion);
  });

  sync_completion_wait(&completion, ZX_TIME_INFINITE);
}

TEST(BlockServer, FailedOnNewSession) {
  class TestInterfaceWithFailedOnNewSession : public TestInterface {
   public:
    void OnNewSession(Session session) override {
      // Do nothing.
    }
  };

  fidl::ServerEnd<fvolume::Volume> server_end;
  zx::result client_end = fidl::CreateEndpoints(&server_end);
  ASSERT_EQ(client_end.status_value(), ZX_OK);

  TestInterfaceWithFailedOnNewSession test_interface;

  std::unique_ptr<block_client::RemoteBlockDevice> client;

  {
    BlockServer block_server(
        PartitionInfo{
            .start_block = 0,
            .block_count = kBlocks,
            .block_size = kBlockSize,
            .type_guid = {1, 2, 3, 4},
            .instance_guid = {5, 6, 7, 8},
            .name = "partition",
        },
        &test_interface);

    block_server.Serve(std::move(server_end));

    auto client_result = block_client::RemoteBlockDevice::Create(*std::move(client_end));
    EXPECT_EQ(client_result.status_value(), ZX_ERR_PEER_CLOSED);
  }

  EXPECT_EQ(test_interface.threads_running(), 0);
}

TEST(BlockServer, FullFifo) {
  fidl::ServerEnd<fvolume::Volume> server_end;
  zx::result client_end = fidl::CreateEndpoints(&server_end);
  ASSERT_EQ(client_end.status_value(), ZX_OK);

  TestInterface test_interface;
  BlockServer block_server(
      PartitionInfo{
          .start_block = 0,
          .block_count = kBlocks,
          .block_size = kBlockSize,
          .type_guid = {1, 2, 3, 4},
          .instance_guid = {5, 6, 7, 8},
          .name = "partition",
      },
      &test_interface);

  block_server.Serve(std::move(server_end));

  auto [session, server] = fidl::Endpoints<fuchsia_hardware_block::Session>::Create();
  ASSERT_TRUE(fidl::WireCall(*client_end)->OpenSession(std::move(server)).ok());
  const fidl::WireResult result = fidl::WireCall(session)->GetFifo();
  ASSERT_TRUE(result.ok());
  fit::result response = result.value();
  ASSERT_TRUE(response.is_ok());
  zx::fifo fifo = std::move(response->fifo);

  block_fifo_request_t request = {
      .command =
          {
              .opcode = BLOCK_OPCODE_WRITE,
          },
      .vmoid = 1,  // Invalid, but doesn't matter.
      .length = 1,
      .vmo_offset = 1,
      .dev_offset = 3,
  };

  // Write 1000 requests without removing the responses.
  for (uint32_t request_id = 0; request_id < 1000; ++request_id) {
    request.reqid = request_id;
    zx_status_t status;
    size_t actual;
    while ((status = fifo.write(sizeof(block_fifo_request_t), &request, 1, &actual)) ==
           ZX_ERR_SHOULD_WAIT) {
      zx_signals_t signals;
      fifo.wait_one(ZX_FIFO_WRITABLE, zx::time::infinite(), &signals);
    }
    ASSERT_EQ(actual, 1u);
    ASSERT_EQ(status, ZX_OK);
  }

  // Now make sure we receive the 1000 requests.
  std::unordered_set<uint32_t> received;
  for (int i = 0; i < 1000; ++i) {
    block_fifo_response_t response;
    zx_status_t status;
    size_t actual;
    while ((status = fifo.read(sizeof(block_fifo_response_t), &response, 1, &actual)) ==
           ZX_ERR_SHOULD_WAIT) {
      zx_signals_t signals;
      fifo.wait_one(ZX_FIFO_READABLE, zx::time::infinite(), &signals);
    }
    ASSERT_EQ(status, ZX_OK);
    EXPECT_TRUE(received.insert(response.reqid).second);
  }
}

TEST(BlockServer, Group) {
  fidl::ServerEnd<fvolume::Volume> server_end;
  zx::result client_end = fidl::CreateEndpoints(&server_end);
  ASSERT_EQ(client_end.status_value(), ZX_OK);

  TestInterface test_interface;
  BlockServer block_server(
      PartitionInfo{
          .start_block = 0,
          .block_count = kBlocks,
          .block_size = kBlockSize,
          .type_guid = {1, 2, 3, 4},
          .instance_guid = {5, 6, 7, 8},
          .name = "partition",
      },
      &test_interface);

  block_server.Serve(std::move(server_end));

  auto [session, server] = fidl::Endpoints<fuchsia_hardware_block::Session>::Create();
  ASSERT_TRUE(fidl::WireCall(*client_end)->OpenSession(std::move(server)).ok());

  zx::fifo fifo;
  {
    fidl::WireResult result = fidl::WireCall(session)->GetFifo();
    ASSERT_TRUE(result.ok());
    fit::result response = result.value();
    ASSERT_TRUE(response.is_ok());
    fifo = std::move(response->fifo);
  }

  zx::vmo vmo;
  ASSERT_EQ(zx::vmo::create(1024 * 1024, 0, &vmo), ZX_OK);
  zx::vmo duplicate;
  ASSERT_EQ(vmo.duplicate(ZX_RIGHT_SAME_RIGHTS, &duplicate), ZX_OK);

  uint16_t vmo_id;
  {
    fidl::WireResult result = fidl::WireCall(session)->AttachVmo(std::move(duplicate));
    ASSERT_TRUE(result.ok());
    fit::result response = result.value();
    ASSERT_TRUE(response.is_ok());
    vmo_id = std::move(response->vmoid.id);
  }

  block_fifo_request_t request = {
      .command =
          {
              .opcode = BLOCK_OPCODE_READ,
              .flags = BLOCK_IO_FLAG_GROUP_ITEM,
          },
      .group = 1234,
      .vmoid = vmo_id,
      .length = 1,
      .vmo_offset = 0,
      .dev_offset = 0,
  };

  // Write 1000 requests as a group.
  for (uint32_t request_id = 0; request_id < 1000; ++request_id) {
    request.reqid = request_id;
    if (request_id == 999)
      request.command.flags |= BLOCK_IO_FLAG_GROUP_LAST;
    zx_status_t status;
    size_t actual;
    while ((status = fifo.write(sizeof(block_fifo_request_t), &request, 1, &actual)) ==
           ZX_ERR_SHOULD_WAIT) {
      zx_signals_t signals;
      fifo.wait_one(ZX_FIFO_WRITABLE, zx::time::infinite(), &signals);
    }
    ASSERT_EQ(actual, 1u);
    ASSERT_EQ(status, ZX_OK);
  }

  block_fifo_response_t response;
  zx_status_t status;
  size_t actual;
  while ((status = fifo.read(sizeof(block_fifo_response_t), &response, 1, &actual)) ==
         ZX_ERR_SHOULD_WAIT) {
    zx_signals_t signals;
    fifo.wait_one(ZX_FIFO_READABLE, zx::time::infinite(), &signals);
  }
  ASSERT_EQ(status, ZX_OK);
  EXPECT_EQ(response.status, ZX_OK);
  EXPECT_EQ(response.group, 1234);
  EXPECT_EQ(response.reqid, 999u);
}

TEST(BlockServer, SplitRequest) {
  Request request = {.operation = {.tag = Operation::Tag::Read,
                                   .read = {
                                       .device_block_offset = 10,
                                       .block_count = 20,
                                       .vmo_offset = 4096,
                                   }}};

  Request head = SplitRequest(request, 5, 512);

  EXPECT_EQ(head.operation.read.device_block_offset, 10u);
  EXPECT_EQ(head.operation.read.block_count, 5u);
  EXPECT_EQ(head.operation.read.vmo_offset, 4096u);

  EXPECT_EQ(request.operation.read.device_block_offset, 15u);
  EXPECT_EQ(request.operation.read.block_count, 15u);
  EXPECT_EQ(request.operation.read.vmo_offset, 6656u);
}

}  // namespace
}  // namespace block_server
