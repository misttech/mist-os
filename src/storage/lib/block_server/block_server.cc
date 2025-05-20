// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/storage/lib/block_server/block_server.h"

#include <zircon/assert.h>

#include "src/storage/lib/block_server/block_server_c.h"

namespace block_server {

BlockServer::BlockServer(const PartitionInfo& info, Interface* interface)
    : interface_(interface),
      server_(block_server_new(
          &info, internal::Callbacks{
                     .context = this,
                     .start_thread =
                         [](void* context, const void* arg) {
                           reinterpret_cast<BlockServer*>(context)->interface_->StartThread(
                               Thread(arg));
                         },
                     .on_new_session =
                         [](void* context, const internal::Session* session) {
                           reinterpret_cast<BlockServer*>(context)->interface_->OnNewSession(
                               Session(session));
                         },
                     .on_requests =
                         [](void* context, Request* requests, uintptr_t request_count) {
                           reinterpret_cast<BlockServer*>(context)->interface_->OnRequests(
                               std::span<Request>(requests, request_count));
                         },
                     .log =
                         [](void* context, const char* msg, size_t len) {
                           reinterpret_cast<BlockServer*>(context)->interface_->Log(
                               std::string_view(msg, len));
                         },
                 })) {}

Session& Session::operator=(Session&& other) {
  if (this == &other)
    return *this;
  if (session_)
    block_server_session_release(session_);
  session_ = other.session_;
  other.session_ = nullptr;
  return *this;
}

Session::~Session() {
  if (session_) {
    block_server_session_release(session_);
  }
}

void Session::Run() { block_server_session_run(session_); }

BlockServer::BlockServer(BlockServer&& other)
    : interface_(other.interface_), server_(other.server_) {
  other.interface_ = nullptr;
  other.server_ = nullptr;
}

BlockServer::~BlockServer() {
  if (server_) {
    block_server_delete(server_);
  }
}

void BlockServer::Serve(fidl::ServerEnd<fuchsia_hardware_block_volume::Volume> server_end) {
  block_server_serve(server_, server_end.TakeChannel().release());
}

void BlockServer::SendReply(RequestId request_id, zx::result<> result) const {
  block_server_send_reply(server_, request_id, result.status_value());
}

Request SplitRequest(Request& request, uint32_t block_offset, uint32_t block_size) {
  Request head = request;
  switch (request.operation.tag) {
    case Operation::Tag::Read:
    case Operation::Tag::Write:
      request.operation.read.vmo_offset += static_cast<uint64_t>(block_offset) * block_size;
      break;
    case Operation::Tag::Trim:
      break;
    case Operation::Tag::Flush:
    case Operation::Tag::CloseVmo:
      ZX_PANIC("Can't split Flush or CloseVmo operations");
  }
  head.operation.read.block_count = block_offset;
  request.operation.read.device_block_offset += block_offset;
  request.operation.read.block_count -= block_offset;
  return head;
}

zx_status_t CheckIoRange(const Request& request, uint64_t total_block_count) {
  uint64_t start;
  uint64_t length;
  switch (request.operation.tag) {
    case Operation::Tag::Read:
      start = request.operation.read.device_block_offset;
      length = request.operation.read.block_count;
      break;
    case Operation::Tag::Write:
      start = request.operation.write.device_block_offset;
      length = request.operation.write.block_count;
      break;
    case Operation::Tag::Trim:
      start = request.operation.trim.device_block_offset;
      length = request.operation.trim.block_count;
      break;
    case Operation::Tag::Flush:
    case Operation::Tag::CloseVmo:
      return ZX_OK;
  }
  if (length == 0 || length > total_block_count) {
    return ZX_ERR_OUT_OF_RANGE;
  }
  if (start >= total_block_count || start > total_block_count - length) {
    return ZX_ERR_OUT_OF_RANGE;
  }
  return ZX_OK;
}

}  // namespace block_server
