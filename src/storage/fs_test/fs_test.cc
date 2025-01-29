// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/storage/fs_test/fs_test.h"

#include <dlfcn.h>
#include <fidl/fuchsia.device/cpp/wire.h>
#include <fidl/fuchsia.hardware.block.volume/cpp/wire.h>
#include <fidl/fuchsia.hardware.block/cpp/wire.h>
#include <fidl/fuchsia.hardware.nand/cpp/wire.h>
#include <fidl/fuchsia.hardware.ramdisk/cpp/wire.h>
#include <fidl/fuchsia.io/cpp/wire.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/device-watcher/cpp/device-watcher.h>
#include <lib/fdio/namespace.h>
#include <lib/fidl/cpp/wire/channel.h>
#include <lib/fzl/vmo-mapper.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/zx/result.h>
#include <lib/zx/time.h>
#include <lib/zx/vmo.h>
#include <string.h>
#include <zircon/errors.h>
#include <zircon/types.h>

#include <cctype>
#include <cstdint>
#include <functional>
#include <iostream>
#include <memory>
#include <optional>
#include <string>
#include <string_view>
#include <utility>
#include <variant>
#include <vector>

#include <ramdevice-client/ramdisk.h>
#include <ramdevice-client/ramnand.h>
#include <rapidjson/rapidjson.h>

#include "src/lib/json_parser/json_parser.h"
#include "src/lib/uuid/uuid.h"
#include "src/storage/blobfs/blob_layout.h"
#include "src/storage/fs_test/blobfs_test.h"
#include "src/storage/fs_test/json_filesystem.h"
#include "src/storage/lib/fs_management/cpp/admin.h"
#include "src/storage/lib/fs_management/cpp/component.h"
#include "src/storage/lib/fs_management/cpp/format.h"
#include "src/storage/lib/fs_management/cpp/fvm.h"
#include "src/storage/lib/fs_management/cpp/mkfs_with_default.h"
#include "src/storage/lib/fs_management/cpp/mount.h"
#include "src/storage/lib/fs_management/cpp/options.h"
#include "src/storage/testing/fvm.h"
#include "src/storage/testing/ram_disk.h"

namespace fs_test {
namespace {

/// Amount of time to wait for a given device to be available.
constexpr zx::duration kDeviceWaitTime = zx::sec(30);

// Creates a ram-disk with an optional FVM partition. Returns the ram-disk and the device path.
zx::result<std::pair<storage::RamDisk, std::string>> CreateRamDisk(
    const TestFilesystemOptions& options) {
  if (options.use_ram_nand) {
    return zx::error(ZX_ERR_NOT_SUPPORTED);
  }

  zx::vmo vmo;
  if (options.vmo->is_valid()) {
    uint64_t vmo_size;
    auto status = zx::make_result(options.vmo->get_size(&vmo_size));
    if (status.is_error()) {
      return status.take_error();
    }
    status = zx::make_result(options.vmo->create_child(ZX_VMO_CHILD_SLICE, 0, vmo_size, &vmo));
    if (status.is_error()) {
      return status.take_error();
    }
  } else {
    fzl::VmoMapper mapper;
    auto status =
        zx::make_result(mapper.CreateAndMap(options.device_block_size * options.device_block_count,
                                            ZX_VM_PERM_READ | ZX_VM_PERM_WRITE, nullptr, &vmo));
    if (status.is_error()) {
      std::cout << "Unable to create VMO for ramdisk: " << status.status_string() << std::endl;
      return status.take_error();
    }

    // Fill the ram-disk with a non-zero value so that we don't inadvertently depend on it being
    // zero filled.
    if (!options.zero_fill) {
      memset(mapper.start(), 0xaf, mapper.size());
    }
  }

  // Create a ram-disk.  The DFv2 driver doesn't support fail_after,
  // ram_disk_discard_random_after_last_flush or FVM.
  auto ram_disk_or = storage::RamDisk::CreateWithVmo(
      std::move(vmo), options.device_block_size,
      storage::RamDisk::Options{
          .use_v2 = !options.fail_after && !options.ram_disk_discard_random_after_last_flush &&
                    !options.use_fvm,
      });
  if (ram_disk_or.is_error()) {
    return ram_disk_or.take_error();
  }

  if (options.fail_after) {
    if (auto status = ram_disk_or->SleepAfter(options.fail_after); status.is_error()) {
      return status.take_error();
    }
  }

  if (options.ram_disk_discard_random_after_last_flush) {
    uint32_t flags = static_cast<uint32_t>(
        fuchsia_hardware_ramdisk::wire::RamdiskFlag::kDiscardRandom |
        fuchsia_hardware_ramdisk::wire::RamdiskFlag::kDiscardNotFlushedOnWake);
    ramdisk_set_flags(ram_disk_or->client(), flags);
  }

  std::string device_path = ram_disk_or.value().path();
  return zx::ok(std::make_pair(std::move(ram_disk_or).value(), std::move(device_path)));
}

// Creates a ram-nand device.  It does not create an FVM partition; that is left to the caller.
zx::result<std::pair<ramdevice_client::RamNand, std::string>> CreateRamNand(
    const TestFilesystemOptions& options) {
  constexpr int kPageSize = 4096;
  constexpr int kPagesPerBlock = 64;
  constexpr int kOobSize = 8;

  uint32_t block_count;
  zx::vmo vmo;
  if (options.vmo->is_valid()) {
    uint64_t vmo_size;
    auto status = zx::make_result(options.vmo->get_size(&vmo_size));
    if (status.is_error()) {
      return status.take_error();
    }
    block_count = static_cast<uint32_t>(vmo_size / (kPageSize + kOobSize) / kPagesPerBlock);
    // For now, when using a ram-nand device, the only supported device block size is 8 KiB, so
    // raise an error if the user tries to ask for something different.
    if ((options.device_block_size != 0 && options.device_block_size != 8192) ||
        (options.device_block_count != 0 &&
         options.device_block_size * options.device_block_count !=
             block_count * kPageSize * kPagesPerBlock)) {
      std::cout << "Bad device parameters" << std::endl;
      return zx::error(ZX_ERR_INVALID_ARGS);
    }
    status = zx::make_result(options.vmo->create_child(ZX_VMO_CHILD_SLICE, 0, vmo_size, &vmo));
    if (status.is_error()) {
      return status.take_error();
    }
  } else if (options.device_block_size != 8192) {  // FTL exports a device with 8 KiB blocks.
    return zx::error(ZX_ERR_INVALID_ARGS);
  } else {
    block_count = static_cast<uint32_t>(options.device_block_size * options.device_block_count /
                                        kPageSize / kPagesPerBlock);
  }

  if (zx::result channel = device_watcher::RecursiveWaitForFile(
          "/dev/sys/platform/00:00:2e/nand-ctl", kDeviceWaitTime);
      channel.is_error()) {
    std::cout << "Failed waiting for /dev/sys/platform/00:00:2e/nand-ctl to appear: "
              << channel.status_string() << std::endl;
    return channel.take_error();
  }

  std::optional<ramdevice_client::RamNand> ram_nand;
  fuchsia_hardware_nand::wire::RamNandInfo config = {
      .vmo = std::move(vmo),
      .nand_info =
          {
              .page_size = kPageSize,
              .pages_per_block = kPagesPerBlock,
              .num_blocks = block_count,
              .ecc_bits = 8,
              .oob_size = kOobSize,
              .nand_class = fuchsia_hardware_nand::wire::Class::kFtl,
          },
      .fail_after = options.fail_after,
  };
  if (zx::result status =
          zx::make_result(ramdevice_client::RamNand::Create(std::move(config), &ram_nand));
      status.is_error()) {
    std::cout << "RamNand::Create failed: " << status.status_string() << std::endl;
    return status.take_error();
  }

  std::string ftl_path = std::string(ram_nand->path()) + "/ftl/block";
  if (zx::result channel = device_watcher::RecursiveWaitForFile(ftl_path.c_str(), kDeviceWaitTime);
      channel.is_error()) {
    std::cout << "Failed waiting for " << ftl_path << " to appear: " << channel.status_string()
              << std::endl;
    return channel.take_error();
  }
  return zx::ok(std::make_pair(*std::move(ram_nand), std::move(ftl_path)));
}

}  // namespace

std::string StripTrailingSlash(const std::string& in) {
  if (!in.empty() && in.back() == '/') {
    return in.substr(0, in.length() - 1);
  }
  return in;
}

zx::result<> FsUnbind(const std::string& mount_path) {
  fdio_ns_t* ns;
  if (auto status = zx::make_result(fdio_ns_get_installed(&ns)); status.is_error()) {
    return status;
  }
  if (auto status = zx::make_result(fdio_ns_unbind(ns, StripTrailingSlash(mount_path).c_str()));
      status.is_error()) {
    std::cout << "Unable to unbind: " << status.status_string() << std::endl;
    return status;
  }
  return zx::ok();
}

// Returns device and associated drivers (e.g. FVM).
zx::result<RamDevice> CreateRamDevice(const TestFilesystemOptions& options) {
  std::optional<RamDevice> ram_device;

  if (options.use_ram_nand) {
    auto ram_nand_or = CreateRamNand(options);
    if (ram_nand_or.is_error()) {
      return ram_nand_or.take_error();
    }
    auto [ram_nand, nand_device_path] = std::move(ram_nand_or).value();
    ram_device.emplace(std::move(ram_nand), nand_device_path);
  } else {
    auto ram_disk_or = CreateRamDisk(options);
    if (ram_disk_or.is_error()) {
      return ram_disk_or.take_error();
    }
    auto [device, ram_disk_path] = std::move(ram_disk_or).value();
    ram_device.emplace(std::move(device), ram_disk_path);
  }

  // Create an FVM partition if requested.
  if (options.use_fvm) {
    storage::FvmOptions fvm_options = {.initial_fvm_slice_count = options.initial_fvm_slice_count};
    auto fvm_partition = storage::CreateFvmPartition(
        ram_device->path(), static_cast<int>(options.fvm_slice_size), fvm_options);
    if (fvm_partition.is_error()) {
      return fvm_partition.take_error();
    }

    if (options.dummy_fvm_partition_size > 0) {
      fidl::Arena arena;
      if (zx::result res = fvm_partition->fvm().fs().CreateVolume(
              "extra",
              fuchsia_fs_startup::wire::CreateOptions::Builder(arena)
                  .type_guid({0x01, 0x02, 0x03, 0x04, 0x01, 0x02, 0x03, 0x04, 0x01, 0x02, 0x03,
                              0x04, 0x01, 0x02, 0x03, 0x04})
                  .guid({0x01, 0x02, 0x03, 0x04, 0x01, 0x02, 0x03, 0x04, 0x01, 0x02, 0x03, 0x04,
                         0x01, 0x02, 0x03, 0x04})
                  .initial_size(options.dummy_fvm_partition_size)
                  .Build(),
              fuchsia_fs_startup::wire::MountOptions());
          res.is_error()) {
        std::cout << "Could not allocate extra FVM partition: " << res.status_string() << std::endl;
        return res.take_error();
      }
    }

    ram_device->set_fvm_partition(*std::move(fvm_partition));
  }
  return zx::ok(*std::move(ram_device));
}

zx::result<> FsFormat(const std::string& device_path, fs_management::FsComponent& component,
                      const fs_management::MkfsOptions& options, bool create_default_volume) {
  zx::result<> status;
  if (create_default_volume) {
    zx::result crypt_client = InitializeCryptService();
    if (crypt_client.is_error())
      return crypt_client.take_error();
    status = fs_management::MkfsWithDefault(device_path.c_str(), component, options,
                                            *std::move(crypt_client));
  } else {
    status = zx::make_result(fs_management::Mkfs(device_path.c_str(), component, options));
  }
  if (status.is_error()) {
    std::cout << "Could not format file system: " << status.status_string() << std::endl;
    return status;
  }
  return zx::ok();
}

zx::result<std::pair<std::unique_ptr<fs_management::SingleVolumeFilesystemInterface>,
                     fs_management::NamespaceBinding>>
FsMount(const std::string& device_path, const std::string& mount_path,
        fs_management::FsComponent& component, const fs_management::MountOptions& options) {
  zx::result device = component::Connect<fuchsia_hardware_block::Block>(device_path);
  if (device.is_error()) {
    std::cout << "Could not open device: " << device_path << ": " << device.status_string()
              << std::endl;
    return device.take_error();
  }

  // Uncomment the following line to force an fsck at the end of every transaction (where
  // supported).
  // options.fsck_after_every_transaction = true;

  auto LogMountError = [](const auto& error) {
    std::cout << "Could not mount file system: " << error.status_string() << std::endl;
  };

  std::unique_ptr<fs_management::SingleVolumeFilesystemInterface> fs;
  if (component.is_multi_volume()) {
    auto result = fs_management::MountMultiVolumeWithDefault(std::move(device.value()), component,
                                                             options, kDefaultVolumeName);
    if (result.is_error()) {
      LogMountError(result);
      return result.take_error();
    }
    fs = std::make_unique<fs_management::StartedSingleVolumeMultiVolumeFilesystem>(
        std::move(*result));
  } else {
    auto result = fs_management::Mount(std::move(device.value()), component, options);
    if (result.is_error()) {
      LogMountError(result);
      return result.take_error();
    }
    fs = std::make_unique<fs_management::StartedSingleVolumeFilesystem>(std::move(*result));
  }
  auto data = fs->DataRoot();
  if (data.is_error()) {
    LogMountError(data);
    return data.take_error();
  }
  auto binding = fs_management::NamespaceBinding::Create(mount_path.c_str(), std::move(*data));
  if (binding.is_error()) {
    LogMountError(binding);
    return binding.take_error();
  }
  return zx::ok(std::make_pair(std::move(fs), std::move(*binding)));
}

// Returns device and associated drivers (e.g. FVM).
zx::result<RamDevice> OpenRamDevice(const TestFilesystemOptions& options) {
  if (!options.vmo->is_valid()) {
    return zx::error(ZX_ERR_NOT_SUPPORTED);
  }

  std::optional<RamDevice> ram_device;

  if (options.use_ram_nand) {
    // First create the ram-nand device.
    auto ram_nand_or = CreateRamNand(options);
    if (ram_nand_or.is_error()) {
      return ram_nand_or.take_error();
    }
    auto [ram_nand, ftl_device_path] = std::move(ram_nand_or).value();
    ram_device.emplace(std::move(ram_nand), std::move(ftl_device_path));
  } else {
    auto ram_disk_or = CreateRamDisk(options);
    if (ram_disk_or.is_error()) {
      std::cout << "Unable to create ram-disk" << std::endl;
    }

    auto [device, ram_disk_path] = std::move(ram_disk_or).value();
    ram_device.emplace(std::move(device), std::move(ram_disk_path));
  }

  if (options.use_fvm) {
    // Now bind FVM to it.
    std::string controller_path = ram_device->path() + "/device_controller";
    zx::result controller = component::Connect<fuchsia_device::Controller>(controller_path);
    if (controller.is_error()) {
      return controller.take_error();
    }
    auto status = storage::BindFvm(controller.value());
    if (status.is_error()) {
      std::cout << "Unable to bind FVM: " << status.status_string() << std::endl;
      return status.take_error();
    }

    ram_device->set_path(ram_device->path() + "/fvm/fs-test-partition-p-1/block");
  }

  if (zx::result channel =
          device_watcher::RecursiveWaitForFile(ram_device->path().c_str(), kDeviceWaitTime);
      channel.is_error()) {
    std::cout << "Failed waiting for " << ram_device->path()
              << " to appear: " << channel.status_string() << std::endl;
    return channel.take_error();
  }

  return zx::ok(*std::move(ram_device));
}

TestFilesystemOptions TestFilesystemOptions::DefaultBlobfs() {
  return TestFilesystemOptions{.description = "Blobfs",
                               .use_fvm = true,
                               .device_block_size = 512,
                               .device_block_count = 196'608,
                               .fvm_slice_size = 32'768,
                               .num_inodes = 512,  // blobfs can grow as needed.
                               .filesystem = &BlobfsFilesystem::SharedInstance()};
}

TestFilesystemOptions TestFilesystemOptions::BlobfsWithoutFvm() {
  TestFilesystemOptions blobfs_with_no_fvm = TestFilesystemOptions::DefaultBlobfs();
  blobfs_with_no_fvm.description = "BlobfsWithoutFvm";
  blobfs_with_no_fvm.use_fvm = false;
  blobfs_with_no_fvm.num_inodes = 2048;
  return blobfs_with_no_fvm;
}

std::ostream& operator<<(std::ostream& out, const TestFilesystemOptions& options) {
  return out << options.description;
}

std::vector<TestFilesystemOptions> AllTestFilesystems() {
  static const std::vector<TestFilesystemOptions>* options = [] {
    const char kConfigFile[] = "/pkg/config/config.json";
    json_parser::JSONParser parser;
    auto config = parser.ParseFromFile(std::string(kConfigFile));
    auto iter = config.FindMember("library");
    std::unique_ptr<Filesystem> filesystem;
    if (iter != config.MemberEnd()) {
      void* handle = dlopen(iter->value.GetString(), RTLD_NOW);
      FX_CHECK(handle) << dlerror();
      auto get_filesystem =
          reinterpret_cast<std::unique_ptr<Filesystem> (*)()>(dlsym(handle, "_Z13GetFilesystemv"));
      FX_CHECK(get_filesystem) << dlerror();
      filesystem = get_filesystem();
    } else {
      filesystem = JsonFilesystem::NewFilesystem(config).value();
    }
    std::string name = config["name"].GetString();
    auto options = new std::vector<TestFilesystemOptions>;
    iter = config.FindMember("options");
    if (iter == config.MemberEnd()) {
      name[0] = static_cast<char>(toupper(name[0]));
      options->push_back(TestFilesystemOptions{.description = name,
                                               .use_fvm = false,
                                               .device_block_size = 512,
                                               .device_block_count = 196'608,
                                               .filesystem = filesystem.get()});
    } else {
      for (rapidjson::SizeType i = 0; i < iter->value.Size(); ++i) {
        const auto& opt = iter->value[i];
        options->push_back(TestFilesystemOptions{
            .description = opt["description"].GetString(),
            .use_fvm = opt["use_fvm"].GetBool(),
            .has_min_volume_size = ConfigGetOrDefault<bool>(opt, "has_min_volume_size", false),
            .device_block_size = ConfigGetOrDefault<uint64_t>(opt, "device_block_size", 512),
            .device_block_count = ConfigGetOrDefault<uint64_t>(opt, "device_block_count", 196'608),
            .fvm_slice_size = 32'768,
            .filesystem = filesystem.get(),
        });
      }
    }
    [[maybe_unused]] Filesystem* fs = filesystem.release();  // Deliberate leak
    return options;
  }();

  return *options;
}

TestFilesystemOptions OptionsWithDescription(std::string_view description) {
  for (const auto& options : AllTestFilesystems()) {
    if (options.description == description) {
      return options;
    }
  }
  FX_LOGS(FATAL) << "No test options with description: " << description;
  abort();
}

std::vector<TestFilesystemOptions> MapAndFilterAllTestFilesystems(
    const std::function<std::optional<TestFilesystemOptions>(const TestFilesystemOptions&)>&
        map_and_filter) {
  std::vector<TestFilesystemOptions> results;
  for (const TestFilesystemOptions& options : AllTestFilesystems()) {
    auto r = map_and_filter(options);
    if (r) {
      results.push_back(*std::move(r));
    }
  }
  return results;
}

// -- FilesystemInstance --

// Default implementation
zx::result<> FilesystemInstance::Unmount(const std::string& mount_path) {
  // Detach from the namespace.
  if (auto status = FsUnbind(mount_path); status.is_error()) {
    std::cerr << "FsUnbind failed: " << status.status_string() << std::endl;
    return status;
  }

  auto filesystem = fs();
  if (!filesystem) {
    return zx::error(ZX_ERR_NOT_SUPPORTED);
  }
  auto status = filesystem->Unmount();
  if (status.is_error()) {
    std::cerr << "Shut down failed: " << status.status_string() << std::endl;
    return status;
  }
  return zx::ok();
}

// -- Blobfs --

class BlobfsInstance : public FilesystemInstance {
 public:
  BlobfsInstance(RamDevice device)
      : device_(std::move(device)),
        component_(fs_management::FsComponent::FromDiskFormat(fs_management::kDiskFormatBlobfs)) {}

  zx::result<> Format(const TestFilesystemOptions& options) override {
    fs_management::MkfsOptions mkfs_options;
    mkfs_options.deprecated_padded_blobfs_format =
        options.blob_layout_format == blobfs::BlobLayoutFormat::kDeprecatedPaddedMerkleTreeAtStart;
    mkfs_options.num_inodes = options.num_inodes;
    return FsFormat(device_.path(), component_, mkfs_options,
                    /*create_default_volume=*/false);
  }

  zx::result<> Mount(const std::string& mount_path,
                     const fs_management::MountOptions& options) override {
    auto res = FsMount(device_.path(), mount_path, component_, options);
    if (res.is_error()) {
      // We can't reuse the component in the face of errors.
      component_ = fs_management::FsComponent::FromDiskFormat(fs_management::kDiskFormatBlobfs);
      return res.take_error();
    }
    fs_ = std::move(res->first);
    binding_ = std::move(res->second);
    return zx::ok();
  }

  zx::result<> Unmount(const std::string& mount_path) override {
    zx::result result = FilesystemInstance::Unmount(mount_path);
    // After unmounting, the component cannot be used again, so set up a new component.
    component_ = fs_management::FsComponent::FromDiskFormat(fs_management::kDiskFormatBlobfs);
    return result;
  }

  zx::result<> Fsck() override {
    fs_management::FsckOptions options{
        .verbose = false,
        .never_modify = true,
        .always_modify = false,
        .force = true,
    };
    return zx::make_result(fs_management::Fsck(device_.path(), component_, options));
  }

  RamDevice* GetRamDevice() override { return &device_; }
  fs_management::SingleVolumeFilesystemInterface* fs() override { return fs_.get(); }
  fidl::UnownedClientEnd<fuchsia_io::Directory> ServiceDirectory() const override {
    return fs_->ExportRoot();
  }
  void Reset() override {
    binding_.Reset();
    fs_.reset();
  }

  std::string GetMoniker() const override {
    return component_.collection_name().has_value()
               ? *component_.collection_name() + ":" + component_.child_name()
               : component_.child_name();
  }

 private:
  RamDevice device_;
  fs_management::FsComponent component_;
  std::unique_ptr<fs_management::SingleVolumeFilesystemInterface> fs_;
  fs_management::NamespaceBinding binding_;
};

std::unique_ptr<FilesystemInstance> BlobfsFilesystem::Create(RamDevice device) const {
  return std::make_unique<BlobfsInstance>(std::move(device));
}

zx::result<std::unique_ptr<FilesystemInstance>> BlobfsFilesystem::Open(
    const TestFilesystemOptions& options) const {
  auto device = OpenRamDevice(options);
  if (device.is_error()) {
    return device.take_error();
  }
  return zx::ok(std::make_unique<BlobfsInstance>(*std::move(device)));
}

}  // namespace fs_test
