// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::resolved_driver::ResolvedDriver,
    fuchsia_async as fasync, fuchsia_zircon as zx,
    futures::SinkExt,
    futures::StreamExt,
    rand::{rngs::SmallRng, seq::SliceRandom, SeedableRng},
};

pub struct Session {
    sender: futures::channel::mpsc::UnboundedSender<ResolvedDriver>,
    max_delay: zx::Duration,
    shuffled_boot_drivers: Vec<ResolvedDriver>,
}

impl Session {
    pub fn new(
        sender: futures::channel::mpsc::UnboundedSender<ResolvedDriver>,
        mut boot_drivers: Vec<ResolvedDriver>,
        max_delay: zx::Duration,
        seed: Option<u64>,
    ) -> Session {
        let seed_val = seed.unwrap_or(rand::random::<u64>());
        tracing::info!("Driver loading fuzzer enabled with RNG seed: {}", seed_val);

        let mut rng = SmallRng::seed_from_u64(seed_val);
        boot_drivers.shuffle(&mut rng);
        Session { sender: sender, max_delay: max_delay, shuffled_boot_drivers: boot_drivers }
    }

    pub async fn run(mut self) {
        let delay = self.max_delay / self.shuffled_boot_drivers.len() as i64;
        let mut timer = fasync::Interval::new(delay);
        for driver in self.shuffled_boot_drivers.into_iter() {
            if timer.next().await.is_none() {
                return;
            }
            let driver_url = driver.component_url.clone();
            if let Err(e) = self.sender.send(driver).await {
                tracing::error!("Failed to send driver {} to the Indexer: {}", driver_url, e);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolved_driver::DriverPackageType;
    use bind::interpreter::decode_bind_rules::DecodedRules;

    fn make_fake_boot_driver(name: &str) -> ResolvedDriver {
        let test_rules = bind::compiler::BindRules {
            instructions: vec![],
            symbol_table: std::collections::HashMap::new(),
            use_new_bytecode: true,
            enable_debug: false,
        };
        let decoded_rules = DecodedRules::new(
            bind::bytecode_encoder::encode_v2::encode_to_bytecode_v2(test_rules).unwrap(),
        )
        .unwrap();
        ResolvedDriver {
            component_url: url::Url::parse(
                &format!("fuchsia-boot:///#meta/{}.cm", name).to_owned(),
            )
            .unwrap(),
            v1_driver_path: Some(format!("fuchsia-boot:///#meta/{}.cm", name).to_owned()),
            bind_rules: decoded_rules,
            bind_bytecode: vec![],
            colocate: false,
            device_categories: vec![],
            fallback: false,
            package_type: DriverPackageType::Boot,
            package_hash: None,
        }
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_driver_load() {
        let test_boot_repo =
            vec![make_fake_boot_driver("driver-1"), make_fake_boot_driver("driver-2")];
        let (sender, mut receiver) = futures::channel::mpsc::unbounded::<ResolvedDriver>();

        let test_seed = 0;
        let session = Session::new(
            sender,
            test_boot_repo.clone(),
            fuchsia_zircon::Duration::from_millis(0),
            Some(test_seed),
        );
        session.run().await;

        let mut received_drivers = vec![];
        while let Some(driver) = receiver.next().await {
            received_drivers.push(driver);
        }

        for driver in test_boot_repo {
            assert!(received_drivers.contains(&driver));
        }
    }
}
