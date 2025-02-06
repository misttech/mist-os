// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <dlfcn.h>
#include <fidl/fuchsia.io/cpp/wire.h>
#include <lib/driver/component/cpp/internal/symbols.h>
#include <lib/driver/devicetree/visitors/default/default.h>
#include <lib/driver/devicetree/visitors/load-visitors.h>
#include <lib/driver/devicetree/visitors/registration.h>
#include <lib/driver/logging/cpp/logger.h>
#include <lib/fdio/directory.h>
#include <zircon/availability.h>
#include <zircon/dlfcn.h>

#include <memory>
#include <string_view>
#include <utility>

namespace {
namespace fio = fuchsia_io;
constexpr const char kVisitorsPath[] = "/pkg/lib/visitors";

struct dirent_t {
  // Describes the inode of the entry.
  uint64_t ino;
  // Describes the length of the dirent name in bytes.
  uint8_t size;
  // Describes the type of the entry. Aligned with the
  // POSIX d_type values. Use `DirentType` constants.
  uint8_t type;
  // Unterminated name of entry.
  char name[0];
} __PACKED;

zx::result<zx::vmo> SetVmoName(zx::vmo vmo, std::string_view vmo_name) {
  if (zx_status_t status = vmo.set_property(ZX_PROP_NAME, vmo_name.data(), vmo_name.size());
      status != ZX_OK) {
    FDF_LOG(ERROR, "Cannot set name on visitor VMO '%.*s' %s", static_cast<int>(vmo_name.length()),
            vmo_name.data(), zx_status_get_string(status));
    return zx::error(status);
  }
  return zx::ok(std::move(vmo));
}

zx::result<zx::vmo> LoadVisitorVmo(fdf::Namespace& incoming, std::string_view visitor_file) {
#if FUCHSIA_API_LEVEL_AT_LEAST(24)
  constexpr fio::Flags kVisitorOpenFlags = fio::Flags::kPermRead | fio::Flags::kPermExecute;
#else
  constexpr fio::OpenFlags kVisitorOpenFlags =
      fio::OpenFlags::kRightReadable | fio::OpenFlags::kRightExecutable;
#endif
  constexpr fio::VmoFlags kVisitorVmoFlag =
      fio::VmoFlags::kRead | fio::VmoFlags::kExecute | fio::VmoFlags::kPrivateClone;

  std::string full_path = std::string(kVisitorsPath) + "/" + visitor_file.data();
  zx::result client_end = incoming.Open<fuchsia_io::File>(full_path.c_str(), kVisitorOpenFlags);
  if (client_end.is_error()) {
    FDF_LOG(ERROR, "Failed to open visitor '%.*s': %s", static_cast<int>(visitor_file.length()),
            visitor_file.data(), client_end.status_string());
    return client_end.take_error();
  }

  fidl::WireSyncClient file_client{std::move(*client_end)};
  fidl::WireResult file_res = file_client->GetBackingMemory(kVisitorVmoFlag);
  if (!file_res.ok()) {
    FDF_LOG(ERROR, "Failed to get visitor '%.*s' vmo: %s", static_cast<int>(visitor_file.length()),
            visitor_file.data(), file_res.FormatDescription().c_str());
    return zx::error(ZX_ERR_INTERNAL);
  }

  if (file_res->is_error()) {
    FDF_LOG(ERROR, "Failed to get visitor '%.*s' vmo: %s", static_cast<int>(visitor_file.length()),
            visitor_file.data(), zx_status_get_string(file_res->error_value()));
    return zx::error(ZX_ERR_INTERNAL);
  }

  return SetVmoName(std::move(file_res->value()->vmo), visitor_file);
}

zx::result<std::vector<std::string>> GetVisitorFiles(fdf::Namespace& incoming) {
  std::vector<std::string> visitor_files;

  zx::result client_end = incoming.Open<fuchsia_io::Directory>(
      kVisitorsPath, fio::Flags::kProtocolDirectory | fio::Flags::kPermEnumerate);
  if (client_end.is_error()) {
    FDF_LOG(ERROR, "Failed to open visitors directory");
    return client_end.take_error();
  }

  fidl::WireSyncClient directory{std::move(*client_end)};
  while (true) {
    auto result = directory->ReadDirents(fio::kMaxBuf);
    if (!result.ok()) {
      FDF_LOG(INFO, "ReadDirents call failed %s", result.status_string());
      break;
    }

    if (result->s != ZX_OK) {
      FDF_LOG(INFO, "ReadDirents failed %d", result->s);
      break;
    }

    if (result->dirents.empty()) {
      break;
    }

    size_t index = 0;
    while (index + sizeof(dirent_t) < result->dirents.count()) {
      auto packed_entry = reinterpret_cast<const dirent_t*>(&result->dirents[index]);
      size_t packed_entry_size = sizeof(dirent_t) + packed_entry->size;
      if (index + packed_entry_size > result->dirents.count()) {
        break;
      }
      index += packed_entry_size;

      std::string name(packed_entry->name, packed_entry->size);
      if (name != ".") {
        FDF_LOG(DEBUG, "Visitor found: %s", name.c_str());
        visitor_files.push_back(std::move(name));
      }
    }
  }

  return zx::ok(std::move(visitor_files));
}

}  // namespace

namespace fdf_devicetree {
zx::result<std::unique_ptr<VisitorRegistry>> LoadVisitors(fdf::Namespace& incoming) {
  auto visitors = std::make_unique<VisitorRegistry>();

  auto status = visitors->RegisterVisitor(std::make_unique<DefaultVisitors<>>());
  if (status.is_error()) {
    FDF_LOG(ERROR, "DefaultVisitors registration failed: %s", status.status_string());
    return status.take_error();
  }

  zx::result visitor_files = GetVisitorFiles(incoming);
  if (visitor_files.is_error()) {
    FDF_LOG(ERROR, "Getting visitor files failed: %s", visitor_files.status_string());
    return visitor_files.take_error();
  }

  for (const auto& visitor_file : *visitor_files) {
    auto vmo = LoadVisitorVmo(incoming, visitor_file);
    if (vmo.is_error() || !vmo->is_valid()) {
      FDF_LOG(ERROR, "failed to load vmo for visitor: '%s'", visitor_file.c_str());
      continue;
    }

    void* visitor_lib = dlopen_vmo(vmo->get(), RTLD_NOW);
    if (!visitor_lib) {
      FDF_LOG(ERROR, "dlopen failed for visitor: '%s'", visitor_file.c_str());
      continue;
    }

    auto registration = static_cast<const VisitorRegistration*>(
        dlsym(visitor_lib, "__devicetree_visitor_registration__"));
    if (registration == nullptr) {
      FDF_LOG(ERROR, "Symbol __devicetree_visitor_registration__ not found in visitor: '%s'",
              visitor_file.c_str());
      continue;
    }

    auto visitor = registration->v1.create_visitor(fdf::Logger::GlobalInstance());
    if (!visitor) {
      FDF_LOG(ERROR, "visitor '%s' creation failed", visitor_file.c_str());
      continue;
    }

    status = visitors->RegisterVisitor(std::move(visitor));
    if (status.is_error()) {
      FDF_LOG(ERROR, "visitor '%s' registration failed: %s", visitor_file.c_str(),
              status.status_string());
      continue;
    }

    FDF_LOG(DEBUG, "visitor '%s' registered", visitor_file.c_str());
  }
  return zx::ok(std::move(visitors));
}

zx::result<std::unique_ptr<VisitorRegistry>> LoadVisitors(
    const std::optional<std::vector<fuchsia_driver_framework::NodeSymbol>>& symbols) {
  if (!symbols.has_value()) {
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  auto visitors = std::make_unique<VisitorRegistry>();

  auto status = visitors->RegisterVisitor(std::make_unique<DefaultVisitors<>>());
  if (status.is_error()) {
    FDF_LOG(ERROR, "DefaultVisitors registration failed: %s", status.status_string());
    return status.take_error();
  }

  std::unordered_set modules = fdf_internal::GetModules(symbols);

  for (const auto& module_name : modules) {
    auto registration = fdf_internal::GetSymbol<const VisitorRegistration*>(
        symbols, module_name, "__devicetree_visitor_registration__");
    if (registration == nullptr) {
      FDF_LOG(ERROR, "Symbol __devicetree_visitor_registration__ not found in visitor: '%s'",
              module_name.c_str());
      continue;
    }

    auto visitor = registration->v1.create_visitor(fdf::Logger::GlobalInstance());
    if (!visitor) {
      FDF_LOG(ERROR, "visitor '%s' creation failed", module_name.c_str());
      continue;
    }

    status = visitors->RegisterVisitor(std::move(visitor));
    if (status.is_error()) {
      FDF_LOG(ERROR, "visitor '%s' registration failed: %s", module_name.c_str(),
              status.status_string());
      continue;
    }

    FDF_LOG(DEBUG, "visitor '%s' registered", module_name.c_str());
  }
  return zx::ok(std::move(visitors));
}

}  // namespace fdf_devicetree
