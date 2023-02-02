// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use {
    fidl_fuchsia_component_resolution as fresolution, fidl_fuchsia_io as fio,
    fuchsia_fs::{file::ReadError, node::OpenError},
    fuchsia_zircon_status::Status,
    thiserror::Error,
};

#[derive(Error, Debug)]
pub enum AbiRevisionFileError {
    #[error("Failed to decode ABI revision value")]
    Decode,
    #[error("Failed to open ABI revision file: {0}")]
    Open(#[from] OpenError),
    #[error("Failed to read ABI revision file: {0}")]
    Read(#[from] ReadError),
}

impl From<AbiRevisionFileError> for fresolution::ResolverError {
    fn from(err: AbiRevisionFileError) -> fresolution::ResolverError {
        match err {
            AbiRevisionFileError::Open(_) => fresolution::ResolverError::AbiRevisionNotFound,
            AbiRevisionFileError::Read(_) | AbiRevisionFileError::Decode => {
                fresolution::ResolverError::InvalidAbiRevision
            }
        }
    }
}

/// Attempt to read an ABI revision value from the given file path, but do not fail if the file is absent.
pub async fn read_abi_revision_optional(
    dir: &fio::DirectoryProxy,
    path: &str,
) -> Result<Option<u64>, AbiRevisionFileError> {
    match read_abi_revision(dir, path).await {
        Ok(abi) => Ok(Some(abi)),
        Err(AbiRevisionFileError::Open(OpenError::OpenError(Status::NOT_FOUND))) => Ok(None),
        Err(e) => Err(e),
    }
}

// TODO(fxbug.dev/111777): return fuchsia.version.AbiRevision & use decode_persistent().
/// Read an ABI revision value from the given file path.
async fn read_abi_revision(
    dir: &fio::DirectoryProxy,
    path: &str,
) -> Result<u64, AbiRevisionFileError> {
    let file = fuchsia_fs::directory::open_file(&dir, path, fio::OpenFlags::RIGHT_READABLE).await?;
    let bytes: [u8; 8] = fuchsia_fs::file::read(&file)
        .await?
        .try_into()
        .map_err(|_| AbiRevisionFileError::Decode)?;
    Ok(u64::from_le_bytes(bytes))
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        fuchsia_fs::directory::open_in_namespace,
        std::sync::Arc,
        version_history::AbiRevision,
        vfs::{
            directory::entry::DirectoryEntry, execution_scope,
            file::vmo::asynchronous::read_only_static, pseudo_directory, remote::remote_dir,
        },
    };

    fn serve_dir(root: Arc<impl DirectoryEntry>) -> fio::DirectoryProxy {
        let (dir_proxy, dir_server) =
            fidl::endpoints::create_proxy::<fio::DirectoryMarker>().unwrap();
        root.open(
            execution_scope::ExecutionScope::new(),
            fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::DIRECTORY,
            vfs::path::Path::dot().into(),
            fidl::endpoints::ServerEnd::new(dir_server.into_channel()),
        );
        dir_proxy
    }

    fn init_fuchsia_abi_dir(filename: &'static str, content: &[u8]) -> fio::DirectoryProxy {
        let dir = pseudo_directory! {
        "meta" => pseudo_directory! {
              "fuchsia.abi" => pseudo_directory! {
                filename => read_only_static(content),
              }
          }
        };
        serve_dir(dir)
    }

    #[fuchsia::test]
    async fn test_read_abi_revision_impl() -> Result<(), AbiRevisionFileError> {
        // Test input that cannot be decoded into a u64 fails
        let invalid = b"A string that is not an Abi Revision";
        let dir = init_fuchsia_abi_dir("abi-revision", invalid);
        let res = read_abi_revision_optional(&dir, AbiRevision::PATH).await;
        assert!(matches!(res.unwrap_err(), AbiRevisionFileError::Decode));

        let invalid = b"";
        let dir = init_fuchsia_abi_dir("abi-revision", invalid);
        let res = read_abi_revision_optional(&dir, AbiRevision::PATH).await;
        assert!(matches!(res.unwrap_err(), AbiRevisionFileError::Decode));

        // Test u64 inputs can be read
        let dir = init_fuchsia_abi_dir("abi-revision", &u64::MAX.to_le_bytes());
        let res = read_abi_revision_optional(&dir, AbiRevision::PATH).await.unwrap();
        assert_eq!(res, Some(u64::MAX));

        let dir = init_fuchsia_abi_dir("abi-revision", &0u64.to_le_bytes());
        let res = read_abi_revision_optional(&dir, AbiRevision::PATH).await.unwrap();
        assert_eq!(res, Some(0u64));

        Ok(())
    }

    #[fuchsia::test]
    async fn test_read_abi_revision_optional_allows_absent_file() -> Result<(), AbiRevisionFileError>
    {
        // Test abi-revision file not found produces Ok(None)
        let dir = init_fuchsia_abi_dir("abi-revision-staging", &u64::MAX.to_le_bytes());
        let res = read_abi_revision_optional(&dir, AbiRevision::PATH).await.unwrap();
        assert_eq!(res, None);

        Ok(())
    }

    #[fuchsia::test]
    async fn test_read_abi_revision_fails_absent_file() -> Result<(), AbiRevisionFileError> {
        let dir = init_fuchsia_abi_dir("a-different-file", &u64::MAX.to_le_bytes());
        let err = read_abi_revision(&dir, AbiRevision::PATH).await.unwrap_err();
        assert!(matches!(err, AbiRevisionFileError::Open(OpenError::OpenError(Status::NOT_FOUND))));
        Ok(())
    }

    // Read this test package's ABI revision.
    #[fuchsia::test]
    async fn read_test_pkg_abi_revision() -> Result<(), AbiRevisionFileError> {
        let root = remote_dir(
            open_in_namespace(
                "/pkg",
                fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_EXECUTABLE,
            )
            .unwrap(),
        );
        let dir_proxy = serve_dir(root);
        let abi_revision = read_abi_revision(&dir_proxy, AbiRevision::PATH)
            .await
            .expect("test package doesn't contain an ABI revision");
        assert!(version_history::is_valid_abi_revision(AbiRevision(abi_revision)));
        Ok(())
    }
}
