// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/component/incoming/cpp/service_watcher.h>
#include <lib/fdio/directory.h>

namespace component {

zx_status_t ServiceWatcher::Begin(fidl::ClientEnd<fuchsia_io::Directory> dir,
                                  ServiceWatcher::Callback callback,
                                  async_dispatcher_t* dispatcher) {
  if (callback_ != nullptr) {
    return ZX_ERR_UNAVAILABLE;
  }
  callback_ = std::move(callback);
  auto [client, server] = fidl::Endpoints<fuchsia_io::DirectoryWatcher>::Create();

  auto watch_mask = fuchsia_io::wire::WatchMask::kAdded | fuchsia_io::wire::WatchMask::kExisting |
                    fuchsia_io::wire::WatchMask::kIdle;
  const fidl::WireResult<::fuchsia_io::Directory::Watch> result =
      fidl::WireCall(dir)->Watch(watch_mask, 0, std::move(server));
  if (!result.ok()) {
    return result.status();
  }
  const fidl::WireResponse<::fuchsia_io::Directory::Watch> response = result.value();
  if (zx_status_t status = response.s; status != ZX_OK) {
    return status;
  }
  buf_ = std::make_shared<uint8_t[fuchsia_io::wire::kMaxBuf]>();
  client_end_ = client.TakeChannel();
  wait_.set_object(client_end_.get());
  wait_.set_trigger(ZX_CHANNEL_READABLE);
  return wait_.Begin(dispatcher);
}

zx_status_t ServiceWatcher::Begin(std::string_view service_name, ServiceWatcher::Callback callback,
                                  async_dispatcher_t* dispatcher) {
  std::string path = service_path_ + "/" + std::string(service_name);
  auto [client, server] = fidl::Endpoints<fuchsia_io::Directory>::Create();
  zx_status_t status = fdio_service_connect(path.c_str(), server.TakeChannel().release());
  if (status != ZX_OK) {
    return status;
  }
  return Begin(std::move(client), std::move(callback), dispatcher);
}

zx_status_t ServiceWatcher::Cancel() { return wait_.Cancel(); }

void ServiceWatcher::OnWatchedEvent(async_dispatcher_t* dispatcher, async::WaitBase* wait,
                                    zx_status_t status, const zx_packet_signal_t* signal) {
  if (status != ZX_OK || !(signal->observed & ZX_CHANNEL_READABLE)) {
    return;
  }

  uint32_t size;
  status = client_end_.read(0, buf_.get(), nullptr, fuchsia_io::wire::kMaxBuf, 0, &size, nullptr);
  if (status != ZX_OK) {
    return;
  }

  std::weak_ptr<uint8_t[fuchsia_io::wire::kMaxBuf]> weak_buf = buf_;
  uint8_t* msg = buf_.get();
  while (size >= 2) {
    // Process message structure, as described by fuchsia_io::wire::WatchedEvent.
    auto event = static_cast<fuchsia_io::wire::WatchEvent>(*msg++);
    uint64_t len = *msg++;
    // Restrict the length to the remaining size of the buffer.
    if (size < (len + 2u)) {
      break;
    }

    // If the entry is valid, invoke the callback.
    if (event == fuchsia_io::wire::WatchEvent::kIdle) {  // we don't need to look for a filename
      callback_(event, "");
    } else {
      std::string filename(reinterpret_cast<char*>(msg), len);
      // "." is not a device, so ignore it.
      if (filename != ".") {
        callback_(event, filename);
      }
    }
    // The callback may have destroyed the ServiceWatcher, so make sure we still exist.
    if (weak_buf.expired()) {
      return;
    }
    msg += len;
    size -= len + 2;
  }

  wait_.Begin(dispatcher);
}

}  // namespace component
