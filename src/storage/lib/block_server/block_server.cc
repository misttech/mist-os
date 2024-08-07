// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/storage/lib/block_server/block_server.h"

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
                  [](void* context, const internal::Session* session, const Request* requests,
                     uintptr_t request_count) {
                    reinterpret_cast<BlockServer*>(context)->interface_->OnRequests(
                        reinterpret_cast<Session&>(session),
                        cpp20::span<const Request>(requests, request_count));
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

void Session::SendReply(RequestId request_id, zx::result<> result) {
  block_server_send_reply(session_, request_id, result.status_value());
}

BlockServer::~BlockServer() {
  if (server_) {
    block_server_delete(server_);
  }
}

void BlockServer::Serve(fidl::ServerEnd<fuchsia_hardware_block_volume::Volume> server_end) {
  block_server_serve(server_, server_end.TakeChannel().release());
}

}  // namespace block_server
