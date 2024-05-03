// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package upgrade

import (
	"bytes"
	"context"
	"flag"
	"fmt"
	"io"
	"log"
	"math/rand"
	"os"
	"path/filepath"
	"testing"
	"time"

	"go.fuchsia.dev/fuchsia/src/sys/pkg/tests/system-tests/check"
	"go.fuchsia.dev/fuchsia/src/sys/pkg/tests/system-tests/flash"
	"go.fuchsia.dev/fuchsia/src/sys/pkg/tests/system-tests/pave"
	"go.fuchsia.dev/fuchsia/src/testing/host-target-testing/artifacts"
	"go.fuchsia.dev/fuchsia/src/testing/host-target-testing/device"
	"go.fuchsia.dev/fuchsia/src/testing/host-target-testing/errutil"
	"go.fuchsia.dev/fuchsia/src/testing/host-target-testing/ffx"
	"go.fuchsia.dev/fuchsia/src/testing/host-target-testing/packages"
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

		repo, err := build.GetPackageRepository(ctx, artifacts.LazilyFetchBlobs, ffx.IsolateDir())
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

	initialBuild := chainedBuilds[0]
	chainedBuilds = chainedBuilds[1:]

	ch := make(chan *sl4f.Configuration, 1)
	if err := util.RunWithTimeout(ctx, c.paveTimeout, func() error {
		currentBootSlot, err := initializeDevice(ctx, deviceClient, ffx, initialBuild)
		ch <- currentBootSlot
		return err
	}); err != nil {
		err = fmt.Errorf("device failed to initialize: %w", err)
		errutil.HandleError(ctx, c.deviceConfig.SerialSocketPath, err)
		return err
	}

	currentBootSlot := <-ch

	return testOTAs(ctx, deviceClient, ffx.IsolateDir(), chainedBuilds, currentBootSlot)
}

func testOTAs(
	ctx context.Context,
	device *device.Client,
	ffxIsolateDir ffx.IsolateDir,
	builds []artifacts.Build,
	currentBootSlot *sl4f.Configuration,
) error {
	for i := uint(1); i <= c.cycleCount; i++ {
		logger.Infof(ctx, "OTA Attempt %d", i)

		for index, build := range builds {
			checkPrime := false
			if index == len(builds)-1 {
				checkPrime = true
			}

			if err := util.RunWithTimeout(ctx, c.cycleTimeout, func() error {
				return doTestOTAs(ctx, device, ffxIsolateDir, build, currentBootSlot, checkPrime)
			}); err != nil {
				return fmt.Errorf("OTA Attempt %d failed: %w", i, err)
			}
		}
	}

	return nil
}

func doTestOTAs(
	ctx context.Context,
	device *device.Client,
	ffxIsolateDir ffx.IsolateDir,
	build artifacts.Build,
	currentBootSlot *sl4f.Configuration,
	checkPrime bool,
) error {
	logger.Infof(ctx, "Starting OTA test cycle. Time out in %s", c.cycleTimeout)

	startTime := time.Now()

	repo, err := build.GetPackageRepository(ctx, artifacts.PrefetchBlobs, ffxIsolateDir)
	if err != nil {
		return fmt.Errorf("error getting repository: %w", err)
	}

	updatePackage, err := repo.OpenUpdatePackage(ctx, "update/0")
	if err != nil {
		return err
	}

	// Install version N on the device if it is not already on that version.
	expectedSystemImage, err := updatePackage.OpenSystemImagePackage(ctx)
	if err != nil {
		return fmt.Errorf("error extracting expected system image merkle from %s: %w", updatePackage.Path(), err)
	}

	// Attempt to check if the device is up-to-date, up to downgradeOTAAttempts times.
	// We retry this since some downgrade builds contain bugs which make them spuriously reboot
	// See https://fxbug.dev/42061177 for more details
	var upToDate bool
	var lastError error
	for attempt := uint(1); attempt <= c.downgradeOTAAttempts; attempt++ {
		logger.Infof(ctx, "checking device version (attempt %d of %d)", attempt, c.downgradeOTAAttempts)
		upToDate, lastError = check.IsDeviceUpToDate(ctx, device, expectedSystemImage)
		if lastError == nil {
			logger.Infof(ctx, "Got device version, upToDate: %t", upToDate)
			break
		}

		if attempt == c.downgradeOTAAttempts {
			return fmt.Errorf(
				"OTA from N-1 -> N failed to check if device is up to date after %d attempts: Last error: %w",
				c.downgradeOTAAttempts,
				lastError,
			)
		} else {
			logger.Warningf(
				ctx,
				"failed to check if device up to date, trying again %d times:: %v",
				c.downgradeOTAAttempts-attempt,
				lastError,
			)
		}

		// Reset our client state since the device has _potentially_ rebooted
		device.Close()

		// We should now be using the ffx from the new build.
		ffx, err := build.GetFfx(ctx, ffxIsolateDir)
		if err != nil {
			return fmt.Errorf("failed to get ffx from build %s: %w", build, err)
		}

		newClient, err := c.deviceConfig.NewDeviceClient(ctx, ffx)
		if err != nil {
			return fmt.Errorf("failed to create ota test client: %w", err)
		}
		*device = *newClient
	}

	// Use a seeded random source so the OTA test is consistent across runs.
	rand := rand.New(rand.NewSource(99))

	if !upToDate {
		// Attempt an N-1 -> N OTA, up to downgradeOTAAttempts times.
		// We optionally retry this OTA because some downgrade builds contain bugs which make them
		// spuriously reboot. Those builds are already cut, but we still need to test them.
		// See https://fxbug.dev/42061177 for more details.
		for attempt := uint(1); attempt <= c.downgradeOTAAttempts; attempt++ {
			logger.Infof(ctx, "starting OTA from N-1 -> N test, attempt %d of %d", attempt, c.downgradeOTAAttempts)
			otaTime := time.Now()
			if lastError = systemOTA(
				ctx,
				rand,
				device,
				ffxIsolateDir,
				repo,
				currentBootSlot,
				!c.buildExpectUnknownFirmware,
			); lastError == nil {
				logger.Infof(ctx, "OTA from N-1 -> N successful in %s", time.Now().Sub(otaTime))
				break
			}

			if attempt == c.downgradeOTAAttempts {
				return fmt.Errorf(
					"OTA from N-1 -> N failed after %d attempts: Last error: %w",
					c.downgradeOTAAttempts,
					lastError,
				)
			} else {
				logger.Warningf(
					ctx,
					"OTA from N-1 -> N failed, trying again %d times: %v",
					c.downgradeOTAAttempts-attempt,
					lastError)
			}

			// Reset our client state since the device has _potentially_
			// rebooted
			device.Close()

			ffx, err := build.GetFfx(ctx, ffxIsolateDir)
			if err != nil {
				return fmt.Errorf("failed to get ffx from build %s: %w", build, err)
			}

			newClient, err := c.deviceConfig.NewDeviceClient(ctx, ffx)
			if err != nil {
				return fmt.Errorf("failed to create ota test client: %w", err)
			}
			*device = *newClient
		}
	}

	if !checkPrime {
		return nil
	}

	logger.Infof(ctx, "starting OTA N -> N' test")
	otaTime := time.Now()
	if err := systemPrimeOTA(ctx, rand, device, repo, currentBootSlot); err != nil {
		return fmt.Errorf("OTA from N -> N' failed: %w", err)
	}
	logger.Infof(ctx, "OTA from N -> N' successful in %s", time.Now().Sub(otaTime))
	logger.Infof(ctx, "OTA cycle sucessful in %s", time.Now().Sub(startTime))

	return nil
}

func initializeDevice(
	ctx context.Context,
	device *device.Client,
	ffx *ffx.FFXTool,
	build artifacts.Build,
) (*sl4f.Configuration, error) {
	logger.Infof(ctx, "Initializing device")

	startTime := time.Now()

	var repo *packages.Repository
	var err error
	var expectedSystemImage *packages.SystemImagePackage

	if build != nil {
		// We don't need to prefetch all the blobs, since we only use a subset of
		// packages from the repository, like run, sl4f.
		repo, err = build.GetPackageRepository(ctx, artifacts.LazilyFetchBlobs, ffx.IsolateDir())
		if err != nil {
			return nil, fmt.Errorf("error getting downgrade repository: %w", err)
		}

		updatePackage, err := repo.OpenUpdatePackage(ctx, "update/0")
		if err != nil {
			return nil, err
		}

		systemImage, err := updatePackage.OpenSystemImagePackage(ctx)
		if err != nil {
			return nil, fmt.Errorf("error extracting expected system image merkle from %s: %w", updatePackage.Path(), err)
		}
		expectedSystemImage = systemImage
	}

	var currentBootSlot *sl4f.Configuration

	if build != nil {
		// Only pave if the device is not running the expected version.
		upToDate, err := check.IsDeviceUpToDate(ctx, device, expectedSystemImage)
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
				if err := flash.FlashDevice(ctx, device, ffx, build, sshPrivateKey.PublicKey()); err != nil {
					return nil, fmt.Errorf("failed to flash device during initialization: %w", err)
				}
			} else {
				if err := pave.PaveDevice(ctx, device, ffx, build, sshPrivateKey.PublicKey()); err != nil {
					return nil, fmt.Errorf("failed to pave device during initialization: %w", err)
				}
			}
		}

		// We always boot into the A partition after a pave.
		config := sl4f.ConfigurationA
		currentBootSlot = &config
	} else if c.checkABR {
		config, err := check.DetermineCurrentABRConfig(ctx, device, repo)
		if err != nil {
			return nil, err
		}
		currentBootSlot = config
	}

	if err := check.ValidateDevice(
		ctx,
		device,
		expectedSystemImage,
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
	rand *rand.Rand,
	device *device.Client,
	ffxIsolateDir ffx.IsolateDir,
	repo *packages.Repository,
	currentBootSlot *sl4f.Configuration,
	checkForUnknownFirmware bool,
) error {
	updatePackage, err := repo.OpenUpdatePackage(ctx, "update/0")
	if err != nil {
		return fmt.Errorf("error opening update/0 package: %w", err)
	}

	return otaToPackage(
		ctx,
		rand,
		device,
		currentBootSlot,
		updatePackage,
		"ota-test-update/0",
		checkForUnknownFirmware,
	)
}

func systemPrimeOTA(
	ctx context.Context,
	rand *rand.Rand,
	device *device.Client,
	repo *packages.Repository,
	currentBootSlot *sl4f.Configuration,
) error {
	avbTool, err := c.installerConfig.AVBTool()
	if err != nil {
		return fmt.Errorf("failed to intialize AVBTool: %w", err)
	}

	zbiTool, err := c.installerConfig.ZBITool()
	if err != nil {
		return fmt.Errorf("failed to intialize ZBITool: %w", err)
	}

	srcUpdate, err := repo.OpenUpdatePackage(ctx, "update/0")
	if err != nil {
		return fmt.Errorf("failed to open update/0 package: %w", err)
	}

	srcSystemImage, err := srcUpdate.OpenSystemImagePackage(ctx)
	if err != nil {
		return fmt.Errorf(
			"failed to open system_image/0 from %s update package: %w",
			srcUpdate.Path(),
			err,
		)
	}

	dstSystemImagePath := "system_image_prime/0"
	dstSystemImage, err := srcSystemImage.EditContents(
		ctx,
		dstSystemImagePath,
		func(tempDir string) error {
			newResource := "Hello World!"
			contents := bytes.NewReader([]byte(newResource))
			data, err := io.ReadAll((contents))
			if err != nil {
				return fmt.Errorf("failed to read new content %q: %w", srcSystemImage.Path(), err)
			}

			tempPath := filepath.Join(tempDir, "dummy2.txt")
			if err := os.MkdirAll(filepath.Dir(tempPath), os.ModePerm); err != nil {
				return fmt.Errorf("failed to create parent directories for %q: %w", tempPath, err)
			}

			if err := os.WriteFile(tempPath, data, 0600); err != nil {
				return fmt.Errorf(
					"failed to write new data for %q to %q: %w",
					dstSystemImagePath,
					tempPath,
					err,
				)
			}

			return nil
		},
	)
	if err != nil {
		return fmt.Errorf("failed to create the %q package: %w", dstSystemImage.Path(), err)
	}

	dstUpdatePath := "ota-test-update_prime/0"
	dstUpdate, err := srcUpdate.EditUpdatePackageWithNewSystemImage(
		ctx,
		avbTool,
		zbiTool,
		"fuchsia.com",
		dstSystemImage,
		dstUpdatePath,
		c.bootfsCompression,
	)
	if err != nil {
		return fmt.Errorf("failed to create the %q package: %w", dstUpdatePath, err)
	}

	return otaToPackage(
		ctx,
		rand,
		device,
		currentBootSlot,
		dstUpdate,
		"ota-test-update_prime2/0",
		true,
	)
}

func otaToPackage(
	ctx context.Context,
	rand *rand.Rand,
	device *device.Client,
	currentBootSlot *sl4f.Configuration,
	srcUpdate *packages.UpdatePackage,
	dstUpdatePath string,
	checkForUnknownFirmware bool,
) error {
	dstUpdate, dstSystemImage, err := AddRandomFilesToUpdate(
		ctx,
		rand,
		srcUpdate,
		dstUpdatePath,
	)
	if err != nil {
		return fmt.Errorf("failed to create update package %s: %w", dstUpdatePath, err)
	}

	upToDate, err := check.IsDeviceUpToDate(ctx, device, dstSystemImage)
	if err != nil {
		return fmt.Errorf("failed to check if device is up to date: %w", err)
	}
	if upToDate {
		return fmt.Errorf(
			"device already updated to the expected version %q",
			dstSystemImage.Merkle(),
		)
	}

	u, err := c.installerConfig.Updater(checkForUnknownFirmware)
	if err != nil {
		return fmt.Errorf("failed to create updater: %w", err)
	}

	if err := u.Update(ctx, device, dstUpdate); err != nil {
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
		dstSystemImage,
		currentBootSlot,
		c.checkABR,
	); err != nil {
		return fmt.Errorf("failed to validate after OTA: %w", err)
	}

	return nil
}

// AddRandomFilesToUpdate creates a new update package with a system image that
// contains a number of extra files filled with random bytes, which should be
// incompressible. It will loop until it has created an update package that is
// smaller than `-max-ota-size`.
func AddRandomFilesToUpdate(
	ctx context.Context,
	rand *rand.Rand,
	srcUpdate *packages.UpdatePackage,
	dstUpdatePath string,
) (*packages.UpdatePackage, *packages.SystemImagePackage, error) {
	avbTool, err := c.installerConfig.AVBTool()
	if err != nil {
		return nil, nil, fmt.Errorf("failed to intialize AVBTool: %w", err)
	}

	zbiTool, err := c.installerConfig.ZBITool()
	if err != nil {
		return nil, nil, fmt.Errorf("failed to initialize ZBITool: %w", err)
	}

	srcSystemImage, err := srcUpdate.OpenSystemImagePackage(ctx)
	if err != nil {
		return nil, nil, err
	}

	systemImageSize, err := srcSystemImage.SystemImageAlignedBlobSize(ctx)
	if err != nil {
		return nil, nil, fmt.Errorf("error determining system image size: %w", err)
	}

	dstSystemImagePath := util.AddSuffixToPackageName(dstUpdatePath, "system-image")

	// Add random files to the system image package in the update. Clamp the
	// package size to the upper bound if we have one, otherwise we'll just add
	// a single block to make it unique.
	dstUpdate, dstSystemImage, err := srcUpdate.EditSystemImagePackage(
		ctx,
		avbTool,
		zbiTool,
		"fuchsia.com",
		dstUpdatePath,
		c.bootfsCompression,
		func(systemImage *packages.SystemImagePackage) (*packages.SystemImagePackage, error) {
			if c.maxSystemImageSize == 0 {
				return systemImage.AddRandomFilesWithAdditionalBytes(
					ctx,
					rand,
					dstSystemImagePath,
					packages.BlobBlockSize,
				)
			} else if c.maxSystemImageSize < systemImageSize {
				return nil, fmt.Errorf(
					"max system image size %d is smaller than the size of the system image %d",
					c.maxSystemImageSize,
					systemImageSize,
				)
			} else {
				return systemImage.AddRandomFilesWithUpperBound(
					ctx,
					rand,
					dstSystemImagePath,
					c.maxSystemImageSize,
				)
			}
		},
	)
	if err != nil {
		return nil, nil, fmt.Errorf(
			"failed to add random files to system images %q in update package %q: %w",
			dstSystemImagePath,
			dstUpdatePath,
			err,
		)
	}

	// Optionally add random files to zbi package in the update images.
	if c.maxUpdateImagesSize != 0 {
		dstZbiPath := util.AddSuffixToPackageName(dstUpdatePath, "update-images-zbi")
		dstUpdate, _, err = dstUpdate.EditUpdateImages(
			ctx,
			dstUpdatePath,
			func(updateImages *packages.UpdateImages) (*packages.UpdateImages, error) {
				return updateImages.AddRandomFilesWithUpperBound(
					ctx,
					rand,
					dstZbiPath,
					c.maxUpdateImagesSize,
				)
			},
		)
		if err != nil {
			return nil, nil, fmt.Errorf(
				"failed to add random files to zbi package %q in update package %q: %w",
				dstZbiPath,
				dstUpdatePath,
				err,
			)
		}
	}

	// Optionally add random files to the update package.
	if c.maxUpdatePackageSize != 0 {
		dstUpdate, err = dstUpdate.EditPackage(
			ctx,
			func(p packages.Package) (packages.Package, error) {
				return p.AddRandomFilesWithUpperBound(
					ctx,
					rand,
					dstUpdatePath,
					c.maxUpdatePackageSize,
				)
			},
		)
		if err != nil {
			return nil, nil, fmt.Errorf(
				"failed to add random files to update package %q: %w",
				dstUpdatePath,
				err,
			)
		}
	}

	return dstUpdate, dstSystemImage, nil
}
