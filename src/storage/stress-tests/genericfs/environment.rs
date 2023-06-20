// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        deletion_actor::DeletionActor, file_actor::FileActor, instance_actor::InstanceActor, Args,
    },
    anyhow::{anyhow, format_err},
    async_trait::async_trait,
    diagnostics_reader::{ArchiveReader, Inspect},
    either::Either,
    fidl::endpoints::Proxy as _,
    fidl_fuchsia_device::ControllerMarker,
    fidl_fuchsia_fxfs::{CryptManagementMarker, CryptMarker, KeyPurpose, MountOptions},
    fidl_fuchsia_io as fio, fidl_fuchsia_logger as flogger,
    fs_management::{filesystem::Filesystem, FSConfig},
    fuchsia_async as fasync,
    fuchsia_component::client::connect_to_protocol_at_path,
    fuchsia_component_test::{Capability, ChildOptions, RealmBuilder, RealmInstance, Ref, Route},
    fuchsia_inspect::hierarchy::DiagnosticsHierarchy,
    fuchsia_zircon::Vmo,
    futures::{lock::Mutex as FuturesMutex, StreamExt as _},
    key_bag::Aes256Key,
    rand::{rngs::SmallRng, Rng, SeedableRng},
    std::ops::Deref,
    std::path::PathBuf,
    std::sync::{Arc, Mutex},
    std::time::Duration,
    storage_stress_test_utils::{
        data::{Compressibility, FileFactory, UncompressedSize},
        fvm::{get_volume_path, FvmInstance, Guid},
        io::Directory,
    },
    stress_test::{actor::ActorRunner, environment::Environment, random_seed},
};

// All partitions in this test have their type set to this arbitrary GUID.
const TYPE_GUID: Guid =
    [0x0, 0x1, 0x2, 0x3, 0x4, 0x5, 0x6, 0x7, 0x8, 0x9, 0xa, 0xb, 0xc, 0xd, 0xe, 0xf];

const ONE_MIB: u64 = 1048576;
const FOUR_MIB: u64 = 4 * ONE_MIB;

const MOUNT_PATH: &str = "/fs";

const DATA_KEY: Aes256Key = Aes256Key::create([
    0xcf, 0x9e, 0x45, 0x2a, 0x22, 0xa5, 0x70, 0x31, 0x33, 0x3b, 0x4d, 0x6b, 0x6f, 0x78, 0x58, 0x29,
    0x04, 0x79, 0xc7, 0xd6, 0xa9, 0x4b, 0xce, 0x82, 0x04, 0x56, 0x5e, 0x82, 0xfc, 0xe7, 0x37, 0xa8,
]);

const METADATA_KEY: Aes256Key = Aes256Key::create([
    0x0f, 0x4d, 0xca, 0x6b, 0x35, 0x0e, 0x85, 0x6a, 0xb3, 0x8c, 0xdd, 0xe9, 0xda, 0x0e, 0xc8, 0x22,
    0x8e, 0xea, 0xd8, 0x05, 0xc4, 0xc9, 0x0b, 0xa8, 0xd8, 0x85, 0x87, 0x50, 0x75, 0x40, 0x1c, 0x4c,
]);

const INSPECT_POLL_INTERVAL: fuchsia_zircon::Duration = fuchsia_zircon::Duration::from_seconds(1);

fn print_inspect_data(data: &DiagnosticsHierarchy) {
    match serde_json::to_string_pretty(&data) {
        Ok(data) => {
            println!("=== START Inspect Data ===");
            println!("{}", data);
            println!("=== END Inspect Data ===");
        }
        Err(e) => {
            eprintln!("Failed to deserialize inspect data: {:?}", e);
        }
    }
}

pub async fn create_hermetic_crypt_service(
    data_key: Aes256Key,
    metadata_key: Aes256Key,
) -> RealmInstance {
    let builder = RealmBuilder::new().await.unwrap();
    let url = "#meta/fxfs-crypt.cm";
    let crypt = builder.add_child("fxfs-crypt", url, ChildOptions::new().eager()).await.unwrap();
    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol::<CryptMarker>())
                .capability(Capability::protocol::<CryptManagementMarker>())
                .from(&crypt)
                .to(Ref::parent()),
        )
        .await
        .unwrap();
    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol::<flogger::LogSinkMarker>())
                .from(Ref::parent())
                .to(&crypt),
        )
        .await
        .unwrap();
    let realm = builder.build().await.expect("realm build failed");
    let crypt_management =
        realm.root.connect_to_protocol_at_exposed_dir::<CryptManagementMarker>().unwrap();
    crypt_management
        .add_wrapping_key(0, data_key.deref())
        .await
        .unwrap()
        .expect("add_wrapping_key failed");
    crypt_management
        .add_wrapping_key(1, metadata_key.deref())
        .await
        .unwrap()
        .expect("add_wrapping_key failed");
    crypt_management
        .set_active_key(KeyPurpose::Data, 0)
        .await
        .unwrap()
        .expect("set_active_key failed");
    crypt_management
        .set_active_key(KeyPurpose::Metadata, 1)
        .await
        .unwrap()
        .expect("set_active_key failed");
    realm
}

pub fn open_dir_at_root(subdir: &str) -> Directory {
    let path = PathBuf::from(MOUNT_PATH).join(subdir);
    Directory::from_namespace(path, fio::OpenFlags::RIGHT_WRITABLE | fio::OpenFlags::RIGHT_READABLE)
        .unwrap()
}

/// Describes the environment that this stress test will run under.
pub struct FsEnvironment<FSC: FSConfig> {
    seed: u64,
    args: Args,
    vmo: Vmo,
    volume_guid: Guid,
    config: FSC,
    crypt_realm: Option<RealmInstance>,
    instance_actor: Arc<FuturesMutex<InstanceActor>>,
    file_actor: Arc<FuturesMutex<FileActor>>,
    deletion_actor: Arc<FuturesMutex<DeletionActor>>,
    _inspect_poll_task: fasync::Task<()>,
    inspect: Arc<Mutex<Option<DiagnosticsHierarchy>>>,
}

impl<FSC: Clone + FSConfig> FsEnvironment<FSC> {
    pub async fn new(config: FSC, args: Args) -> Self {
        let crypt_realm = if config.is_multi_volume() {
            Some(create_hermetic_crypt_service(DATA_KEY, METADATA_KEY).await)
        } else {
            None
        };
        // Create the VMO that the ramdisk is backed by
        let vmo_size = args.ramdisk_block_count * args.ramdisk_block_size;
        let vmo = Vmo::create(vmo_size).unwrap();

        // Initialize the VMO with FVM partition style and a single filesystem partition

        // Create a ramdisk and setup FVM.
        let mut fvm =
            FvmInstance::new(true, &vmo, args.fvm_slice_size, args.ramdisk_block_size).await;

        // Initialize the filesystem on a new volume
        let volume_guid = fvm.new_volume("default", &TYPE_GUID, Some(fvm.free_space().await)).await;
        let volume_path = get_volume_path(&volume_guid).await;
        let controller =
            connect_to_protocol_at_path::<ControllerMarker>(volume_path.to_str().unwrap()).unwrap();
        let mut fs = Filesystem::new(controller, config.clone());
        fs.format().await.unwrap();
        let moniker = fs.get_component_moniker();

        let instance = if fs.config().is_multi_volume() {
            let crypt = Some(
                crypt_realm
                    .as_ref()
                    .unwrap()
                    .root
                    .connect_to_protocol_at_exposed_dir::<CryptMarker>()
                    .unwrap()
                    .into_channel()
                    .unwrap()
                    .into_zx_channel()
                    .into(),
            );
            let mut instance = fs.serve_multi_volume().await.unwrap();
            let vol = instance
                .create_volume("default", MountOptions { crypt, as_blob: false })
                .await
                .unwrap();
            vol.bind_to_path(MOUNT_PATH).unwrap();
            Either::Right(instance)
        } else {
            let mut instance = fs.serve().await.unwrap();
            instance.bind_to_path(MOUNT_PATH).unwrap();
            Either::Left(instance)
        };

        let seed = match args.seed {
            Some(seed) => seed,
            None => random_seed(),
        };

        let mut rng = SmallRng::seed_from_u64(seed);

        // Make a home directory for file actor and deletion actor
        let root_dir = Directory::from_namespace(
            MOUNT_PATH,
            fio::OpenFlags::RIGHT_WRITABLE | fio::OpenFlags::RIGHT_READABLE,
        )
        .unwrap();
        root_dir
            .create_directory(
                "home1",
                fio::OpenFlags::RIGHT_WRITABLE | fio::OpenFlags::RIGHT_READABLE,
            )
            .await
            .unwrap();
        // Home directory must be recovered because SPO occurs in a crash test.
        // Syncronize the home directory to ensure consistency.
        open_dir_at_root("home1").sync_directory().await.unwrap();

        let file_actor = {
            let rng = SmallRng::from_seed(rng.gen());
            let uncompressed_size = UncompressedSize::InRange(ONE_MIB, FOUR_MIB);
            let compressibility = Compressibility::Compressible;
            let factory = FileFactory::new(rng, uncompressed_size, compressibility);
            let home_dir = open_dir_at_root("home1");
            let file_actor = FileActor::new(factory, home_dir);
            file_actor.set_progress_timer(std::time::Duration::from_secs(60)).await;
            Arc::new(FuturesMutex::new(file_actor))
        };
        let deletion_actor = {
            let rng = SmallRng::from_seed(rng.gen());
            let home_dir = open_dir_at_root("home1");
            Arc::new(FuturesMutex::new(DeletionActor::new(rng, home_dir)))
        };

        let instance_actor = Arc::new(FuturesMutex::new(InstanceActor::new(fvm, instance)));

        let inspect = Arc::new(Mutex::new(None));
        let inspect_cloned = inspect.clone();
        Self {
            seed,
            args,
            vmo,
            volume_guid,
            crypt_realm,
            file_actor,
            deletion_actor,
            instance_actor,
            config,
            _inspect_poll_task: fasync::Task::spawn(async move {
                Self::inspect_poll_task(moniker.unwrap(), inspect_cloned).await;
            }),
            inspect,
        }
    }

    async fn inspect_poll_task(moniker: String, inspect: Arc<Mutex<Option<DiagnosticsHierarchy>>>) {
        let mut timer = fuchsia_async::Interval::new(INSPECT_POLL_INTERVAL);
        loop {
            timer.next().await;
            match ArchiveReader::new()
                .select_all_for_moniker(&moniker)
                .snapshot::<Inspect>()
                .await
                .map_err(|e| anyhow!(e))
                .and_then(|d| {
                    d.into_iter()
                        .next()
                        .and_then(|res| res.payload)
                        .ok_or(format_err!("expected one inspect hierarchy"))
                }) {
                Ok(data) => {
                    let mut inspect = inspect.lock().unwrap();
                    if inspect.replace(data).is_none() {
                        // Whenever we first receive data for a new instance, dump it out.
                        print_inspect_data(inspect.as_ref().unwrap());
                    }
                }
                Err(e) => {
                    eprintln!("Failed to read inspect: {:?}", e);
                    continue;
                }
            };
        }
    }
}

impl<FSC: FSConfig> std::fmt::Debug for FsEnvironment<FSC> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Environment").field("seed", &self.seed).field("args", &self.args).finish()
    }
}

#[async_trait]
impl<FSC: 'static + FSConfig + Clone + Send + Sync> Environment for FsEnvironment<FSC> {
    fn target_operations(&self) -> Option<u64> {
        self.args.num_operations
    }

    fn timeout_seconds(&self) -> Option<u64> {
        self.args.time_limit_secs
    }

    async fn actor_runners(&mut self) -> Vec<ActorRunner> {
        let mut runners = vec![
            ActorRunner::new("file_actor", None, self.file_actor.clone()),
            ActorRunner::new(
                "deletion_actor",
                Some(Duration::from_secs(5)),
                self.deletion_actor.clone(),
            ),
        ];

        if let Some(secs) = self.args.disconnect_secs {
            if secs > 0 {
                let runner = ActorRunner::new(
                    "instance_actor",
                    Some(Duration::from_secs(secs)),
                    self.instance_actor.clone(),
                );
                runners.push(runner);
            }
        }

        runners
    }

    async fn reset(&mut self) {
        {
            let mut actor = self.instance_actor.lock().await;

            // The environment is only reset when the instance is killed.
            // TODO(72385): Pass the actor error here, so it can be printed out on assert failure.
            assert!(actor.instance.is_none());

            // Create a ramdisk and setup FVM.
            let fvm = FvmInstance::new(
                false,
                &self.vmo,
                self.args.fvm_slice_size,
                self.args.ramdisk_block_size,
            )
            .await;
            let volume_path = get_volume_path(&self.volume_guid).await;
            let controller =
                connect_to_protocol_at_path::<ControllerMarker>(volume_path.to_str().unwrap())
                    .unwrap();

            let mut fs = Filesystem::new(controller, self.config.clone());
            fs.fsck().await.unwrap();
            let instance = if fs.config().is_multi_volume() {
                let mut instance = fs.serve_multi_volume().await.unwrap();
                let crypt = Some(
                    self.crypt_realm
                        .as_ref()
                        .unwrap()
                        .root
                        .connect_to_protocol_at_exposed_dir::<CryptMarker>()
                        .unwrap()
                        .into_channel()
                        .unwrap()
                        .into_zx_channel()
                        .into(),
                );
                instance.check_volume("default", crypt).await.unwrap();
                let crypt = Some(
                    self.crypt_realm
                        .as_ref()
                        .unwrap()
                        .root
                        .connect_to_protocol_at_exposed_dir::<CryptMarker>()
                        .unwrap()
                        .into_channel()
                        .unwrap()
                        .into_zx_channel()
                        .into(),
                );
                let vol = instance
                    .open_volume("default", MountOptions { crypt, as_blob: false })
                    .await
                    .unwrap();
                vol.bind_to_path(MOUNT_PATH).unwrap();
                Either::Right(instance)
            } else {
                let mut instance = fs.serve().await.unwrap();
                instance.bind_to_path(MOUNT_PATH).unwrap();
                Either::Left(instance)
            };

            *self.inspect.lock().unwrap() = None;

            // Replace the fvm and fs instances
            actor.instance = Some((fvm, instance));
        }

        // Replace the root directory with a new one
        {
            let mut actor = self.file_actor.lock().await;
            actor.home_dir = open_dir_at_root("home1");
        }

        {
            let mut actor = self.deletion_actor.lock().await;
            actor.home_dir = open_dir_at_root("home1");
        }
    }

    fn panic_hook(&self) -> Option<Box<dyn Fn() + 'static + Sync + Send>> {
        let inspect = self.inspect.clone();
        Some(Box::new(move || {
            eprintln!("Printing inspect data for test due to panic.");
            let mut inspect = match inspect.try_lock() {
                Ok(v) if v.is_none() => {
                    eprintln!("No inspect data was collected; can't print additional debug info");
                    return;
                }
                Ok(inspect) => inspect,
                Err(_) => {
                    eprintln!("Failed to acquire lock; can't print additional debug info");
                    return;
                }
            };
            let inspect = inspect.as_mut().unwrap();
            inspect.sort();
            print_inspect_data(&*inspect);
        }))
    }
}
