// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "fvm.h"

#include <dirent.h>
#include <fidl/fuchsia.device/cpp/fidl.h>
#include <fidl/fuchsia.hardware.block.partition/cpp/wire.h>
#include <fidl/fuchsia.hardware.block.volume/cpp/wire.h>
#include <fidl/fuchsia.hardware.block/cpp/wire.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/device-watcher/cpp/device-watcher.h>
#include <lib/fdio/cpp/caller.h>
#include <lib/fdio/directory.h>
#include <lib/fzl/vmo-mapper.h>
#include <lib/zx/channel.h>
#include <lib/zx/fifo.h>
#include <lib/zx/time.h>
#include <lib/zx/vmo.h>
#include <zircon/status.h>
#include <zircon/syscalls.h>

#include <algorithm>
#include <cstddef>
#include <memory>

#include <fbl/algorithm.h>
#include <fbl/array.h>
#include <fbl/string_buffer.h>
#include <fbl/unique_fd.h>
#include <ramdevice-client/ramdisk.h>
#include <safemath/safe_math.h>

#include "pave-logging.h"
#include "src/lib/storage/block_client/cpp/client.h"
#include "src/lib/storage/fs_management/cpp/fvm.h"
#include "src/lib/uuid/uuid.h"
#include "src/security/lib/zxcrypt/client.h"
#include "src/storage/fvm/format.h"
#include "src/storage/fvm/fvm_sparse.h"

namespace paver {
namespace {

namespace block = fuchsia_hardware_block;
namespace partition = fuchsia_hardware_block_partition;
namespace volume = fuchsia_hardware_block_volume;
namespace device = fuchsia_device;

using fuchsia_hardware_block_volume::wire::VolumeManagerInfo;

// The number of additional slices a partition will need to become
// zxcrypt'd.
//
// TODO(aarongreen): Replace this with a value supplied by ulib/zxcrypt.
constexpr size_t kZxcryptExtraSlices = 1;

zx::result<std::string> GetTopoPath(fidl::UnownedClientEnd<fuchsia_device::Controller> controller) {
  fidl::Result result = fidl::Call(controller)->GetTopologicalPath();
  if (result.is_error()) {
    if (result.error_value().is_domain_error()) {
      return zx::error(result.error_value().domain_error());
    }
    if (result.error_value().is_framework_error()) {
      return zx::error(result.error_value().framework_error().status());
    }
  }
  return zx::ok(std::move(result.value().path()));
}

// Confirm that the file descriptor to the underlying partition exists within an
// FVM, not, for example, a GPT or MBR.
//
// |out| is true if |fd| is a VPartition, else false.
zx::result<bool> FvmIsVirtualPartition(
    fidl::UnownedClientEnd<fuchsia_device::Controller> controller) {
  zx::result path = GetTopoPath(controller);
  if (path.is_error()) {
    return path.take_error();
  }
  return zx::ok(path.value().find("fvm") != std::string::npos);
}

// Describes the state of a partition actively being written
// out to disk.
struct PartitionInfo {
  fvm::PartitionDescriptor* pd = nullptr;
  fvm::PartitionDescriptor aligned_pd = {};
  fbl::unique_fd new_part;
  bool active = false;
};

ptrdiff_t GetExtentOffset(size_t extent) {
  return safemath::checked_cast<ptrdiff_t>(sizeof(fvm::PartitionDescriptor) +
                                           extent * sizeof(fvm::ExtentDescriptor));
}

fvm::ExtentDescriptor GetExtent(fvm::PartitionDescriptor* pd, size_t extent) {
  fvm::ExtentDescriptor descriptor = {};
  const auto* descriptor_ptr = reinterpret_cast<uint8_t*>(pd) + GetExtentOffset(extent);
  memcpy(&descriptor, descriptor_ptr, sizeof(fvm::ExtentDescriptor));
  return descriptor;
}

zx_status_t FlushClient(block_client::Client& client) {
  block_fifo_request_t request;
  request.group = 0;
  request.vmoid = block::wire::kVmoidInvalid;
  request.command = {.opcode = BLOCK_OPCODE_FLUSH, .flags = 0};
  request.length = 0;
  request.vmo_offset = 0;
  request.dev_offset = 0;

  return client.Transaction(&request, 1);
}

// Stream an FVM partition to disk.
zx_status_t StreamFvmPartition(fvm::SparseReader* reader, PartitionInfo* part,
                               const fzl::VmoMapper& mapper, block_client::Client& client,
                               size_t block_size, block_fifo_request_t* request) {
  size_t slice_size = reader->Image()->slice_size;
  const size_t vmo_cap = mapper.size();
  for (size_t e = 0; e < part->aligned_pd.extent_count; e++) {
    LOG("Writing extent %zu... \n", e);
    fvm::ExtentDescriptor ext = GetExtent(part->pd, e);
    size_t offset = ext.slice_start * slice_size;
    size_t bytes_left = ext.extent_length;

    // Write real data
    while (bytes_left > 0) {
      size_t actual;
      zx_status_t status = reader->ReadData(reinterpret_cast<uint8_t*>(mapper.start()),
                                            std::min(bytes_left, vmo_cap), &actual);
      if (status != ZX_OK) {
        ERROR("Error reading extent data with %zu bytes of %zu remaining: %s\n", bytes_left,
              ext.extent_length, zx_status_get_string(status));
        return status;
      }

      const size_t vmo_sz = actual;
      bytes_left -= actual;

      if (vmo_sz == 0) {
        ERROR("Read nothing from src_fd; %zu bytes left\n", bytes_left);
        return ZX_ERR_IO;
      }
      if (vmo_sz % block_size != 0) {
        ERROR("Cannot write non-block size multiple: %zu\n", vmo_sz);
        return ZX_ERR_IO;
      }

      uint64_t length = vmo_sz / block_size;
      if (length > UINT32_MAX) {
        ERROR("Error writing partition: Too large\n");
        return ZX_ERR_OUT_OF_RANGE;
      }
      request->length = static_cast<uint32_t>(length);
      request->vmo_offset = 0;
      request->dev_offset = offset / block_size;

      if (zx_status_t status = client.Transaction(request, 1); status != ZX_OK) {
        ERROR("Error writing partition data\n");
        return status;
      }

      offset += vmo_sz;
    }

    // Write trailing zeroes (which are implied, but were omitted from
    // transfer).
    bytes_left = (ext.slice_count * slice_size) - ext.extent_length;
    if (bytes_left > 0) {
      LOG("%zu bytes written, %zu zeroes left\n", ext.extent_length, bytes_left);
      memset(mapper.start(), 0, vmo_cap);
    }
    while (bytes_left > 0) {
      uint64_t length = std::min(bytes_left, vmo_cap) / block_size;
      if (length > UINT32_MAX) {
        ERROR("Error writing trailing zeroes: Too large(%lu)\n", length);
        return ZX_ERR_OUT_OF_RANGE;
      }
      request->length = static_cast<uint32_t>(length);
      request->vmo_offset = 0;
      request->dev_offset = offset / block_size;

      if (zx_status_t status = client.Transaction(request, 1); status != ZX_OK) {
        ERROR("Error writing trailing zeroes length:%u dev_offset:%lu vmo_offset:%lu\n",
              request->length, request->dev_offset, request->vmo_offset);
        return status;
      }

      offset += request->length * block_size;
      bytes_left -= request->length * block_size;
    }
  }
  return ZX_OK;
}

}  // namespace

fbl::unique_fd TryBindToFvmDriver(const fbl::unique_fd& devfs_root,
                                  fidl::UnownedClientEnd<fuchsia_device::Controller> partition,
                                  zx::duration timeout) {
  zx::result path = GetTopoPath(partition);
  if (path.is_error()) {
    ERROR("Failed to get topological path: %s\n", path.status_string());
    return {};
  }

  std::string fvm_path = path.value() + "/fvm";
  fvm_path.erase(0, 5);

  fbl::unique_fd fvm;
  if (zx_status_t status =
          fdio_open_fd_at(devfs_root.get(), fvm_path.c_str(), 0, fvm.reset_and_get_address());
      status != ZX_OK) {
    LOG("Failed to open fvm: %s, proceeding to rebind...\n", zx_status_get_string(status));
  } else {
    return fvm;
  }

  constexpr char kFvmDriverLib[] = "fvm.cm";
  fidl::WireResult result = fidl::WireCall(partition)->Rebind(fidl::StringView(kFvmDriverLib));
  if (!result.ok()) {
    ERROR("Could not call rebind driver: %s\n", zx_status_get_string(result.status()));
    return {};
  }
  if (result.value().is_error() && result.value().error_value() != ZX_ERR_ALREADY_BOUND) {
    ERROR("Could not rebind fvm driver: %s\n", zx_status_get_string(result.value().error_value()));
    return fbl::unique_fd();
  }

  zx::result channel =
      device_watcher::RecursiveWaitForFile(devfs_root.get(), fvm_path.c_str(), timeout);
  if (channel.is_error()) {
    ERROR("Error waiting for fvm driver to bind: %s\n", channel.status_string());
    return fbl::unique_fd();
  }
  fbl::unique_fd fd;
  if (zx_status_t status = fdio_fd_create(channel.value().release(), fd.reset_and_get_address());
      status != ZX_OK) {
    ERROR("Failed to create fvm device fd at \"%s\": %s\n", fvm_path.c_str(),
          zx_status_get_string(status));
    return fbl::unique_fd();
  }
  return fd;
}

fbl::unique_fd FvmPartitionFormat(
    const fbl::unique_fd& devfs_root,
    fidl::UnownedClientEnd<fuchsia_hardware_block::Block> partition,
    fidl::UnownedClientEnd<fuchsia_device::Controller> partition_controller,
    const fvm::SparseImage& header, BindOption option, FormatResult* format_result) {
  // Although the format (based on the magic in the FVM superblock)
  // indicates this is (or at least was) an FVM image, it may be invalid.
  //
  // Attempt to bind the FVM driver to this partition, but fall-back to
  // reinitializing the FVM image so the rest of the paving
  // process can continue successfully.
  fbl::unique_fd fvm_fd;
  if (format_result != nullptr) {
    *format_result = FormatResult::kUnknown;
  }
  if (option == BindOption::TryBind) {
    fs_management::DiskFormat df = fs_management::DetectDiskFormat(partition);
    if (df == fs_management::kDiskFormatFvm) {
      fvm_fd = TryBindToFvmDriver(devfs_root, partition_controller, zx::sec(3));
      if (fvm_fd) {
        LOG("Found already formatted FVM.\n");
        fdio_cpp::UnownedFdioCaller volume_manager(fvm_fd.get());
        auto result = fidl::WireCall(volume_manager.borrow_as<volume::VolumeManager>())->GetInfo();
        if (result.status() == ZX_OK) {
          auto get_maximum_slice_count = [](const fvm::SparseImage& header) {
            return fvm::Header::FromDiskSize(fvm::kMaxUsablePartitions, header.maximum_disk_size,
                                             header.slice_size)
                .GetAllocationTableAllocatedEntryCount();
          };
          if (result.value().info->slice_size != header.slice_size) {
            ERROR("Mismatched slice size. Reinitializing FVM.\n");
          } else if (header.maximum_disk_size > 0 &&
                     result.value().info->maximum_slice_count < get_maximum_slice_count(header)) {
            ERROR("Mismatched maximum slice count. Reinitializing FVM.\n");
          } else {
            if (format_result != nullptr) {
              *format_result = FormatResult::kPreserved;
            }
            return fvm_fd;
          }
        } else {
          ERROR("Could not query FVM for info. Reinitializing FVM.\n");
        }
      } else {
        ERROR(
            "Saw fs_management::kDiskFormatFvm, but could not bind driver. Reinitializing FVM.\n");
      }
    }
  }

  LOG("Initializing partition as FVM\n");
  {
    if (format_result != nullptr) {
      *format_result = FormatResult::kReformatted;
    }

    const fidl::WireResult result = fidl::WireCall(partition)->GetInfo();
    if (!result.ok()) {
      ERROR("Failed to query block info: %s\n", result.FormatDescription().c_str());
      return fbl::unique_fd();
    }
    const fit::result response = result.value();
    if (response.is_error()) {
      ERROR("Failed to query block info: %s\n", zx_status_get_string(response.error_value()));
      return fbl::unique_fd();
    }
    const fuchsia_hardware_block::wire::BlockInfo& info = response.value()->info;

    uint64_t initial_disk_size = info.block_count * info.block_size;
    uint64_t max_disk_size =
        (header.maximum_disk_size == 0) ? initial_disk_size : header.maximum_disk_size;

    zx_status_t status = fs_management::FvmInitPreallocated(partition, initial_disk_size,
                                                            max_disk_size, header.slice_size);
    if (status != ZX_OK) {
      ERROR("Failed to initialize fvm: %s\n", zx_status_get_string(status));
      return fbl::unique_fd();
    }
  }

  return TryBindToFvmDriver(devfs_root, partition_controller, zx::sec(3));
}

namespace {

// Formats a block device as a zxcrypt volume.
//
// On success, returns the VolumeManager for the zxcrypt instance, and writes the inner block
// device's into |part|.
zx::result<zxcrypt::VolumeManager> ZxcryptCreate(PartitionInfo* part) {
  // TODO(security): fxbug.dev/31073. We need to bind with channel in order to pass a key here.
  // TODO(security): fxbug.dev/31733. The created volume must marked as needing key rotation.

  fbl::unique_fd devfs_root;
  if (zx_status_t status = fdio_open_fd("/dev", 0, devfs_root.reset_and_get_address());
      status != ZX_OK) {
    return zx::error(status);
  }

  zxcrypt::VolumeManager zxcrypt_manager(std::move(part->new_part), std::move(devfs_root));
  zx::channel client_chan;
  if (zx_status_t status = zxcrypt_manager.OpenClient(zx::sec(3), client_chan); status != ZX_OK) {
    ERROR("Could not open zxcrypt volume manager\n");
    return zx::error(status);
  }
  zxcrypt::EncryptedVolumeClient zxcrypt_client(std::move(client_chan));
  uint8_t slot = 0;
  if (zx_status_t status = zxcrypt_client.FormatWithImplicitKey(slot); status != ZX_OK) {
    ERROR("Could not create zxcrypt volume\n");
    return zx::error(status);
  }

  if (zx_status_t status = zxcrypt_client.UnsealWithImplicitKey(slot); status != ZX_OK) {
    ERROR("Could not unseal zxcrypt volume\n");
    return zx::error(status);
  }

  if (zx_status_t status = zxcrypt_manager.OpenInnerBlockDevice(zx::sec(3), &part->new_part);
      status != ZX_OK) {
    ERROR("Could not open zxcrypt volume\n");
    return zx::error(status);
  }

  return zx::ok(std::move(zxcrypt_manager));
}

zx::result<bool> FvmPartitionIsChild(fidl::UnownedClientEnd<fuchsia_device::Controller> fvm,
                                     fidl::UnownedClientEnd<fuchsia_device::Controller> partition) {
  zx::result fvm_path = GetTopoPath(fvm);
  if (fvm_path.is_error()) {
    ERROR("Couldn't get topological path of FVM: %s\n", fvm_path.status_string());
    return fvm_path.take_error();
  }
  zx::result partition_path = GetTopoPath(partition);
  if (partition_path.is_error()) {
    ERROR("Couldn't get topological path of partition: %s\n", partition_path.status_string());
    return partition_path.take_error();
  }
  if (cpp20::starts_with(std::string_view(fvm_path.value()),
                         std::string_view(partition_path.value()))) {
    ERROR("Partition does not exist within FVM: partition='%s' fvm='%s'\n",
          partition_path.value().c_str(), fvm_path.value().c_str());
    return zx::ok(false);
  }
  return zx::ok(true);
}

void RecommendWipe(const char* problem) {
  Warn(problem, "Please run 'install-disk-image wipe' to wipe your partitions");
}

// Calculate the amount of space necessary for the incoming partitions,
// validating the header along the way. Additionally, deletes any old partitions
// which match the type GUID of the provided partition.
//
// Parses the information from the |reader| into |parts|.
zx_status_t PreProcessPartitions(const fbl::unique_fd& fvm_fd,
                                 const std::unique_ptr<fvm::SparseReader>& reader,
                                 const fbl::Array<PartitionInfo>& parts,
                                 size_t* out_requested_slices) {
  fvm::PartitionDescriptor* part = reader->Partitions();
  fvm::SparseImage* hdr = reader->Image();

  // Validate the header and determine the necessary slice requirements for
  // all partitions and all offsets.
  size_t requested_slices = 0;
  for (size_t p = 0; p < hdr->partition_count; p++) {
    parts[p].pd = part;
    memcpy(&parts[p].aligned_pd, part, sizeof(fvm::PartitionDescriptor));
    if (parts[p].pd->magic != fvm::kPartitionDescriptorMagic) {
      ERROR("Bad partition magic\n");
      return ZX_ERR_IO;
    }

    // TODO(http://fxbug.dev/112484): Remove this as it relies on multiplexing.
    fdio_cpp::UnownedFdioCaller fvm_caller(fvm_fd);

    zx_status_t status = WipeAllFvmPartitionsWithGuid(
        fvm_caller.borrow_as<fuchsia_device::Controller>(), parts[p].pd->type);
    if (status != ZX_OK) {
      ERROR("Failure wiping old partitions matching this GUID\n");
      return status;
    }

    fvm::ExtentDescriptor ext = GetExtent(parts[p].pd, 0);
    if (ext.magic != fvm::kExtentDescriptorMagic) {
      ERROR("Bad extent magic\n");
      return ZX_ERR_IO;
    }
    if (ext.slice_start != 0) {
      ERROR("First slice must start at zero\n");
      return ZX_ERR_IO;
    }
    if (ext.slice_count == 0) {
      ERROR("Extents must have > 0 slices\n");
      return ZX_ERR_IO;
    }
    if (ext.extent_length > ext.slice_count * hdr->slice_size) {
      ERROR("Extent length(%lu) must fit within allocated slice count(%lu * %lu)\n",
            ext.extent_length, ext.slice_count, hdr->slice_size);
      return ZX_ERR_IO;
    }

    // Filter drivers may require additional space.
    if ((parts[p].aligned_pd.flags & fvm::kSparseFlagZxcrypt) != 0) {
      requested_slices += kZxcryptExtraSlices;
    }

    for (size_t e = 1; e < parts[p].aligned_pd.extent_count; e++) {
      ext = GetExtent(parts[p].pd, e);
      if (ext.magic != fvm::kExtentDescriptorMagic) {
        ERROR("Bad extent magic\n");
        return ZX_ERR_IO;
      }
      if (ext.slice_count == 0) {
        ERROR("Extents must have > 0 slices\n");
        return ZX_ERR_IO;
      }
      if (ext.extent_length > ext.slice_count * hdr->slice_size) {
        char name[sizeof(parts[p].aligned_pd.name) + 1];
        name[sizeof(parts[p].aligned_pd.name)] = '\0';
        memcpy(name, parts[p].aligned_pd.name, sizeof(parts[p].aligned_pd.name));
        ERROR("Partition(%s) extent length(%lu) must fit within allocated slice count(%lu * %lu)\n",
              name, ext.extent_length, ext.slice_count, hdr->slice_size);
        return ZX_ERR_IO;
      }

      requested_slices += ext.slice_count;
    }
    part = reinterpret_cast<fvm::PartitionDescriptor*>(
        reinterpret_cast<uint8_t*>(parts[p].pd) + sizeof(fvm::PartitionDescriptor) +
        parts[p].aligned_pd.extent_count * sizeof(fvm::ExtentDescriptor));
  }

  *out_requested_slices = requested_slices;
  return ZX_OK;
}

struct BoundZxcryptDevice {
 public:
  BoundZxcryptDevice() = default;
  BoundZxcryptDevice(BoundZxcryptDevice&&) = default;
  BoundZxcryptDevice(const BoundZxcryptDevice&) = delete;
  explicit BoundZxcryptDevice(zxcrypt::VolumeManager&& manager) : manager(std::move(manager)) {}
  ~BoundZxcryptDevice() {
    if (manager) {
      if (zx_status_t status = manager->Unbind(); status != ZX_OK) {
        ERROR("Failed to unbind Zxcrypt: %s.  The driver may be unavailable.",
              zx_status_get_string(status));
      }
    }
  }
  std::optional<zxcrypt::VolumeManager> manager;
};

// Allocates the space requested by the partitions by creating new
// partitions and filling them with extents. This guarantees that
// streaming the data to the device will not run into "no space" issues
// later.
// Partitions which are zxcrypt-enabled will have their driver bound.  A set of all such drivers is
// returned so the caller can unbind them when finished writing into them.
zx::result<std::vector<BoundZxcryptDevice>> AllocatePartitions(
    const fbl::unique_fd& devfs_root,
    fidl::UnownedClientEnd<fuchsia_hardware_block_volume::VolumeManager> fvm_device,
    fbl::Array<PartitionInfo>* parts) {
  fdio_cpp::UnownedFdioCaller devfs_caller(devfs_root);
  fidl::UnownedClientEnd devfs_client_end = devfs_caller.directory();

  std::vector<BoundZxcryptDevice> bound_devices;
  for (PartitionInfo& part_info : *parts) {
    fvm::ExtentDescriptor ext = GetExtent(part_info.pd, 0);
    // Allocate this partition as inactive so it gets deleted on the next
    // reboot if this stream fails.
    uint32_t flags = part_info.active ? 0 : volume::wire::kAllocatePartitionFlagInactive;
    uint64_t slice_count = ext.slice_count;
    uuid::Uuid type_guid(part_info.pd->type);
    uuid::Uuid instance_guid = uuid::Uuid::Generate();
    const char* name = reinterpret_cast<const char*>(part_info.pd->name);
    std::string_view name_view(name, strnlen(name, sizeof(part_info.pd->name)));
    {
      char name[sizeof(part_info.pd->name) + 1];
      name[sizeof(part_info.pd->name)] = '\0';
      memcpy(name, part_info.pd->name, sizeof(part_info.pd->name));
      LOG("Allocating partition %s consisting of %zu slices\n", name, slice_count);
    }
    if (zx::result channel = fs_management::FvmAllocatePartitionWithDevfs(
            devfs_client_end, fvm_device, slice_count, type_guid, instance_guid, name_view, flags);
        channel.is_error()) {
      ERROR("Couldn't allocate partition\n");
      return zx::error(ZX_ERR_NO_SPACE);
    } else {
      fbl::unique_fd partition;
      if (zx_status_t status = fdio_fd_create(channel.value().TakeChannel().release(),
                                              partition.reset_and_get_address());
          status != ZX_OK) {
        return zx::error(status);
      }
      part_info.new_part = std::move(partition);
    }

    // Add filter drivers.
    if ((part_info.pd->flags & fvm::kSparseFlagZxcrypt) != 0) {
      LOG("Creating zxcrypt volume\n");
      auto volume_manager = ZxcryptCreate(&part_info);
      if (volume_manager.is_error()) {
        return volume_manager.take_error();
      }
      bound_devices.emplace_back(std::move(*volume_manager));
    }

    // The 0th index extent is allocated alongside the partition, so we
    // begin indexing from the 1st extent here.
    for (size_t e = 1; e < part_info.pd->extent_count; e++) {
      ext = GetExtent(part_info.pd, e);
      uint64_t offset = ext.slice_start;
      uint64_t length = ext.slice_count;

      fdio_cpp::UnownedFdioCaller partition_connection(part_info.new_part.get());
      auto result =
          fidl::WireCall(partition_connection.borrow_as<volume::Volume>())->Extend(offset, length);
      auto status = result.ok() ? result.value().status : result.status();
      if (status != ZX_OK) {
        ERROR("Failed to extend partition: %s\n", zx_status_get_string(status));
        return zx::error(status);
      }
    }
  }

  return zx::ok(std::move(bound_devices));
}

// Holds the description of a partition with a single extent. Note that even though some code asks
// for a PartitionDescriptor, in reality it treats that as a descriptor followed by a bunch of
// extents, so this copes with that de-facto pattern.
struct FvmPartition {
  // Returns an FVM partition with no real information about extents.  In order to
  // use the partitions, they should be formatted with the appropriate filesystem.
  static FvmPartition Make(const std::array<uint8_t, fvm::kGuidSize> partition_type,
                           std::string_view name) {
    FvmPartition partition{.extent = {
                               .slice_count = 1,
                           }};
    std::copy(std::begin(partition_type), std::end(partition_type),
              std::begin(partition.descriptor.type));
    std::copy(name.begin(), name.end(), std::begin(partition.descriptor.name));
    return partition;
  }

  fvm::PartitionDescriptor descriptor;
  fvm::ExtentDescriptor extent;
};

}  // namespace

// Deletes all partitions within the FVM with a type GUID matching |type_guid|
// until there are none left.
zx_status_t WipeAllFvmPartitionsWithGuid(fidl::UnownedClientEnd<fuchsia_device::Controller> fvm,
                                         const uint8_t type_guid[]) {
  zx::result fvm_topo_path = GetTopoPath(fvm);
  if (fvm_topo_path.is_error()) {
    ERROR("Couldn't get topological path of FVM! %s\n", fvm_topo_path.status_string());
    return fvm_topo_path.error_value();
  }

  fs_management::PartitionMatcher matcher{
      .type_guids = {uuid::Uuid(&type_guid[0])},
      .parent_device = fvm_topo_path->c_str(),
  };
  for (;;) {
    zx::result old_partition = fs_management::OpenPartition(matcher, /* wait=*/false);
    if (old_partition.is_error()) {
      if (old_partition.error_value() == ZX_ERR_NOT_FOUND) {
        return ZX_OK;
      }
      return old_partition.error_value();
    }
    zx::result is_vpartition = FvmIsVirtualPartition(*old_partition);
    if (is_vpartition.is_error()) {
      ERROR("Couldn't confirm old vpartition type: %s\n", is_vpartition.status_string());
      return ZX_ERR_IO;
    }

    if (zx::result result = FvmPartitionIsChild(fvm, *old_partition); result.is_ok()) {
      if (!result.value()) {
        RecommendWipe("Streaming a partition type which also exists outside the target FVM");
        return ZX_ERR_BAD_STATE;
      }
    } else {
      std::string error = std::string("Failed to check if partition type is a child of the FVM: ") +
                          result.status_string();
      RecommendWipe(error.c_str());
      return result.error_value();
    }
    if (!is_vpartition.value()) {
      RecommendWipe("Streaming a partition type which also exists in a GPT");
      return ZX_ERR_BAD_STATE;
    }

    // We're paving a partition that already exists within the FVM: let's
    // destroy it before we pave anew.

    zx::result volume_endpoints = fidl::CreateEndpoints<volume::Volume>();
    if (volume_endpoints.is_error()) {
      return volume_endpoints.error_value();
    }
    auto& [volume, volume_server] = volume_endpoints.value();
    if (fidl::OneWayError status =
            fidl::WireCall(*old_partition)->ConnectToDeviceFidl(volume_server.TakeChannel());
        !status.ok()) {
      return status.status();
    }
    fidl::WireResult result = fidl::WireCall(volume)->Destroy();
    zx_status_t status = result.ok() ? result.value().status : result.status();
    if (status != ZX_OK) {
      ERROR("Couldn't destroy partition: %s\n", zx_status_get_string(status));
      return status;
    }
  }
}

zx::result<> AllocateEmptyPartitions(
    const fbl::unique_fd& devfs_root,
    fidl::UnownedClientEnd<fuchsia_hardware_block_volume::VolumeManager> fvm_device) {
  FvmPartition fvm_partitions[] = {
      FvmPartition::Make(std::array<uint8_t, fvm::kGuidSize>(GUID_BLOB_VALUE),
                         paver::kBlobfsPartitionLabel),
      FvmPartition::Make(std::array<uint8_t, fvm::kGuidSize>(GUID_DATA_VALUE),
                         paver::kDataPartitionLabel)};
  fbl::Array<PartitionInfo> partitions(new PartitionInfo[2]{{
                                                                .pd = &fvm_partitions[0].descriptor,
                                                                .active = true,
                                                            },
                                                            {
                                                                .pd = &fvm_partitions[1].descriptor,
                                                                .active = true,
                                                            }},
                                       2);
  zx::result res = AllocatePartitions(devfs_root, fvm_device, &partitions);
  if (res.is_error()) {
    return res.take_error();
  }
  return zx::ok();
}

zx::result<> FvmStreamPartitions(const fbl::unique_fd& devfs_root,
                                 std::unique_ptr<PartitionClient> partition_client,
                                 std::unique_ptr<fvm::ReaderInterface> payload) {
  zx::result block_or = partition_client->GetBlockDevice();
  if (block_or.is_error()) {
    return block_or.take_error();
  }
  BlockDeviceClient& block = block_or.value().get();

  std::unique_ptr<fvm::SparseReader> reader;
  zx::result<> status = zx::ok();
  if (status = zx::make_result(fvm::SparseReader::Create(std::move(payload), &reader));
      status.is_error()) {
    return status.take_error();
  }

  LOG("Header Validated - OK\n");

  fvm::SparseImage* hdr = reader->Image();
  // Acquire an fd to the FVM, either by finding one that already
  // exists, or formatting a new one.
  fbl::unique_fd fvm_fd(FvmPartitionFormat(devfs_root, block.block_channel(),
                                           block.controller_channel(), *hdr, BindOption::TryBind));
  if (!fvm_fd) {
    ERROR("Couldn't find FVM partition\n");
    return zx::error(ZX_ERR_IO);
  }

  fbl::Array<PartitionInfo> parts(new PartitionInfo[hdr->partition_count], hdr->partition_count);

  // Parse the incoming image and calculate its size.
  //
  // Additionally, delete the old versions of any new partitions.
  size_t requested_slices = 0;
  if (status = zx::make_result(PreProcessPartitions(fvm_fd, reader, parts, &requested_slices));
      status.is_error()) {
    ERROR("Failed to validate partitions: %s\n", status.status_string());
    return status.take_error();
  }

  // Contend with issues from an image that may be too large for this device.
  VolumeManagerInfo info;
  if (auto info_or = fs_management::FvmQuery(fvm_fd.get()); info_or.is_error()) {
    ERROR("Failed to acquire FVM info: %s\n", status.status_string());
    return info_or.take_error();
  } else {
    info = reinterpret_cast<VolumeManagerInfo&>(*info_or);
  }
  size_t free_slices = info.slice_count - info.assigned_slice_count;
  if (info.slice_count < requested_slices) {
    char buf[256];
    snprintf(buf, sizeof(buf), "Image size (%zu) > Storage size (%zu)",
             requested_slices * hdr->slice_size, info.slice_count * hdr->slice_size);
    Warn(buf, "Image is too large to be paved to device");
    return zx::error(ZX_ERR_NO_SPACE);
  }
  if (free_slices < requested_slices) {
    Warn("Not enough space to non-destructively pave",
         "Automatically reinitializing FVM; Expect data loss");
    fvm_fd = FvmPartitionFormat(devfs_root, block.block_channel(), block.controller_channel(), *hdr,
                                BindOption::Reformat);
    if (!fvm_fd) {
      ERROR("Couldn't reformat FVM partition.\n");
      return zx::error(ZX_ERR_IO);
    }
    LOG("FVM Reformatted successfully.\n");
  }

  LOG("Partitions pre-validated successfully: Enough space exists to pave.\n");

  // Actually allocate the storage for the incoming image.
  fdio_cpp::FdioCaller volume_manager(std::move(fvm_fd));
  zx::result devices =
      AllocatePartitions(devfs_root, volume_manager.borrow_as<volume::VolumeManager>(), &parts);
  if (devices.is_error()) {
    ERROR("Failed to allocate partitions: %s\n", devices.status_string());
    return devices.take_error();
  }

  LOG("Partition space pre-allocated successfully.\n");

  constexpr size_t vmo_size = 1 << 20;

  fzl::VmoMapper mapping;
  zx::vmo vmo;
  if (mapping.CreateAndMap(vmo_size, ZX_VM_PERM_READ | ZX_VM_PERM_WRITE, nullptr, &vmo) != ZX_OK) {
    ERROR("Failed to create stream VMO\n");
    return zx::error(ZX_ERR_NO_MEMORY);
  }

  // Now that all partitions are preallocated, begin streaming data to them.
  for (size_t p = 0; p < parts.size(); p++) {
    fdio_cpp::UnownedFdioCaller partition_connection(parts[p].new_part.get());
    fidl::UnownedClientEnd device = partition_connection.borrow_as<block::Block>();

    const fidl::WireResult result = fidl::WireCall(device)->GetInfo();
    if (!result.ok()) {
      ERROR("Couldn't get partition block info: %s\n", result.FormatDescription().c_str());
      return zx::error(result.status());
    }
    fit::result response = result.value();
    if (response.is_error()) {
      ERROR("Couldn't get partition block info: %s\n",
            zx_status_get_string(response.error_value()));
      return response.take_error();
    }
    const fuchsia_hardware_block::wire::BlockInfo& info = response.value()->info;

    zx::result endpoints = fidl::CreateEndpoints<block::Session>();
    if (endpoints.is_error()) {
      return endpoints.take_error();
    }
    auto& [session, server] = endpoints.value();
    if (fidl::Status result = fidl::WireCall(device)->OpenSession(std::move(server));
        !result.ok()) {
      return zx::error(result.status());
    }
    const fidl::WireResult fifo_result = fidl::WireCall(session)->GetFifo();
    if (!fifo_result.ok()) {
      return zx::error(fifo_result.status());
    }
    fit::result fifo_response = fifo_result.value();
    if (fifo_response.is_error()) {
      return fifo_response.take_error();
    }
    block_client::Client client(std::move(session), std::move(fifo_response.value()->fifo));
    zx::result vmoid = client.RegisterVmo(vmo);
    if (vmoid.is_error()) {
      return vmoid.take_error();
    }

    block_fifo_request_t request = {
        .command = {.opcode = BLOCK_OPCODE_WRITE, .flags = 0},
        .group = 0,
        .vmoid = vmoid->TakeId(),
    };

    LOG("Streaming partition %zu\n", p);
    status = zx::make_result(
        StreamFvmPartition(reader.get(), &parts[p], mapping, client, info.block_size, &request));
    LOG("Done streaming partition %zu\n", p);
    if (status.is_error()) {
      ERROR("Failed to stream partition status=%d\n", status.error_value());
      return status.take_error();
    }
    if (status = zx::make_result(FlushClient(client)); status.is_error()) {
      ERROR("Failed to flush client\n");
      return status.take_error();
    }
    LOG("Done flushing partition %zu\n", p);
  }

  for (const PartitionInfo& part_info : parts) {
    fdio_cpp::UnownedFdioCaller partition_connection(part_info.new_part.get());
    // Upgrade the old partition (currently active) to the new partition (currently
    // inactive) so the new partition persists.
    auto result =
        fidl::WireCall(partition_connection.borrow_as<partition::Partition>())->GetInstanceGuid();
    if (!result.ok() || result.value().status != ZX_OK) {
      ERROR("Failed to get unique GUID of new partition\n");
      return zx::error(ZX_ERR_BAD_STATE);
    }
    auto* guid = result.value().guid.get();

    auto result2 =
        fidl::WireCall(volume_manager.borrow_as<volume::VolumeManager>())->Activate(*guid, *guid);
    if (result2.status() != ZX_OK || result2.value().status != ZX_OK) {
      ERROR("Failed to upgrade partition\n");
      return zx::error(ZX_ERR_IO);
    }
  }

  return zx::ok();
}

// Unbinds the FVM driver from the given device. Assumes that the driver is either
// loaded or not (but not in the process of being loaded).
zx_status_t FvmUnbind(const fbl::unique_fd& devfs_root, const char* device) {
  size_t len = strnlen(device, PATH_MAX);
  constexpr const char* kDevPath = "/dev/";
  constexpr size_t kDevPathLen = std::char_traits<char>::length(kDevPath);

  if (len == PATH_MAX || len <= kDevPathLen) {
    ERROR("Invalid device name: %s\n", device);
    return ZX_ERR_INVALID_ARGS;
  }
  fbl::StringBuffer<PATH_MAX - 1> name_buffer;
  name_buffer.Append(device + kDevPathLen);
  name_buffer.Append("/fvm");

  fdio_cpp::UnownedFdioCaller caller(devfs_root.get());
  zx::result channel = component::ConnectAt<device::Controller>(caller.directory(), name_buffer);
  if (channel.is_error()) {
    ERROR("Unable to connect to FVM service: %s on device %s\n", channel.status_string(),
          name_buffer.c_str());
    return channel.status_value();
  }
  auto resp = fidl::WireCall(channel.value())->ScheduleUnbind();
  if (resp.status() != ZX_OK) {
    ERROR("Failed to schedule FVM unbind: %s on device %s\n", zx_status_get_string(resp.status()),
          name_buffer.data());
    return resp.status();
  }
  if (resp->is_error()) {
    ERROR("FVM unbind failed: %s on device %s\n", zx_status_get_string(resp->error_value()),
          name_buffer.data());
    return resp->error_value();
  }
  return ZX_OK;
}

}  // namespace paver
