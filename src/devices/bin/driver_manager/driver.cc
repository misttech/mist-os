// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "driver.h"

#include <fidl/fuchsia.io/cpp/wire.h>
#include <lib/ddk/binding.h>
#include <lib/fdio/directory.h>
#include <lib/fdio/io.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <zircon/status.h>
#include <zircon/types.h>

#include <new>
#include <string>

#include <driver-info/driver-info.h>
#include <fbl/string_printf.h>

#include "fdio.h"
#include "src/devices/lib/log/log.h"

namespace {

namespace fio = fuchsia_io;

struct AddContext {
  fidl::WireSyncClient<fuchsia_boot::Arguments>* boot_args;
  const char* libname;
  DriverLoadCallback func;
  // This is optional. If present, holds the driver shared library that was loaded ephemerally.
  zx::vmo vmo;
};

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

void found_driver(zircon_driver_note_payload_t* note, void* cookie) {
  auto context = static_cast<AddContext*>(cookie);

  // ensure strings are terminated
  note->name[sizeof(note->name) - 1] = 0;
  note->vendor[sizeof(note->vendor) - 1] = 0;
  note->version[sizeof(note->version) - 1] = 0;

  if (is_driver_disabled(context->boot_args, note->name)) {
    return;
  }

  auto drv = std::make_unique<Driver>();
  if (drv == nullptr) {
    return;
  }

  drv->flags = note->flags;
  drv->libname = context->libname;
  drv->name = note->name;
  if (note->version[0] == '*') {
    drv->fallback = true;
    // TODO(fxbug.dev/44586): remove this once a better solution for driver prioritization is
    // implemented.
    if (is_driver_eager_fallback(context->boot_args, drv->name.c_str())) {
      LOGF(INFO, "Marking fallback driver '%s' as eager.", drv->name.c_str());
      drv->fallback = false;
    }
  }

  if (context->vmo.is_valid()) {
    drv->dso_vmo = std::move(context->vmo);
  }

  VLOGF(2, "Found driver: %s", (char*)cookie);
  VLOGF(2, "        name: %s", note->name);
  VLOGF(2, "      vendor: %s", note->vendor);
  VLOGF(2, "     version: %s", note->version);
  VLOGF(2, "       flags: %#x", note->flags);

  context->func(drv.release(), note->version);
}

}  // namespace

zx_status_t load_driver_vmo(fidl::WireSyncClient<fuchsia_boot::Arguments>* boot_args,
                            std::string_view libname_view, zx::vmo vmo, DriverLoadCallback func) {
  std::string libname(libname_view);
  zx_handle_t vmo_handle = vmo.get();
  AddContext context = {boot_args, libname.c_str(), std::move(func), std::move(vmo)};

  auto di_vmo_read = [](void* vmo, void* data, size_t len, size_t off) {
    return zx_vmo_read(*((zx_handle_t*)vmo), data, off, len);
  };
  zx_status_t status = di_read_driver_info_etc(&vmo_handle, di_vmo_read, &context, found_driver);

  if (status == ZX_ERR_NOT_FOUND) {
    LOGF(INFO, "Missing info from driver '%s'", libname.c_str());
  } else if (status != ZX_OK) {
    LOGF(ERROR, "Failed to read info from driver '%s': %s", libname.c_str(),
         zx_status_get_string(status));
  }
  return status;
}

zx::result<zx::vmo> load_vmo(std::string_view libname_view) {
  std::string libname(libname_view);
  fbl::unique_fd fd;
  constexpr uint32_t file_rights = static_cast<uint32_t>(fio::wire::OpenFlags::kRightReadable |
                                                         fio::wire::OpenFlags::kRightExecutable);
  if (zx_status_t status = fdio_open_fd(libname.c_str(), file_rights, fd.reset_and_get_address());
      status != ZX_OK) {
    LOGF(ERROR, "Cannot open driver '%s': %d", libname.c_str(), status);
    return zx::error(ZX_ERR_IO);
  }

  zx::vmo vmo;
  if (zx_status_t status = fdio_get_vmo_exec(fd.get(), vmo.reset_and_get_address());
      status != ZX_OK) {
    LOGF(ERROR, "Cannot get driver VMO '%s'", libname.c_str());
    return zx::error(status);
  }

  // Libname is either a URL or a filesystem path.
  // Set the vmo_name to the final part of the path, i.e the characters after the last slash '/'.
  const char* vmo_name = libname.c_str();
  size_t last_slash = libname.rfind('/');
  if (last_slash != std::string::npos) {
    vmo_name = &libname[last_slash + 1];
  }

  if (zx_status_t status = vmo.set_property(ZX_PROP_NAME, vmo_name, strlen(vmo_name));
      status != ZX_OK) {
    LOGF(ERROR, "Cannot set name on driver VMO to '%s'", libname.c_str());
    return zx::error(status);
  }
  return zx::ok(std::move(vmo));
}
