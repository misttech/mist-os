// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use crate::{EmulatorInstanceData, EngineOption, EngineState};
use anyhow::{anyhow, Context, Result};
use std::{
    fs::{create_dir_all, File},
    path::PathBuf,
};

/// The root directory for storing instance specific data. Instances
/// should create a subdirectory in this directory to store data.
pub const EMU_INSTANCE_ROOT_DIR: &'static str = "emu.instance_dir";

pub(crate) const SERIALIZE_FILE_NAME: &str = "engine.json";

/// Return a PathBuf with the path to the instance directory for this engine. If the "create" flag
/// is set, the directory and its ancestors will be created if it doesn't already exist.
pub async fn get_instance_dir(instance_name: &str, create: bool) -> Result<PathBuf> {
    let root_dir: String = ffx_config::get(EMU_INSTANCE_ROOT_DIR)
        .await
        .context("Error encountered accessing FFX config for the emulator instance root.")?;
    let path = PathBuf::from(root_dir).join(&instance_name);
    if !path.exists() {
        if create {
            tracing::debug!("Creating {:?} for {}", path, instance_name);
            create_dir_all(&path.as_path())?;
        } else {
            tracing::debug!(
                "Path {} doesn't exist. Check the spelling of the instance name.",
                instance_name
            );
        }
    }
    Ok(path)
}

/// Given an instance name, empty and remove the instance directory associated with that name.
/// Fails if the directory can't be removed; returns Ok(()) if the directory doesn't exist.
pub async fn clean_up_instance_dir(instance_name: &str) -> Result<()> {
    let path = get_instance_dir(instance_name, false).await?;
    if path.exists() {
        tracing::debug!("Removing {:?} for {:?}", path, path.as_path().file_name().unwrap());
        std::fs::remove_dir_all(&path.as_path()).context("Request to remove directory failed")
    } else {
        // It's already gone, so just return Ok(()).
        Ok(())
    }
}

/// Retrieve a list of all of the names of instances currently present on the local system.
pub async fn get_all_instances() -> Result<Vec<EmulatorInstanceData>> {
    let mut result = Vec::new();
    let root_dir: String = ffx_config::get(EMU_INSTANCE_ROOT_DIR)
        .await
        .context("Error encountered accessing FFX config for the emulator instance root.")?;
    let buf = PathBuf::from(root_dir);
    let root = buf.as_path();
    if root.is_dir() {
        for entry in root.read_dir()? {
            if let Ok(entry) = entry {
                if !entry.path().is_dir() {
                    continue;
                }
                if entry.path().join(SERIALIZE_FILE_NAME).exists() {
                    if let Some(name_as_os_str) = entry.path().file_name() {
                        if let Some(name) = name_as_os_str.to_str() {
                            match read_from_disk(name).await {
                                Ok(EngineOption::DoesExist(data)) => result.push(data),
                                Ok(EngineOption::DoesNotExist(name)) => result.push(
                                    EmulatorInstanceData::new_with_state(&name, EngineState::Error),
                                ),
                                Err(e) => {
                                    tracing::error!(
                                        "Cannot read emulator instance data for {}: {:?}",
                                        name,
                                        e
                                    );
                                    result.push(EmulatorInstanceData::new_with_state(
                                        name,
                                        EngineState::Error,
                                    ));
                                }
                            };
                        }
                    }
                }
            }
        }
    }
    return Ok(result);
}

pub async fn read_from_disk(instance_name: &str) -> Result<EngineOption> {
    let filepath = get_instance_dir(instance_name, false).await?.join(SERIALIZE_FILE_NAME);

    // Read the engine.json file and deserialize it from disk into a new TypedEngine instance
    if filepath.exists() {
        let file = File::open(&filepath)
            .context(format!("Unable to open file {:?} for deserialization", filepath))?;
        let value: EmulatorInstanceData = serde_json::from_reader(file)
            .context(format!("Invalid JSON syntax in {:?}", filepath))?;
        Ok(EngineOption::DoesExist(value))
    } else {
        Ok(EngineOption::DoesNotExist(instance_name.to_string()))
    }
}

pub fn read_from_disk_untyped(instance_directory: &PathBuf) -> Result<serde_json::Value> {
    // Get the engine's location, which is in the instance directory.
    let filepath = instance_directory.join(SERIALIZE_FILE_NAME);

    // Read the engine.json file and deserialize it from disk into a new TypedEngine instance
    if filepath.exists() {
        let file = File::open(&filepath)
            .context(format!("Unable to open file {:?} for deserialization", filepath))?;
        let value: serde_json::Value = serde_json::from_reader(file)
            .context(format!("Invalid JSON syntax in {:?}", filepath))?;
        Ok(value)
    } else {
        Err(anyhow!("Engine file doesn't exist at {:?}", filepath))
    }
}

pub fn write_to_disk(data: &EmulatorInstanceData, instance_directory: &PathBuf) -> Result<()> {
    // The engine's serialized form will be saved in ${EMU_INSTANCE_ROOT_DIR}/${runtime.name}.
    // This is the path set up by the EngineBuilder, so it's expected to already exist.

    // Create the engine.json file to hold the serialized data, and write it out to disk,
    let filepath = instance_directory.join(SERIALIZE_FILE_NAME);
    let file = File::create(&filepath)
        .context(format!("Unable to create file {:?} for serialization", filepath))?;
    tracing::debug!("Writing serialized engine out to {:?}", filepath);
    match serde_json::to_writer(file, data) {
        Ok(_) => Ok(()),
        Err(e) => Err(anyhow!(e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EmulatorInstanceInfo, EngineType};
    use anyhow::Result;
    use ffx_config::ConfigLevel;
    use serde_json::json;
    use std::{
        fs::{create_dir_all, remove_file, File},
        io::Write,
    };
    use tempfile::tempdir;

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_get_instance_dir() -> Result<()> {
        let env = ffx_config::test_init().await.unwrap();
        let temp_dir = tempdir()
            .expect("Couldn't get a temporary directory for testing.")
            .path()
            .to_str()
            .expect("Couldn't convert Path to str")
            .to_string();
        env.context
            .query(EMU_INSTANCE_ROOT_DIR)
            .level(Some(ConfigLevel::User))
            .set(json!(temp_dir))
            .await?;

        // Create a new directory.
        let path1 = get_instance_dir("create_me", true).await?;
        assert_eq!(path1, PathBuf::from(&temp_dir).join("create_me"));
        assert!(path1.exists());

        // Look for a dir that doesn't exist, but don't create it.
        let path2 = get_instance_dir("dont_create", false).await?;
        assert!(!path2.exists());

        // Look for a dir that already exists, but don't allow creation.
        let mut path3 = get_instance_dir("create_me", false).await?;
        assert_eq!(path3, PathBuf::from(&temp_dir).join("create_me"));
        assert!(path3.exists());

        // Get an existing directory, but set the create flag too. Make sure it didn't get replaced.
        path3 = path3.join("foo.txt");
        let _ = File::create(&path3)?;
        let path4 = get_instance_dir("create_me", true).await?;
        assert!(path4.exists());
        assert!(path3.exists());
        assert_eq!(path4, PathBuf::from(&temp_dir).join("create_me"));
        for entry in path4.as_path().read_dir()? {
            assert_eq!(entry?.path(), path3);
        }

        Ok(())
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_get_all_instances() -> Result<()> {
        let env = ffx_config::test_init().await.unwrap();
        let temp_dir = tempdir()
            .expect("Couldn't get a temporary directory for testing.")
            .path()
            .to_str()
            .expect("Couldn't convert Path to str")
            .to_string();
        env.context
            .query(EMU_INSTANCE_ROOT_DIR)
            .level(Some(ConfigLevel::User))
            .set(json!(temp_dir))
            .await?;

        // Create three mock instance directories, and make sure they're all included.
        let path1 = PathBuf::from(&temp_dir).join("path1");
        create_dir_all(path1.as_path())?;
        let file1_path = path1.join(SERIALIZE_FILE_NAME);
        let _file1 = File::create(&file1_path)?;

        let path2 = PathBuf::from(&temp_dir).join("path2");
        create_dir_all(path2.as_path())?;
        let file2_path = path2.join(SERIALIZE_FILE_NAME);
        let _file2 = File::create(&file2_path)?;

        let path3 = PathBuf::from(&temp_dir).join("path3");
        create_dir_all(path3.as_path())?;
        let file3_path = path3.join(SERIALIZE_FILE_NAME);
        let _file3 = File::create(&file3_path)?;

        let instances = get_all_instances().await?;
        assert!(instances.iter().any(|e| e.get_name() == "path1"));
        assert!(instances.iter().any(|e| e.get_name() == "path2"));
        assert!(instances.iter().any(|e| e.get_name() == "path3"));

        // If the directory doesn't contain an engine.json file, it's not an instance.
        // Remove the file for path2, and make sure it's excluded from the results.
        assert!(remove_file(&file2_path).is_ok());

        let instances = get_all_instances().await?;
        assert!(instances.iter().any(|e| e.get_name() == "path1"));
        assert!(!instances.iter().any(|e| e.get_name() == "path2"));
        assert!(instances.iter().any(|e| e.get_name() == "path3"));

        // Other files in the root shouldn't be included either. Create an empty file in the root
        // and make sure it's excluded too.
        let file_path = PathBuf::from(&temp_dir).join("empty_file");
        let _empty_file = File::create(&file_path)?;

        let instances = get_all_instances().await?;
        assert!(instances.iter().any(|e| e.get_name() == "path1"));
        assert!(!instances.iter().any(|e| e.get_name() == "path2"));
        assert!(instances.iter().any(|e| e.get_name() == "path3"));

        Ok(())
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_clean_up_instance_dir() -> Result<()> {
        let env = ffx_config::test_init().await.unwrap();
        let temp_dir = tempdir()
            .expect("Couldn't get a temporary directory for testing.")
            .path()
            .to_str()
            .expect("Couldn't convert Path to str")
            .to_string();
        env.context
            .query(EMU_INSTANCE_ROOT_DIR)
            .level(Some(ConfigLevel::User))
            .set(json!(temp_dir))
            .await?;

        let path1 = PathBuf::from(&temp_dir).join("path1");
        create_dir_all(path1.as_path())?;
        assert!(path1.exists());

        let path2 = PathBuf::from(&temp_dir).join("path2");
        create_dir_all(path2.as_path())?;
        assert!(path2.exists());

        let file_path = path2.join("foo.txt");
        let _ = File::create(&file_path)?;
        assert!(file_path.exists());

        // Clean up an existing, empty directory
        assert!(clean_up_instance_dir("path1").await.is_ok());
        assert!(!path1.exists());
        assert!(path2.exists());

        // Clean up an existing, populated directory
        assert!(clean_up_instance_dir("path2").await.is_ok());
        assert!(!path2.exists());
        assert!(!file_path.exists());

        // Clean up an non-existing directory
        assert!(clean_up_instance_dir("path3").await.is_ok());
        assert!(!path1.exists());
        Ok(())
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_broken_reads() -> Result<()> {
        let env = ffx_config::test_init().await.unwrap();
        let temp_dir = tempdir()
            .expect("Couldn't get a temporary directory for testing.")
            .path()
            .to_str()
            .expect("Couldn't convert Path to str")
            .to_string();
        env.context
            .query(EMU_INSTANCE_ROOT_DIR)
            .level(Some(ConfigLevel::User))
            .set(json!(temp_dir))
            .await?;

        // Create a test directory in TempFile::tempdir.
        let name = "test_write_then_read";
        let temp_path = PathBuf::from(temp_dir).join(name);
        let file_path = temp_path.join(SERIALIZE_FILE_NAME);
        create_dir_all(&temp_path)?;

        let bad_json = "This is not valid JSON";
        let no_pid = r#"{ "engine_type":"femu" }"#;
        let bad_pid = r#"{ "engine_type":"femu","pid":"string" }"#;
        let has_pid = r#"{ "engine_type":"femu","pid":123456 }"#;

        // Note: This string is a currently valid and complete instance of a FEMU config as it
        // would be serialized to disk. The test on this string should fail if a change (to the
        // EmulatorConfiguration data structure, for example) would break deserialization of
        // existing emulation instances. If your change causes this test to fail, consider wrapping
        // the fields you changed in Option<foo>, or providing a default value for the field to
        // deserialize with. Do not simply update this text to match your change, or users will
        // see [Broken] emulators on their next update. Wait until the field has had time to "bake"
        // before updating this text for your changes.
        let valid_femu = r#"{"emulator_configuration":{"device":{"audio":{"model":"hda"},"cpu":{
            "architecture":"x64","count":0},"memory":{"quantity":8192,"units":"megabytes"},
            "pointing_device":"mouse","screen":{"height":800,"width":1280,"units":"pixels"},
            "storage":{"quantity":2,"units":"gigabytes"}},"flags":{"args":[],"envs":{},"features":[],
            "kernel_args":[],"options":[]},"guest":{"fvm_image":"/path/to/fvm.blk","kernel_image":
            "/path/to/multiboot.bin","zbi_image":"/path/to/fuchsia.zbi"},"host":{"acceleration":
            "hyper","architecture":"x64","gpu":"auto","log":"/path/to/emulator.log","networking"
            :"tap","os":"linux","port_map":{}},"runtime":{"console":"none","debugger":false,
            "dry_run":false,"headless":true,"hidpi_scaling":false,"instance_directory":"/some/dir",
            "log_level":"info","mac_address":"52:54:47:5e:82:ef","name":"fuchsia-emulator",
            "startup_timeout":{"secs":60,"nanos":0},"template":"/path/to/config","upscript":null}},
            "pid":657042,"engine_type":"femu"}"#;

        let mut file = File::create(&file_path)?;
        write!(file, "{}", &bad_json)?;
        let box_engine = read_from_disk(name).await;
        assert!(box_engine.is_err());
        let value = read_from_disk_untyped(&temp_path);
        assert!(value.is_err());

        remove_file(&file_path).expect("Problem removing serialized file during test.");
        let mut file = File::create(&file_path)?;
        write!(file, "{}", &no_pid)?;
        let box_engine = read_from_disk(name).await;
        assert!(box_engine.is_err());
        let value = read_from_disk_untyped(&temp_path);
        assert!(value.is_ok(), "{:?}", value);
        assert!(value.unwrap().get("pid").is_none());

        remove_file(&file_path).expect("Problem removing serialized file during test.");
        let mut file = File::create(&file_path)?;
        write!(file, "{}", &bad_pid)?;
        let box_engine = read_from_disk(name).await;
        assert!(box_engine.is_err());
        let value = read_from_disk_untyped(&temp_path);
        assert!(value.is_ok(), "{:?}", value);
        assert!(value.as_ref().unwrap().get("pid").is_some());
        assert!(value.unwrap().get("pid").unwrap().as_i64().is_none());

        remove_file(&file_path).expect("Problem removing serialized file during test.");
        let mut file = File::create(&file_path)?;
        write!(file, "{}", &has_pid)?;
        let box_engine = read_from_disk(name).await;
        assert!(box_engine.is_err());
        let value = read_from_disk_untyped(&temp_path);
        assert!(value.is_ok(), "{:?}", value);
        assert!(value.as_ref().unwrap().get("pid").is_some());
        assert!(value.as_ref().unwrap().get("pid").unwrap().as_i64().is_some());
        assert_eq!(value.unwrap().get("pid").unwrap().as_i64().unwrap(), 123456);

        remove_file(&file_path).expect("Problem removing serialized file during test.");
        let mut file = File::create(&file_path)?;
        write!(file, "{}", &valid_femu)?;
        let box_engine = read_from_disk(name).await;
        assert!(box_engine.is_ok(), "{:?}", box_engine.err());
        match box_engine? {
            EngineOption::DoesExist(data) => assert_eq!(data.get_engine_type(), EngineType::Femu),
            other => panic!("Expected DoesExist, got {other:?}"),
        }

        Ok(())
    }
}
