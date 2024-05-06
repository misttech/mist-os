// Copyright 2024 Mist Tecnologia LTDA
// Copyright 2016 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include "lib/mistos/zbi_parser/bootfs.h"

#include <lib/debuglog.h>
#include <lib/fit/result.h>
#include <lib/zbitl/error-stdio.h>
#include <stdarg.h>
#include <trace.h>
#include <zircon/errors.h>
#include <zircon/types.h>

#include <ktl/enforce.h>

#define LOCAL_TRACE 0

#define check(log, status, fmt, ...)                                   \
  do {                                                                 \
    if (status != ZX_OK) {                                             \
      TRACEF("%s: " fmt, zx_status_get_string(status), ##__VA_ARGS__); \
      return fit::error(status);                                       \
    }                                                                  \
  } while (0)

namespace zbi_parser {

void PrintBootfsError(const Bootfs::BootfsView::Error& error) {
  zbitl::PrintBootfsError(error, [](const char* fmt, ...) {
    va_list args;
    va_start(args, fmt);
    vprintf(fmt, args);
    va_end(args);
  });
}

Bootfs::Bootfs(zx::unowned_vmar vmar, zx::vmo vmo, zx::resource vmex_resource)
    : vmex_resource_(ktl::move(vmex_resource)) {
  zbitl::MapOwnedVmo mapvmo{ktl::move(vmo), /*writable=*/false, ktl::move(vmar)};
  if (auto result = BootfsReader::Create(ktl::move(mapvmo)); result.is_error()) {
    PrintBootfsError(result.error_value());
  } else {
    bootfs_reader_ = ktl::move(result.value());
    is_valid_ = true;
  }
}

fit::result<zx_status_t, zx::vmo> Bootfs::Open(ktl::string_view root, ktl::string_view filename,
                                               ktl::string_view purpose) {
  LTRACEF_LEVEL(2, "searching BOOTFS for '%.*s%s%.*s' (%.*s)\n", static_cast<int>(root.size()),
                root.data(), root.empty() ? "" : "/", static_cast<int>(filename.size()),
                filename.data(), static_cast<int>(purpose.size()), purpose.data());

  if (!is_valid_) {
    TRACEF("Bootfs is not valid.\n");
    return fit::error(ZX_ERR_BAD_STATE);
  }

  BootfsView bootfs = bootfs_reader_.root();
  auto it = root.empty() ? bootfs.find(filename) : bootfs.find({root, filename});
  if (auto result = bootfs.take_error(); result.is_error()) {
    PrintBootfsError(result.error_value());
    return fit::error(ZX_ERR_NOT_FOUND);
  }

  if (it == bootfs.end()) {
    LTRACEF_LEVEL(2, "failed to find '%.*s%s%.*s'\n", static_cast<int>(root.size()), root.data(),
                  root.empty() ? "" : "/", static_cast<int>(filename.size()), filename.data());
    return fit::error(ZX_ERR_NOT_FOUND);
  }

  // Clone a private, read-only snapshot of the file's subset of the bootfs VMO.
  zx::vmo file_vmo;
  zx_status_t status = bootfs_reader_.storage().vmo().create_child(
      ZX_VMO_CHILD_SNAPSHOT | ZX_VMO_CHILD_NO_WRITE, it->offset, it->size, &file_vmo);
  if (status != ZX_OK) {
    return fit::error(status);
  }

  status = file_vmo.set_property(ZX_PROP_NAME, filename.data(), filename.size());
  check(log_, status, "failed to set ZX_PROP_NAME\n");

  uint64_t size = it->size;
  status = file_vmo.set_property(ZX_PROP_VMO_CONTENT_SIZE, &size, sizeof(size));
  check(log_, status, "failed to set ZX_PROP_VMO_CONTENT_SIZE\n");

  status = file_vmo.replace_as_executable(vmex_resource_, &file_vmo);
  check(log_, status, "zx_vmo_replace_as_executable failed");

  return fit::ok(ktl::move(file_vmo));
}

}  // namespace zbi_parser
