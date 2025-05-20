// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_BLUETOOTH_TESTING_PANDORA_BT_PANDORA_SERVER_SRC_GRPC_SERVICES_L2CAP_H_
#define SRC_CONNECTIVITY_BLUETOOTH_TESTING_PANDORA_BT_PANDORA_SERVER_SRC_GRPC_SERVICES_L2CAP_H_

#include "third_party/github.com/google/bt-test-interfaces/src/pandora/l2cap.grpc.pb.h"

class L2capService : public pandora::l2cap::L2CAP::Service {
 public:
  explicit L2capService() = default;

  ::grpc::Status Connect(::grpc::ServerContext* context,
                         const ::pandora::l2cap::ConnectRequest* request,
                         ::pandora::l2cap::ConnectResponse* response) override;

  ::grpc::Status WaitConnection(::grpc::ServerContext* context,
                                const ::pandora::l2cap::WaitConnectionRequest* request,
                                ::pandora::l2cap::WaitConnectionResponse* response) override;

  ::grpc::Status Disconnect(::grpc::ServerContext* context,
                            const ::pandora::l2cap::DisconnectRequest* request,
                            ::pandora::l2cap::DisconnectResponse* response) override;

  ::grpc::Status WaitDisconnection(::grpc::ServerContext* context,
                                   const ::pandora::l2cap::WaitDisconnectionRequest* request,
                                   ::pandora::l2cap::WaitDisconnectionResponse* response) override;

  ::grpc::Status Receive(::grpc::ServerContext* context,
                         const ::pandora::l2cap::ReceiveRequest* request,
                         ::grpc::ServerWriter<::pandora::l2cap::ReceiveResponse>* writer) override;

  ::grpc::Status Send(::grpc::ServerContext* context, const ::pandora::l2cap::SendRequest* request,
                      ::pandora::l2cap::SendResponse* response) override;
};

#endif  // SRC_CONNECTIVITY_BLUETOOTH_TESTING_PANDORA_BT_PANDORA_SERVER_SRC_GRPC_SERVICES_L2CAP_H_
