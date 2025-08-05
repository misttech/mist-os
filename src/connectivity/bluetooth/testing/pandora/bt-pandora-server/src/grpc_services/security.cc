// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "security.h"

#include <lib/syslog/cpp/macros.h>

#include "src/connectivity/bluetooth/testing/bt-affordances/ffi_c/bindings.h"

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

::grpc::Status SecurityService::OnPairing(
    ::grpc::ServerContext* context,
    ::grpc::ServerReaderWriter<::pandora::PairingEvent, ::pandora::PairingEventAnswer>* stream) {
  return Status(StatusCode::UNIMPLEMENTED, "");
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
