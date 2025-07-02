// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/sshd-host/authorized_keys.h"

#include <fcntl.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/zx/result.h>
#include <lib/zx/vmo.h>
#include <stdio.h>
#include <sys/stat.h>

#include <memory>

#include <fbl/unique_fd.h>

#include "src/developer/sshd-host/constants.h"

namespace sshd_host {

zx_status_t provision_authorized_keys_from_bootloader_file(
    fidl::SyncClient<fuchsia_boot::Items>& boot_items) {
  auto result =
      boot_items->GetBootloaderFile({{.filename = std::string(kAuthorizedKeysBootloaderFileName)}});

  if (result.is_error()) {
    FX_PLOGS(ERROR, result.error_value().status())
        << "Provisioning keys from boot item: GetBootloaderFile failed";
    return result.error_value().status();
  }
  zx::vmo vmo = std::move(result->payload());

  if (!vmo.is_valid()) {
    FX_LOGS(INFO) << "Provisioning keys from boot item: bootloader file not found: "
                  << kAuthorizedKeysBootloaderFileName;
    return ZX_ERR_NOT_FOUND;
  }

  uint64_t size;
  if (zx_status_t status = vmo.get_property(ZX_PROP_VMO_CONTENT_SIZE, &size, sizeof(size));
      status != ZX_OK) {
    FX_PLOGS(ERROR, status) << "Provisioning keys from boot item: unable to get file size";
    return status;
  }

  std::unique_ptr buffer = std::make_unique<uint8_t[]>(size);
  if (zx_status_t status = vmo.read(buffer.get(), 0, size); status != ZX_OK) {
    FX_PLOGS(ERROR, status) << "Provisioning keys from boot item: failed to read file";
    return status;
  }

  if (mkdir(kSshDirectory, 0700) && errno != EEXIST) {
    FX_LOGS(ERROR) << "Provisioning keys from boot item: failed to create directory: "
                   << kSshDirectory << " Error: " << strerror(errno);
    return ZX_ERR_IO;
  }

  fbl::unique_fd kfd(open(kAuthorizedKeysPath, O_CREAT | O_EXCL | O_WRONLY, S_IRUSR | S_IWUSR));
  if (!kfd) {
    FX_LOGS(ERROR) << "Provisioning keys from boot item: open failed: " << kAuthorizedKeysPath
                   << " error: " << strerror(errno);
    return errno == EEXIST ? ZX_ERR_ALREADY_EXISTS : ZX_ERR_IO;
  }

  if (write(kfd.get(), buffer.get(), size) != static_cast<ssize_t>(size)) {
    FX_LOGS(ERROR) << "Provisioning keys from boot item: write failed: " << strerror(errno);
    return ZX_ERR_IO;
  }

  fsync(kfd.get());

  if (close(kfd.release())) {
    FX_LOGS(ERROR) << "Provisioning keys from boot item: close failed: " << strerror(errno);
    return ZX_ERR_IO;
  }

  FX_LOGS(INFO) << "Provisioning keys from boot item: authorized_keys provisioned";
  return ZX_OK;
}

}  // namespace sshd_host
