// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "l2cap.h"

#include <lib/syslog/cpp/macros.h>

#include <cstdlib>

#include "src/connectivity/bluetooth/testing/bt-affordances/ffi_c/bindings.h"

using grpc::Status;
using grpc::StatusCode;

grpc::Status L2capService::Connect(::grpc::ServerContext* context,
                                   const ::pandora::l2cap::ConnectRequest* request,
                                   ::pandora::l2cap::ConnectResponse* response) {
  if (connect_l2cap_channel(
          std::strtoul(request->connection().cookie().value().c_str(), nullptr, 10),
          static_cast<uint16_t>(request->basic().psm())) != ZX_OK) {
    return Status(StatusCode::INTERNAL, "Error in Rust affordances (check logs)");
  }
  return {/*OK*/};
}

::grpc::Status L2capService::WaitConnection(::grpc::ServerContext* context,
                                            const ::pandora::l2cap::WaitConnectionRequest* request,
                                            ::pandora::l2cap::WaitConnectionResponse* response) {
  return Status(StatusCode::UNIMPLEMENTED, "");
}

::grpc::Status L2capService::Disconnect(::grpc::ServerContext* context,
                                        const ::pandora::l2cap::DisconnectRequest* request,
                                        ::pandora::l2cap::DisconnectResponse* response) {
  return Status(StatusCode::UNIMPLEMENTED, "");
}

::grpc::Status L2capService::WaitDisconnection(
    ::grpc::ServerContext* context, const ::pandora::l2cap::WaitDisconnectionRequest* request,
    ::pandora::l2cap::WaitDisconnectionResponse* response) {
  return Status(StatusCode::UNIMPLEMENTED, "");
}

::grpc::Status L2capService::Receive(
    ::grpc::ServerContext* context, const ::pandora::l2cap::ReceiveRequest* request,
    ::grpc::ServerWriter<::pandora::l2cap::ReceiveResponse>* writer) {
  return Status(StatusCode::UNIMPLEMENTED, "");
}

::grpc::Status L2capService::Send(::grpc::ServerContext* context,
                                  const ::pandora::l2cap::SendRequest* request,
                                  ::pandora::l2cap::SendResponse* response) {
  return Status(StatusCode::UNIMPLEMENTED, "");
}
