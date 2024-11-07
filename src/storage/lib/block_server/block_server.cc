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
          reinterpret_cast<const internal::PartitionInfo*>(&info),
          internal::Callbacks{
              .context = this,
              .start_thread =
                  [](void* context, const void* arg) {
                    reinterpret_cast<BlockServer*>(context)->interface_->StartThread(Thread(arg));
                  },
              .on_new_session =
                  [](void* context, const internal::Session* session) {
                    reinterpret_cast<BlockServer*>(context)->interface_->OnNewSession(
                        Session(session));
                  },
              .on_requests =
                  [](void* context, const internal::Session* session, Request* requests,
                     uintptr_t request_count) {
                    // Use a union so that the destructor for session does not run.
                    union U {
                      U(const internal::Session* session) : session(session) {}
                      ~U() {}
                      Session session;
                    } u(session);
                    reinterpret_cast<BlockServer*>(context)->interface_->OnRequests(
                        u.session, std::span<Request>(requests, request_count));
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

void Session::SendReply(RequestId request_id, zx::result<> result) const {
  block_server_send_reply(session_, request_id, result.status_value());
}

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

}  // namespace block_server
