// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_BLUETOOTH_CORE_BT_HOST_PUBLIC_PW_BLUETOOTH_SAPPHIRE_INTERNAL_HOST_ISO_ISO_COMMON_H_
#define SRC_CONNECTIVITY_BLUETOOTH_CORE_BT_HOST_PUBLIC_PW_BLUETOOTH_SAPPHIRE_INTERNAL_HOST_ISO_ISO_COMMON_H_

#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/hci-spec/constants.h"

#include <pw_bluetooth/hci_data.emb.h>

namespace bt::iso {

// Maximum possible size of an Isochronous data packet.
// See Core Spec v5.4, Volume 4, Part E, Section 5.4.5
constexpr size_t kMaxIsochronousDataPacketSize =
    pw::bluetooth::emboss::IsoDataFrameHeader::MaxSizeInBytes() +
    hci_spec::kMaxIsochronousDataPacketPayloadSize;
}  // namespace bt::iso

#endif  // SRC_CONNECTIVITY_BLUETOOTH_CORE_BT_HOST_PUBLIC_PW_BLUETOOTH_SAPPHIRE_INTERNAL_HOST_ISO_ISO_COMMON_H_
