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

  buf_.resize(fuchsia_io::wire::kMaxBuf);
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
  assert(buf_.size() < std::numeric_limits<uint32_t>::max());
  uint32_t size = static_cast<uint32_t>(buf_.size());
  status = client_end_.read(0, buf_.data(), nullptr, size, 0, &size, nullptr);
  if (status != ZX_OK) {
    return;
  }

  for (auto i = buf_.begin(), end = buf_.begin() + size; std::distance(i, end) > 2;) {
    // Process message structure, as described by fuchsia_io::wire::WatchedEvent.
    auto event = static_cast<fuchsia_io::wire::WatchEvent>(*i++);
    uint64_t len = *i++;
    // Restrict the length to the remaining size of the buffer.
    len = std::min<uint64_t>(len, std::max(0l, std::distance(i, end)));
    // If the entry is valid, invoke the callback.
    if (len != 1 || *i != '.') {
      std::string instance(reinterpret_cast<char*>(i.base()), len);
      callback_(event, std::move(instance));
    }
    i += len;
  }

  wait_.Begin(dispatcher);
}

}  // namespace component
