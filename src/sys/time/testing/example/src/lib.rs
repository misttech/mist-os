// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Context;
use fidl::{endpoints, HandleBased};
use fidl_fuchsia_testing_harness as ftth;
use fidl_fuchsia_time as fft;
use fidl_fuchsia_time_external::TimeSample;
use fidl_test_time_realm as fttr;
use fuchsia_async as fasync;
use fuchsia_component::client;
use fuchsia_zircon as zx;
use lazy_static::lazy_static;

lazy_static! {
    // A sample backstop time.
    static ref BACKSTOP_TIME: zx::Time = from_rfc2822("Sun, 20 Sep 2020 01:01:01 GMT");

    // A sample valid time.  It is strictly after backstop.
    static ref VALID_TIME: zx::Time = from_rfc2822("Tue, 29 Sep 2020 02:19:01 GMT");
}

fn from_rfc2822(date: &str) -> zx::Time {
    zx::Time::from_nanos(
        chrono::DateTime::parse_from_rfc2822(date).unwrap().timestamp_nanos_opt().unwrap(),
    )
}

// An annotated example test that sets up the timekeeper test realm, and starts up the UTC clock.
#[fuchsia::test]
async fn test_example() {
    // Connecting to this proxy gets us to all services that are started
    // by the Timekeeper Test Realm (TTR) Factory (TTRF).
    let ttr_proxy = client::connect_to_protocol::<fttr::RealmFactoryMarker>()
        .with_context(|| {
            format!(
                "while connecting to: {}",
                <fttr::RealmFactoryMarker as fidl::endpoints::ProtocolMarker>::DEBUG_NAME
            )
        })
        .expect("should be able to connect to the realm factory");

    // Prepare the objects needed to initialize the timekeeper test realm.

    // This is the UTC clock object that timekeeper will manage.
    //
    // We give it an arbitrary backstop time. On real systems the backstop
    // time is generated based on the timestamp of the last change that
    // made it into the release.
    let utc_clock = zx::Clock::create(zx::ClockOpts::empty(), Some(*BACKSTOP_TIME))
        .expect("zx calls should not fail");

    // RealmProxy endpoint is useful for connecting to any protocols in the test realm, by name.
    //
    // _rp_keepalive needs to be held to ensure that the realm continues to live.
    // We can also use this client end to request any protocol that is available in TTRF by its
    // name. In this test case, however, we don't do any of that.
    let (_rp_keepalive, rp_server_end) = endpoints::create_endpoints::<ftth::RealmProxy_Marker>();

    // Ignore the bits of the return values that we don't care about.
    let (push_source_puppet_client_end, _ignore_opts, _ignore_cobalt) = ttr_proxy
        .create_realm(
            // Use a real monotonic clock, this will become important later in the code.
            fttr::RealmOptions { use_real_monotonic_clock: Some(true), ..Default::default() },
            // We must duplicate the clock object if we want to pass it into the test realm, and
            // also keep a reference for this test.
            utc_clock.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("duplicated"),
            rp_server_end,
        )
        .await
        .expect("FIDL protocol error")
        .expect("Error value returned from the call");

    // Sampling the monotonic clock directly is OK since we configured the timekeeper test realm
    // to use the real monotonic clock. See `fttr::RealmOptions` above.
    let sample_monotonic = zx::Time::get_monotonic();

    // Convert to a proxy so we can send RPCs.
    let push_source_puppet = push_source_puppet_client_end.into_proxy().expect("infallible");

    // Let's tell Timekeeper to set a UTC time sample.
    //
    // We do this by establishing a correspondence between a reading of the monotonic clock, and
    // the reading of the UTC clock, then also providing a standard deviation of the estimation
    // error.
    //
    // The "push source puppet" is an endpoint that allows us to inject "fake" readings of
    // the time source.  When we inject the time sample as shown below, Timekeeper will see
    // that as if the time source provided a time sample, and will adjust all clocks
    // accordingly.
    const STD_DEV: zx::Duration = zx::Duration::from_millis(50);
    push_source_puppet
        .set_sample(&TimeSample {
            utc: Some(VALID_TIME.into_nanos()),
            monotonic: Some(sample_monotonic.into_nanos()),
            standard_deviation: Some(STD_DEV.into_nanos()),
            ..Default::default()
        })
        .await
        .expect("FIDL call succeeds");

    // Wait until the UTC clock is started. This is the canonical way to do that.
    // The signal below is a direct consequence of the `set_sample` call above.
    // The above call is synchronized because it goes to a separate process, so
    // we only need to write all of this in a sequential manner.
    fasync::OnSignals::new(
        &utc_clock,
        zx::Signals::from_bits(fft::SIGNAL_UTC_CLOCK_LOGGING_QUALITY).unwrap(),
    )
    .await
    .expect("wait on signal is a success");
}
