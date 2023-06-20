// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "nand-broker.h"

#include <fcntl.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/fdio/cpp/caller.h>
#include <lib/fdio/directory.h>
#include <lib/fdio/fd.h>
#include <lib/fdio/fdio.h>
#include <lib/fdio/watcher.h>
#include <lib/stdcompat/string_view.h>
#include <lib/zx/vmo.h>
#include <stdio.h>
#include <zircon/assert.h>
#include <zircon/status.h>
#include <zircon/syscalls.h>

#include <new>

#include <pretty/hexdump.h>

namespace {

const char* kBrokerFilename = "broker";

// Looks for a device named "broker" from the path provided. Fails if there is no
// device after 5 seconds.
zx_status_t FindBroker(const std::string& path) {
  auto callback = [](int dir_fd, int event, const char* filename, void* cookie) {
    if (event != WATCH_EVENT_ADD_FILE || strcmp(filename, kBrokerFilename) != 0) {
      return ZX_OK;
    }
    return ZX_ERR_STOP;
  };

  fbl::unique_fd dir(open(path.c_str(), O_DIRECTORY));
  if (!dir) {
    return ZX_ERR_NOT_FOUND;
  }
  zx_time_t deadline = zx_deadline_after(ZX_SEC(5));
  return fdio_watch_directory(dir.get(), callback, deadline, nullptr);
}

}  // namespace.

NandBroker::NandBroker(const char* path) : path_(path) {}

bool NandBroker::Initialize() {
  auto broker_channel = LoadBroker();
  if (broker_channel.is_error()) {
    printf("Failed to get device handle: %s\n", zx_status_get_string(broker_channel.error_value()));
    return false;
  }
  broker_client_.Bind(std::move(broker_channel.value()));

  if (!Query()) {
    printf("Failed to open or query the device\n");
    return false;
  }
  const uint32_t size = (info_.page_size() + info_.oob_size()) * info_.pages_per_block();
  if (mapping_.CreateAndMap(size, "nand-broker-vmo") != ZX_OK) {
    printf("Failed to allocate VMO\n");
    return false;
  }
  return true;
}

void NandBroker::SetFtl(std::unique_ptr<FtlInfo> ftl) { ftl_ = std::move(ftl); }

bool NandBroker::Query() {
  if (!broker_client_.is_valid()) {
    return false;
  }

  auto result = broker_client_->GetInfo();
  if (result.is_ok() && result->status() == ZX_OK) {
    info_ = std::move(result->info().value());
    return true;
  }
  return false;
}

void NandBroker::ShowInfo() const {
  printf(
      "Page size: %d\nPages per block: %d\nTotal Blocks: %d\nOOB size: %d\nECC bits: %d\n"
      "Nand class: %d\n",
      info_.page_size(), info_.pages_per_block(), info_.num_blocks(), info_.oob_size(),
      info_.ecc_bits(), static_cast<int>(info_.nand_class()));
}

bool NandBroker::ReadPages(uint32_t first_page, uint32_t count) const {
  ZX_DEBUG_ASSERT(count <= info_.pages_per_block());
  zx::vmo vmo;
  if (mapping_.vmo().duplicate(ZX_RIGHT_SAME_RIGHTS, &vmo) != ZX_OK) {
    printf("Failed to duplicate VMO\n");
    return false;
  }

  fuchsia_nand::BrokerRequestData request{{
      .vmo = std::move(vmo),
      .length = count,
      .offset_nand = first_page,
      .offset_oob_vmo = info_.pages_per_block(),  // OOB is at the end of the VMO.
      .data_vmo = true,
      .oob_vmo = true,
  }};

  auto result = broker_client_->Read(std::move(request));

  if (result.is_error()) {
    printf("Failed to issue command to driver: %s\n", result.error_value().status_string());
    return false;
  }

  if (result->status() != ZX_OK) {
    printf("Read to %d pages starting at %d failed with %s\n", count, first_page,
           zx_status_get_string(result->status()));
    return false;
  }

  if (result->corrected_bit_flips() > info_.ecc_bits()) {
    printf("Read to %d pages starting at %d unable to correct all bit flips\n", count, first_page);
  } else if (result->corrected_bit_flips()) {
    // If the nand protocol is modified to provide more info, we could display something
    // like average bit flips.
    printf("Read to %d pages starting at %d corrected %d errors\n", count, first_page,
           result->corrected_bit_flips());
  }

  return true;
}

bool NandBroker::DumpPage(uint32_t page) const {
  if (!ReadPages(page, 1)) {
    return false;
  }
  ZX_DEBUG_ASSERT(info_.page_size() % 16 == 0);

  uint32_t address = page * info_.page_size();
  hexdump8_ex(data(), 16, address);
  int skip = 0;

  for (uint32_t line = 16; line < info_.page_size(); line += 16) {
    if (memcmp(data() + line, data() + line - 16, 16) == 0) {
      skip++;
      if (skip < 50) {
        printf(".");
      }
      continue;
    }
    if (skip) {
      printf("\n");
      skip = 0;
    }
    hexdump8_ex(data() + line, 16, address + line);
  }

  if (skip) {
    printf("\n");
  }

  printf("OOB:\n");
  hexdump8_ex(oob(), info_.oob_size(), address + info_.page_size());
  return true;
}

bool NandBroker::EraseBlock(uint32_t block) const {
  auto result = broker_client_->Erase({{.request = {{.length = 1, .offset_nand = block}}}});

  if (result.is_error()) {
    printf("Failed to issue erase command for block %d: %s\n", block,
           result.error_value().lossy_description());
    return false;
  }

  if (result->status() != ZX_OK) {
    printf("Erase block %d failed with %s\n", block, zx_status_get_string(result->status()));
    return false;
  }

  return true;
}

zx::result<fidl::ClientEnd<fuchsia_nand::Broker>> NandBroker::LoadBroker() {
  std::string broker_path;
  if (!cpp20::ends_with(std::string_view{path_}, "/broker")) {
    auto controller_channel = component::Connect<fuchsia_device::Controller>(path_);

    // A broker driver may or may not be loaded. Try to load it and see if that
    // fails.
    if (controller_channel.is_error()) {
      printf("Could not connect to device controller at: %s\n", path_.c_str());
      return controller_channel.take_error();
    }

    const char kBroker[] = "/boot/meta/nand-broker.cm";
    auto resp = fidl::WireCall(controller_channel.value())->Bind(kBroker);
    zx_status_t call_status = ZX_OK;
    auto status = resp.status();
    if (resp->is_error()) {
      call_status = resp->error_value();
    }

    bool bind_failed = (status != ZX_OK || call_status != ZX_OK);

    status = FindBroker(path_);

    if (status != ZX_OK) {
      if (bind_failed) {
        printf("Failed to issue bind command for broker\n");
      } else {
        printf("Failed to bind broker\n");
      }
      return zx::error(status);
    }
    broker_path = path_ + '/' + kBrokerFilename;
  } else {
    // The passed-in device is already a broker.
    broker_path = path_;
  }
  return component::Connect<fuchsia_nand::Broker>(broker_path);
}
