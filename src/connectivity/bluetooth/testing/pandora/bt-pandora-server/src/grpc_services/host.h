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

 private:
  class PairingDelegateImpl : public fidl::Server<fuchsia_bluetooth_sys::PairingDelegate> {
    void OnPairingRequest(OnPairingRequestRequest& request,
                          OnPairingRequestCompleter::Sync& completer) override;
    void OnPairingComplete(OnPairingCompleteRequest& request,
                           OnPairingCompleteCompleter::Sync& completer) override;
    void OnRemoteKeypress(OnRemoteKeypressRequest& request,
                          OnRemoteKeypressCompleter::Sync& completer) override {}
  };

  // Wait for a Peer with the given |addr| to become known. If |enforce_connected| is set, wait
  // until the Peer is also connected. Returns an iterator to the peer.
  std::vector<fuchsia_bluetooth_sys::Peer>::const_iterator WaitForPeer(
      const std::string& addr, bool enforce_connected = false);

  // The synchronization primitives are utilized for configuring waiting/timeouts on FIDL callbacks
  // and enforcing mutual exclusivity when writing/reading the structures that cache the updated
  // state information that is received in these callbacks.
  fidl::SharedClient<fuchsia_bluetooth_sys::Access> access_client_;
  std::condition_variable cv_access_;
  std::mutex m_access_;
  std::vector<fuchsia_bluetooth_sys::Peer> peers_;
  bool peer_watching_{false};

  fidl::SyncClient<fuchsia_bluetooth_sys::Pairing> pairing_client_;
};

#endif  // SRC_CONNECTIVITY_BLUETOOTH_TESTING_PANDORA_BT_PANDORA_SERVER_SRC_GRPC_SERVICES_HOST_H_
