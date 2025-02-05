// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "ftl_test_observer.h"

#include <fidl/fuchsia.hardware.nand/cpp/wire.h>
#include <lib/fdio/directory.h>
#include <lib/fdio/fdio.h>
#include <lib/fdio/namespace.h>

#include <fbl/unique_fd.h>
#include <zxtest/zxtest.h>

FtlTestObserver::FtlTestObserver() = default;

void FtlTestObserver::OnProgramStart() {
  CreateDevice();
  if (zx_status_t status = WaitForBlockDevice(); status != ZX_OK) {
    printf("Unable to wait for block device. Error: %s\n", zx_status_get_string(status));
    return;
  }
  ok_ = true;
}

void FtlTestObserver::CreateDevice() {
  driver_integration_test::IsolatedDevmgr::Args args;
  if (zx_status_t status = driver_integration_test::IsolatedDevmgr::Create(&args, &devmgr_);
      status != ZX_OK) {
    printf("Unable to create devmgr: %s\n", zx_status_get_string(status));
    return;
  }
  std::unique_ptr<ramdevice_client_test::RamNandCtl> ctl;
  zx_status_t status =
      ramdevice_client_test::RamNandCtl::Create(devmgr_.devfs_root().duplicate(), &ctl);
  if (status != ZX_OK) {
    printf("Unable to create ram-nand-ctl\n");
    return;
  }
  ram_nand_ctl_ = std::move(ctl);

  if (zx_status_t status = ram_nand_ctl_->CreateRamNand(
          {
              .nand_info =
                  {

                      .page_size = 4096,
                      .pages_per_block = 64,
                      .num_blocks = 96,
                      .ecc_bits = 8,
                      .oob_size = 8,
                      .nand_class = fuchsia_hardware_nand::wire::Class::kFtl,
                  },
          },
          &ram_nand_);
      status != ZX_OK) {
    printf("Unable to create ram-nand: %s\n", zx_status_get_string(status));
  }
}

zx_status_t FtlTestObserver::WaitForBlockDevice() {
  if (!ram_nand_) {
    return ZX_ERR_BAD_STATE;
  }

  zx_status_t status =
      device_watcher::RecursiveWaitForFile(devfs_root().get(),
                                           "sys/platform/00:00:2e/nand-ctl/ram-nand-0/ftl/block")
          .status_value();
  if (status != ZX_OK) {
    printf("Unable to open device, %d\n", status);
    return status;
  }

  fdio_ns_t* name_space;
  status = fdio_ns_get_installed(&name_space);
  if (status != ZX_OK) {
    printf("Unable to get name_space, %d\n", status);
    return status;
  }

  status = fdio_ns_bind_fd(name_space, "/fake/dev", devfs_root().get());
  if (status != ZX_OK) {
    printf("Bind failed, %d\n", status);
    return status;
  }

  int fd;
  status = fdio_fd_create(devmgr_.RealmExposedDir().channel().release(), &fd);
  if (status != ZX_OK) {
    printf("fd create failed, %d\n", status);
    return status;
  }

  status = fdio_ns_bind_fd(name_space, "/driver_exposed", fd);
  if (status != ZX_OK) {
    printf("Bind failed, %d\n", status);
    return status;
  }

  return status;
}
