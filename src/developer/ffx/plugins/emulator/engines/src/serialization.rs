// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::EngineBuilder;
use anyhow::{bail, Result};
use emulator_instance::{read_from_disk, EngineOption};
use ffx_emulator_config::EmulatorEngine;

pub async fn read_engine_from_disk(name: &str) -> Result<Box<dyn EmulatorEngine>> {
    match read_from_disk(name).await {
        Ok(EngineOption::DoesExist(data)) => {
            let engine: Box<dyn EmulatorEngine> = EngineBuilder::from_data(data)?;

            Ok(engine)
        }
        Ok(EngineOption::DoesNotExist(_)) => bail!("{name} instance does not exist"),
        Err(e) => bail!("Could not read engine from disk: {e:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::qemu_based::qemu::QemuEngine;
    use crate::FemuEngine;
    use emulator_instance::{get_instance_dir, EmulatorInstanceData, EngineState, EngineType};
    use ffx_config::{query, ConfigLevel};
    use ffx_emulator_common::config::EMU_INSTANCE_ROOT_DIR;
    use serde_json::json;
    use std::{fs::File, io::Write};
    use tempfile::tempdir;

    #[fuchsia::test]
    async fn test_write_then_read() -> Result<()> {
        let _env = ffx_config::test_init().await.unwrap();
        let temp_dir = tempdir()
            .expect("Couldn't get a temporary directory for testing.")
            .path()
            .to_str()
            .expect("Couldn't convert Path to str")
            .to_string();
        query(EMU_INSTANCE_ROOT_DIR).level(Some(ConfigLevel::User)).set(json!(temp_dir)).await?;

        // Create a test directory in TempFile::tempdir.
        let qemu_name = "qemu_test_write_then_read";
        let femu_name = "femu_test_write_then_read";

        // Set up some test data.
        let mut qemu_instance_data =
            EmulatorInstanceData::new_with_state(qemu_name, EngineState::New);
        qemu_instance_data.set_engine_type(EngineType::Qemu);
        let q_engine = QemuEngine::new(qemu_instance_data);
        let mut femu_instance_data =
            EmulatorInstanceData::new_with_state(femu_name, EngineState::New);
        femu_instance_data.set_engine_type(EngineType::Qemu);
        let f_engine = FemuEngine::new(femu_instance_data);

        // Serialize the QEMU engine to disk.
        q_engine.save_to_disk().await.expect("Problem serializing QEMU engine to disk.");

        let qemu_copy =
            read_engine_from_disk(qemu_name).await.expect("Problem reading QEMU engine from disk.");

        assert_eq!(qemu_copy.get_instance_data(), q_engine.get_instance_data());

        // Serialize the FEMU engine to disk.
        f_engine.save_to_disk().await.expect("Problem serializing FEMU engine to disk.");
        let box_engine = read_engine_from_disk(femu_name).await;
        assert!(box_engine.is_ok(), "Read from disk failed for FEMU: {:?}", box_engine.err());

        Ok(())
    }

    #[fuchsia::test]
    async fn test_read_unknown_engine_type() -> Result<()> {
        let unknown_engine_type = include_str!("../test_data/unknown_engine_type_engine.json");
        let _env = ffx_config::test_init().await.unwrap();
        let temp_dir = tempdir()
            .expect("Couldn't get a temporary directory for testing.")
            .path()
            .to_str()
            .expect("Couldn't convert Path to str")
            .to_string();
        query(EMU_INSTANCE_ROOT_DIR).level(Some(ConfigLevel::User)).set(json!(temp_dir)).await?;

        let name = "unknown-type";

        {
            // stage the instance data since we can't write it via the emulator_instance API.

            let instance_path = get_instance_dir(name, true).await?;
            let data_path = instance_path.join("engine.json");
            let mut file = File::create(&data_path)?;
            write!(file, "{}", &unknown_engine_type)?;
        }

        let box_engine = read_from_disk(name).await;
        assert!(box_engine.is_err());

        Ok(())
    }
}
