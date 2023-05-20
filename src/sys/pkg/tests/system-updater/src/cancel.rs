// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    super::*, assert_matches::assert_matches, fidl_fuchsia_update_installer_ext::State,
    pretty_assertions::assert_eq,
};

#[fasync::run_singlethreaded(test)]
async fn cancel_update() {
    let env = TestEnv::builder().build().await;

    let mut handle_update_pkg = env.resolver.url(UPDATE_PKG_URL).block_once();

    let mut attempt = env.start_update().await.unwrap();
    assert_eq!(attempt.next().await.unwrap().unwrap(), State::Prepare);
    handle_update_pkg.wait().await;

    env.installer_proxy().cancel_update(None).await.unwrap().unwrap();
    assert_eq!(attempt.next().await.unwrap().unwrap(), State::Canceled);
    assert_matches!(attempt.next().await, None);

    assert_eq!(
        env.get_ota_metrics().await,
        OtaMetrics {
            initiator:
                metrics::OtaResultAttemptsMigratedMetricDimensionInitiator::UserInitiatedCheck
                    as u32,
            phase: metrics::OtaResultAttemptsMigratedMetricDimensionPhase::Tufupdate as u32,
            status_code: metrics::OtaResultAttemptsMigratedMetricDimensionStatusCode::Canceled
                as u32,
        }
    );
}
