// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_BLUETOOTH_TESTING_PANDORA_BT_PANDORA_SERVER_SRC_GRPC_SERVICES_SECURITY_H_
#define SRC_CONNECTIVITY_BLUETOOTH_TESTING_PANDORA_BT_PANDORA_SERVER_SRC_GRPC_SERVICES_SECURITY_H_

#include "third_party/github.com/google/bt-test-interfaces/src/pandora/security.grpc.pb.h"

class SecurityStorageService : public pandora::SecurityStorage::Service {
  ::grpc::Status IsBonded(::grpc::ServerContext* context, const ::pandora::IsBondedRequest* request,
                          ::google::protobuf::BoolValue* response) override;
  ::grpc::Status DeleteBond(::grpc::ServerContext* context,
                            const ::pandora::DeleteBondRequest* request,
                            ::google::protobuf::Empty* response) override;
};

#endif  // SRC_CONNECTIVITY_BLUETOOTH_TESTING_PANDORA_BT_PANDORA_SERVER_SRC_GRPC_SERVICES_SECURITY_H_
