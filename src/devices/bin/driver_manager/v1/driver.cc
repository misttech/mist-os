// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "driver.h"

#include <elf.h>
#include <fcntl.h>
#include <fidl/fuchsia.io/cpp/wire.h>
#include <lib/ddk/binding.h>
#include <lib/fdio/directory.h>
#include <lib/fdio/io.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <zircon/status.h>
#include <zircon/types.h>

#include <new>
#include <string>

#include <fbl/algorithm.h>
#include <fbl/string_printf.h>

#include "src/devices/bin/driver_manager/v1/fdio.h"
#include "src/devices/lib/log/log.h"

namespace {

namespace fio = fuchsia_io;

using NoteFunc = fit::function<zx_status_t(void* note, size_t size)>;

using elfhdr = Elf64_Ehdr;
using elfphdr = Elf64_Phdr;
using notehdr = Elf64_Nhdr;

zx_status_t FindNote(uint8_t* data, size_t size, NoteFunc& func) {
  const std::string kNoteName = ZIRCON_NOTE_NAME;
  constexpr uint32_t kType = ZIRCON_NOTE_DRIVER;
  while (size >= sizeof(notehdr)) {
    const auto* header = reinterpret_cast<const notehdr*>(data);
    uint32_t name_size = fbl::round_up(header->n_namesz, 4u);
    if (name_size > (size - sizeof(notehdr))) {
      return ZX_ERR_INTERNAL;
    }
    size_t header_size = sizeof(notehdr) + name_size;
    data += header_size;
    size -= header_size;

    uint32_t desc_size = fbl::round_up(header->n_descsz, 4u);
    if (desc_size > size) {
      return ZX_ERR_INTERNAL;
    }

    if ((header->n_type == kType) && (header->n_namesz - 1 == kNoteName.size()) &&
        (kNoteName ==
         std::string_view(reinterpret_cast<const char*>(header + 1), header->n_namesz - 1))) {
      return func(data - header_size, header->n_descsz + header_size);
    }

    data += desc_size;
    size -= desc_size;
  }
  return ZX_ERR_NOT_FOUND;
}

zx_status_t ForEachNote(zx::unowned_vmo dso, uint8_t* data, size_t dsize, NoteFunc func) {
  elfphdr ph[64];
  elfhdr eh;
  if (dso->read(&eh, 0, sizeof(eh)) != ZX_OK) {
    LOGF(ERROR, "zx_vmo_read(eh) failed");
    return ZX_ERR_IO;
  }
  if (memcmp(&eh, ELFMAG, 4) || (eh.e_ehsize != sizeof(elfhdr)) ||
      (eh.e_phentsize != sizeof(elfphdr))) {
    LOGF(ERROR, "bad elf magic");
    return ZX_ERR_INTERNAL;
  }
  size_t size = sizeof(elfphdr) * eh.e_phnum;
  if (size > sizeof(ph)) {
    LOGF(ERROR, "too many phdrs");
    return ZX_ERR_INTERNAL;
  }
  if (dso->read(ph, eh.e_phoff, size) != ZX_OK) {
    LOGF(ERROR, "zx_vmo_read(eh.e_phoff, size) failed");
    return ZX_ERR_IO;
  }
  for (int i = 0; i < eh.e_phnum; i++) {
    if ((ph[i].p_type != PT_NOTE) || (ph[i].p_filesz > dsize)) {
      continue;
    }
    if (dso->read(data, ph[i].p_offset, ph[i].p_filesz) != ZX_OK) {
      LOGF(ERROR, "zx_vmo_read(ph[i]) failed");
      return ZX_ERR_IO;
    }
    int r = FindNote(data, ph[i].p_filesz, func);
    if (r == ZX_OK) {
      return r;
    }
  }
  return ZX_ERR_NOT_FOUND;
}

using InfoFunc = fit::function<void(zircon_driver_note_payload_t* note)>;

zx_status_t ReadDriverInfo(zx::unowned_vmo dso, InfoFunc func) {
  constexpr size_t kBufferSize = 4096;
  uint8_t data[kBufferSize];
  return ForEachNote(std::move(dso), data, sizeof(data), [&](void* note, size_t size) {
    if (size < sizeof(zircon_driver_note_t)) {
      return ZX_ERR_INTERNAL;
    }
    func(&reinterpret_cast<zircon_driver_note_t*>(note)->payload);
    return ZX_OK;
  });
}

bool is_driver_disabled(fidl::WireSyncClient<fuchsia_boot::Arguments>* boot_args,
                        const char* name) {
  if (!boot_args) {
    return false;
  }
  // driver.<driver_name>.disable
  auto option = fbl::StringPrintf("driver.%s.disable", name);
  auto disabled = (*boot_args)->GetBool(fidl::StringView::FromExternal(option), false);
  return disabled.ok() && disabled.value().value;
}

bool is_driver_eager_fallback(fidl::WireSyncClient<fuchsia_boot::Arguments>* boot_args,
                              const char* name) {
  if (!boot_args) {
    return false;
  }
  std::vector<fbl::String> eager_fallback_drivers;
  auto drivers = (*boot_args)->GetString("devmgr.bind-eager");
  if (drivers.ok() && !drivers.value().value.is_null() && !drivers.value().value.empty()) {
    std::string list(drivers.value().value.data(), drivers.value().value.size());
    size_t pos;
    while ((pos = list.find(',')) != std::string::npos) {
      eager_fallback_drivers.emplace_back(list.substr(0, pos));
      list.erase(0, pos + 1);
    }
    eager_fallback_drivers.emplace_back(std::move(list));
  }

  for (auto& driver : eager_fallback_drivers) {
    if (driver.compare(name) == 0) {
      return true;
    }
  }
  return false;
}

void found_driver(zircon_driver_note_payload_t* note,
                  fidl::WireSyncClient<fuchsia_boot::Arguments>* boot_args, std::string_view url,
                  zx::vmo& driver_vmo, const std::vector<std::string>& service_uses,
                  DriverLoadCallback& func) {
  // ensure strings are terminated
  note->name[sizeof(note->name) - 1] = 0;
  note->vendor[sizeof(note->vendor) - 1] = 0;
  note->version[sizeof(note->version) - 1] = 0;

  if (is_driver_disabled(boot_args, note->name)) {
    return;
  }

  auto drv = std::make_unique<Driver>();
  if (drv == nullptr) {
    return;
  }

  drv->is_asan = (note->flags & ZIRCON_DRIVER_NOTE_FLAG_ASAN) != 0;
  drv->url = url;
  drv->name = note->name;
  drv->service_uses = service_uses;
  if (note->version[0] == '*') {
    drv->fallback = true;
    // TODO(fxbug.dev/44586): remove this once a better solution for driver prioritization is
    // implemented.
    if (is_driver_eager_fallback(boot_args, drv->name.c_str())) {
      LOGF(INFO, "Marking fallback driver '%s' as eager.", drv->name.c_str());
      drv->fallback = false;
    }
  }

  drv->dso_vmo = std::move(driver_vmo);

  VLOGF(2, "Found driver: %s", url.data());
  VLOGF(2, "        name: %s", note->name);
  VLOGF(2, "      vendor: %s", note->vendor);
  VLOGF(2, "     version: %s", note->version);
  VLOGF(2, "       flags: %#x", note->flags);

  func(drv.release(), note->version);
}

zx::result<zx::vmo> SetVmoName(zx::vmo vmo, const std::string& path) {
  // Path is either a URL or a filesystem path.
  // Set the vmo_name to the final part of the path, i.e the characters after the last slash '/'.
  const char* vmo_name = path.c_str();
  size_t last_slash = path.rfind('/');
  if (last_slash != std::string::npos) {
    vmo_name = &vmo_name[last_slash + 1];
  }

  if (zx_status_t status = vmo.set_property(ZX_PROP_NAME, vmo_name, strlen(vmo_name));
      status != ZX_OK) {
    LOGF(ERROR, "Cannot set name on driver VMO '%s' to '%s' %s", path.c_str(), vmo_name,
         zx_status_get_string(status));
    return zx::error(status);
  }
  return zx::ok(std::move(vmo));
}

}  // namespace

zx_status_t load_driver(fidl::WireSyncClient<fuchsia_boot::Arguments>* boot_args,
                        std::string_view url_view, zx::vmo driver_vmo,
                        const std::vector<std::string>& service_uses, DriverLoadCallback func) {
  std::string url(url_view);
  zx::unowned_vmo dso = driver_vmo.borrow();

  zx_status_t status = ReadDriverInfo(std::move(dso), [&](zircon_driver_note_payload_t* note) {
    found_driver(note, boot_args, url, driver_vmo, service_uses, func);
  });

  if (status == ZX_ERR_NOT_FOUND) {
    LOGF(INFO, "Missing info from driver '%s'", url.c_str());
  } else if (status != ZX_OK) {
    LOGF(ERROR, "Failed to read info from driver '%s': %s", url.c_str(),
         zx_status_get_string(status));
  }
  return status;
}

zx::result<zx::vmo> load_manifest_vmo(
    const fidl::WireSyncClient<fuchsia_io::Directory>& package_dir,
    std::string_view resource_path) {
  const fio::wire::OpenFlags kFileRights = fio::wire::OpenFlags::kRightReadable;
  const fio::wire::VmoFlags kManifestVmoFlags = fio::wire::VmoFlags::kRead;

  zx::result endpoints = fidl::CreateEndpoints<fuchsia_io::File>();
  if (endpoints.is_error()) {
    return endpoints.take_error();
  }
  fidl::OneWayStatus file_open_result =
      package_dir->Open(kFileRights, {} /* mode */, fidl::StringView::FromExternal(resource_path),
                        fidl::ServerEnd<fuchsia_io::Node>(endpoints->server.TakeChannel()));
  if (!file_open_result.ok()) {
    LOGF(ERROR, "Failed to open manifest file: %.*s", static_cast<int>(resource_path.size()),
         resource_path.data());
    return zx::error(ZX_ERR_INTERNAL);
  }

  fidl::WireSyncClient file_client{std::move(endpoints->client)};
  fidl::WireResult file_res = file_client->GetBackingMemory(kManifestVmoFlags);
  if (!file_res.ok()) {
    LOGF(ERROR, "Failed to get manifest vmo: %s", file_res.FormatDescription().c_str());
    return zx::error(ZX_ERR_INTERNAL);
  }
  if (file_res->is_error()) {
    LOGF(ERROR, "Failed to get manifest vmo: %s", zx_status_get_string(file_res->error_value()));
    return zx::error(ZX_ERR_INTERNAL);
  }
  return zx::ok(std::move(file_res->value()->vmo));
}

zx::result<zx::vmo> load_driver_vmo(const fidl::WireSyncClient<fuchsia_io::Directory>& package_dir,
                                    const std::string& resource_path) {
  const fio::wire::OpenFlags kFileRights =
      fio::wire::OpenFlags::kRightReadable | fio::wire::OpenFlags::kRightExecutable;
  const fio::wire::VmoFlags kDriverVmoFlags = fio::wire::VmoFlags::kRead |
                                              fio::wire::VmoFlags::kExecute |
                                              fio::wire::VmoFlags::kPrivateClone;

  zx::result endpoints = fidl::CreateEndpoints<fuchsia_io::File>();
  if (endpoints.is_error()) {
    return endpoints.take_error();
  }
  fidl::OneWayStatus file_open_result =
      package_dir->Open(kFileRights, {} /* mode */, fidl::StringView::FromExternal(resource_path),
                        fidl::ServerEnd<fuchsia_io::Node>(endpoints->server.TakeChannel()));
  if (!file_open_result.ok()) {
    LOGF(ERROR, "Failed to open driver file: %.*s", static_cast<int>(resource_path.size()),
         resource_path.data());
    return zx::error(ZX_ERR_INTERNAL);
  }

  fidl::WireSyncClient file_client{std::move(endpoints->client)};
  fidl::WireResult file_res = file_client->GetBackingMemory(kDriverVmoFlags);
  if (!file_res.ok()) {
    LOGF(ERROR, "Failed to get driver vmo: %s", file_res.FormatDescription().c_str());
    return zx::error(ZX_ERR_INTERNAL);
  }
  if (file_res->is_error()) {
    LOGF(ERROR, "Failed to get driver vmo: %s", zx_status_get_string(file_res->error_value()));
    return zx::error(ZX_ERR_INTERNAL);
  }
  return SetVmoName(std::move(file_res->value()->vmo), resource_path);
}
