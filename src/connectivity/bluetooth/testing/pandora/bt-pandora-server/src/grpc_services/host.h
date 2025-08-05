// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_BLUETOOTH_TESTING_PANDORA_BT_PANDORA_SERVER_SRC_GRPC_SERVICES_HOST_H_
#define SRC_CONNECTIVITY_BLUETOOTH_TESTING_PANDORA_BT_PANDORA_SERVER_SRC_GRPC_SERVICES_HOST_H_

#include <fidl/fuchsia.bluetooth.sys/cpp/fidl.h>
#include <lib/syslog/cpp/macros.h>

#include "fidl/fuchsia.bluetooth.sys/cpp/markers.h"
#include "fidl/fuchsia.bluetooth.sys/cpp/natural_types.h"
#include "src/connectivity/bluetooth/testing/bt-affordances/ffi_c/bindings.h"
#include "third_party/github.com/google/bt-test-interfaces/src/pandora/host.grpc.pb.h"

class HostService : public pandora::Host::Service {
 public:
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

  // The public address field in `request` should hold an std::to_string encoding of the PeerId to
  // connect.
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
  static void LeScanCb(void* context, const LePeer* peer) {
    HostService* svc = static_cast<HostService*>(context);
    std::lock_guard lock(svc->m_scan_scp_writer_);
    if (svc->scan_rsp_writer) {
      pandora::ScanningResponse scan_rsp;
      scan_rsp.set_public_(std::to_string(peer->id));
      scan_rsp.set_connectable(peer->connectable);
      scan_rsp.mutable_data()->set_complete_local_name(peer->name);
      if (!svc->scan_rsp_writer->Write(scan_rsp)) {
        FX_LOGS(INFO) << "LE scan canceled by gRPC client.";
        svc->scan_rsp_writer = nullptr;
        stop_le_scan();
      }
    }
  }

  std::mutex m_scan_scp_writer_;
  // If this Writer is non-null, there is an ongoing `Scan` response streaming RPC.
  grpc::ServerWriter<::pandora::ScanningResponse>* scan_rsp_writer = nullptr;

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
};

#endif  // SRC_CONNECTIVITY_BLUETOOTH_TESTING_PANDORA_BT_PANDORA_SERVER_SRC_GRPC_SERVICES_HOST_H_
