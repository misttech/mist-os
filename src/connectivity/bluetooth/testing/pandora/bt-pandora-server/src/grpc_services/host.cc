// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "host.h"

#include <lib/syslog/cpp/macros.h>

#include <algorithm>

#include "fidl/fuchsia.bluetooth.sys/cpp/common_types.h"
#include "lib/component/incoming/cpp/protocol.h"
#include "src/connectivity/bluetooth/testing/pandora/bt-pandora-server/src/rust_affordances/ffi_c/bindings.h"

using grpc::Status;
using grpc::StatusCode;

using namespace std::chrono_literals;

HostService::HostService(async_dispatcher_t* dispatcher) {
  // Connect to fuchsia.bluetooth.sys.Access
  zx::result access_client_end = component::Connect<fuchsia_bluetooth_sys::Access>();
  if (!access_client_end.is_ok()) {
    FX_LOGS(ERROR) << "Error connecting to Access service: " << access_client_end.error_value();
    return;
  }
  access_client_.Bind(std::move(*access_client_end), dispatcher);

  // Connect to fuchsia.bluetooth.sys.Pairing
  zx::result pairing_client_end = component::Connect<fuchsia_bluetooth_sys::Pairing>();
  if (!pairing_client_end.is_ok()) {
    FX_LOGS(ERROR) << "Error connecting to Pairing service: " << pairing_client_end.error_value();
    return;
  }
  pairing_client_.Bind(std::move(*pairing_client_end));

  // Connect to fuchsia.bluetooth.sys.PairingDelegate and set PairingDelegate
  zx::result<fidl::Endpoints<fuchsia_bluetooth_sys::PairingDelegate>> endpoints =
      fidl::CreateEndpoints<fuchsia_bluetooth_sys::PairingDelegate>();
  if (!endpoints.is_ok()) {
    FX_LOGS(ERROR) << "Error creating PairingDelegate endpoints: " << endpoints.status_string();
    return;
  }
  auto [pairing_delegate_client_end, pairing_delegate_server_end] = *std::move(endpoints);
  auto result = pairing_client_->SetPairingDelegate({fuchsia_bluetooth_sys::InputCapability::kNone,
                                                     fuchsia_bluetooth_sys::OutputCapability::kNone,
                                                     std::move(pairing_delegate_client_end)});
  if (result.is_error()) {
    FX_LOGS(ERROR) << "Error setting PairingDelegate: " << result.error_value();
    return;
  }
  fidl::BindServer(dispatcher, std::move(pairing_delegate_server_end),
                   std::make_unique<PairingDelegateImpl>());
}

// TODO(https://fxbug.dev/316721276): Implement gRPCs necessary to enable GAP/A2DP testing.

Status HostService::FactoryReset(grpc::ServerContext* context,
                                 const google::protobuf::Empty* request,
                                 google::protobuf::Empty* response) {
  return Status(StatusCode::UNIMPLEMENTED, "");
}

Status HostService::Reset(grpc::ServerContext* context, const google::protobuf::Empty* request,
                          google::protobuf::Empty* response) {
  // No-op for now; return OK status.
  return {/*OK*/};
}

Status HostService::ReadLocalAddress(grpc::ServerContext* context,
                                     const google::protobuf::Empty* request,
                                     pandora::ReadLocalAddressResponse* response) {
  std::array<uint8_t, 6> host_addr;
  if (read_local_address(host_addr.data()) != ZX_OK) {
    return Status(StatusCode::INTERNAL, "Error in Rust affordances (check logs)");
  }
  std::ranges::reverse(host_addr);
  response->set_address(host_addr.data(), 6);
  return {/*OK*/};
}

Status HostService::Connect(grpc::ServerContext* context, const pandora::ConnectRequest* request,
                            pandora::ConnectResponse* response) {
  auto peer_it = WaitForPeer(request->address());
  if (!peer_it->connected() || !*peer_it->connected()) {
    FX_LOGS(INFO) << "Peer not connected; sending connection request";
    access_client_->Connect({*peer_it->id()})
        .Then([this,
               id = *peer_it->id()](fidl::Result<fuchsia_bluetooth_sys::Access::Connect>& connect) {
          if (connect.is_error()) {
            auto err = connect.error_value();
            FX_LOGS(ERROR) << "Connect error: " << err.FormatDescription();
          } else {
            FX_LOGS(INFO) << "Connected peer: " << std::hex << id.value();
          }
          cv_access_.notify_one();
        });

    std::unique_lock<std::mutex> lock(m_access_);
    cv_access_.wait(lock);
  }

  if (peer_it->id()) {
    response->mutable_connection()->mutable_cookie()->set_value(
        std::to_string(peer_it->id()->value()));
  }
  return {/*OK*/};
}

Status HostService::WaitConnection(grpc::ServerContext* context,
                                   const pandora::WaitConnectionRequest* request,
                                   pandora::WaitConnectionResponse* response) {
  auto peer_it = WaitForPeer(request->address(), true);
  if (peer_it->id()) {
    response->mutable_connection()->mutable_cookie()->set_value(
        std::to_string(peer_it->id()->value()));
  }
  return {/*OK*/};
}

Status HostService::ConnectLE(::grpc::ServerContext* context,
                              const ::pandora::ConnectLERequest* request,
                              ::pandora::ConnectLEResponse* response) {
  return Status(StatusCode::UNIMPLEMENTED, "");
}

Status HostService::Disconnect(::grpc::ServerContext* context,
                               const ::pandora::DisconnectRequest* request,
                               ::google::protobuf::Empty* response) {
  auto peer_it =
      std::find_if(peers_.begin(), peers_.end(),
                   [id = request->connection()](const fuchsia_bluetooth_sys::Peer& candidate) {
                     if (candidate.id()) {
                       return std::to_string(candidate.id()->value()) == id.cookie().value();
                     }
                     return false;
                   });

  // Disconnect from peer if found
  if (peer_it != peers_.end() && peer_it->connected() && *peer_it->connected()) {
    access_client_->Disconnect({*peer_it->id()})
        .Then([this, id = *peer_it->id()](
                  fidl::Result<fuchsia_bluetooth_sys::Access::Disconnect>& disconnect) {
          if (disconnect.is_error()) {
            auto err = disconnect.error_value();
            FX_LOGS(ERROR) << "Disconnect error: " << err.FormatDescription();
          } else {
            FX_LOGS(INFO) << "Disconnected peer: " << std::hex << id.value();
          }
          cv_access_.notify_one();
        });

    std::unique_lock<std::mutex> lock(m_access_);
    cv_access_.wait(lock);
  }

  return {/*OK*/};
}

Status HostService::WaitDisconnection(::grpc::ServerContext* context,
                                      const ::pandora::WaitDisconnectionRequest* request,
                                      ::google::protobuf::Empty* response) {
  return Status(StatusCode::UNIMPLEMENTED, "");
}

Status HostService::Advertise(::grpc::ServerContext* context,
                              const ::pandora::AdvertiseRequest* request,
                              ::grpc::ServerWriter<::pandora::AdvertiseResponse>* writer) {
  return Status(StatusCode::UNIMPLEMENTED, "");
}

Status HostService::Scan(::grpc::ServerContext* context, const ::pandora::ScanRequest* request,
                         ::grpc::ServerWriter<::pandora::ScanningResponse>* writer) {
  return Status(StatusCode::UNIMPLEMENTED, "");
}

Status HostService::Inquiry(::grpc::ServerContext* context,
                            const ::google::protobuf::Empty* request,
                            ::grpc::ServerWriter<::pandora::InquiryResponse>* writer) {
  return Status(StatusCode::UNIMPLEMENTED, "");
}

Status HostService::SetDiscoverabilityMode(::grpc::ServerContext* context,
                                           const ::pandora::SetDiscoverabilityModeRequest* request,
                                           ::google::protobuf::Empty* response) {
  return Status(StatusCode::UNIMPLEMENTED, "");
}

Status HostService::SetConnectabilityMode(::grpc::ServerContext* context,
                                          const ::pandora::SetConnectabilityModeRequest* request,
                                          ::google::protobuf::Empty* response) {
  return Status(StatusCode::UNIMPLEMENTED, "");
}

void HostService::PairingDelegateImpl::OnPairingRequest(
    OnPairingRequestRequest& request, OnPairingRequestCompleter::Sync& completer) {
  FX_LOGS(INFO) << "PairingDelegate received pairing request; accepting";
  completer.Reply({true, {}});
}

void HostService::PairingDelegateImpl::OnPairingComplete(
    OnPairingCompleteRequest& request, OnPairingCompleteCompleter::Sync& completer) {
  if (request.success()) {
    FX_LOGS(INFO) << "Succesfully paired to peer id: " << request.id().value();
    return;
  }
  FX_LOGS(ERROR) << "Error pairing to peer id: " << request.id().value();
}

std::vector<fuchsia_bluetooth_sys::Peer>::const_iterator HostService::WaitForPeer(
    const std::string& addr, bool enforce_connected) {
  std::vector<fuchsia_bluetooth_sys::Peer>::const_iterator peer_it;
  std::unique_lock<std::mutex> lock(m_access_);

  do {
    if (!peer_watching_) {
      peer_watching_ = true;
      access_client_->WatchPeers().Then(
          [this](fidl::Result<fuchsia_bluetooth_sys::Access::WatchPeers>& watch_peers) {
            if (watch_peers.is_error()) {
              fidl::Error err = watch_peers.error_value();
              FX_LOGS(ERROR) << "Host watcher error: " << err.error() << "\n";
              return;
            }

            std::unique_lock<std::mutex> lock(this->m_access_);
            peers_ = watch_peers->updated();
            peer_watching_ = false;
            cv_access_.notify_one();
          });
    }

    cv_access_.wait_for(lock, 1000ms, [this] { return !peer_watching_; });
  } while ((peer_it = std::find_if(
                peers_.begin(), peers_.end(),
                [&addr, enforce_connected](const fuchsia_bluetooth_sys::Peer& candidate) {
                  for (size_t i = 0; i < 6; ++i) {
                    if (candidate.address()->bytes()[5 - i] !=
                        static_cast<unsigned char>(addr[i])) {
                      return false;
                    }
                  }
                  return !enforce_connected || (candidate.connected() && *candidate.connected());
                })) == peers_.end());

  return peer_it;
}
