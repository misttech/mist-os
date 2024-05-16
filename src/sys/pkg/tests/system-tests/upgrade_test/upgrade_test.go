// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package upgrade

import (
	"context"
	"flag"
	"fmt"
	"log"
	"math/rand"
	"os"
	"testing"
	"time"

	"go.fuchsia.dev/fuchsia/src/sys/pkg/tests/system-tests/check"
	"go.fuchsia.dev/fuchsia/src/sys/pkg/tests/system-tests/flash"
	"go.fuchsia.dev/fuchsia/src/sys/pkg/tests/system-tests/pave"
	"go.fuchsia.dev/fuchsia/src/testing/host-target-testing/artifacts"
	"go.fuchsia.dev/fuchsia/src/testing/host-target-testing/device"
	"go.fuchsia.dev/fuchsia/src/testing/host-target-testing/errutil"
	"go.fuchsia.dev/fuchsia/src/testing/host-target-testing/ffx"
	"go.fuchsia.dev/fuchsia/src/testing/host-target-testing/sl4f"
	"go.fuchsia.dev/fuchsia/src/testing/host-target-testing/util"
	"go.fuchsia.dev/fuchsia/tools/lib/color"
	"go.fuchsia.dev/fuchsia/tools/lib/logger"
)

var c *config

func TestMain(m *testing.M) {
	log.SetPrefix("upgrade-test: ")
	log.SetFlags(log.Ldate | log.Ltime | log.LUTC | log.Lshortfile)

	var err error
	c, err = newConfig(flag.CommandLine)
	if err != nil {
		log.Fatalf("failed to create config: %s", err)
	}

	flag.Parse()

	if err = c.validate(); err != nil {
		log.Fatalf("config is invalid: %s", err)
	}

	os.Exit(m.Run())
}

func TestOTA(t *testing.T) {
	ctx := context.Background()
	l := logger.NewLogger(
		logger.TraceLevel,
		color.NewColor(color.ColorAuto),
		os.Stdout,
		os.Stderr,
		"upgrade-test: ")
	l.SetFlags(logger.Ldate | logger.Ltime | logger.LUTC | logger.Lshortfile)
	ctx = logger.WithLogger(ctx, l)

	if err := doTest(ctx); err != nil {
		logger.Errorf(ctx, "test failed: %v", err)
		errutil.HandleError(ctx, c.deviceConfig.SerialSocketPath, err)
		t.Fatal(err)
	}
}

func doTest(ctx context.Context) error {
	defer c.installerConfig.Shutdown(ctx)

	outputDir, outputCleanup, err := c.archiveConfig.OutputDir()
	if err != nil {
		return fmt.Errorf("failed to get output directory: %w", err)
	}
	defer outputCleanup()

	ffx, ffxCleanup, err := c.ffxConfig.NewFfxTool(ctx)
	if err != nil {
		return fmt.Errorf("failed to create ffx: %w", err)
	}
	defer ffxCleanup()

	deviceClient, err := c.deviceConfig.NewDeviceClient(ctx, ffx)
	if err != nil {
		return fmt.Errorf("failed to create ota test client: %w", err)
	}
	defer deviceClient.Close()

	// Now that we're connected to the device we can emit logs with the
	// estimated device monotonic time.
	l := logger.NewLogger(
		logger.TraceLevel,
		color.NewColor(color.ColorAuto),
		os.Stdout,
		os.Stderr,
		device.NewEstimatedMonotonicTime(deviceClient, "upgrade-test: "),
	)
	l.SetFlags(logger.Ldate | logger.Ltime | logger.LUTC | logger.Lshortfile)
	ctx = logger.WithLogger(ctx, l)

	chainedBuilds, err := c.chainedBuildConfig.GetBuilds(ctx, deviceClient, outputDir)
	if err != nil {
		return fmt.Errorf("failed to get builds: %w", err)
	}

	for i, build := range chainedBuilds {
		// FIXME(https://fxbug.dev/336897946): We need to use the latest ffx because
		// F11's ffx doesn't actually refresh metadata. We can remove this once
		// we cut the next stepping stone.
		logger.Infof(ctx, "Refreshing TUF metadata in build %s with latest ffx", build)

		repo, err := build.GetPackageRepository(ctx, artifacts.PrefetchBlobs, ffx.IsolateDir())
		if err != nil {
			return fmt.Errorf("error getting repository: %w", err)
		}

		if err := repo.RefreshMetadataWithFfx(ctx, ffx); err != nil {
			return fmt.Errorf("failed to refresh TUF metadata latest ffx: %w", err)
		}

		// Adapt the build for the installer.
		build, err = c.installerConfig.ConfigureBuild(ctx, deviceClient, build)
		if err != nil {
			return fmt.Errorf("failed to configure build for device: %w", err)
		}

		if build == nil {
			return fmt.Errorf("installer did not configure a build")
		}

		chainedBuilds[i] = build
	}

	if len(chainedBuilds) == 0 {
		return nil
	}

	// Use a seeded random source so the OTA test is consistent across runs.
	rand := rand.New(rand.NewSource(99))

	// Generate OTAs for each build.
	otas, err := newOtas(ctx, rand, ffx, chainedBuilds)
	if err != nil {
		return err
	}

	ch := make(chan *sl4f.Configuration, 1)
	if err := util.RunWithTimeout(ctx, c.paveTimeout, func() error {
		currentBootSlot, err := initializeDevice(ctx, deviceClient, ffx, otas[0])
		ch <- currentBootSlot
		return err
	}); err != nil {
		err = fmt.Errorf("device failed to initialize: %w", err)
		errutil.HandleError(ctx, c.deviceConfig.SerialSocketPath, err)
		return err
	}

	currentBootSlot := <-ch

	return testOTAs(ctx, deviceClient, ffx.IsolateDir(), otas, currentBootSlot)
}

func testOTAs(
	ctx context.Context,
	device *device.Client,
	ffxIsolateDir ffx.IsolateDir,
	otas []*otaData,
	currentBootSlot *sl4f.Configuration,
) error {
	// Perform the OTA cycles.
	for i := uint(1); i <= c.cycleCount; i++ {
		logger.Infof(ctx, "OTA Attempt Cycle %d. Time out in %s", i, c.cycleTimeout)

		startTime := time.Now()

		if err := util.RunWithTimeout(ctx, c.cycleTimeout, func() error {
			// Actually OTA through all the builds.
			for i := 1; i < len(otas); i++ {
				srcOta := otas[i-1]
				dstOta := otas[i]

				logger.Infof(ctx, "Starting OTA Attempt from %s -> %s", srcOta, dstOta)

				if err := systemOTA(
					ctx,
					device,
					ffxIsolateDir,
					srcOta,
					dstOta,
					currentBootSlot,
				); err != nil {
					return err
				}
			}

			return nil
		}); err != nil {
			return fmt.Errorf("OTA Attempt %d failed: %w", i, err)
		}

		logger.Infof(ctx, "OTA cycle %d sucessful in %s", i, time.Now().Sub(startTime))
	}

	return nil
}

func initializeDevice(
	ctx context.Context,
	device *device.Client,
	ffx *ffx.FFXTool,
	ota *otaData,
) (*sl4f.Configuration, error) {
	logger.Infof(ctx, "Initializing device")

	startTime := time.Now()

	systemImage, err := ota.updatePackage.OpenSystemImagePackage(ctx)
	if err != nil {
		return nil, err
	}

	// Only pave if the device is not running the expected version.
	upToDate, err := check.IsDeviceUpToDate(ctx, device, systemImage)
	if err != nil {
		return nil, fmt.Errorf("failed to check if up to date during initialization: %w", err)
	}

	if !c.installerConfig.NeedsInitialization() && upToDate {
		logger.Infof(ctx, "device already up to date")
	} else {
		sshPrivateKey, err := c.deviceConfig.SSHPrivateKey()
		if err != nil {
			return nil, fmt.Errorf("failed to get ssh key: %w", err)
		}

		if c.useFlash {
			if err := flash.FlashDevice(ctx, device, ffx, ota.build, sshPrivateKey.PublicKey()); err != nil {
				return nil, fmt.Errorf("failed to flash device during initialization: %w", err)
			}
		} else {
			if err := pave.PaveDevice(ctx, device, ffx, ota.build, sshPrivateKey.PublicKey()); err != nil {
				return nil, fmt.Errorf("failed to pave device during initialization: %w", err)
			}
		}
	}

	// We always boot into the A partition after initialization.
	config := sl4f.ConfigurationA
	currentBootSlot := &config

	if err := check.ValidateDevice(
		ctx,
		device,
		systemImage,
		currentBootSlot,
		c.checkABR,
	); err != nil {
		return nil, fmt.Errorf("failed to validate during initialization: %w", err)
	}

	logger.Infof(ctx, "initialization successful in %s", time.Now().Sub(startTime))

	return currentBootSlot, nil
}

func systemOTA(
	ctx context.Context,
	device *device.Client,
	ffxIsolateDir ffx.IsolateDir,
	srcOta *otaData,
	dstOta *otaData,
	currentBootSlot *sl4f.Configuration,
) error {
	var err error

	// Attempt an N-1 -> N OTA, up to downgradeOTAAttempts times.
	// We optionally retry this OTA because some downgrade builds contain bugs which make them
	// spuriously reboot. Those builds are already cut, but we still need to test them.
	// See https://fxbug.dev/42061177 for more details.
	for attempt := uint(1); attempt <= c.downgradeOTAAttempts; attempt++ {
		logger.Infof(
			ctx,
			"starting OTA from %s -> %s test, attempt %d of %d",
			srcOta,
			dstOta,
			attempt,
			c.downgradeOTAAttempts,
		)

		otaTime := time.Now()

		if err = otaToPackage(
			ctx,
			device,
			dstOta,
			currentBootSlot,
			!c.buildExpectUnknownFirmware,
		); err == nil {
			logger.Infof(
				ctx,
				"OTA from %s -> %s successful in %s",
				srcOta,
				dstOta,
				time.Now().Sub(otaTime),
			)
			return nil
		}

		logger.Warningf(
			ctx,
			"OTA from %s -> %s failed, trying again %d times: %v",
			srcOta,
			dstOta,
			c.downgradeOTAAttempts-attempt,
			err,
		)

		// Reset our client state since the device has _potentially_
		// rebooted
		device.Close()

		// We should now be using the ffx from the new build.
		ffx, err := dstOta.build.GetFfx(ctx, ffxIsolateDir)
		if err != nil {
			return fmt.Errorf("failed to get ffx from build %s: %w", dstOta, err)
		}

		newClient, err := c.deviceConfig.NewDeviceClient(ctx, ffx)
		if err != nil {
			return fmt.Errorf("failed to create ota test client: %w", err)
		}
		*device = *newClient
	}

	return fmt.Errorf(
		"OTA from %s -> %s failed after %d attempts: Last error: %w",
		srcOta,
		dstOta,
		c.downgradeOTAAttempts,
		err,
	)
}

func otaToPackage(
	ctx context.Context,
	device *device.Client,
	ota *otaData,
	currentBootSlot *sl4f.Configuration,
	checkForUnknownFirmware bool,
) error {
	u, err := c.installerConfig.Updater(checkForUnknownFirmware)
	if err != nil {
		return fmt.Errorf("failed to create updater: %w", err)
	}

	systemImage, err := ota.updatePackage.OpenSystemImagePackage(ctx)
	if err != nil {
		return err
	}

	if err := u.Update(ctx, device, ota.updatePackage); err != nil {
		return fmt.Errorf("failed to download OTA: %w", err)
	}

	logger.Infof(ctx, "Validating device")

	if currentBootSlot != nil {
		switch *currentBootSlot {
		case sl4f.ConfigurationA:
			*currentBootSlot = sl4f.ConfigurationB
		case sl4f.ConfigurationB:
			*currentBootSlot = sl4f.ConfigurationA
		case sl4f.ConfigurationRecovery:
			return fmt.Errorf("device should not be in ABR recovery")
		}
	}

	if err := check.ValidateDevice(
		ctx,
		device,
		systemImage,
		currentBootSlot,
		c.checkABR,
	); err != nil {
		return fmt.Errorf("failed to validate after OTA: %w", err)
	}

	return nil
}
