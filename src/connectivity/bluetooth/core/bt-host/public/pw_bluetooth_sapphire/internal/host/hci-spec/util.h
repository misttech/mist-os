// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_BLUETOOTH_CORE_BT_HOST_PUBLIC_PW_BLUETOOTH_SAPPHIRE_INTERNAL_HOST_HCI_SPEC_UTIL_H_
#define SRC_CONNECTIVITY_BLUETOOTH_CORE_BT_HOST_PUBLIC_PW_BLUETOOTH_SAPPHIRE_INTERNAL_HOST_HCI_SPEC_UTIL_H_

#include <string>

#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/hci-spec/protocol.h"

namespace bt::hci_spec {

// Helper functions to convert HCI data types to library objects.

// Returns a user-friendly string representation of |version|.
std::string HCIVersionToString(
    pw::bluetooth::emboss::CoreSpecificationVersion version);

// Returns a user-friendly string representation of |status|.
std::string StatusCodeToString(pw::bluetooth::emboss::StatusCode code);

// Returns a user-friendly string representation of |link_type|.
const char* LinkTypeToString(pw::bluetooth::emboss::LinkType link_type);

// Returns a user-friendly string representation of |key_type|.
const char* LinkKeyTypeToString(hci_spec::LinkKeyType key_type);

// Returns a user-friendly string representation of |role|.
std::string ConnectionRoleToString(pw::bluetooth::emboss::ConnectionRole role);

}  // namespace bt::hci_spec

#endif  // SRC_CONNECTIVITY_BLUETOOTH_CORE_BT_HOST_PUBLIC_PW_BLUETOOTH_SAPPHIRE_INTERNAL_HOST_HCI_SPEC_UTIL_H_
