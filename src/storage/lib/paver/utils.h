// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
#ifndef SRC_STORAGE_LIB_PAVER_UTILS_H_
#define SRC_STORAGE_LIB_PAVER_UTILS_H_

#include <fidl/fuchsia.device/cpp/wire.h>
#include <fidl/fuchsia.fshost/cpp/wire.h>
#include <fidl/fuchsia.hardware.block.partition/cpp/wire.h>
#include <fidl/fuchsia.hardware.skipblock/cpp/wire.h>
#include <fidl/fuchsia.io/cpp/wire.h>
#include <lib/fit/function.h>
#include <lib/zx/channel.h>
#include <lib/zx/result.h>

#include <memory>
#include <optional>
#include <string_view>

#include <fbl/unique_fd.h>

#include "src/lib/uuid/uuid.h"
#include "src/storage/lib/paver/block-devices.h"

namespace paver {

// Helper function to auto-deduce type.
template <typename T>
std::unique_ptr<T> WrapUnique(T* ptr) {
  return std::unique_ptr<T>(ptr);
}

zx::result<std::unique_ptr<VolumeConnector>> OpenBlockPartition(
    const paver::BlockDevices& devices, std::optional<uuid::Uuid> unique_guid,
    std::optional<uuid::Uuid> type_guid, zx_duration_t timeout);

zx::result<std::unique_ptr<VolumeConnector>> OpenSkipBlockPartition(
    const paver::BlockDevices& devices, const uuid::Uuid& type_guid, zx_duration_t timeout);

bool HasSkipBlockDevice(const paver::BlockDevices& devices);

// Attempts to open and overwrite the first block of the underlying
// partition. Does not rebind partition drivers.
//
// At most one of |unique_guid| and |type_guid| may be nullptr.
zx::result<> WipeBlockPartition(const paver::BlockDevices& devices,
                                std::optional<uuid::Uuid> unique_guid,
                                std::optional<uuid::Uuid> type_guid);

zx::result<> IsBoard(fidl::UnownedClientEnd<fuchsia_io::Directory> svc_root,
                     std::string_view board_name);

zx::result<> IsBootloader(fidl::UnownedClientEnd<fuchsia_io::Directory> svc_root,
                          std::string_view vendor);

}  // namespace paver

#endif  // SRC_STORAGE_LIB_PAVER_UTILS_H_
