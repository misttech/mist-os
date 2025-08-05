// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "security.h"

#include <lib/syslog/cpp/macros.h>

#include "fidl/fuchsia.bluetooth.sys/cpp/common_types.h"
#include "lib/component/incoming/cpp/protocol.h"
#include "src/connectivity/bluetooth/testing/bt-affordances/ffi_c/bindings.h"

using fuchsia_bluetooth_sys::PairingMethod;
using grpc::Status;
using grpc::StatusCode;

Status SecurityStorageService::IsBonded(::grpc::ServerContext* context,
                                        const ::pandora::IsBondedRequest* request,
                                        ::google::protobuf::BoolValue* response) {
  return Status(StatusCode::UNIMPLEMENTED, "");
}

Status SecurityStorageService::DeleteBond(::grpc::ServerContext* context,
                                          const ::pandora::DeleteBondRequest* request,
                                          ::google::protobuf::Empty* response) {
  if (request->address_case() == ::pandora::DeleteBondRequest::AddressCase::ADDRESS_NOT_SET) {
    return Status(StatusCode::INVALID_ARGUMENT, "DeleteBondRequest address not set");
  }
  std::string address;
  if (request->address_case() == ::pandora::DeleteBondRequest::AddressCase::kPublic) {
    address = request->public_();
  } else {
    address = request->random();
  }

  uint64_t peer_id = get_peer_id(address.c_str());
  if (peer_id && forget_peer(peer_id) != ZX_OK) {
    return Status(StatusCode::INTERNAL, "Error in Rust affordances (check logs)");
  }

  return {/*OK*/};
}

SecurityService::SecurityService(async_dispatcher_t* dispatcher) {
  // Connect to fuchsia.bluetooth.sys.Pairing
  zx::result pairing_client_end = component::Connect<fuchsia_bluetooth_sys::Pairing>();
  if (!pairing_client_end.is_ok()) {
    FX_LOGS(ERROR) << "Error connecting to Pairing service: " << pairing_client_end.error_value();
    return;
  }
  pairing_client_.Bind(std::move(*pairing_client_end));

  // Connect to fuchsia.bluetooth.sys.PairingDelegate and set PairingDelegate
  // TODO(b/423700622): Move PairingDelegate to bt-affordances?
  zx::result<fidl::Endpoints<fuchsia_bluetooth_sys::PairingDelegate>> endpoints =
      fidl::CreateEndpoints<fuchsia_bluetooth_sys::PairingDelegate>();
  if (!endpoints.is_ok()) {
    FX_LOGS(ERROR) << "Error creating PairingDelegate endpoints: " << endpoints.status_string();
    return;
  }
  auto [pairing_delegate_client_end, pairing_delegate_server_end] = *std::move(endpoints);
  auto result = pairing_client_->SetPairingDelegate(
      {fuchsia_bluetooth_sys::InputCapability::kConfirmation,
       fuchsia_bluetooth_sys::OutputCapability::kDisplay, std::move(pairing_delegate_client_end)});
  if (result.is_error()) {
    FX_LOGS(ERROR) << "Error setting PairingDelegate: " << result.error_value();
    return;
  }
  fidl::BindServer(dispatcher, std::move(pairing_delegate_server_end),
                   std::make_unique<PairingDelegateImpl>(m_pairing_stream_, &pairing_stream_));
}

::grpc::Status SecurityService::OnPairing(
    ::grpc::ServerContext* context,
    ::grpc::ServerReaderWriter<::pandora::PairingEvent, ::pandora::PairingEventAnswer>* stream) {
  {
    std::unique_lock<std::mutex> lock(m_pairing_stream_);
    pairing_stream_ = stream;
  }

  for (pandora::PairingEventAnswer msg; stream->Read(&msg);) {
    // TODO(https://fxbug.dev/396500079): Process these events.
  }

  std::unique_lock<std::mutex> lock(m_pairing_stream_);
  pairing_stream_ = nullptr;
  return {/*OK*/};
}

::grpc::Status SecurityService::Secure(::grpc::ServerContext* context,
                                       const ::pandora::SecureRequest* request,
                                       ::pandora::SecureResponse* response) {
  uint32_t pairing_level;
  if (request->level_case() == ::pandora::SecureRequest::LevelCase::kClassic) {
    return Status(StatusCode::UNIMPLEMENTED, "Only implemented LE pairing security so far");
  }
  switch (request->le()) {
    case pandora::LE_LEVEL1: {
      return Status(StatusCode::INVALID_ARGUMENT, "LE pairing with no security is not supported");
    }
    case pandora::LE_LEVEL2: {
      // Encrypted unauthenticated
      pairing_level = 1;
      break;
    }
    case pandora::LE_LEVEL3: {
      // Encrypted authenticated
      pairing_level = 2;
      break;
    }
    case pandora::LE_LEVEL4: {
      return Status(StatusCode::UNIMPLEMENTED,
                    "Have not yet handled LE Secure Connections pairing");
    }
    default: {
      return Status(StatusCode::INVALID_ARGUMENT, "Invalid LESecurityLevel");
    }
  }

  pair(std::strtoul(request->connection().cookie().value().c_str(), nullptr, /*base=*/10),
       pairing_level);

  return {/*OK*/};
}

::grpc::Status SecurityService::WaitSecurity(::grpc::ServerContext* context,
                                             const ::pandora::WaitSecurityRequest* request,
                                             ::pandora::WaitSecurityResponse* response) {
  return Status(StatusCode::UNIMPLEMENTED, "");
}

void SecurityService::PairingDelegateImpl::OnPairingRequest(
    OnPairingRequestRequest& request, OnPairingRequestCompleter::Sync& completer) {
  FX_LOGS(INFO) << "PairingDelegate received pairing request; accepting";

  std::unique_lock<std::mutex> lock(m_pairing_stream_);
  if (*pairing_stream_) {
    pandora::PairingEvent event;

    std::array<uint8_t, 6> peer_addr = request.peer().address()->bytes();
    // Convert from LE bytes to BE bytes
    std::ranges::reverse(peer_addr);
    event.set_address(peer_addr.data(), 6);
    if (request.method() == PairingMethod::kPasskeyDisplay ||
        request.method() == PairingMethod::kPasskeyComparison) {
      event.set_passkey_entry_notification(request.displayed_passkey());
    }

    FX_LOGS(INFO) << "Writing pairing event to stream";
    (*pairing_stream_)->Write(event);
  }

  completer.Reply({true, {}});
}

void SecurityService::PairingDelegateImpl::OnPairingComplete(
    OnPairingCompleteRequest& request, OnPairingCompleteCompleter::Sync& completer) {
  if (request.success()) {
    FX_LOGS(INFO) << "Succesfully paired to peer id: " << request.id().value();
    return;
  }
  FX_LOGS(ERROR) << "Error pairing to peer id: " << request.id().value();
}
