// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/storage/lib/paver/abr-client.h"

#include <dirent.h>
#include <endian.h>
#include <fidl/fuchsia.boot/cpp/wire.h>
#include <fidl/fuchsia.device/cpp/wire.h>
#include <fidl/fuchsia.io/cpp/wire.h>
#include <fidl/fuchsia.paver/cpp/wire.h>
#include <lib/abr/abr.h>
#include <lib/cksum.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/fdio/cpp/caller.h>
#include <lib/fdio/directory.h>
#include <lib/fidl/cpp/wire/wire_messaging.h>
#include <lib/fit/defer.h>
#include <stdio.h>
#include <string.h>
#include <zircon/errors.h>
#include <zircon/hw/gpt.h>
#include <zircon/status.h>

#include <string_view>

#include <fbl/algorithm.h>

#include "src/lib/uuid/uuid.h"
#include "src/storage/lib/paver/partition-client.h"
#include "src/storage/lib/paver/pave-logging.h"

namespace abr {

namespace partition = fuchsia_hardware_block_partition;
using fuchsia_paver::wire::Asset;
using fuchsia_paver::wire::Configuration;

zx::result<Configuration> CurrentSlotToConfiguration(std::string_view slot) {
  // Some bootloaders prefix slot with dash or underscore. We strip them for consistency.
  slot.remove_prefix(std::min(slot.find_first_not_of("_-"), slot.size()));
  if (slot.compare("a") == 0) {
    return zx::ok(Configuration::kA);
  }
  if (slot.compare("b") == 0) {
    return zx::ok(Configuration::kB);
  }
  if (slot.compare("r") == 0) {
    return zx::ok(Configuration::kRecovery);
  }
  ERROR("Invalid value `%.*s` found in zvb.current_slot!\n", static_cast<int>(slot.size()),
        slot.data());
  return zx::error(ZX_ERR_NOT_SUPPORTED);
}

bool FindPartitionLabelByGuid(const fbl::unique_fd& devfs_root, const uint8_t* guid,
                              std::string& out) {
  out.clear();
  fbl::unique_fd d_fd;
  if (zx_status_t status =
          fdio_open_fd_at(devfs_root.get(), "class/block", 0, d_fd.reset_and_get_address());
      status != ZX_OK) {
    ERROR("Cannot inspect block devices: %s\n", zx_status_get_string(status));
    return false;
  }

  DIR* d = fdopendir(d_fd.release());
  if (d == nullptr) {
    ERROR("Cannot inspect block devices\n");
    return false;
  }
  const auto closer = fit::defer([&]() { closedir(d); });

  struct dirent* de;
  while ((de = readdir(d)) != nullptr) {
    fdio_cpp::UnownedFdioCaller caller(dirfd(d));
    zx::result channel = component::ConnectAt<partition::Partition>(caller.directory(), de->d_name);
    if (channel.is_error()) {
      continue;
    }
    {
      auto result = fidl::WireCall(channel.value())->GetInstanceGuid();
      if (!result.ok()) {
        continue;
      }
      const auto& response = result.value();
      if (response.status != ZX_OK) {
        continue;
      }
      if (memcmp(response.guid->value.data_, guid, GPT_GUID_LEN) != 0) {
        continue;
      }
    }

    auto result = fidl::WireCall(channel.value())->GetName();
    if (!result.ok()) {
      continue;
    }

    const auto& response = result.value();
    if (response.status != ZX_OK) {
      continue;
    }
    std::string_view name(response.name.data(), response.name.size());
    out.append(name);
    return true;
  }

  return false;
}

zx::result<Configuration> PartitionUuidToConfiguration(const fbl::unique_fd& devfs_root,
                                                       uuid::Uuid uuid) {
  std::string name;
  auto result = FindPartitionLabelByGuid(devfs_root, uuid.bytes(), name);
  if (!result) {
    ERROR("Failed to get partition label by GUID\n");
    return zx::error(ZX_ERR_NOT_SUPPORTED);
  }

  // Partition label should be zircon-<slot> or zircon_<slot>. This is case insensitive.
  static const size_t kZirconLength = sizeof("zircon") - 1;  // no null terminator.
  // Partition must start with "zircon".
  if (strncasecmp(name.data(), "zircon", kZirconLength) != 0) {
    ERROR("Incorrect partition name: %s \n", name.c_str());
    return zx::error(ZX_ERR_NOT_SUPPORTED);
  }

  name.erase(0, kZirconLength);

  if (name[0] != '-' && name[0] != '_') {
    ERROR("Incorrect partition name: %s \n", name.c_str());
    return zx::error(ZX_ERR_NOT_SUPPORTED);
  }

  switch (name[1]) {
    case 'a':
    case 'A':
      return zx::ok(Configuration::kA);
    case 'b':
    case 'B':
      return zx::ok(Configuration::kB);
    case 'r':
    case 'R':
      return zx::ok(Configuration::kRecovery);
  }

  ERROR("Incorrect partition name: %s \n", name.c_str());
  return zx::error(ZX_ERR_NOT_SUPPORTED);
}

zx::result<Configuration> QueryBootConfig(const fbl::unique_fd& devfs_root,
                                          fidl::UnownedClientEnd<fuchsia_io::Directory> svc_root) {
  auto client_end = component::ConnectAt<fuchsia_boot::Arguments>(svc_root);
  if (!client_end.is_ok()) {
    return client_end.take_error();
  }
  fidl::WireSyncClient client{std::move(*client_end)};
  std::array<fidl::StringView, 3> arguments{
      fidl::StringView{"zvb.current_slot"},
      fidl::StringView{"zvb.boot-partition-uuid"},
      fidl::StringView{"androidboot.slot_suffix"},
  };
  auto result = client->GetStrings(fidl::VectorView<fidl::StringView>::FromExternal(arguments));
  if (!result.ok()) {
    return zx::error(result.status());
  }

  const auto response = result.Unwrap();
  if (!response->values[0].is_null()) {
    return CurrentSlotToConfiguration(response->values[0].get());
  }
  if (!response->values[1].is_null()) {
    std::string_view uuid_str = response->values[1].get();
    auto uuid = uuid::Uuid::FromString(uuid_str);
    if (uuid == std::nullopt) {
      ERROR("Invalid UUID in zvb.boot-partition-uuid: %.*s \n", (int)uuid_str.size(),
            uuid_str.data());
      return zx::error(ZX_ERR_NOT_SUPPORTED);
    }

    return PartitionUuidToConfiguration(devfs_root, uuid.value());
  }
  if (!response->values[2].is_null()) {
    std::string_view prefix_str = response->values[2].get();
    if (prefix_str.length() == 1) {
      return CurrentSlotToConfiguration(prefix_str.substr(0, 1));
    } else if (prefix_str.length() == 2 && prefix_str[0] == '_') {
      return CurrentSlotToConfiguration(prefix_str.substr(1));
    }

    ERROR("Invalid prefix string: %.*s \n", (int)prefix_str.length(), prefix_str.data());
    return zx::error(ZX_ERR_NOT_SUPPORTED);
  }

  ERROR(
      "Kernel cmdline param zvb.current_slot, zvb.boot-partition-uuid "
      "or androidboot.slot_suffix not found!\n");
  return zx::error(ZX_ERR_NOT_SUPPORTED);
}

namespace {

zx::result<> SupportsVerifiedBoot(const fbl::unique_fd& devfs_root,
                                  fidl::UnownedClientEnd<fuchsia_io::Directory> svc_root) {
  return zx::make_result(QueryBootConfig(devfs_root, svc_root).status_value());
}
}  // namespace

zx::result<std::unique_ptr<abr::Client>> AbrPartitionClient::Create(
    std::unique_ptr<paver::PartitionClient> partition) {
  auto status = partition->GetBlockSize();
  if (status.is_error()) {
    ERROR("Unabled to get block size\n");
    return status.take_error();
  }
  size_t block_size = status.value();

  zx::vmo vmo;
  if (auto status = zx::make_result(
          zx::vmo::create(fbl::round_up(block_size, zx_system_get_page_size()), 0, &vmo));
      status.is_error()) {
    ERROR("Failed to create vmo\n");
    return status.take_error();
  }

  if (auto status = partition->Read(vmo, block_size); status.is_error()) {
    ERROR("Failed to read from partition\n");
    return status.take_error();
  }

  return zx::ok(new AbrPartitionClient(std::move(partition), std::move(vmo), block_size));
}

zx::result<> AbrPartitionClient::Read(uint8_t* buffer, size_t size) {
  if (auto status = partition_->Read(vmo_, block_size_); status.is_error()) {
    return status.take_error();
  }
  if (auto status = zx::make_result(vmo_.read(buffer, 0, size)); status.is_error()) {
    return status.take_error();
  }
  return zx::ok();
}

zx::result<> AbrPartitionClient::Write(const uint8_t* buffer, size_t size) {
  if (auto status = zx::make_result(vmo_.write(buffer, 0, size)); status.is_error()) {
    return status.take_error();
  }
  if (auto status = partition_->Write(vmo_, block_size_); status.is_error()) {
    return status.take_error();
  }
  return zx::ok();
}

std::vector<std::unique_ptr<ClientFactory>>* ClientFactory::registered_factory_list() {
  static std::vector<std::unique_ptr<ClientFactory>>* registered_factory_list = nullptr;
  if (registered_factory_list == nullptr) {
    registered_factory_list = new std::vector<std::unique_ptr<ClientFactory>>();
  }
  return registered_factory_list;
}

zx::result<std::unique_ptr<abr::Client>> ClientFactory::Create(
    fbl::unique_fd devfs_root, fidl::UnownedClientEnd<fuchsia_io::Directory> svc_root,
    std::shared_ptr<paver::Context> context) {
  if (auto status = SupportsVerifiedBoot(devfs_root, svc_root); status.is_error()) {
    ERROR("System doesn't support verified boot\n");
    return status.take_error();
  }

  for (auto& factory : *registered_factory_list()) {
    if (auto status = factory->New(devfs_root.duplicate(), svc_root, std::move(context));
        status.is_ok()) {
      return status.take_value();
    }
  }

  ERROR("Unabled to initialize ABR Client\n");
  return zx::error(ZX_ERR_NOT_FOUND);
}

void ClientFactory::Register(std::unique_ptr<ClientFactory> factory) {
  registered_factory_list()->push_back(std::move(factory));
}

bool Client::ReadAbrMetaData(void* context, size_t size, uint8_t* buffer) {
  if (auto res = static_cast<Client*>(context)->Read(buffer, size); res.is_error()) {
    ERROR("Failed to read abr data from storage. %s\n", res.status_string());
    return false;
  }
  return true;
}

bool Client::WriteAbrMetaData(void* context, const uint8_t* buffer, size_t size) {
  if (auto res = static_cast<Client*>(context)->Write(buffer, size); res.is_error()) {
    ERROR("Failed to write abr data to storage. %s\n", res.status_string());
    return false;
  }
  return true;
}

bool Client::ReadAbrMetadataCustom(void* context, AbrSlotData* a, AbrSlotData* b,
                                   uint8_t* one_shot_recovery) {
  if (auto res = static_cast<Client*>(context)->ReadCustom(a, b, one_shot_recovery);
      res.is_error()) {
    ERROR("Failed to read abr data from storage. %s\n", res.status_string());
    return false;
  }
  return true;
}

bool Client::WriteAbrMetadataCustom(void* context, const AbrSlotData* a, const AbrSlotData* b,
                                    uint8_t one_shot_recovery) {
  if (auto res = static_cast<Client*>(context)->WriteCustom(a, b, one_shot_recovery);
      res.is_error()) {
    ERROR("Failed to read abr data from storage. %s\n", res.status_string());
    return false;
  }
  return true;
}

zx::result<> Client::AbrResultToZxStatus(AbrResult status) {
  switch (status) {
    case kAbrResultOk:
      return zx::ok();
    case kAbrResultErrorIo:
      return zx::error(ZX_ERR_IO);
    case kAbrResultErrorInvalidData:
      return zx::error(ZX_ERR_INVALID_ARGS);
    case kAbrResultErrorUnsupportedVersion:
      return zx::error(ZX_ERR_NOT_SUPPORTED);
  }
  ERROR("Unknown Abr result code %d!\n", status);
  return zx::error(ZX_ERR_INTERNAL);
}

extern "C" uint32_t AbrCrc32(const void* buf, size_t buf_size) {
  return crc32(0UL, reinterpret_cast<const uint8_t*>(buf), buf_size);
}

}  // namespace abr
