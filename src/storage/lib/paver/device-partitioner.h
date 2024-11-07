// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_STORAGE_LIB_PAVER_DEVICE_PARTITIONER_H_
#define SRC_STORAGE_LIB_PAVER_DEVICE_PARTITIONER_H_

#include <fidl/fuchsia.io/cpp/markers.h>
#include <fidl/fuchsia.paver/cpp/wire.h>
#include <lib/zx/channel.h>
#include <lib/zx/result.h>
#include <stdbool.h>
#include <zircon/types.h>

#include <memory>
#include <optional>
#include <span>
#include <string_view>
#include <utility>
#include <vector>

#include <fbl/string.h>
#include <fbl/unique_fd.h>

#include "src/lib/uuid/uuid.h"
#include "src/storage/lib/paver/block-devices.h"
#include "src/storage/lib/paver/partition-client.h"
#include "src/storage/lib/paver/paver-context.h"

namespace paver {

// Whether the device uses the new or legacy partition scheme.
enum class PartitionScheme { kNew, kLegacy };

enum class Partition {
  kUnknown,
  kBootloaderA,
  kBootloaderB,
  kBootloaderR,
  kZirconA,
  kZirconB,
  kZirconR,
  kSysconfig,
  kVbMetaA,
  kVbMetaB,
  kVbMetaR,
  kAbrMeta,
  kFuchsiaVolumeManager,
};

const char* PartitionName(Partition partition, PartitionScheme scheme);
std::optional<uuid::Uuid> PartitionTypeGuid(Partition partition, PartitionScheme scheme);

enum class Arch {
  kX64,
  kArm64,
  kRiscv64,
};

constexpr char kOpaqueVolumeContentType[] = "opauqe_volume";

// Operations on a specific partition take two identifiers, a partition type
// and a content type.
//
// The first is the conceptual partition type. This may not necessarily map 1:1
// with on-disk partitions. For example, some devices have multiple bootloader
// stages which live in different partitions on-disk but which would both have
// type kBootloader.
//
// The second is a device-specific string identifying the contents the caller
// wants to read/write. The backend uses this to decide which on-disk partitions
// to use and where the content lives in them. The default content type is
// null or empty.
struct PartitionSpec {
 public:
  // Creates a spec with the given partition and default (null) content type.
  explicit constexpr PartitionSpec(Partition partition)
      : PartitionSpec(partition, std::string_view()) {}

  // Creates a spec with the given partition and content type.
  constexpr PartitionSpec(Partition partition, std::string_view content_type)
      : partition(partition), content_type(content_type) {}

  // Returns a descriptive string for logging.
  //
  // Does not necessary match the on-disk partition name, just meant to
  // indicate the conceptual partition type in a device-agnostic way.
  fbl::String ToString() const;

  Partition partition;
  std::string_view content_type;
};

inline bool SpecMatches(const PartitionSpec& a, const PartitionSpec& b) {
  return a.partition == b.partition && a.content_type == b.content_type;
}

// Abstract device partitioner definition.
// This class defines common APIs for interacting with a device partitioner.
class DevicePartitioner {
 public:
  virtual ~DevicePartitioner() = default;

  // Whether or not the Fuchsia Volume Manager exists within an FTL.
  virtual bool IsFvmWithinFtl() const = 0;

  // Checks if the device supports the given partition spec.
  //
  // This is the only function that will definitively say whether a spec is
  // supported or not. Other partition functions may return ZX_ERR_NOT_SUPPORTED
  // on unsupported spec, but they may also return it for other reasons such as
  // a lower-level driver error. They also may return other errors even if given
  // an unsupported spec if something else goes wong.
  virtual bool SupportsPartition(const PartitionSpec& spec) const = 0;

  // Returns a PartitionClient matching |spec| if one exists.
  virtual zx::result<std::unique_ptr<PartitionClient>> FindPartition(
      const PartitionSpec& spec) const = 0;

  // Finalizes the PartitionClient matching |spec| after it has been written.
  virtual zx::result<> FinalizePartition(const PartitionSpec& spec) const = 0;

  // Wipes Fuchsia Volume Manager partition.
  virtual zx::result<> WipeFvm() const = 0;

  // Reset partition tables to a board-specific initial state.
  // This is only supported on some boards; in general it is preferred to use fastboot to
  // reinitialize partition tables.
  virtual zx::result<> ResetPartitionTables() const = 0;

  // Determine if the given data file is a valid image for this device.
  //
  // This analysis is best-effort only, providing only basic safety checks.
  virtual zx::result<> ValidatePayload(const PartitionSpec& spec,
                                       std::span<const uint8_t> data) const = 0;

  // Flush all buffered write to persistant storage.
  virtual zx::result<> Flush() const = 0;

  // Called by paver when Lifetime::Stop event is received
  // Can be used to update ABR oneshot flags for reboot.
  virtual zx::result<> OnStop() const = 0;
};

struct BlockAndController {
  fidl::ClientEnd<fuchsia_hardware_block::Block> device;
  fidl::ClientEnd<fuchsia_device::Controller> controller;
};

class DevicePartitionerFactory {
 public:
  // Factory method which automatically returns the correct DevicePartitioner
  // implementation based factories registered with it.
  // |block_device| is root block device which contains the logical partitions we wish to operate
  // against. It's only meaningful for EFI and CROS devices which may have multiple storage devices.
  static zx::result<std::unique_ptr<DevicePartitioner>> Create(
      const BlockDevices& devices, fidl::UnownedClientEnd<fuchsia_io::Directory> svc_root,
      Arch arch, std::shared_ptr<Context> context, BlockAndController block_device = {});

  static void Register(std::unique_ptr<DevicePartitionerFactory> factory);

  virtual ~DevicePartitionerFactory() = default;

 private:
  // This method is overridden by derived classes that implement different kinds
  // of DevicePartitioners.
  virtual zx::result<std::unique_ptr<DevicePartitioner>> New(
      const BlockDevices& devices, fidl::UnownedClientEnd<fuchsia_io::Directory> svc_root,
      Arch arch, std::shared_ptr<Context> context,
      fidl::ClientEnd<fuchsia_device::Controller> block_device) = 0;

  static std::vector<std::unique_ptr<DevicePartitionerFactory>>* registered_factory_list();
};

// DevicePartitioner implementation for devices which have fixed partition maps (e.g. ARM
// devices). It will not attempt to write a partition map of any kind to the device.
// Assumes legacy partition layout structure (e.g. ZIRCON-A, ZIRCON-B,
// ZIRCON-R).
class FixedDevicePartitioner : public DevicePartitioner {
 public:
  static zx::result<std::unique_ptr<DevicePartitioner>> Initialize(const BlockDevices& devices);

  bool IsFvmWithinFtl() const override { return false; }

  bool SupportsPartition(const PartitionSpec& spec) const override;

  zx::result<std::unique_ptr<PartitionClient>> FindPartition(
      const PartitionSpec& spec) const override;

  zx::result<> FinalizePartition(const PartitionSpec& spec) const override { return zx::ok(); }

  zx::result<> WipeFvm() const override;

  zx::result<> ResetPartitionTables() const override;

  zx::result<> ValidatePayload(const PartitionSpec& spec,
                               std::span<const uint8_t> data) const override;

  zx::result<> Flush() const override { return zx::ok(); }
  zx::result<> OnStop() const override { return zx::ok(); }

 private:
  explicit FixedDevicePartitioner(BlockDevices devices) : devices_(std::move(devices)) {}

  BlockDevices devices_;
};

class DefaultPartitionerFactory : public DevicePartitionerFactory {
 public:
  zx::result<std::unique_ptr<DevicePartitioner>> New(
      const BlockDevices& devices, fidl::UnownedClientEnd<fuchsia_io::Directory> svc_root,
      Arch arch, std::shared_ptr<Context> context,
      fidl::ClientEnd<fuchsia_device::Controller> block_device) final;
};

// Get the architecture of the currently running platform.
inline constexpr Arch GetCurrentArch() {
#if defined(__x86_64__)
  return Arch::kX64;
#elif defined(__aarch64__)
  return Arch::kArm64;
#elif defined(__riscv)
  return Arch::kRiscv64;
#else
#error "Unknown arch"
#endif
}

}  // namespace paver

#endif  // SRC_STORAGE_LIB_PAVER_DEVICE_PARTITIONER_H_
