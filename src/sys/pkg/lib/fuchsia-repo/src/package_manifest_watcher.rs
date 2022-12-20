// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::{anyhow, format_err, Context as _, Result},
    camino::Utf8PathBuf,
    fuchsia_pkg::PackageManifestList,
    futures::{executor::block_on, stream::BoxStream, SinkExt, StreamExt as _},
    notify::{immediate_watcher, EventKind, RecursiveMode, Watcher as _},
    std::{
        collections::{HashMap, HashSet},
        fs::File,
        io::BufReader,
        sync::{Arc, Mutex},
    },
    tracing::warn,
    typed_builder::TypedBuilder,
};

/// Buffer size for file change events.
const NOTIFY_CHANNEL_BUFFER_SIZE: usize = 10;

#[derive(TypedBuilder)]
pub struct PackageManifestWatcherBuilder<'a> {
    #[builder(setter(transform = |i: impl Iterator<Item = &'a Utf8PathBuf> + 'a| Box::new(i) as Box<dyn Iterator<Item = &'a Utf8PathBuf>>))]
    manifests: Box<dyn Iterator<Item = &'a Utf8PathBuf> + 'a>,

    #[builder(setter(transform = |i: impl Iterator<Item = &'a Utf8PathBuf> + 'a| Box::new(i) as Box<dyn Iterator<Item = &'a Utf8PathBuf>>))]
    lists: Box<dyn Iterator<Item = &'a Utf8PathBuf> + 'a>,
}

/// Watches changed to manifests and manifest list files.
pub struct PackageManifestWatcher {
    // HashMap of watched package manifest lists, using file path as a key.
    watched_list_files: HashMap<Utf8PathBuf, PackageManifestList>,
    // Manages watched manifest files.
    file_watcher: ManifestFileWatcher,
}

impl PackageManifestWatcher {
    // Watch paths to package manifests and package manifests lists, populated from builder.
    pub fn watch(builder: PackageManifestWatcherBuilder<'_>) -> Result<BoxStream<'static, ()>> {
        let (sender, receiver) =
            futures::channel::mpsc::channel::<notify::Event>(NOTIFY_CHANNEL_BUFFER_SIZE);
        let sender = Mutex::new(sender);
        let mut watcher = immediate_watcher(move |event: notify::Result<notify::Event>| {
            let event = match event {
                Ok(event) => event,
                Err(err) => {
                    warn!("error receving notify event: {}", err);
                    return;
                }
            };
            if let Err(e) = block_on(sender.lock().unwrap().send(event)) {
                warn!("Error sending event: {:?}", e);
            }
        })?;
        watcher.configure(notify::Config::PreciseEvents(true))?;
        let manifest_list_watcher =
            Arc::new(Mutex::new(PackageManifestWatcher::new(builder, watcher)?));

        let stream = receiver.filter_map(move |event| {
            let manifest_list_watcher = manifest_list_watcher.clone();
            async move {
                manifest_list_watcher
                    .lock()
                    .unwrap()
                    .handle_notify_event(event)
                    .unwrap_or_else(|e| {
                        warn!("Error handling notify event: {:?}", e);
                        false
                    })
                    .then_some(())
            }
        });
        Ok(stream.boxed())
    }

    // Creates a new instance of `PackageManifestWatcher` and starts watching provided manidests and
    // manifest lists.
    fn new(
        builder: PackageManifestWatcherBuilder<'_>,
        watcher: notify::RecommendedWatcher,
    ) -> Result<Self> {
        let mut file_watcher = ManifestFileWatcher::new(watcher)?;
        for manifest in builder.manifests {
            if !manifest.is_absolute() {
                warn!("Package manifest paths must be absolute, ignoring: {:}", manifest);
                continue;
            }
            file_watcher
                .watch_file(manifest)
                .with_context(|| format!("setting up file watcher for manifest {:?}", manifest))?;
        }
        let watched_list_files = HashMap::new();
        let mut instance = PackageManifestWatcher { watched_list_files, file_watcher };
        instance.set_lists(builder.lists)?;
        Ok(instance)
    }

    // Populates `PackageManifestWatcher` with package manifest lists from iterator.
    fn set_lists<'a>(&mut self, lists: impl Iterator<Item = &'a Utf8PathBuf>) -> Result<()> {
        for list_file_path in lists {
            // If list_file_path is relative, it's assumed it's relative to the current working dir.
            self.file_watcher.watch_file(list_file_path).with_context(|| {
                format!("setting up file watcher for list {:?}", list_file_path)
            })?;
            let package_list_manifest = PackageManifestWatcher::read_list(list_file_path)?;
            let list_parent = list_file_path
                .parent()
                .ok_or_else(|| format_err!("invalid path, no parent: {:?}", list_file_path))?;
            for file_path in package_list_manifest.iter() {
                if file_path.is_absolute() {
                    warn!(
                        "Package manifest paths must be relative to the package manifest list, \
                         ignoring: {:}",
                        file_path
                    );
                    continue;
                }
                // Package manifests are relative to the package manifest list.
                let manifest_path = list_parent.join(file_path);
                self.file_watcher.watch_file(&manifest_path).with_context(|| {
                    format!("setting up file watcher for file {:?}", list_file_path)
                })?;
            }
            self.watched_list_files.entry(list_file_path.into()).or_insert(package_list_manifest);
        }
        Ok(())
    }

    // Reads `PackageManifestList` from path.
    fn read_list(path: &Utf8PathBuf) -> Result<PackageManifestList> {
        let file = File::open(path.as_std_path()).with_context(|| {
            format!("opening package list manifest file {}", path.as_std_path().display())
        })?;
        PackageManifestList::from_reader(BufReader::new(file))
            .with_context(|| format!("reading package list manifest file {}", path))
    }

    // Handles event from underlying `notify` library.
    // Returns true if the file changed is in monitored package manifests or package manifest lists.
    fn handle_notify_event(&mut self, event: notify::Event) -> Result<bool> {
        let paths = event
            .paths
            .iter()
            .filter_map(|p| {
                Utf8PathBuf::from_path_buf(p.to_path_buf())
                    .map_err(|err| warn!("error converting path: {}", err.display()))
                    .ok()
            })
            .collect();
        Ok(match event.kind {
            EventKind::Modify(_) | EventKind::Create(_) | EventKind::Remove(_) => {
                self.handle_paths_changed(paths)?
            }
            _ => false,
        })
    }

    // Returns true if in the paths provided are watched for changes. This includes package
    // manifests as well as package manifests lists.
    fn handle_paths_changed(&mut self, paths: Vec<Utf8PathBuf>) -> Result<bool> {
        let mut lists_changed = HashSet::new();
        let mut is_file_changed: bool = false;
        for path in paths.iter() {
            if self.watched_list_files.contains_key(path) {
                lists_changed.insert(path);
            } else if !is_file_changed {
                // Currently, the `PackageManifestWatcher` only propagates the fact that
                // whole watched set was changed, so any file that's not a package manifest list
                // should return true.
                // It's too early to exit the function here though as changes to the package
                // manifest lists should be processed to handle new and deleted manifests.
                is_file_changed = self.file_watcher.is_file_watched(path)?;
            }
        }
        if lists_changed.is_empty() && !is_file_changed {
            return Ok(false);
        }
        let should_notify = lists_changed
            .into_iter()
            .filter_map(|path| {
                self.handle_list_changed(path.into())
                    .map_err(|e| warn!("error processing list {:?}: {:?}", path, e))
                    .ok()
            })
            .fold(is_file_changed, |should_notify, item| item || should_notify);
        Ok(should_notify)
    }

    // Returns true of the watched list's contents were changed.
    fn handle_list_changed(&mut self, list_path: Utf8PathBuf) -> Result<bool> {
        let list_parent = list_path
            .parent()
            .ok_or_else(|| format_err!("invalid path, no parent: {:?}", list_path))?;
        // Will be empty if a new list is added.
        let new_list = PackageManifestWatcher::read_list(&list_path).or_else(|e| {
            match e.downcast::<std::io::Error>() {
                Ok(err) if err.kind() == std::io::ErrorKind::NotFound => {
                    Ok(PackageManifestList::new())
                }
                Ok(err) => Err(anyhow!(err)),
                Err(err) => Err(err),
            }
        })?;
        let new_manifests = new_list.iter().collect::<HashSet<_>>();

        // Will be empty if the list is removed.
        let old_manifests = {
            if let Some(list) = self.watched_list_files.get(&list_path) {
                list.iter().collect::<HashSet<_>>()
            } else {
                HashSet::new()
            }
        };
        let added_files = &new_manifests - &old_manifests;
        let removed_files = &old_manifests - &new_manifests;

        let mut should_notify = !added_files.is_empty();
        for file_path in added_files {
            self.file_watcher.watch_file(&list_parent.join(file_path))?;
        }

        should_notify |= !removed_files.is_empty();
        for file_path in removed_files {
            self.file_watcher.unwatch_file(&list_parent.join(file_path))?;
        }

        self.watched_list_files.insert(list_path, new_list);
        Ok(should_notify)
    }
}

/// Watches manifest files and provides methods for watching and unwatching them.
struct ManifestFileWatcher {
    // Hashed by parent dir to a set of file paths watched in that dir.
    watched_files: HashMap<Utf8PathBuf, HashSet<Utf8PathBuf>>,
    watcher: notify::RecommendedWatcher,
    // The notify library reports changed files with the absolute path. The paths need to be
    // adjusted relative to this directory in order to correctly determine which manifests changed.
    current_dir: Utf8PathBuf,
}

impl ManifestFileWatcher {
    fn new(watcher: notify::RecommendedWatcher) -> Result<Self> {
        let watched_files = HashMap::new();
        let current_dir = Utf8PathBuf::from_path_buf(std::env::current_dir()?)
            .map_err(|err| anyhow!("error converting current path: {}", err.display()))?;
        Ok(ManifestFileWatcher { watcher, current_dir, watched_files })
    }

    fn is_file_watched(&self, path: &Utf8PathBuf) -> Result<bool> {
        let parent =
            path.parent().ok_or_else(|| format_err!("invalid path, no parent: {:?}", path))?;
        if let Some(files_hash) = self.watched_files.get(parent) {
            Ok(files_hash.contains(path))
        } else {
            Ok(false)
        }
    }

    // If the path is relative, it's `current_dir` is added before it.
    fn watch_file(&mut self, path: &Utf8PathBuf) -> Result<()> {
        // If the `path` is absolute, the `current_dir` will be ignored.
        let path = self.current_dir.join(path);
        let parent =
            path.parent().ok_or_else(|| format_err!("invalid path, no parent: {:?}", path))?;
        let files_hash = self.watched_files.entry(parent.into()).or_default();
        if files_hash.is_empty() {
            // Watch the parent path instead of directly watching the file to avoid
            // https://github.com/notify-rs/notify/issues/165.
            self.watcher.watch(parent, RecursiveMode::NonRecursive)?;
        }
        files_hash.insert(path);
        Ok(())
    }

    fn unwatch_file(&mut self, path: &Utf8PathBuf) -> Result<()> {
        let parent =
            path.parent().ok_or_else(|| format_err!("invalid path, no parent: {:?}", path))?;
        if let Some(ref mut files_hash) = self.watched_files.get_mut(parent) {
            files_hash.remove(path);
            if files_hash.is_empty() {
                self.watcher.unwatch(parent)?;
            }
        } else {
            warn!("trying to unwatch a file that wastn't watched");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::test_utils,
        camino::Utf8Path,
        fuchsia_async::{self as fasync, Duration},
        futures::FutureExt,
        std::{fs::File, io::Write},
    };

    struct TestEnv {
        _tmp: tempfile::TempDir,
        // Package manifests, not included into a list.
        standalone_manifests: Vec<Utf8PathBuf>,
        manifest_list: Utf8PathBuf,
        package_manifests: Vec<Utf8PathBuf>,
        another_manifest_list: Utf8PathBuf,
        other_package_manifests: Vec<Utf8PathBuf>,
        watch_stream: BoxStream<'static, ()>,
    }

    impl TestEnv {
        fn new() -> Self {
            let tmp = tempfile::tempdir().unwrap();
            let dir = Utf8Path::from_path(tmp.path()).unwrap();
            let package_dir = dir.join("pkg_manifest_list_dir");
            std::fs::create_dir(&package_dir).unwrap();
            let package_manifests =
                (0..2).map(|i| create_package_manifest(&package_dir, i)).collect::<Vec<_>>();
            let manifest_list =
                create_populated_manifest_list(dir, "pkg_manifest_list", package_manifests.iter());

            let package_dir = dir.join("another_manifest_list_dir");
            std::fs::create_dir(&package_dir).unwrap();
            let other_package_manifests =
                (10..12).map(|i| create_package_manifest(&package_dir, i)).collect::<Vec<_>>();
            let another_manifest_list = create_populated_manifest_list(
                dir,
                "another_manifest_list",
                other_package_manifests.iter(),
            );

            let manifests_dir = dir.join("standalone_manifests");
            std::fs::create_dir(&manifests_dir).unwrap();
            let standalone_manifests =
                (20..22).map(|i| create_package_manifest(&manifests_dir, i)).collect::<Vec<_>>();

            let watch_stream = PackageManifestWatcher::watch(
                PackageManifestWatcherBuilder::builder()
                    .lists([&another_manifest_list, &manifest_list].into_iter())
                    .manifests(standalone_manifests.iter())
                    .build(),
            )
            .unwrap();
            Self {
                _tmp: tmp,
                another_manifest_list,
                other_package_manifests,
                manifest_list,
                package_manifests,
                watch_stream,
                standalone_manifests,
            }
        }
    }

    fn create_populated_manifest_list<'a>(
        dir: &Utf8Path,
        name: &str,
        manifests: impl Iterator<Item = &'a Utf8PathBuf>,
    ) -> Utf8PathBuf {
        let manifest_list_dir = dir.join(name.to_owned() + "_dir");
        let manifest_list = manifest_list_dir.join(name);
        create_manifest_list(&manifest_list, manifests);
        manifest_list
    }

    fn create_package_manifest(dir: &Utf8Path, i: u32) -> Utf8PathBuf {
        let package_name = format!("pkg{}", i);
        let pkg_dir = dir.join(&package_name);
        let (_meta_far_path, manifest) =
            test_utils::make_package_manifest(&package_name, pkg_dir.as_std_path(), Vec::new());
        let manifest_path = pkg_dir.join(format!("{}.manifest", package_name));
        serde_json::to_writer(std::fs::File::create(&manifest_path).unwrap(), &manifest).unwrap();
        manifest_path
    }

    fn create_manifest_list<'a>(
        file_path: &Utf8Path,
        manifests: impl Iterator<Item = &'a Utf8PathBuf>,
    ) {
        let file = File::create(file_path).unwrap();
        let parent = file_path.parent().unwrap();
        PackageManifestList::from_iter(
            manifests.map(|path| path.strip_prefix(parent).unwrap()).map(Into::into),
        )
        .to_writer(file)
        .unwrap();
    }

    fn update_manifest_list<'a>(
        file_path: &Utf8Path,
        manifests: impl Iterator<Item = &'a Utf8PathBuf>,
    ) {
        let file = std::fs::OpenOptions::new().write(true).truncate(true).open(file_path).unwrap();
        let parent = file_path.parent().unwrap();
        PackageManifestList::from_iter(
            manifests.map(|path| path.strip_prefix(parent).unwrap()).map(Into::into),
        )
        .to_writer(file)
        .unwrap();
    }

    fn create_file(file_path: &Utf8PathBuf, bytes: &[u8]) {
        let mut file = File::create(file_path).unwrap();
        file.write_all(bytes).unwrap();
    }

    fn update_file(path: &Utf8PathBuf, bytes: &[u8]) {
        let mut file = std::fs::OpenOptions::new().write(true).truncate(true).open(path).unwrap();
        file.write_all(bytes).unwrap();
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_create_modify_remove_recreate_manifest() {
        let env = TestEnv::new();
        let mut watch_stream = env.watch_stream;

        // Try to read from the stream. This should not return anything since there were no
        // updates to files in the package manifest list.
        futures::select! {
            _ = watch_stream.next().fuse() => panic!("should not have received an event"),
            _ = fasync::Timer::new(Duration::from_millis(10)).fuse() => (),
        };

        // Next, write to the file and make sure we observe an event
        update_file(&env.package_manifests[0], br#"{"version":1}"#);

        futures::select! {
            result = watch_stream.next().fuse() => {
                assert_eq!(result, Some(()));
            },
            _ = fasync::Timer::new(Duration::from_secs(10)).fuse() => {
                panic!("wrote to package manifest, but did not get an event");
            },
        };

        // Next, write to another file and make sure we observe an event.
        update_file(&env.standalone_manifests[1], br#"{"version":1}"#);

        futures::select! {
            result = watch_stream.next().fuse() => {
                assert_eq!(result, Some(()));
            },
            _ = fasync::Timer::new(Duration::from_secs(10)).fuse() => {
                panic!("wrote to another package manifest, but did not get an event");
            },
        };

        // Next, delete a file and make sure we observe an event.
        std::fs::remove_file(&env.package_manifests[1]).unwrap();

        futures::select! {
            result = watch_stream.next().fuse() => {
                assert_eq!(result, Some(()));
            },
            _ = fasync::Timer::new(Duration::from_secs(10)).fuse() => {
                panic!("deleted package manifest, but did not get an event");
            },
        };

        // Next, re-create a file and make sure we observe an event.
        create_file(&env.package_manifests[1], br#"{}"#);

        futures::select! {
            result = watch_stream.next().fuse() => {
                assert_eq!(result, Some(()));
            },
            _ = fasync::Timer::new(Duration::from_secs(10)).fuse() => {
                panic!("recreated package manifest, but did not get an event");
            },
        };

        // FIXME(https://github.com/notify-rs/notify/pull/337): On OSX, notify uses a
        // crossbeam-channel in `Drop` to shut down the interior thread. Unfortunately this can
        // trip over an issue where OSX will tear down the thread local storage before shutting
        // down the thread, which can trigger a panic. To avoid this issue, sleep a little bit
        // after shutting down our stream.
        drop(watch_stream);
        fasync::Timer::new(Duration::from_millis(100)).await;
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_updating_manifest_list() {
        let env = TestEnv::new();
        let mut watch_stream = env.watch_stream;

        // Create a file next to watched manifest.
        let sibling = &env.package_manifests[1].parent().unwrap().join("sibling.txt");
        create_file(sibling, br#"{}"#);

        // Create a file next to watched manifest list.
        let sibling_list = &env.manifest_list.parent().unwrap().join("sibling_list.txt");
        create_manifest_list(sibling_list, [sibling].into_iter());

        // Try to read from the stream. This should not return anything since there were no
        // updates to watched files in the package manifest list.
        futures::select! {
            _ = watch_stream.next().fuse() => panic!("should not have received an event"),
            _ = fasync::Timer::new(Duration::from_millis(10)).fuse() => (),
        };

        // Update package manifest list to include the newly created file.
        let another_sibling = &env.other_package_manifests[1].parent().unwrap().join("sibling.txt");
        create_file(another_sibling, br#"{}"#);
        update_manifest_list(&env.another_manifest_list, [another_sibling].into_iter());
        futures::select! {
            result = watch_stream.next().fuse() => {
                assert_eq!(result, Some(()));
            },
            _ = fasync::Timer::new(Duration::from_secs(10)).fuse() => {
                panic!("updated another package manifest list, but did not get an event");
            },
        };

        // Next, write to another file and make sure we observe an event.
        update_file(another_sibling, br#"{"version": 1}"#);

        futures::select! {
            result = watch_stream.next().fuse() => {
                assert_eq!(result, Some(()));
            },
            _ = fasync::Timer::new(Duration::from_secs(10)).fuse() => {
                panic!("modified new package manifest, but did not get an event");
            },
        };

        // FIXME(https://github.com/notify-rs/notify/pull/337): On OSX, notify uses a
        // crossbeam-channel in `Drop` to shut down the interior thread. Unfortunately this can
        // trip over an issue where OSX will tear down the thread local storage before shutting
        // down the thread, which can trigger a panic. To avoid this issue, sleep a little bit
        // after shutting down our stream.
        drop(watch_stream);
        fasync::Timer::new(Duration::from_millis(100)).await;
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_remove_manifest_list_stops_watching_manifests() {
        let env = TestEnv::new();
        let mut watch_stream = env.watch_stream;

        // Remove the manifest list and make sure we observe an event.
        std::fs::remove_file(&env.manifest_list).unwrap();
        futures::select! {
            result = watch_stream.next().fuse() => {
                assert_eq!(result, Some(()));
            },
            _ = fasync::Timer::new(Duration::from_secs(10)).fuse() => {
                panic!("deleted package manifest list, but did not get an event");
            },
        };

        // Next, write to manifest file which is no longer watched.
        update_file(&env.package_manifests[1], br#"{"version":1}"#);

        // Next, remove manifest file which is no longer watched.
        std::fs::remove_file(&env.package_manifests[0]).unwrap();

        // Try to read from the stream. This should not return anything since there were no
        // updates to files watched.
        futures::select! {
            _ = watch_stream.next().fuse() => panic!("should not have received an event"),
            _ = fasync::Timer::new(Duration::from_millis(10)).fuse() => (),
        };

        // FIXME(https://github.com/notify-rs/notify/pull/337): On OSX, notify uses a
        // crossbeam-channel in `Drop` to shut down the interior thread. Unfortunately this can
        // trip over an issue where OSX will tear down the thread local storage before shutting
        // down the thread, which can trigger a panic. To avoid this issue, sleep a little bit
        // after shutting down our stream.
        drop(watch_stream);
        fasync::Timer::new(Duration::from_millis(100)).await;
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_delete_and_recreate_manifest_list() {
        let env = TestEnv::new();
        let mut watch_stream = env.watch_stream;

        // Delete the manifest list and make sure we observe an event.
        std::fs::remove_file(&env.manifest_list).unwrap();
        futures::select! {
            result = watch_stream.next().fuse() => {
                assert_eq!(result, Some(()));
            },
            _ = fasync::Timer::new(Duration::from_secs(10)).fuse() => {
                panic!("deleted package manifest list, but did not get an event");
            },
        };

        // Next, recreate manifest list and make sure we observe an event.
        create_manifest_list(&env.manifest_list, env.package_manifests.iter());
        futures::select! {
            result = watch_stream.next().fuse() => {
                assert_eq!(result, Some(()));
            },
            _ = fasync::Timer::new(Duration::from_secs(10)).fuse() => {
                panic!("recreated package manifest list, but did not get an event");
            },
        };

        // Next, write to now-watched file and make sure we observe an event.
        update_file(&env.package_manifests[0], br#"{"version":1}"#);

        futures::select! {
            result = watch_stream.next().fuse() => {
                assert_eq!(result, Some(()));
            },
            _ = fasync::Timer::new(Duration::from_secs(10)).fuse() => {
                panic!("wrote to package manifest, but did not get an event");
            },
        };

        // FIXME(https://github.com/notify-rs/notify/pull/337): On OSX, notify uses a
        // crossbeam-channel in `Drop` to shut down the interior thread. Unfortunately this can
        // trip over an issue where OSX will tear down the thread local storage before shutting
        // down the thread, which can trigger a panic. To avoid this issue, sleep a little bit
        // after shutting down our stream.
        drop(watch_stream);
        fasync::Timer::new(Duration::from_millis(100)).await;
    }
}
