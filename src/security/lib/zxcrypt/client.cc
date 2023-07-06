// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/security/lib/zxcrypt/client.h"

#include <errno.h>
#include <fcntl.h>
#include <fidl/fuchsia.device/cpp/wire.h>
#include <inttypes.h>
#include <lib/device-watcher/cpp/device-watcher.h>
#include <lib/fdio/cpp/caller.h>
#include <lib/fdio/directory.h>
#include <lib/fdio/fd.h>
#include <lib/zircon-internal/debug.h>
#include <lib/zx/channel.h>
#include <unistd.h>
#include <zircon/status.h>

#include <memory>
#include <utility>

#include <fbl/string_buffer.h>
#include <fbl/vector.h>

#include "lib/stdcompat/string_view.h"
#include "src/security/lib/kms-stateless/kms-stateless.h"

#define ZXDEBUG 0

namespace zxcrypt {

// The zxcrypt driver
const char* kDriverLib = "zxcrypt.cm";

namespace {

// Null key should be 32 bytes.
const size_t kKeyLength = 32;
const char kHardwareKeyInfo[] = "zxcrypt";

// How many bytes to read from /boot/config/zxcrypt?
const size_t kMaxKeySourcePolicyLength = 32;
const char kZxcryptConfigFileLocation1[] = "/pkg/config/zxcrypt";
const char kZxcryptConfigFileLocation2[] = "/boot/config/zxcrypt";

zx::result<fbl::String> RelativeTopologicalPath(
    fidl::UnownedClientEnd<fuchsia_device::Controller> controller) {
  const fidl::WireResult result = fidl::WireCall(controller)->GetTopologicalPath();
  if (!result.ok()) {
    return zx::error(result.status());
  }
  const fit::result response = result.value();
  if (response.is_error()) {
    return zx::error(response.error_value());
  }
  std::string_view path = response.value()->path.get();
  constexpr std::string_view kSlashDevSlash = "/dev/";
  if (!cpp20::starts_with(path, kSlashDevSlash)) {
    return zx::error(ZX_ERR_INTERNAL);
  }
  path.remove_prefix(kSlashDevSlash.size());
  return zx::ok(fbl::String(path));
}

}  // namespace

__EXPORT
zx::result<KeySourcePolicy> SelectKeySourcePolicy() {
  const char* file_used = kZxcryptConfigFileLocation1;
  fbl::unique_fd fd(open(file_used, O_RDONLY));
  if (!fd) {
    xprintf("zxcrypt: couldn't open %s\n", file_used);
    file_used = kZxcryptConfigFileLocation2;
    fd.reset(open(file_used, O_RDONLY));
    if (!fd) {
      xprintf("zxcrypt: couldn't open %s\n", file_used);
      return zx::error(ZX_ERR_NOT_FOUND);
    }
  }

  char key_source_buf[kMaxKeySourcePolicyLength + 1];
  ssize_t len = read(fd.get(), key_source_buf, sizeof(key_source_buf) - 1);
  if (len < 0) {
    xprintf("zxcrypt: couldn't read %s\n", file_used);
    return zx::error(ZX_ERR_IO);
  }
  // add null terminator
  key_source_buf[len] = '\0';
  // Dispatch if recognized
  if (strcmp(key_source_buf, "null") == 0) {
    return zx::ok(NullSource);
  }
  if (strcmp(key_source_buf, "tee") == 0) {
    return zx::ok(TeeRequiredSource);
  }
  if (strcmp(key_source_buf, "tee-transitional") == 0) {
    return zx::ok(TeeTransitionalSource);
  }
  if (strcmp(key_source_buf, "tee-opportunistic") == 0) {
    return zx::ok(TeeOpportunisticSource);
  }
  return zx::error(ZX_ERR_BAD_STATE);
}

// Returns a ordered vector of |KeySource|s, representing all key sources,
// ordered from most-preferred to least-preferred, that we should try for the
// purposes of creating a new volume
__EXPORT
fbl::Vector<KeySource> ComputeEffectiveCreatePolicy(KeySourcePolicy ksp) {
  fbl::Vector<KeySource> r;
  switch (ksp) {
    case NullSource:
      r = {kNullSource};
      break;
    case TeeRequiredSource:
    case TeeTransitionalSource:
      r = {kTeeSource};
      break;
    case TeeOpportunisticSource:
      r = {kTeeSource, kNullSource};
      break;
  }
  return r;
}

// Returns a ordered vector of |KeySource|s, representing all key sources,
// ordered from most-preferred to least-preferred, that we should try for the
// purposes of unsealing an existing volume
__EXPORT
fbl::Vector<KeySource> ComputeEffectiveUnsealPolicy(KeySourcePolicy ksp) {
  fbl::Vector<KeySource> r;
  switch (ksp) {
    case NullSource:
      r = {kNullSource};
      break;
    case TeeRequiredSource:
      r = {kTeeSource};
      break;
    case TeeTransitionalSource:
    case TeeOpportunisticSource:
      r = {kTeeSource, kNullSource};
      break;
  }
  return r;
}

__EXPORT
fbl::Vector<KeySource> ComputeEffectivePolicy(KeySourcePolicy ksp, Activity activity) {
  fbl::Vector<KeySource> r;
  switch (activity) {
    case Create:
      r = ComputeEffectiveCreatePolicy(ksp);
      break;
    case Unseal:
      r = ComputeEffectiveUnsealPolicy(ksp);
      break;
  }
  return r;
}

__EXPORT
zx_status_t TryWithImplicitKeys(
    Activity activity, fit::function<zx_status_t(std::unique_ptr<uint8_t[]>, size_t)> callback) {
  auto source = SelectKeySourcePolicy();
  if (source.is_error()) {
    return source.status_value();
  }

  zx_status_t rc = ZX_ERR_INTERNAL;
  auto ordered_key_sources = ComputeEffectivePolicy(*source, activity);

  for (auto& key_source : ordered_key_sources) {
    switch (key_source) {
      case kNullSource: {
        auto key_buf = std::unique_ptr<uint8_t[]>(new uint8_t[kKeyLength]);
        memset(key_buf.get(), 0, kKeyLength);
        rc = callback(std::move(key_buf), kKeyLength);
      } break;
      case kTeeSource: {
        // key info is |kHardwareKeyInfo| padded with 0.
        uint8_t key_info[kms_stateless::kExpectedKeyInfoSize] = {0};
        memcpy(key_info, kHardwareKeyInfo, sizeof(kHardwareKeyInfo));
        // make names for these so the callback to kms_stateless can
        // copy them out later
        std::unique_ptr<uint8_t[]> key_buf;
        size_t key_size;
        zx_status_t kms_rc = kms_stateless::GetHardwareDerivedKey(
            [&](std::unique_ptr<uint8_t[]> cb_key_buffer, size_t cb_key_size) {
              key_size = cb_key_size;
              key_buf = std::unique_ptr<uint8_t[]>(new uint8_t[cb_key_size]);
              memcpy(key_buf.get(), cb_key_buffer.get(), cb_key_size);
              return ZX_OK;
            },
            key_info);

        if (kms_rc != ZX_OK) {
          rc = kms_rc;
          break;
        }

        rc = callback(std::move(key_buf), key_size);
      } break;
    }
    if (rc == ZX_OK) {
      return rc;
    }
  }

  xprintf("TryWithImplicitKeys (%s): none of the %lu key sources succeeded\n",
          activity == Activity::Create ? "create" : "unseal", ordered_key_sources.size());
  return rc;
}

EncryptedVolumeClient::EncryptedVolumeClient(zx::channel&& channel)
    : client_end_(std::move(channel)) {}

zx_status_t EncryptedVolumeClient::Format(const uint8_t* key, size_t key_len, uint8_t slot) {
  const fidl::WireResult result =
      fidl::WireCall(client_end_)
          ->Format(fidl::VectorView<uint8_t>::FromExternal(const_cast<uint8_t*>(key), key_len),
                   slot);
  if (!result.ok()) {
    xprintf("failed to call Format: %s\n", result.FormatDescription().c_str());
    return result.status();
  }
  const fidl::WireResponse response = result.value();
  if (response.status != ZX_OK) {
    xprintf("failed to Format: %s\n", zx_status_get_string(response.status));
  }
  return response.status;
}

zx_status_t EncryptedVolumeClient::FormatWithImplicitKey(uint8_t slot) {
  return TryWithImplicitKeys(Activity::Create,
                             [&](std::unique_ptr<uint8_t[]> key_buffer, size_t key_size) {
                               return Format(key_buffer.get(), key_size, slot);
                             });
}

zx_status_t EncryptedVolumeClient::Unseal(const uint8_t* key, size_t key_len, uint8_t slot) {
  const fidl::WireResult result =
      fidl::WireCall(client_end_)
          ->Unseal(fidl::VectorView<uint8_t>::FromExternal(const_cast<uint8_t*>(key), key_len),
                   slot);
  if (!result.ok()) {
    xprintf("failed to call Unseal: %s\n", result.FormatDescription().c_str());
    return result.status();
  }
  const fidl::WireResponse response = result.value();
  if (response.status != ZX_OK) {
    xprintf("failed to Unseal: %s\n", zx_status_get_string(response.status));
  }
  return response.status;
}

zx_status_t EncryptedVolumeClient::UnsealWithImplicitKey(uint8_t slot) {
  return TryWithImplicitKeys(Activity::Unseal,
                             [&](std::unique_ptr<uint8_t[]> key_buffer, size_t key_size) {
                               return Unseal(key_buffer.get(), key_size, slot);
                             });
}

zx_status_t EncryptedVolumeClient::Seal() {
  const fidl::WireResult result = fidl::WireCall(client_end_)->Seal();
  if (!result.ok()) {
    xprintf("failed to call Seal: %s\n", result.FormatDescription().c_str());
    return result.status();
  }
  const fidl::WireResponse response = result.value();
  if (response.status != ZX_OK) {
    xprintf("failed to Seal: %s\n", zx_status_get_string(response.status));
  }
  return response.status;
}

zx_status_t EncryptedVolumeClient::Shred() {
  const fidl::WireResult result = fidl::WireCall(client_end_)->Shred();
  if (!result.ok()) {
    xprintf("failed to call Shred: %s\n", result.FormatDescription().c_str());
    return result.status();
  }
  const fidl::WireResponse response = result.value();
  if (response.status != ZX_OK) {
    xprintf("failed to Shred: %s\n", zx_status_get_string(response.status));
  }
  return response.status;
}

VolumeManager::VolumeManager(fidl::ClientEnd<fuchsia_device::Controller> block_controller,
                             fbl::unique_fd devfs_root_fd)
    : block_controller_(std::move(block_controller)), devfs_root_fd_(std::move(devfs_root_fd)) {}

zx_status_t VolumeManager::Unbind() {
  return fidl::WireCall(block_controller_)->UnbindChildren().status();
}

zx_status_t VolumeManager::OpenInnerBlockDevice(const zx::duration& timeout, fbl::unique_fd* out) {
  zx::result path_base = RelativeTopologicalPath(block_controller_);
  if (path_base.is_error()) {
    xprintf("could not get topological path: %s\n", path_base.status_string());
    return path_base.error_value();
  }
  fbl::String path_block_exposed =
      fbl::String::Concat({path_base.value(), "/zxcrypt/unsealed/block"});

  // Wait for the unsealed and block devices to bind
  zx::result channel = device_watcher::RecursiveWaitForFile(devfs_root_fd_.get(),
                                                            path_block_exposed.c_str(), timeout);
  if (channel.is_error()) {
    xprintf("failure waiting for %s to exist: %s\n", path_block_exposed.c_str(),
            channel.status_string());
    return channel.error_value();
  }
  if (zx_status_t status = fdio_fd_create(channel.value().release(), out->reset_and_get_address());
      status != ZX_OK) {
    xprintf("failed to open zxcrypt volume: %s\n", zx_status_get_string(status));
    return status;
  }
  return ZX_OK;
}

zx_status_t VolumeManager::OpenClient(const zx::duration& timeout, zx::channel& out) {
  zx::result path_base = RelativeTopologicalPath(block_controller_);
  if (path_base.is_error()) {
    xprintf("could not get topological path: %s\n", path_base.status_string());
    return path_base.error_value();
  }
  fbl::String path_manager = fbl::String::Concat({path_base.value(), "/zxcrypt"});

  if (faccessat(devfs_root_fd_.get(), path_manager.c_str(), 0, F_OK) == 0) {
    fdio_cpp::UnownedFdioCaller caller(devfs_root_fd_.get());
    if (!caller) {
      xprintf("could not convert fd to io\n");
      return ZX_ERR_BAD_STATE;
    }
    zx::channel server;
    if (zx_status_t status = zx::channel::create(0, &out, &server); status != ZX_OK) {
      xprintf("failed to create channel: %s\n", zx_status_get_string(status));
      return status;
    }
    if (zx_status_t status = fdio_service_connect_at(caller.borrow_channel(), path_manager.c_str(),
                                                     server.release());
        status != ZX_OK) {
      xprintf("failed to connect to zxcrypt manager: %s\n", zx_status_get_string(status));
      return status;
    }
    return ZX_OK;
  }

  // No manager device in the /dev tree yet.  Try binding the zxcrypt
  // driver and waiting for it to appear.
  fidl::WireResult resp =
      fidl::WireCall(block_controller_)->Bind(::fidl::StringView::FromExternal(kDriverLib));
  zx_status_t status = resp.status();
  if (status == ZX_OK) {
    if (resp.value().is_error()) {
      status = resp.value().error_value();
    }
  }
  if (status != ZX_OK) {
    xprintf("could not bind zxcrypt driver: %s\n", zx_status_get_string(status));
    return status;
  }

  // Await the appearance of the zxcrypt device.
  zx::result channel =
      device_watcher::RecursiveWaitForFile(devfs_root_fd_.get(), path_manager.c_str(), timeout);
  if (channel.is_error()) {
    xprintf("failue waiting for zxcrypt driver to bind: %s\n", channel.status_string());
    return channel.error_value();
  }
  out = std::move(channel.value());
  return ZX_OK;
}

}  // namespace zxcrypt
