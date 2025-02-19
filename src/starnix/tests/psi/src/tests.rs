// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl_fuchsia_starnix_psi::{
    PsiProviderGetMemoryPressureStatsResponse, PsiProviderRequest, PsiStats,
};
use std::sync::Arc;
use test_case::test_case;

mod fake_psi_provider;
use fake_psi_provider::*;
mod puppet;
use puppet::*;

async fn start_puppet_with_fake_psi_provider() -> (PuppetInstance, Arc<FakePsiProvider>) {
    // Start Starnix with our fake PsiProvider and expect its initial probe.
    let fake_psi_provider = Arc::new(FakePsiProvider::new());
    let puppet = fake_psi_provider
        .clone()
        .with_expected_request(
            |request| {
                // Before the puppet starts to run, the Starnix kernel will make an initial request
                // to probe if the PsiProvider is actually connected. As it doesn't use the returned
                // data, we can just reply with zeros.
                let PsiProviderRequest::GetMemoryPressureStats { responder } = request else {
                    panic!("Unexpected request received")
                };
                let zeros = PsiStats {
                    avg10: Some(0.0),
                    avg60: Some(0.0),
                    avg300: Some(0.0),
                    total: Some(0),
                    ..Default::default()
                };
                let response = PsiProviderGetMemoryPressureStatsResponse {
                    some: Some(zeros.clone()),
                    full: Some(zeros.clone()),
                    ..Default::default()
                };
                responder.send(Ok(&response)).unwrap();
            },
            async { PuppetInstance::new(Some(fake_psi_provider.clone())).await },
        )
        .await;

    (puppet, fake_psi_provider)
}

#[fuchsia::test]
async fn test_read_psi_memory_stats() {
    let (mut puppet, fake_psi_provider) = start_puppet_with_fake_psi_provider().await;

    fake_psi_provider
        .with_expected_request(
            |request| {
                let PsiProviderRequest::GetMemoryPressureStats { responder } = request else {
                    panic!("Unexpected request received")
                };
                let some = PsiStats {
                    avg10: Some(0.08),
                    avg60: Some(0.9),
                    avg300: Some(1.0),
                    total: Some(5678 * 1000),
                    ..Default::default()
                };
                let full = PsiStats {
                    avg10: Some(0.5),
                    avg60: Some(0.6),
                    avg300: Some(0.77),
                    total: Some(1234 * 1000),
                    ..Default::default()
                };
                let response = PsiProviderGetMemoryPressureStatsResponse {
                    some: Some(some),
                    full: Some(full),
                    ..Default::default()
                };
                responder.send(Ok(&response)).unwrap();
            },
            async {
                let fd = puppet.open("/proc/pressure/memory").await;
                let contents = puppet.read_to_end(fd).await;
                assert_eq!(
                    contents,
                    "some avg10=0.08 avg60=0.90 avg300=1.00 total=5678\n\
                     full avg10=0.50 avg60=0.60 avg300=0.77 total=1234\n"
                );
                puppet.close(fd).await;
            },
        )
        .await;

    puppet.check_exit_clean().await;
}

// Test files that are just stubs too.
#[test_case("cpu")]
#[test_case("io")]
#[fuchsia::test]
async fn test_read_psi_stub_stats(kind: &str) {
    let (mut puppet, _fake_psi_provider) = start_puppet_with_fake_psi_provider().await;

    let fd = puppet.open(&format!("/proc/pressure/{kind}")).await;
    let contents = puppet.read_to_end(fd).await;
    assert_eq!(
        contents,
        "some avg10=0.00 avg60=0.00 avg300=0.00 total=0\n\
         full avg10=0.00 avg60=0.00 avg300=0.00 total=0\n"
    );
    puppet.close(fd).await;

    puppet.check_exit_clean().await;
}

// Verify that the pressure directory is not created if no PsiProvider is given.
#[fuchsia::test]
async fn test_psi_unavailable() {
    // Start Starnix without a PsiProvider (which is optional in its manifest).
    let mut puppet = PuppetInstance::new(None).await;

    assert!(puppet.check_exists("/proc/pressure").await == false);

    puppet.check_exit_clean().await;
}
