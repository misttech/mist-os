// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// TODO(fxb/66157): Migrate to the new library that substitutes io_utils and fuchsia_fs::directory.
// Ask for host-side support on the new library (fxr/467217).

use {
    anyhow::{anyhow, format_err, Error, Result},
    async_trait::async_trait,
    fidl::endpoints::{create_endpoints, ClientEnd},
    fidl_fuchsia_io as fio,
    fuchsia_fs::directory::readdir,
    fuchsia_fs::directory::DirEntry,
    fuchsia_fs::{
        directory::{clone_no_describe, open_directory_no_describe, open_file_no_describe},
        file::{close, read, read_to_string, write},
    },
    fuchsia_zircon_status::Status,
    futures::lock::Mutex,
    std::{
        env, fs,
        path::{Path, PathBuf},
    },
};

pub enum DirentKind {
    File,
    Directory,
}

#[async_trait]
pub trait Directory: Sized {
    /// Attempts to open the directory at `relative_path` with read-only rights.
    fn open_dir_readonly<P: AsRef<Path> + Send>(&self, relative_path: P) -> Result<Self>;

    /// Attempts to open the directory at `relative_path` with read/write rights.
    fn open_dir_readwrite<P: AsRef<Path> + Send>(&self, relative_path: P) -> Result<Self>;

    /// Attempts to create a directory at `relative_path` with read right (if `readwrite` is false) or read/write rights (if `readwrite` is true).
    fn create_dir<P: AsRef<Path> + Send>(&self, relative_path: P, readwrite: bool) -> Result<Self>;

    /// Return a copy of self.
    fn clone(&self) -> Result<Self>;

    /// Returns the contents of the file at `relative_path` as a string.
    async fn read_file<P: AsRef<Path> + Send>(&self, relative_path: P) -> Result<String>;

    /// Returns the contents of the file at `relative_path` as bytes.
    async fn read_file_bytes<P: AsRef<Path> + Send>(&self, relative_path: P) -> Result<Vec<u8>>;

    /// Returns true if an entry called `filename` exists in this directory. `filename` must be
    /// a plain file name, not a relative path.
    async fn exists(&self, filename: &str) -> Result<bool>;

    /// Returns the type of entry specified by `filename`, or None if no entry by that name
    /// is found. `filename` must be a plan file name, not a relative path.
    async fn entry_type(&self, filename: &str) -> Result<Option<DirentKind>>;

    /// Deletes the file at `relative_path`.
    async fn remove(&self, relative_path: &str) -> Result<()>;

    /// Writes `data` to a file at `relative_path`. Overwrites if the file already
    /// exists.
    async fn write_file<P: AsRef<Path> + Send>(&self, relative_path: P, data: &[u8]) -> Result<()>;

    /// Returns the size of the file at `relative_path` in bytes.
    async fn get_file_size<P: AsRef<Path> + Send>(&self, relative_path: P) -> Result<u64>;

    /// Returns a list of directory entry names as strings.
    async fn entry_names(&self) -> Result<Vec<String>>;
}

/// A convenience wrapper over a local directory.
#[derive(Debug)]
pub struct LocalDirectory {
    path: PathBuf,
}

impl LocalDirectory {
    /// Returns a new local directory wrapper with no base path component. All operations will
    /// be on paths relative to the environment used by std::fs.
    pub fn new() -> Self {
        LocalDirectory { path: PathBuf::new() }
    }

    /// Returns a new local directory such that the methods on `Self` will
    /// operate as expected on `path`:
    ///
    ///     - if `path` is absolute, returns a directory at "/"
    ///     - if `path` is relative, returns a directory at `cwd`
    pub fn for_path(path: &PathBuf) -> Self {
        if path.is_absolute() {
            LocalDirectory { path: "/".into() }
        } else {
            LocalDirectory { path: env::current_dir().unwrap() }
        }
    }
}

#[async_trait]
impl Directory for LocalDirectory {
    fn open_dir_readonly<P: AsRef<Path> + Send>(&self, relative_path: P) -> Result<Self> {
        let full_path = self.path.join(relative_path);
        Ok(LocalDirectory { path: full_path })
    }

    fn open_dir_readwrite<P: AsRef<Path> + Send>(&self, relative_path: P) -> Result<Self> {
        self.open_dir_readonly(relative_path)
    }

    fn create_dir<P: AsRef<Path> + Send>(
        &self,
        relative_path: P,
        _readwrite: bool,
    ) -> Result<Self> {
        let full_path = self.path.join(relative_path);
        fs::create_dir(full_path.clone())?;
        Ok(LocalDirectory { path: full_path })
    }

    fn clone(&self) -> Result<Self> {
        Ok(LocalDirectory { path: self.path.clone() })
    }

    async fn read_file<P: AsRef<Path> + Send>(&self, relative_path: P) -> Result<String> {
        let full_path = self.path.join(relative_path);
        fs::read_to_string(full_path).map_err(Into::into)
    }

    async fn read_file_bytes<P: AsRef<Path> + Send>(&self, relative_path: P) -> Result<Vec<u8>> {
        let full_path = self.path.join(relative_path);
        fs::read(full_path).map_err(Into::into)
    }

    async fn exists(&self, filename: &str) -> Result<bool> {
        Ok(self.path.join(filename).exists())
    }

    async fn entry_type(&self, filename: &str) -> Result<Option<DirentKind>> {
        let full_path = self.path.join(filename);
        if !full_path.exists() {
            return Ok(None);
        }
        let metadata = fs::metadata(full_path)?;
        if metadata.is_file() {
            Ok(Some(DirentKind::File))
        } else if metadata.is_dir() {
            Ok(Some(DirentKind::Directory))
        } else {
            Err(anyhow!("Unsupported entry type: {:?}", metadata.file_type()))
        }
    }

    async fn remove(&self, relative_path: &str) -> Result<()> {
        let full_path = self.path.join(relative_path);
        fs::remove_file(full_path).map_err(Into::into)
    }

    async fn write_file<P: AsRef<Path> + Send>(&self, relative_path: P, data: &[u8]) -> Result<()> {
        let full_path = self.path.join(relative_path);
        fs::write(full_path, data).map_err(Into::into)
    }

    async fn get_file_size<P: AsRef<Path> + Send>(&self, relative_path: P) -> Result<u64> {
        let full_path = self.path.join(relative_path);
        let metadata = fs::metadata(full_path)?;
        if metadata.is_file() {
            Ok(metadata.len())
        } else {
            Err(anyhow!("Cannot get size of non-file"))
        }
    }

    async fn entry_names(&self) -> Result<Vec<String>> {
        let paths = fs::read_dir(self.path.clone())?;
        Ok(paths.into_iter().map(|p| p.unwrap().file_name().into_string().unwrap()).collect())
    }
}

/// A convenience wrapper over a FIDL DirectoryProxy.
#[derive(Debug)]
pub struct RemoteDirectory {
    path: PathBuf,
    proxy: fio::DirectoryProxy,
    // The `fuchsia.io.RemoteDirectory` protocol is stateful in readdir, and the associated `fuchsia_fs::directory`
    // library used for enumerating the directory has no mechanism for synchronization of readdir
    // operations, as such this mutex must be held throughout directory enumeration in order to
    // avoid race conditions from concurrent rewinds and reads.
    readdir_mutex: Mutex<()>,
}

impl RemoteDirectory {
    #[cfg(target_os = "fuchsia")]
    pub fn from_namespace<P: AsRef<Path> + Send>(path: P) -> Result<Self> {
        let path_str = path
            .as_ref()
            .as_os_str()
            .to_str()
            .ok_or_else(|| format_err!("could not convert path to string"))?;
        let proxy =
            fuchsia_fs::directory::open_in_namespace(path_str, fio::OpenFlags::RIGHT_READABLE)?;
        let path = path.as_ref().to_path_buf();
        Ok(Self { path, proxy, readdir_mutex: Mutex::new(()) })
    }

    pub fn from_proxy(proxy: fio::DirectoryProxy) -> Self {
        let path = PathBuf::from(".");
        Self { path, proxy, readdir_mutex: Mutex::new(()) }
    }

    pub fn clone_proxy(&self) -> Result<fio::DirectoryProxy> {
        let (clone, clone_server) = create_endpoints::<fio::NodeMarker>();
        self.proxy.clone(fio::OpenFlags::CLONE_SAME_RIGHTS, clone_server)?;

        match ClientEnd::<fio::DirectoryMarker>::new(clone.into_channel()).into_proxy() {
            Ok(cloned_proxy) => Ok(cloned_proxy),
            Err(e) => Err(format_err!("Could not clone proxy. {}", e)),
        }
    }

    async fn entries(&self) -> Result<Vec<DirEntry>, Error> {
        let _lock = self.readdir_mutex.lock().await;
        match readdir(&self.proxy).await {
            Ok(entries) => Ok(entries),
            Err(e) => Err(format_err!(
                "could not get entries of `{}`: {}",
                self.path.as_path().display(),
                e
            )),
        }
    }

    fn open_dir<P: AsRef<Path> + Send>(
        &self,
        relative_path: P,
        flags: fio::OpenFlags,
    ) -> Result<Self> {
        let path = self.path.join(relative_path.as_ref());
        let relative_path = match relative_path.as_ref().to_str() {
            Some(relative_path) => relative_path,
            None => return Err(format_err!("could not convert relative path to &str")),
        };
        match open_directory_no_describe(&self.proxy, relative_path, flags) {
            Ok(proxy) => Ok(Self { path, proxy, readdir_mutex: Mutex::new(()) }),
            Err(e) => Err(format_err!("could not open dir `{}`: {}", path.as_path().display(), e)),
        }
    }
}

#[async_trait]
impl Directory for RemoteDirectory {
    fn open_dir_readonly<P: AsRef<Path> + Send>(&self, relative_path: P) -> Result<Self> {
        self.open_dir(relative_path, fio::OpenFlags::RIGHT_READABLE)
    }

    fn open_dir_readwrite<P: AsRef<Path> + Send>(&self, relative_path: P) -> Result<Self> {
        self.open_dir(
            relative_path,
            fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE,
        )
    }

    fn create_dir<P: AsRef<Path> + Send>(&self, relative_path: P, readwrite: bool) -> Result<Self> {
        let mut flags = fio::OpenFlags::CREATE | fio::OpenFlags::RIGHT_READABLE;
        if readwrite {
            flags = flags | fio::OpenFlags::RIGHT_WRITABLE;
        }
        self.open_dir(relative_path, flags)
    }

    async fn read_file<P: AsRef<Path> + Send>(&self, relative_path: P) -> Result<String> {
        let path = self.path.join(relative_path.as_ref());
        let relative_path = match relative_path.as_ref().to_str() {
            Some(relative_path) => relative_path,
            None => return Err(format_err!("relative path is not valid unicode")),
        };

        let proxy =
            match open_file_no_describe(&self.proxy, relative_path, fio::OpenFlags::RIGHT_READABLE)
            {
                Ok(proxy) => proxy,
                Err(e) => {
                    return Err(format_err!(
                        "could not open file `{}`: {}",
                        path.as_path().display(),
                        e
                    ))
                }
            };

        match read_to_string(&proxy).await {
            Ok(data) => Ok(data),
            Err(e) => Err(format_err!(
                "could not read file `{}` as string: {}",
                path.as_path().display(),
                e
            )),
        }
    }

    async fn read_file_bytes<P: AsRef<Path> + Send>(&self, relative_path: P) -> Result<Vec<u8>> {
        let path = self.path.join(relative_path.as_ref());
        let relative_path = match relative_path.as_ref().to_str() {
            Some(relative_path) => relative_path,
            None => return Err(format_err!("relative path is not valid unicode")),
        };

        let proxy =
            match open_file_no_describe(&self.proxy, relative_path, fio::OpenFlags::RIGHT_READABLE)
            {
                Ok(proxy) => proxy,
                Err(e) => {
                    return Err(format_err!(
                        "could not open file `{}`: {}",
                        path.as_path().display(),
                        e
                    ))
                }
            };

        match read(&proxy).await {
            Ok(data) => Ok(data),
            Err(e) => Err(format_err!("could not read file `{}`: {}", path.as_path().display(), e)),
        }
    }

    async fn exists(&self, filename: &str) -> Result<bool> {
        match self.entry_names().await {
            Ok(entries) => Ok(entries.iter().any(|s| s == filename)),
            Err(e) => Err(format_err!(
                "could not check if `{}` exists in `{}`: {}",
                filename,
                self.path.as_path().display(),
                e
            )),
        }
    }

    async fn entry_type(&self, filename: &str) -> Result<Option<DirentKind>> {
        let entries = self.entries().await?;

        entries
            .into_iter()
            .find(|e| e.name == filename)
            .map(|e| {
                match e.kind {
                    // TODO(https://fxbug.dev/127335): Update component_manager vfs to assign proper DirentType when installing the directory tree.
                    fio::DirentType::Directory | fio::DirentType::Unknown => {
                        Ok(Some(DirentKind::Directory))
                    }
                    fio::DirentType::File => Ok(Some(DirentKind::File)),
                    _ => {
                        return Err(anyhow!(
                            "Unsupported entry type for file {}: {:?}",
                            &filename,
                            e.kind,
                        ));
                    }
                }
            })
            .unwrap_or(Ok(None))
    }

    async fn remove(&self, filename: &str) -> Result<()> {
        let options = fio::UnlinkOptions::default();
        match self.proxy.unlink(filename, &options).await {
            Ok(r) => match r {
                Ok(()) => Ok(()),
                Err(e) => Err(format_err!(
                    "could not delete `{}` from `{}`: {}",
                    filename,
                    self.path.as_path().display(),
                    e
                )),
            },
            Err(e) => Err(format_err!(
                "proxy error while deleting `{}` from `{}`: {}",
                filename,
                self.path.as_path().display(),
                e
            )),
        }
    }

    async fn write_file<P: AsRef<Path> + Send>(&self, relative_path: P, data: &[u8]) -> Result<()> {
        let path = self.path.join(relative_path.as_ref());
        let relative_path = match relative_path.as_ref().to_str() {
            Some(relative_path) => relative_path,
            None => return Err(format_err!("relative path is not valid unicode")),
        };

        let file = match open_file_no_describe(
            &self.proxy,
            relative_path,
            fio::OpenFlags::RIGHT_WRITABLE | fio::OpenFlags::CREATE,
        ) {
            Ok(proxy) => proxy,
            Err(e) => {
                return Err(format_err!(
                    "could not open file `{}`: {}",
                    path.as_path().display(),
                    e
                ))
            }
        };

        let () = file
            .resize(0)
            .await
            .map_err(|e| {
                format_err!("could not truncate file `{}`: {}", path.as_path().display(), e)
            })?
            .map_err(Status::from_raw)
            .map_err(|status| {
                format_err!("could not truncate file `{}`: {}", path.as_path().display(), status)
            })?;

        match write(&file, data).await {
            Ok(()) => {}
            Err(e) => {
                return Err(format_err!(
                    "could not write to file `{}`: {}",
                    path.as_path().display(),
                    e
                ))
            }
        }

        match close(file).await {
            Ok(()) => Ok(()),
            Err(e) => {
                Err(format_err!("could not close file `{}`: {}", path.as_path().display(), e))
            }
        }
    }

    async fn get_file_size<P: AsRef<Path> + Send>(&self, relative_path: P) -> Result<u64> {
        let path = self.path.join(relative_path.as_ref());
        let relative_path = match relative_path.as_ref().to_str() {
            Some(relative_path) => relative_path,
            None => return Err(format_err!("relative path is not valid unicode")),
        };

        let file =
            match open_file_no_describe(&self.proxy, relative_path, fio::OpenFlags::RIGHT_READABLE)
            {
                Ok(proxy) => proxy,
                Err(e) => {
                    return Err(format_err!(
                        "could not open file `{}`: {}",
                        path.as_path().display(),
                        e
                    ))
                }
            };

        match file.get_attr().await {
            Ok((raw_status_code, attr)) => {
                Status::ok(raw_status_code)?;
                Ok(attr.storage_size)
            }
            Err(e) => {
                Err(format_err!("Unexpected FIDL error during file attribute retrieval: {}", e))
            }
        }
    }

    async fn entry_names(&self) -> Result<Vec<String>> {
        match self.entries().await {
            Ok(entries) => Ok(entries.into_iter().map(|e| e.name).collect()),
            Err(e) => Err(format_err!(
                "could not get entry names of `{}`: {}",
                self.path.as_path().display(),
                e
            )),
        }
    }

    fn clone(&self) -> Result<Self> {
        let proxy = clone_no_describe(&self.proxy, Some(fio::OpenFlags::RIGHT_READABLE))?;
        Ok(Self { path: self.path.clone(), proxy, readdir_mutex: Mutex::new(()) })
    }
}
