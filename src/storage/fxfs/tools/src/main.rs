// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::Error,
    argh::FromArgs,
    fxfs::{
        filesystem::{mkfs_with_default, FxFilesystem, FxFilesystemBuilder},
        fsck,
    },
    fxfs_crypto::Crypt,
    fxfs_insecure_crypto::InsecureCrypt,
    std::{io::Read, ops::Deref, path::Path, sync::Arc},
    storage_device::{file_backed_device::FileBackedDevice, DeviceHolder},
    tools::ops,
};

#[cfg(target_os = "linux")]
use {
    fuse3::{raw::prelude::Session, MountOptions},
    libc,
    tokio::runtime::Runtime,
    tools::fuse_fs::FuseFs,
};

#[derive(FromArgs, PartialEq, Debug)]
/// fxfs
struct TopLevel {
    /// whether to run the tool verbosely
    #[argh(switch, short = 'v')]
    verbose: bool,
    #[argh(subcommand)]
    subcommand: SubCommand,
}

#[derive(FromArgs, PartialEq, Debug)]
#[argh(subcommand)]
enum SubCommand {
    ImageEdit(ImageEditCommand),
    CreateGolden(CreateGoldenSubCommand),
    CheckGolden(CheckGoldenSubCommand),
    #[cfg(target_os = "linux")]
    RunInMemoryFuse(InMemoryFuseSubCommand),
    #[cfg(target_os = "linux")]
    CreateFileFuse(CreateFileFuseSubCommand),
    #[cfg(target_os = "linux")]
    OpenFileFuse(OpenFileFuseSubCommand),
}

#[derive(FromArgs, PartialEq, Debug)]
#[argh(subcommand, name = "image", description = "disk image manipulation commands")]
struct ImageEditCommand {
    /// path to the image file to read or write
    #[argh(option, short = 'f')]
    file: String,
    #[argh(subcommand)]
    subcommand: ImageSubCommand,
}

#[derive(FromArgs, PartialEq, Debug)]
#[argh(subcommand)]
enum ImageSubCommand {
    Format(FormatSubCommand),
    Fsck(FsckSubCommand),
    Get(GetSubCommand),
    Ls(LsSubCommand),
    Mkdir(MkdirSubCommand),
    Put(PutSubCommand),
    Rm(RmSubCommand),
    Rmdir(RmdirSubCommand),
}

#[derive(FromArgs, PartialEq, Debug)]
/// copies files from the image to the host filesystem, overwriting existing files.
#[argh(subcommand, name = "get")]
struct GetSubCommand {
    /// source file in image.
    #[argh(positional)]
    src: String,
    /// destination filename on host filesystem.
    #[argh(positional)]
    dst: String,
}
#[derive(FromArgs, PartialEq, Debug)]
/// copies files from the host filesystem to the image, overwriting existing files.
#[argh(subcommand, name = "put")]
struct PutSubCommand {
    /// source file on host filesystem.
    #[argh(positional)]
    src: String,
    /// destination filename in image.
    #[argh(positional)]
    dst: String,
}

#[derive(FromArgs, PartialEq, Debug)]
/// copies files from the host filesystem to the image, overwriting existing files.
#[argh(subcommand, name = "rm")]
struct RmSubCommand {
    /// path to remove from image.
    #[argh(positional)]
    path: String,
}

#[derive(FromArgs, PartialEq, Debug)]
/// format the file or block device as an empty Fxfs filesystem
// TODO(fxbug.dev/97324): Mkfs should be able to create instances with one or more volumes, with a
// set of encryption engines.
#[argh(subcommand, name = "mkfs")]
struct FormatSubCommand {}

#[derive(FromArgs, PartialEq, Debug)]
/// verify the integrity of the filesystem image
#[argh(subcommand, name = "fsck")]
struct FsckSubCommand {}

#[derive(FromArgs, PartialEq, Debug)]
/// List all files
#[argh(subcommand, name = "ls")]
struct LsSubCommand {
    #[argh(positional)]
    /// path to list.
    path: String,
}

#[derive(FromArgs, PartialEq, Debug)]
/// Create a new directory
#[argh(subcommand, name = "mkdir")]
struct MkdirSubCommand {
    #[argh(positional)]
    /// path to create.
    path: String,
}

#[derive(FromArgs, PartialEq, Debug)]
/// Create a new directory
#[argh(subcommand, name = "rmdir")]
struct RmdirSubCommand {
    #[argh(positional)]
    /// path to create.
    path: String,
}

#[derive(FromArgs, PartialEq, Debug)]
/// Create a golden image at current filesystem version.
#[argh(subcommand, name = "create_golden")]
struct CreateGoldenSubCommand {}

#[cfg(target_os = "linux")]
#[derive(FromArgs, PartialEq, Debug)]
/// Mount the filesystem on Linux using in-memory device.
#[argh(subcommand, name = "in_memory_fuse")]
struct InMemoryFuseSubCommand {
    #[argh(positional)]
    /// path to the mounted directory.
    path: String,
}

#[cfg(target_os = "linux")]
#[derive(FromArgs, PartialEq, Debug)]
/// Mount the filesystem on Linux by creating a new file-backed device.
#[argh(subcommand, name = "create_file_fuse")]
struct CreateFileFuseSubCommand {
    #[argh(positional)]
    /// path to the mounted directory.
    mount_path: String,
    #[argh(positional)]
    /// path to the file-backed device.
    device_path: String,
}

#[cfg(target_os = "linux")]
#[derive(FromArgs, PartialEq, Debug)]
/// Mount the filesystem on Linux by opening an existing file-backed device.
#[argh(subcommand, name = "open_file_fuse")]
struct OpenFileFuseSubCommand {
    #[argh(positional)]
    /// path to the mounted directory.
    mount_path: String,
    #[argh(positional)]
    /// path to the file-backed device.
    device_path: String,
}

#[derive(FromArgs, PartialEq, Debug)]
/// Check all golden images at current filesystem version.
#[argh(subcommand, name = "check_golden")]
struct CheckGoldenSubCommand {
    #[argh(option)]
    /// path to golden images directory. derived from FUCHSIA_DIR if not set.
    images_dir: Option<String>,
}

#[fuchsia::main(threads = 2)]
async fn main() -> Result<(), Error> {
    tracing::debug!("fxfs {:?}", std::env::args());

    let args: TopLevel = argh::from_env();
    match args.subcommand {
        SubCommand::ImageEdit(cmd) => {
            // TODO(fxbug.dev/95403): Add support for side-loaded encryption keys.
            let crypt: Arc<dyn Crypt> = Arc::new(InsecureCrypt::new());
            match cmd.subcommand {
                ImageSubCommand::Rm(rmargs) => {
                    let device = DeviceHolder::new(FileBackedDevice::new(
                        std::fs::OpenOptions::new().read(true).write(true).open(cmd.file)?,
                        512,
                    ));
                    let fs = FxFilesystem::open(device).await?;
                    let vol = ops::open_volume(&fs, crypt.clone()).await?;
                    ops::unlink(&fs, &vol, &Path::new(&rmargs.path)).await?;
                    fs.close().await?;
                    ops::fsck(&fs, args.verbose).await
                }
                ImageSubCommand::Get(getargs) => {
                    let device = DeviceHolder::new(FileBackedDevice::new(
                        std::fs::OpenOptions::new().read(true).open(cmd.file)?,
                        512,
                    ));
                    let fs = FxFilesystemBuilder::new().read_only(true).open(device).await?;
                    let vol = ops::open_volume(&fs, crypt).await?;
                    let data = ops::get(&vol, &Path::new(&getargs.src)).await?;
                    let mut reader = std::io::Cursor::new(&data);
                    let mut writer = std::fs::File::create(getargs.dst)?;
                    std::io::copy(&mut reader, &mut writer)?;
                    Ok(())
                }
                ImageSubCommand::Put(putargs) => {
                    let device = DeviceHolder::new(FileBackedDevice::new(
                        std::fs::OpenOptions::new().read(true).write(true).open(cmd.file)?,
                        512,
                    ));
                    let fs = FxFilesystem::open(device).await?;
                    let vol = ops::open_volume(&fs, crypt.clone()).await?;
                    let mut data = Vec::new();
                    std::fs::File::open(&putargs.src)?.read_to_end(&mut data)?;
                    ops::put(&fs, &vol, &Path::new(&putargs.dst), data).await?;
                    fs.close().await?;
                    ops::fsck(&fs, args.verbose).await
                }
                ImageSubCommand::Format(_) => {
                    let device = DeviceHolder::new(FileBackedDevice::new(
                        std::fs::OpenOptions::new().read(true).write(true).open(cmd.file)?,
                        512,
                    ));
                    mkfs_with_default(device, Some(crypt)).await?;
                    Ok(())
                }
                ImageSubCommand::Fsck(_) => {
                    let device = DeviceHolder::new(FileBackedDevice::new(
                        std::fs::OpenOptions::new().read(true).open(cmd.file)?,
                        512,
                    ));
                    let fs = FxFilesystemBuilder::new().read_only(true).open(device).await?;
                    let options = fsck::FsckOptions {
                        on_error: Box::new(|err| eprintln!("{:?}", err.to_string())),
                        verbose: args.verbose,
                        ..Default::default()
                    };
                    fsck::fsck_with_options(fs.deref().clone(), &options).await
                }
                ImageSubCommand::Ls(lsargs) => {
                    let device = DeviceHolder::new(FileBackedDevice::new(
                        std::fs::OpenOptions::new().read(true).open(cmd.file)?,
                        512,
                    ));
                    let fs = FxFilesystemBuilder::new().read_only(true).open(device).await?;
                    let vol = ops::open_volume(&fs, crypt).await?;
                    let dir = ops::walk_dir(&vol, &Path::new(&lsargs.path)).await?;
                    ops::print_ls(&dir).await?;
                    Ok(())
                }
                ImageSubCommand::Mkdir(mkdirargs) => {
                    let device = DeviceHolder::new(FileBackedDevice::new(
                        std::fs::OpenOptions::new().read(true).write(true).open(cmd.file)?,
                        512,
                    ));
                    let fs = FxFilesystem::open(device).await?;
                    let vol = ops::open_volume(&fs, crypt.clone()).await?;
                    ops::mkdir(&fs, &vol, &Path::new(&mkdirargs.path)).await?;
                    fs.close().await?;
                    ops::fsck(&fs, args.verbose).await
                }
                ImageSubCommand::Rmdir(rmdirargs) => {
                    let device = DeviceHolder::new(FileBackedDevice::new(
                        std::fs::OpenOptions::new().read(true).write(true).open(cmd.file)?,
                        512,
                    ));
                    let fs = FxFilesystem::open(device).await?;
                    let vol = ops::open_volume(&fs, crypt.clone()).await?;
                    ops::unlink(&fs, &vol, &Path::new(&rmdirargs.path)).await?;
                    fs.close().await?;
                    ops::fsck(&fs, args.verbose).await
                }
            }
        }
        SubCommand::CreateGolden(_) => tools::golden::create_image().await,
        SubCommand::CheckGolden(args) => tools::golden::check_images(args.images_dir).await,
        #[cfg(target_os = "linux")]
        SubCommand::RunInMemoryFuse(args) => run_in_memory_fuse(args.path),
        #[cfg(target_os = "linux")]
        SubCommand::CreateFileFuse(args) => run_file_fuse_create(args.mount_path, args.device_path),
        #[cfg(target_os = "linux")]
        SubCommand::OpenFileFuse(args) => run_file_fuse_open(args.mount_path, args.device_path),
    }
}

/// Run FUSE-Fxfs with a fake in-memory device.
/// This is used for running unit tests for FUSE-Fxfs.
#[cfg(target_os = "linux")]
fn run_in_memory_fuse(path: String) -> Result<(), Error> {
    let rt = Runtime::new()?;

    rt.block_on(async {
        let uid = unsafe { libc::getuid() };
        let gid = unsafe { libc::getgid() };

        let mut mount_options = MountOptions::default();
        mount_options.fs_name("fxfs").nonempty(true).write_back(true).uid(uid).gid(gid);

        let fs = FuseFs::new_in_memory(path.clone()).await;

        Session::new(mount_options).mount_with_unprivileged(fs, path).await.unwrap().await.unwrap();
    });

    Ok(())
}

/// Run FUSE-Fxfs by creating a new file-backed device.
#[cfg(target_os = "linux")]
fn run_file_fuse_create(mount_path: String, device_path: String) -> Result<(), Error> {
    let rt = Runtime::new()?;

    rt.block_on(async {
        let uid = unsafe { libc::getuid() };
        let gid = unsafe { libc::getgid() };

        let mut mount_options = MountOptions::default();
        mount_options.fs_name("fxfs").nonempty(true).write_back(true).uid(uid).gid(gid);

        let fs = FuseFs::new_file_backed(device_path.as_str(), mount_path.clone()).await;
        let handle = fs.notify_destroy();
        handle.await;

        Session::new(mount_options)
            .mount_with_unprivileged(fs, mount_path)
            .await
            .unwrap()
            .await
            .unwrap();
    });

    Ok(())
}

/// Run FUSE-Fxfs by opening an existing file-backed device.
#[cfg(target_os = "linux")]
fn run_file_fuse_open(mount_path: String, device_path: String) -> Result<(), Error> {
    let rt = Runtime::new()?;

    rt.block_on(async {
        let uid = unsafe { libc::getuid() };
        let gid = unsafe { libc::getgid() };

        let mut mount_options = MountOptions::default();
        mount_options.fs_name("fxfs").nonempty(true).write_back(true).uid(uid).gid(gid);

        let fs = FuseFs::open_file_backed(device_path.as_str(), mount_path.clone()).await;
        let handle = fs.notify_destroy();
        handle.await;

        Session::new(mount_options)
            .mount_with_unprivileged(fs, mount_path)
            .await
            .unwrap()
            .await
            .unwrap();
    });

    Ok(())
}
