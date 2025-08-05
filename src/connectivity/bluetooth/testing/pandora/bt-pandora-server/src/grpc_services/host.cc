// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "host.h"

#include <algorithm>

#include <google/protobuf/io/coded_stream.h>
#include <google/protobuf/message.h>

using grpc::Status;
using grpc::StatusCode;

using namespace std::chrono_literals;

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
  uint64_t peer_id = get_peer_id(request->address().c_str());
  if (!peer_id || connect_peer(peer_id) != ZX_OK) {
    return Status(StatusCode::INTERNAL, "Error in Rust affordances (check logs)");
  }
  response->mutable_connection()->mutable_cookie()->set_value(std::to_string(peer_id));
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
  if (!request->has_public_()) {
    return Status(StatusCode::INVALID_ARGUMENT, "Expected PeerId encoded in public address field");
  }

  uint64_t peer_id;
  if (request->public_().size() == 6) {
    peer_id = get_peer_id(request->public_().c_str());
  } else {
    peer_id = std::strtoul(request->public_().c_str(), nullptr, /*base=*/10);
  }

  if (connect_le(peer_id) != ZX_OK) {
    return Status(StatusCode::INTERNAL, "Error in Rust affordances (check logs)");
  }
  response->mutable_connection()->mutable_cookie()->set_value(std::to_string(peer_id));
  return {/*OK*/};
}

Status HostService::Disconnect(::grpc::ServerContext* context,
                               const ::pandora::DisconnectRequest* request,
                               ::google::protobuf::Empty* response) {
  uint64_t peer_id =
      std::strtoul(request->connection().cookie().value().c_str(), nullptr, /*base=*/10);
  if (disconnect_peer(peer_id) != ZX_OK) {
    return Status(StatusCode::INTERNAL, "Error in Rust affordances (check logs)");
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
  uint8_t address_type =
      request->own_address_type() == pandora::OwnAddressType::PUBLIC ||
              request->own_address_type() == pandora::OwnAddressType::RESOLVABLE_OR_PUBLIC
          ? 1
          : 2;
  uint64_t peer_id = advertise_peripheral(request->connectable(), address_type);
  if (!peer_id) {
    return Status(StatusCode::INTERNAL, "Error in Rust affordances (check logs)");
  }
  pandora::AdvertiseResponse response;
  response.mutable_connection()->mutable_cookie()->set_value(std::to_string(peer_id));
  writer->Write(response);
  return {/*OK*/};
}

Status HostService::Scan(::grpc::ServerContext* context, const ::pandora::ScanRequest* request,
                         ::grpc::ServerWriter<::pandora::ScanningResponse>* writer) {
  {
    std::lock_guard lock(m_scan_scp_writer_);
    scan_rsp_writer = writer;
  }

  if (start_le_scan(/*context=*/this, LeScanCb) != ZX_OK) {
    return Status(StatusCode::INTERNAL, "Failure to start_le_scan (check logs)");
  }

  // TODO(https://fxbug.dev/396500079): Potentially migrate to gRPC async callback API and remove
  // this timeout. Since we are using the sync API, `writer` is invalidated when this handler exits,
  // so we keep it alive for an arbitrary sleep period allowing the scan to proceed, after which we
  // cancel the scan. In practice, the mmi2gRPC client cancels the scan earlier (when the test peer
  // is found).
  std::this_thread::sleep_for(std::chrono::seconds(1));

  std::lock_guard lock(m_scan_scp_writer_);
  scan_rsp_writer = nullptr;
  zx_status_t status = stop_le_scan();
  if (status == ZX_OK) {
    FX_LOGS(WARNING) << "LE scan stopped after timeout.";
  } else if (status == ZX_ERR_BAD_STATE) {
    FX_LOGS(INFO) << "LE scan was already stopped after timeout.";
  } else {
    return Status(StatusCode::INTERNAL, "Unexpected error in stop_le_scan");
  }
  return {/*OK*/};
}

Status HostService::Inquiry(::grpc::ServerContext* context,
                            const ::google::protobuf::Empty* request,
                            ::grpc::ServerWriter<::pandora::InquiryResponse>* writer) {
  return Status(StatusCode::UNIMPLEMENTED, "");
}

Status HostService::SetDiscoverabilityMode(::grpc::ServerContext* context,
                                           const ::pandora::SetDiscoverabilityModeRequest* request,
                                           ::google::protobuf::Empty* response) {
  if (set_discoverability(request->mode() != ::pandora::DiscoverabilityMode::NOT_DISCOVERABLE) !=
      ZX_OK) {
    return Status(StatusCode::INTERNAL, "Error in Rust affordances (check logs)");
  }
  return {/*OK*/};
}

Status HostService::SetConnectabilityMode(::grpc::ServerContext* context,
                                          const ::pandora::SetConnectabilityModeRequest* request,
                                          ::google::protobuf::Empty* response) {
  return Status(StatusCode::UNIMPLEMENTED, "");
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
