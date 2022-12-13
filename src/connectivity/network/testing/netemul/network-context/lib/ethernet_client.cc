// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "ethernet_client.h"

#include <fcntl.h>
#include <fuchsia/hardware/ethernet/cpp/fidl.h>
#include <fuchsia/io/cpp/fidl.h>
#include <inttypes.h>
#include <lib/async/cpp/wait.h>
#include <lib/async/default.h>
#include <lib/fdio/cpp/caller.h>
#include <lib/fdio/directory.h>
#include <lib/fdio/fd.h>
#include <lib/fdio/fdio.h>
#include <lib/fdio/watcher.h>
#include <lib/fzl/fifo.h>
#include <lib/syslog/cpp/macros.h>
#include <zircon/status.h>

#include <iostream>
#include <memory>
#include <sstream>

#include <fbl/intrusive_single_list.h>

namespace netemul {
using ZController = fuchsia::hardware::ethernet::Controller;
using ZDevice = fuchsia::hardware::ethernet::Device;
using ZFifos = fuchsia::hardware::ethernet::Fifos;
using ZFifoEntry = eth_fifo_entry_t;
constexpr uint32_t ETH_SIGNAL_STATUS = fuchsia::hardware::ethernet::SIGNAL_STATUS;

// WatchCbArgs is intended to be used *only* to capture context for fdio dir
// watching. Note that base_dir and search_mac are references, not copies. That
// is only allowed because fdio dir watch callback is called synchronously.
struct WatchCbArgs {
  std::string result;
  const std::string& base_dir;
  const EthernetClient::Mac& search_mac;
};

class FifoHolder {
 public:
  struct LLFifoEntry : public fbl::SinglyLinkedListable<std::unique_ptr<LLFifoEntry>> {
    ZFifoEntry e;
  };

  explicit FifoHolder(async_dispatcher_t* dispatcher, ZFifos fifos, const EthernetConfig& config)
      : dispatcher_(dispatcher), buf_config_(config) {
    tx_.reset(fifos.tx.release());
    rx_.reset(fifos.rx.release());
  }

  ~FifoHolder() {
    if (mapped_ > 0) {
      zx::vmar::root_self()->unmap(mapped_, vmo_size_);
    }
  }

  void Startup(fidl::InterfacePtr<ZDevice>& device, fit::function<void(zx_status_t)> callback) {
    vmo_size_ = 2u * buf_config_.nbufs * buf_config_.buff_size;

    zx_status_t status = zx::vmo::create(vmo_size_, 0, &buf_);
    if (status != ZX_OK) {
      FX_PLOGS(ERROR, status) << "could not create a vmo of size " << vmo_size_;
      callback(status);
      return;
    }

    status = zx::vmar::root_self()->map(ZX_VM_PERM_READ | ZX_VM_PERM_WRITE, 0, buf_, 0, vmo_size_,
                                        &mapped_);
    if (status != ZX_OK) {
      FX_PLOGS(ERROR, status) << "failed to map vmo";
      callback(status);
      return;
    }

    zx::vmo buf_copy;
    status = buf_.duplicate(ZX_RIGHT_SAME_RIGHTS, &buf_copy);
    if (status != ZX_OK) {
      FX_PLOGS(ERROR, status) << "failed to duplicate vmo";
      callback(status);
      return;
    }

    device->SetIoBuffer(
        std::move(buf_copy), [this, callback = std::move(callback)](zx_status_t status) {
          if (status != ZX_OK) {
            callback(status);
            return;
          }

          uint32_t idx = 0;
          for (; idx < buf_config_.nbufs; idx++) {
            ZFifoEntry entry = {
                .offset = idx * buf_config_.buff_size,
                .length = buf_config_.buff_size,
                .flags = 0,
                .cookie = 0,
            };
            status = rx_.write_one(entry);
            if (status != ZX_OK) {
              FX_PLOGS(WARNING, status) << "failed call to write()";
              callback(status);
              return;
            }
          }

          for (; idx < 2 * buf_config_.nbufs; idx++) {
            auto entry = std::unique_ptr<LLFifoEntry>(new LLFifoEntry);
            entry->e.offset = idx * buf_config_.buff_size;
            entry->e.length = buf_config_.buff_size;
            entry->e.flags = 0;
            entry->e.cookie = reinterpret_cast<uintptr_t>(mapped_) + entry->e.offset;
            tx_available_.push_front(std::move(entry));
          }

          // register waiter when rx fifo is hit
          fifo_data_wait_.set_object(rx_.get_handle());
          fifo_data_wait_.set_trigger(ZX_FIFO_READABLE | ZX_FIFO_PEER_CLOSED | ETH_SIGNAL_STATUS);
          fifo_signal_wait_.set_object(rx_.get_handle());
          fifo_signal_wait_.set_trigger(ETH_SIGNAL_STATUS | ZX_FIFO_PEER_CLOSED);
          WaitOnFifoData();
          WaitOnFifoSignal();
          callback(ZX_OK);
        });
  }

  fzl::fifo<ZFifoEntry>& tx_fifo() { return tx_; }

  fzl::fifo<ZFifoEntry>& rx_fifo() { return rx_; }

  ZFifoEntry* GetTxBuffer() {
    auto ptr = tx_available_.pop_front();
    ZFifoEntry* entry = nullptr;
    if (ptr != nullptr) {
      entry = &ptr->e;
      tx_pending_.push_front(std::move(ptr));
      entry->length = buf_config_.buff_size;
    }
    return entry;
  }

  uint8_t* GetRxBuffer(uint32_t offset) { return reinterpret_cast<uint8_t*>(mapped_) + offset; }

  void ReturnTxBuffer(ZFifoEntry* entry) {
    auto ptr = tx_pending_.erase_if(
        [entry](const LLFifoEntry& tx_entry) { return tx_entry.e.cookie == entry->cookie; });
    if (ptr != nullptr) {
      tx_available_.push_front(std::move(ptr));
    }
  }

  void ReturnSentTxBuffers() {
    zx_signals_t obs;
    while (tx_.wait_one(ZX_FIFO_READABLE, zx::time(), &obs) == ZX_OK) {
      if (!(obs & ZX_FIFO_READABLE)) {
        break;
      }
      ZFifoEntry return_entry;
      if (tx_.read_one(&return_entry) != ZX_OK) {
        break;
      }
      if (!(return_entry.flags & ETH_FIFO_TX_OK)) {
        break;
      }
      ReturnTxBuffer(&return_entry);
    }
  }

  void WaitOnFifoData() {
    zx_status_t status = fifo_data_wait_.Begin(dispatcher_);
    if (status != ZX_OK) {
      FX_PLOGS(WARNING, status) << "EthernetClient can't wait on fifo data";
    }
  }

  void WaitOnFifoSignal() {
    zx_status_t status = fifo_signal_wait_.Begin(dispatcher_);
    // ZX_ERR_ALREADY_EXISTS is returned if the waiter is already installed, which is an expected
    // error. Avoid unnecessary logging on it.
    if (status != ZX_OK && status != ZX_ERR_ALREADY_EXISTS) {
      FX_PLOGS(WARNING, status) << "EthernetClient can't wait on fifo signal";
    }
  }

  void OnFifoSignal(async_dispatcher_t* dispatcher, async::WaitBase* wait, zx_status_t status,
                    const zx_packet_signal_t* signal) {
    if (status != ZX_OK) {
      FX_PLOGS(WARNING, status) << "EthernetClient fifo signal watch failed";
      return;
    }
    if (link_signal_callback_ && (signal->observed & ETH_SIGNAL_STATUS)) {
      link_signal_callback_();
    }
  }

  void OnRxData(async_dispatcher_t* dispatcher, async::WaitBase* wait, zx_status_t status,
                const zx_packet_signal_t* signal) {
    switch (status) {
      case ZX_OK:
      case ZX_ERR_CANCELED:
        break;
      default:
        FX_PLOGS(ERROR, status) << "EthernetClient fifo rx failed";
        return;
    }

    if (signal->observed & ZX_FIFO_READABLE) {
      ZFifoEntry entry;
      status = rx_.read_one(&entry);
      if (status != ZX_OK) {
        FX_PLOGS(WARNING, status) << "EthernetClient fifo rx read failed";
        return;
      }

      auto buf_data = GetRxBuffer(entry.offset);
      // call out to listener
      if (data_callback_) {
        data_callback_(buf_data, entry.length);
      }

      // return buffer to the driver
      entry.length = buf_config_.buff_size;
      status = rx_.write_one(entry);
      if (status != ZX_OK) {
        FX_PLOGS(WARNING, status) << "EthernetClient can't return rx buffer";
      }
    }

    if (signal->observed & ZX_FIFO_PEER_CLOSED) {
      if (peer_closed_callback_) {
        peer_closed_callback_();
      }
    } else {
      WaitOnFifoData();
    }
  }

  void SetDataCallback(EthernetClient::DataCallback cb) { data_callback_ = std::move(cb); }

  void SetPeerClosedCallback(EthernetClient::PeerClosedCallback cb) {
    peer_closed_callback_ = std::move(cb);
  }

  void WatchLinkSignal(fit::callback<void()> on_link_signal) {
    link_signal_callback_ = std::move(on_link_signal);
    WaitOnFifoSignal();
  }

 private:
  async_dispatcher_t* dispatcher_;
  uint64_t vmo_size_ = 0;
  zx::vmo buf_;
  uintptr_t mapped_ = 0;
  EthernetConfig buf_config_{};
  EthernetClient::DataCallback data_callback_;
  EthernetClient::PeerClosedCallback peer_closed_callback_;
  fit::callback<void()> link_signal_callback_;
  fzl::fifo<ZFifoEntry> tx_;
  fzl::fifo<ZFifoEntry> rx_;
  fbl::SinglyLinkedList<std::unique_ptr<LLFifoEntry>> tx_available_;
  fbl::SinglyLinkedList<std::unique_ptr<LLFifoEntry>> tx_pending_;
  async::WaitMethod<FifoHolder, &FifoHolder::OnRxData> fifo_data_wait_{this};
  async::WaitMethod<FifoHolder, &FifoHolder::OnFifoSignal> fifo_signal_wait_{this};
};

static zx_status_t WatchCb(int dirfd, int event, const char* fn, void* cookie) {
  if (event != WATCH_EVENT_ADD_FILE) {
    return ZX_OK;
  } else if (!strcmp(fn, ".") || !strcmp(fn, "..")) {
    return ZX_OK;
  }

  auto args = reinterpret_cast<WatchCbArgs*>(cookie);

  fidl::SynchronousInterfacePtr<ZDevice> device;
  {
    fdio_cpp::UnownedFdioCaller caller(dirfd);
    fidl::SynchronousInterfacePtr<ZController> controller;
    if (zx_status_t status =
            fdio_service_connect_at(caller.directory().channel()->get(), fn,
                                    controller.NewRequest().TakeChannel().release());
        status != ZX_OK) {
      return status;
    }
    if (zx_status_t status = controller->OpenSession(device.NewRequest()); status != ZX_OK) {
      FX_PLOGS(WARNING, status) << "failed to open session for " << args->base_dir << "/" << fn;
      return ZX_OK;
    }
  }

  // See if this device is our ethertap device

  fuchsia::hardware::ethernet::Info info;
  zx_status_t status = device->GetInfo(&info);
  if (status != ZX_OK) {
    FX_PLOGS(WARNING, status) << "could not get ethernet info for " << args->base_dir << "/" << fn;
    // Return ZX_OK to keep watching for devices.
    return ZX_OK;
  }
  if ((info.features & fuchsia::hardware::ethernet::Features::SYNTHETIC) !=
      fuchsia::hardware::ethernet::Features::SYNTHETIC) {
    // Not a match, keep looking.
    return ZX_OK;
  }
  if (memcmp(args->search_mac.octets.data(), info.mac.octets.data(),
             args->search_mac.octets.size()) != 0) {
    // not a match, keep looking
    return ZX_OK;
  }

  std::ostringstream ss;
  ss << args->base_dir << "/" << fn;
  args->result = ss.str();
  return ZX_ERR_STOP;
}

void EthernetClient::Setup(const EthernetConfig& config,
                           fit::function<void(zx_status_t)> callback) {
  device_->SetClientName("EthernetClient", [](zx_status_t stat) {});
  device_->GetFifos([this, callback = std::move(callback),
                     config](fuchsia::hardware::ethernet::Device_GetFifos_Result result) mutable {
    if (result.is_err()) {
      callback(result.err());
      return;
    }
    fifos_ = std::make_unique<FifoHolder>(dispatcher_, std::move(result.response().fifos), config);

    fifos_->SetPeerClosedCallback([this]() {
      if (peer_closed_callback_) {
        peer_closed_callback_();
      }
    });

    fifos_->Startup(this->device_,
                    [this, callback = std::move(callback)](zx_status_t status) mutable {
                      if (status != ZX_OK) {
                        callback(status);
                        return;
                      }

                      WatchLinkSignal();

                      this->device_->Start(std::move(callback));
                    });
  });
}

EthernetClient::EthernetClient(async_dispatcher_t* dispatcher,
                               fidl::InterfacePtr<fuchsia::hardware::ethernet::Device> ptr)
    : dispatcher_(dispatcher), online_(false), device_(std::move(ptr)) {
  device_.set_error_handler([this](zx_status_t status) {
    FX_PLOGS(WARNING, status) << "EthernetClient error";
    if (peer_closed_callback_) {
      peer_closed_callback_();
    }
  });
}

EthernetClient::~EthernetClient() {
  if (device_) {
    device_.Unbind().BindSync()->Stop();
  }
}

zx_status_t EthernetClient::Send(const void* data, uint16_t len) {
  return AcquireAndSend([data, len](void* dst, uint16_t* dlen) {
    if (len < *dlen) {
      *dlen = len;
    }
    memcpy(dst, data, *dlen);
  });
}

zx_status_t EthernetClient::AcquireAndSend(fit::function<void(void*, uint16_t*)> writer) {
  fifos_->ReturnSentTxBuffers();
  auto txBuffer = fifos_->GetTxBuffer();
  if (!txBuffer) {
    return ZX_ERR_NO_RESOURCES;
  }
  writer(reinterpret_cast<void*>(txBuffer->cookie), &txBuffer->length);
  auto status = fifos_->tx_fifo().write_one(*txBuffer);
  if (status != ZX_OK) {
    return status;
  }
  return status;
}

void EthernetClient::SetDataCallback(EthernetClient::DataCallback cb) {
  fifos_->SetDataCallback(std::move(cb));
}

void EthernetClient::SetPeerClosedCallback(PeerClosedCallback cb) {
  peer_closed_callback_ = std::move(cb);
}

void EthernetClient::SetLinkStatusChangedCallback(LinkStatusChangedCallback cb) {
  link_status_changed_callback_ = std::move(cb);
}

bool EthernetClient::online() const { return online_; }

void EthernetClient::set_online(bool online) {
  if (online != online_) {
    online_ = online;
    if (link_status_changed_callback_) {
      link_status_changed_callback_(online_);
    }
  }
}

zx::result<std::string> EthernetClientFactory::MountPointWithMAC(const EthernetClient::Mac& mac,
                                                                 zx::duration timeout) {
  WatchCbArgs args{.base_dir = base_dir_, .search_mac = mac};

  int ethdir = OpenDir();

  if (ethdir < 0) {
    FX_LOGS(ERROR) << "could not open " << base_dir_ << ": " << strerror(errno);
    return zx::error(ZX_ERR_INTERNAL);
  }

  zx_status_t status = fdio_watch_directory(ethdir, WatchCb, zx::deadline_after(timeout).get(),
                                            reinterpret_cast<void*>(&args));
  close(ethdir);
  if (status == ZX_ERR_STOP) {
    return zx::ok(std::move(args.result));
  } else {
    return zx::error(status);
  }
}

zx_status_t EthernetClientFactory::Connect(
    const std::string& path, fidl::InterfaceRequest<fuchsia::hardware::ethernet::Controller> req) {
  if (devfs_root_.is_valid()) {
    return fdio_service_connect_at(devfs_root_.get(), path.c_str(), req.TakeChannel().release());
  }
  return fdio_service_connect(path.c_str(), req.TakeChannel().release());
}

EthernetClient::Ptr EthernetClientFactory::Create(const std::string& path,
                                                  async_dispatcher_t* dispatcher) {
  if (dispatcher == nullptr) {
    dispatcher = async_get_default_dispatcher();
  }

  fidl::SynchronousInterfacePtr<ZController> controller;
  auto status = Connect(path, controller.NewRequest());
  if (status != ZX_OK) {
    FX_PLOGS(ERROR, status) << "could not open " << path;
    return nullptr;
  }

  fidl::InterfaceHandle<ZDevice> handle;
  if (zx_status_t status = controller->OpenSession(handle.NewRequest()); status != ZX_OK) {
    FX_PLOGS(WARNING, status) << "could not open session on " << path;
    return nullptr;
  }

  return std::make_unique<EthernetClient>(dispatcher, handle.Bind());
}

zx::result<EthernetClient::Ptr> EthernetClientFactory::RetrieveWithMAC(
    const EthernetClient::Mac& mac, zx::duration timeout, async_dispatcher_t* dispatcher) {
  zx::result mount = MountPointWithMAC(mac, timeout);
  if (!mount.is_ok()) {
    return zx::error(mount.status_value());
  }
  return zx::ok(Create(mount.value(), dispatcher));
}

int EthernetClientFactory::OpenDir() {
  if (devfs_root_.is_valid()) {
    zx::channel eth, srv;
    if (zx::channel::create(0, &eth, &srv) != ZX_OK) {
      return -1;
    }
    auto status = fdio_open_at(devfs_root_.get(), base_dir_.c_str(),
                               static_cast<uint32_t>(fuchsia::io::OpenFlags::DIRECTORY |
                                                     fuchsia::io::OpenFlags::RIGHT_READABLE),
                               srv.release());
    if (status != ZX_OK) {
      std::cerr << "Failed to open ethernet directory " << base_dir_ << ", "
                << static_cast<bool>(devfs_root_) << " - " << zx_status_get_string(status);
      return -1;
    }
    int ethdir;
    if (fdio_fd_create(eth.release(), &ethdir) != ZX_OK) {
      std::cerr << "Failed to create fdio fd directory " << base_dir_ << ", "
                << static_cast<bool>(devfs_root_) << " - " << zx_status_get_string(status);
      return -1;
    }

    return ethdir;
  } else {
    return open(base_dir_.c_str(), O_RDONLY);
  }
}

void EthernetClient::WatchLinkSignal() {
  fifos_->WatchLinkSignal([this]() {
    device_->GetStatus([this](fuchsia::hardware::ethernet::DeviceStatus status) mutable {
      set_online(static_cast<bool>(status & fuchsia::hardware::ethernet::DeviceStatus::ONLINE));
      WatchLinkSignal();
    });
  });
}

}  // namespace netemul
