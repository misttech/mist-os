// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "security.h"

#include <lib/syslog/cpp/macros.h>

#include "src/connectivity/bluetooth/testing/pandora/bt-pandora-server/src/rust_affordances/ffi_c/bindings.h"

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
