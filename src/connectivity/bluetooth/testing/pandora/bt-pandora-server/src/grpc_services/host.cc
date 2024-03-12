// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "host.h"

#include <lib/syslog/cpp/macros.h>

#include <algorithm>

#include "fidl/fuchsia.bluetooth.sys/cpp/common_types.h"
#include "lib/component/incoming/cpp/protocol.h"

using grpc::Status;

using namespace std::chrono_literals;

HostService::HostService(async_dispatcher_t* dispatcher) { static_cast<void>(dispatcher); }

// TODO(https://fxbug.dev/316721276): Implement gRPCs necessary to enable GAP/A2DP testing.

Status HostService::FactoryReset(grpc::ServerContext* context,
                                 const google::protobuf::Empty* request,
                                 google::protobuf::Empty* response) {
  return Status(grpc::StatusCode::UNIMPLEMENTED, "");
}

Status HostService::Reset(grpc::ServerContext* context, const google::protobuf::Empty* request,
                          google::protobuf::Empty* response) {
  return Status(grpc::StatusCode::UNIMPLEMENTED, "");
}

Status HostService::ReadLocalAddress(grpc::ServerContext* context,
                                     const google::protobuf::Empty* request,
                                     pandora::ReadLocalAddressResponse* response) {
  return Status(grpc::StatusCode::UNIMPLEMENTED, "");
}

Status HostService::Connect(grpc::ServerContext* context, const pandora::ConnectRequest* request,
                            pandora::ConnectResponse* response) {
  return Status(grpc::StatusCode::UNIMPLEMENTED, "");
}

Status HostService::WaitConnection(grpc::ServerContext* context,
                                   const pandora::WaitConnectionRequest* request,
                                   pandora::WaitConnectionResponse* response) {
  return Status(grpc::StatusCode::UNIMPLEMENTED, "");
}

Status HostService::ConnectLE(::grpc::ServerContext* context,
                              const ::pandora::ConnectLERequest* request,
                              ::pandora::ConnectLEResponse* response) {
  return Status(grpc::StatusCode::UNIMPLEMENTED, "");
}

Status HostService::Disconnect(::grpc::ServerContext* context,
                               const ::pandora::DisconnectRequest* request,
                               ::google::protobuf::Empty* response) {
  return Status(grpc::StatusCode::UNIMPLEMENTED, "");
}

Status HostService::WaitDisconnection(::grpc::ServerContext* context,
                                      const ::pandora::WaitDisconnectionRequest* request,
                                      ::google::protobuf::Empty* response) {
  return Status(grpc::StatusCode::UNIMPLEMENTED, "");
}

Status HostService::Advertise(::grpc::ServerContext* context,
                              const ::pandora::AdvertiseRequest* request,
                              ::grpc::ServerWriter<::pandora::AdvertiseResponse>* writer) {
  return Status(grpc::StatusCode::UNIMPLEMENTED, "");
}

Status HostService::Scan(::grpc::ServerContext* context, const ::pandora::ScanRequest* request,
                         ::grpc::ServerWriter<::pandora::ScanningResponse>* writer) {
  return Status(grpc::StatusCode::UNIMPLEMENTED, "");
}

Status HostService::Inquiry(::grpc::ServerContext* context,
                            const ::google::protobuf::Empty* request,
                            ::grpc::ServerWriter<::pandora::InquiryResponse>* writer) {
  return Status(grpc::StatusCode::UNIMPLEMENTED, "");
}

Status HostService::SetDiscoverabilityMode(::grpc::ServerContext* context,
                                           const ::pandora::SetDiscoverabilityModeRequest* request,
                                           ::google::protobuf::Empty* response) {
  return Status(grpc::StatusCode::UNIMPLEMENTED, "");
}

Status HostService::SetConnectabilityMode(::grpc::ServerContext* context,
                                          const ::pandora::SetConnectabilityModeRequest* request,
                                          ::google::protobuf::Empty* response) {
  return Status(grpc::StatusCode::UNIMPLEMENTED, "");
}
