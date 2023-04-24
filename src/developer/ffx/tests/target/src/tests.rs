// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::time::Duration;

use ffx_testing::{base_fixture, Emu, TestContext};
use fixture::fixture;
use fuchsia_async as _;
use futures::{
    io::{AsyncBufReadExt, AsyncReadExt, BufReader},
    AsyncWriteExt, StreamExt,
};

/// Test `ffx target flash` by bringing up an emulator in fastboot.
///
/// The fastboot implementation used by this emulator is Gigaboot (//src/firmware/gigaboot).
#[fixture(base_fixture)]
#[fuchsia::test]
async fn test_target_flash_gigaboot(ctx: TestContext) {
    let isolate = ctx.isolate();

    let emu = Emu::start(&ctx);

    let serial = emu.serial().await;

    // On initial boot, press `f` to enter fastboot.
    {
        let (reader, mut writer) = serial.split();
        let reader = BufReader::new(reader);
        let mut lines = reader.lines();
        while let Some(res) = lines.next().await {
            let line = res.unwrap();
            eprintln!("{}", line);
            if line.contains("Press f to enter fastboot.") {
                writer.write_all(b"f").await.expect("failed to press f");
                break;
            }
        }
    }

    std::thread::sleep(Duration::from_secs(5));

    let product_bundle_json = Emu::product_bundle_dir();

    // Flash in fastboot, then verify that target boots to product.
    let output = isolate
        .ffx(&[
            "--target",
            emu.nodename(),
            "target",
            "flash",
            "-b",
            product_bundle_json.to_str().unwrap(),
        ])
        .await
        .expect("flash target");
    assert!(output.status.success(), "Failed to run command: {}\n{}", output.stdout, output.stderr);
    std::thread::sleep(Duration::from_secs(4));

    // Retry ffx target show as it may take the device up to 30 seconds to initialize SSH.
    let mut times = 2;
    while times >= 0 {
        let out =
        isolate.ffx(&["--target", emu.nodename(), "target", "show"]).await.expect("target show");

        if times > 0 && !out.status.success() {
            times -= 1;
            continue
        }

        assert!(out.status.success(), "status is unexpected: {:?}", out);
        assert!(out.stdout.contains("Product:"), "stdout is unexpected: {:?}", out);
        assert!(out.stderr.lines().count() == 0, "stderr is unexpected: {:?}", out);
        break
    }
}
