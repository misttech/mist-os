// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_BLUETOOTH_TESTING_PANDORA_BT_PANDORA_SERVER_SRC_GRPC_SERVICES_HOST_H_
#define SRC_CONNECTIVITY_BLUETOOTH_TESTING_PANDORA_BT_PANDORA_SERVER_SRC_GRPC_SERVICES_HOST_H_

#include <fidl/fuchsia.bluetooth.sys/cpp/fidl.h>

#include "fidl/fuchsia.bluetooth.sys/cpp/markers.h"
#include "fidl/fuchsia.bluetooth.sys/cpp/natural_types.h"
#include "third_party/github.com/google/bt-test-interfaces/src/pandora/host.grpc.pb.h"

class HostService : public pandora::Host::Service {
 public:
  explicit HostService(async_dispatcher_t* dispatcher);

  ::grpc::Status FactoryReset(::grpc::ServerContext* context,
                              const ::google::protobuf::Empty* request,
                              ::google::protobuf::Empty* response) override;

  ::grpc::Status Reset(::grpc::ServerContext* context, const ::google::protobuf::Empty* request,
                       ::google::protobuf::Empty* response) override;

  ::grpc::Status ReadLocalAddress(::grpc::ServerContext* context,
                                  const ::google::protobuf::Empty* request,
                                  ::pandora::ReadLocalAddressResponse* response) override;

  ::grpc::Status Connect(::grpc::ServerContext* context, const ::pandora::ConnectRequest* request,
                         ::pandora::ConnectResponse* response) override;

  ::grpc::Status WaitConnection(::grpc::ServerContext* context,
                                const ::pandora::WaitConnectionRequest* request,
                                ::pandora::WaitConnectionResponse* response) override;

  ::grpc::Status ConnectLE(::grpc::ServerContext* context,
                           const ::pandora::ConnectLERequest* request,
                           ::pandora::ConnectLEResponse* response) override;

  ::grpc::Status Disconnect(::grpc::ServerContext* context,
                            const ::pandora::DisconnectRequest* request,
                            ::google::protobuf::Empty* response) override;

  ::grpc::Status WaitDisconnection(::grpc::ServerContext* context,
                                   const ::pandora::WaitDisconnectionRequest* request,
                                   ::google::protobuf::Empty* response) override;

  ::grpc::Status Advertise(::grpc::ServerContext* context,
                           const ::pandora::AdvertiseRequest* request,
                           ::grpc::ServerWriter<::pandora::AdvertiseResponse>* writer) override;

  ::grpc::Status Scan(::grpc::ServerContext* context, const ::pandora::ScanRequest* request,
                      ::grpc::ServerWriter<::pandora::ScanningResponse>* writer) override;

  ::grpc::Status Inquiry(::grpc::ServerContext* context, const ::google::protobuf::Empty* request,
                         ::grpc::ServerWriter<::pandora::InquiryResponse>* writer) override;

  ::grpc::Status SetDiscoverabilityMode(::grpc::ServerContext* context,
                                        const ::pandora::SetDiscoverabilityModeRequest* request,
                                        ::google::protobuf::Empty* response) override;

  ::grpc::Status SetConnectabilityMode(::grpc::ServerContext* context,
                                       const ::pandora::SetConnectabilityModeRequest* request,
                                       ::google::protobuf::Empty* response) override;
};

#endif  // SRC_CONNECTIVITY_BLUETOOTH_TESTING_PANDORA_BT_PANDORA_SERVER_SRC_GRPC_SERVICES_HOST_H_
