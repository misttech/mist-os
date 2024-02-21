// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/iso/iso_stream_manager.h"

namespace bt::iso {

IsoStreamManager::IsoStreamManager(hci::CommandChannel::WeakPtr cmd_channel)
    : cmd_(cmd_channel), weak_self_(this) {}

IsoStreamManager::~IsoStreamManager() {}

}  // namespace bt::iso
