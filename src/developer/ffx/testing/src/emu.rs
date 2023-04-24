// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::{
    ffi::OsString,
    io::ErrorKind,
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    process::{Child, Command},
};

use async_io::Async;
use once_cell::sync::Lazy;
use tempfile::TempDir;

pub struct Emu {
    dir: TempDir,
    child: Child,
}

impl Emu {
    pub fn product_bundle_dir() -> &'static Path {
        static PRODUCT_BUNDLE_DIR: Lazy<PathBuf> =
            Lazy::new(|| crate::ROOT_BUILD_DIR.join(env!("PRODUCT_BUNDLE")));
        &*PRODUCT_BUNDLE_DIR
    }

    // Partially inlined from `make-fuchsia-vol`.
    // Sets up a disk with the required partitions, and then images the EFI partition.
    fn make_empty_disk(disk_path: &Path, efi_data: &[u8]) -> anyhow::Result<()> {
        use anyhow::Context;

        use gpt::{
            partition_types::{OperatingSystem, Type as PartType},
            GptDisk,
        };

        use std::{fs::OpenOptions, ops::Range, os::unix::prelude::FileExt};

        const fn part_type(guid: &'static str) -> PartType {
            PartType { guid, os: OperatingSystem::None }
        }

        const ZIRCON_A_GUID: PartType = part_type("DE30CC86-1F4A-4A31-93C4-66F147D33E05");
        const ZIRCON_B_GUID: PartType = part_type("23CC04DF-C278-4CE7-8471-897D1A4BCDF7");
        const ZIRCON_R_GUID: PartType = part_type("A0E5CF57-2DEF-46BE-A80C-A2067C37CD49");
        const VBMETA_A_GUID: PartType = part_type("A13B4D9A-EC5F-11E8-97D8-6C3BE52705BF");
        const VBMETA_B_GUID: PartType = part_type("A288ABF2-EC5F-11E8-97D8-6C3BE52705BF");
        const VBMETA_R_GUID: PartType = part_type("6A2460C3-CD11-4E8B-80A8-12CCE268ED0A");
        const MISC_GUID: PartType = part_type("1D75395D-F2C6-476B-A8B7-45CC1C97B476");
        const FVM_GUID: PartType = part_type("41D0E340-57E3-954E-8C1E-17ECAC44CFF5");

        const EFI_SIZE: u64 = 63 * 1024 * 1024;
        const VBMETA_SIZE: u64 = 64 * 1024;
        const ABR_SIZE: u64 = 256 * 1024 * 1024;

        // For x64, the FVM size is hardcoded to 16GB.
        // If the partition size is less than 16GB, it fails to mount.
        const DISK_SIZE: u64 = 20 * 1024 * 1024 * 1024;

        // Returns the partition range given a partition ID.
        fn part_range(disk: &GptDisk<'_>, part_id: u32) -> Range<u64> {
            let lbs = u64::from(disk.logical_block_size().clone());
            let part = &disk.partitions()[&part_id];
            part.first_lba * lbs..(part.last_lba + 1) * lbs
        }

        // Adds a partition and returns the partition byte range.
        fn add_partition(
            disk: &mut GptDisk<'_>,
            name: &str,
            size: u64,
            part_type: PartType,
        ) -> anyhow::Result<Range<u64>> {
            let part_id = disk
                .add_partition(name, size, part_type, 0, None)
                .context(format!("Failed to add {} partition (size={})", name, size))?;
            Ok(part_range(disk, part_id))
        }

        let mut disk = OpenOptions::new()
            .read(true)
            .write(true)
            .truncate(true)
            .create(true)
            .open(disk_path)
            .context(format!("Failed to open output file {}", disk_path.display()))?;

        disk.set_len(DISK_SIZE)?;

        let config = gpt::GptConfig::new().writable(true).initialized(false);
        let mut gpt_disk = config.create_from_device(Box::new(&mut disk), None)?;
        gpt_disk.update_partitions(std::collections::BTreeMap::new())?;

        #[allow(dead_code)]
        struct Partitions {
            efi: Range<u64>,
            zircon_a: Range<u64>,
            vbmeta_a: Range<u64>,
            zircon_b: Range<u64>,
            vbmeta_b: Range<u64>,
            zircon_r: Range<u64>,
            vbmeta_r: Range<u64>,
            misc: Range<u64>,
        }

        let part = Partitions {
            efi: add_partition(&mut gpt_disk, "fuchsia-esp", EFI_SIZE, gpt::partition_types::EFI)?,
            zircon_a: add_partition(&mut gpt_disk, "zircon-a", ABR_SIZE, ZIRCON_A_GUID)?,
            vbmeta_a: add_partition(&mut gpt_disk, "vbmeta_a", VBMETA_SIZE, VBMETA_A_GUID)?,
            zircon_b: add_partition(&mut gpt_disk, "zircon-b", ABR_SIZE, ZIRCON_B_GUID)?,
            vbmeta_b: add_partition(&mut gpt_disk, "vbmeta_b", VBMETA_SIZE, VBMETA_B_GUID)?,
            zircon_r: add_partition(&mut gpt_disk, "zircon-r", ABR_SIZE, ZIRCON_R_GUID)?,
            vbmeta_r: add_partition(&mut gpt_disk, "vbmeta_r", VBMETA_SIZE, VBMETA_R_GUID)?,
            misc: add_partition(&mut gpt_disk, "misc", VBMETA_SIZE, MISC_GUID)?,
        };

        let block_size: u64 = gpt_disk.logical_block_size().clone().into();

        let fvm_size =
            gpt_disk.find_free_sectors().iter().map(|(_offset, length)| length).max().unwrap()
                * block_size;

        let _fvm_part = add_partition(&mut gpt_disk, "fvm", fvm_size, FVM_GUID)?;

        gpt_disk.write()?;

        // Create a protective MBR
        // The size here should be the number of logical-blocks on the disk less one for the MBR itself.
        let mbr = gpt::mbr::ProtectiveMBR::with_lb_size(
            u32::try_from((DISK_SIZE - 1) / block_size).unwrap_or(0xffffffff),
        );
        mbr.overwrite_lba0(&mut disk)?;

        disk.write_all_at(efi_data, part.efi.start)?;

        Ok(())
    }

    pub fn start(ctx: &crate::TestContext) -> Emu {
        let emu_dir = TempDir::new_in(&*crate::TEMP_DIR).expect("could not create emu temp dir");

        let esp_blk = std::fs::read(crate::ROOT_BUILD_DIR.join(env!("BOOTLOADER")))
            .expect("failed to read bootloader");
        let disk_path = emu_dir.path().join("disk.img");
        Emu::make_empty_disk(&disk_path, &esp_blk).expect("failed to make empty disk");

        static RUN_ZIRCON_PATH: Lazy<PathBuf> =
            Lazy::new(|| crate::ROOT_BUILD_DIR.join(env!("RUN_ZIRCON")));

        static QEMU_PATH: Lazy<PathBuf> =
            Lazy::new(|| crate::ROOT_BUILD_DIR.join(concat!(env!("QEMU_PATH"), "/bin")));

        let emu_serial = emu_dir.path().join("serial");
        let emu_serial_log = ctx.isolate().log_dir().join("emulator.serial.log");

        // run-zircon -a x64 -N --uefi --disktype=nvme -D <disk> -S <serial> -q <QEMU path>

        let mut command = Command::new(&*RUN_ZIRCON_PATH);

        command
            .args(["-a", "x64", "-N", "--uefi", "--disktype=nvme", "-M", "null"])
            .arg("-D")
            .arg(&disk_path)
            .arg("-S")
            .arg({
                let mut arg = OsString::from("unix:");
                arg.push(&emu_serial);
                arg.push(",server,nowait,logfile=");
                arg.push(&emu_serial_log);
                arg
            })
            .arg("-q")
            .arg(&*QEMU_PATH);

        Emu { dir: emu_dir, child: command.spawn().unwrap() }
    }

    pub fn nodename(&self) -> &str {
        "fuchsia-5254-0063-5e7a"
    }

    pub async fn serial(&self) -> Async<UnixStream> {
        let mut tries = 1000 / 50;

        loop {
            let res = Async::<UnixStream>::connect(self.dir.path().join("serial")).await;

            return match res {
                Err(e) if e.kind() == ErrorKind::NotFound && tries > 0 => {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                    tries -= 1;
                    continue;
                }
                res @ _ => res.expect("failed to connect to emulator socket"),
            };
        }
    }
}

impl Drop for Emu {
    fn drop(&mut self) {
        while let None = self.child.try_wait().unwrap() {
            match self.child.kill() {
                Ok(()) => continue,
                Err(err) if err.kind() == ErrorKind::InvalidInput => continue,
                res @ _ => res.expect("could not kill qemu"),
            };
        }
        // TempDir's Drop impl deletes the directory, so it needs to live as long as Emu.
        // This silences unused variable warnings.
        let _ = self.dir;
    }
}
