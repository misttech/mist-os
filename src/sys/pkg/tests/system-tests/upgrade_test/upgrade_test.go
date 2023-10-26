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
	"go.fuchsia.dev/fuchsia/src/sys/pkg/tests/system-tests/script"
	"go.fuchsia.dev/fuchsia/src/testing/host-target-testing/artifacts"
	"go.fuchsia.dev/fuchsia/src/testing/host-target-testing/device"
	"go.fuchsia.dev/fuchsia/src/testing/host-target-testing/errutil"
	"go.fuchsia.dev/fuchsia/src/testing/host-target-testing/packages"
	"go.fuchsia.dev/fuchsia/src/testing/host-target-testing/sl4f"
	"go.fuchsia.dev/fuchsia/src/testing/host-target-testing/util"
	"go.fuchsia.dev/fuchsia/tools/lib/color"
	"go.fuchsia.dev/fuchsia/tools/lib/logger"
)

const (
	blobBlockSize = 4096
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

	defer c.installerConfig.Shutdown(ctx)

	deviceClient, err := c.deviceConfig.NewDeviceClient(ctx)
	if err != nil {
		t.Fatalf("failed to create ota test client: %v", err)
	}
	defer deviceClient.Close()

	if err := doTest(ctx, deviceClient); err != nil {
		logger.Errorf(ctx, "test failed: %v", err)
		errutil.HandleError(ctx, c.deviceConfig.SerialSocketPath, err)
		t.Fatal(err)
	}
}

func doTest(ctx context.Context, deviceClient *device.Client) error {
	outputDir, cleanup, err := c.archiveConfig.OutputDir()
	if err != nil {
		return fmt.Errorf("failed to get output directory: %w", err)
	}
	defer cleanup()

	l := logger.NewLogger(
		logger.TraceLevel,
		color.NewColor(color.ColorAuto),
		os.Stdout,
		os.Stderr,
		device.NewEstimatedMonotonicTime(deviceClient, "upgrade-test: "),
	)
	l.SetFlags(logger.Ldate | logger.Ltime | logger.LUTC | logger.Lshortfile)
	ctx = logger.WithLogger(ctx, l)

	// Adapt the builds for the device.
	chainedBuilds, err := c.chainedBuildConfig.GetBuilds(ctx, deviceClient, outputDir)
	if err != nil {
		return fmt.Errorf("failed to get builds: %w", err)
	}

	for i, build := range chainedBuilds {
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

	ch := make(chan *sl4f.Client, 1)
	if err := util.RunWithTimeout(ctx, c.paveTimeout, func() error {
		rpcClient, err := initializeDevice(ctx, deviceClient, initialBuild)
		ch <- rpcClient
		return err
	}); err != nil {
		err = fmt.Errorf("device failed to initialize: %w", err)
		errutil.HandleError(ctx, c.deviceConfig.SerialSocketPath, err)
		return err
	}

	rpcClient := <-ch
	defer func() {
		if rpcClient != nil {
			rpcClient.Close()
		}
	}()

	return testOTAs(ctx, deviceClient, chainedBuilds, &rpcClient)
}

func testOTAs(
	ctx context.Context,
	device *device.Client,
	builds []artifacts.Build,
	rpcClient **sl4f.Client,
) error {
	for i := uint(1); i <= c.cycleCount; i++ {
		logger.Infof(ctx, "OTA Attempt %d", i)

		for index, build := range builds {
			checkPrime := false
			if index == len(builds)-1 {
				checkPrime = true
			}

			if err := util.RunWithTimeout(ctx, c.cycleTimeout, func() error {
				return doTestOTAs(ctx, device, build, rpcClient, checkPrime)
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
	build artifacts.Build,
	rpcClient **sl4f.Client,
	checkPrime bool,
) error {
	logger.Infof(ctx, "Starting OTA test cycle. Time out in %s", c.cycleTimeout)

	startTime := time.Now()

	ffx, err := c.deviceConfig.FFXTool()
	if err != nil {
		return fmt.Errorf("error getting FFXTool: %w", err)
	}
	repo, err := build.GetPackageRepository(ctx, artifacts.PrefetchBlobs, ffx)
	if err != nil {
		return fmt.Errorf("error getting repository: %w", err)
	}

	updatePackage, err := repo.OpenUpdatePackage(ctx, "update/0")
	if err != nil {
		return err
	}

	// Install version N on the device if it is not already on that version.
	expectedSystemImage, err := updatePackage.OpenPackage(ctx, "system_image/0")
	if err != nil {
		return fmt.Errorf("error extracting expected system image merkle: %w", err)
	}

	// Attempt to check if the device is up-to-date, up to downgradeOTAAttempts times.
	// We retry this since some downgrade builds contain bugs which make them spuriously reboot
	// See fxbug.dev/109811 for more details
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
		newClient, err := c.deviceConfig.NewDeviceClient(ctx)
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
		// See fxbug.dev/109811 for more details.
		for attempt := uint(1); attempt <= c.downgradeOTAAttempts; attempt++ {
			logger.Infof(ctx, "starting OTA from N-1 -> N test, attempt %d of %d", attempt, c.downgradeOTAAttempts)
			otaTime := time.Now()
			if lastError = systemOTA(
				ctx,
				rand,
				device,
				rpcClient,
				repo,
				true,
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

			// Reset our client state since the device has _potentially_ rebooted
			device.Close()
			newClient, err := c.deviceConfig.NewDeviceClient(ctx)
			if err != nil {
				return fmt.Errorf("failed to create ota test client: %w", err)
			}
			*device = *newClient

			if rpcClient != nil && *rpcClient != nil {
				(*rpcClient).Close()
				*rpcClient = nil
			}

			*rpcClient, err = device.StartRpcSession(ctx, repo)
			if err != nil {
				return fmt.Errorf("unable to connect to sl4f while retrying OTA: %w", err)
			}
		}
	}

	if !checkPrime {
		return nil
	}

	logger.Infof(ctx, "starting OTA N -> N' test")
	otaTime := time.Now()
	if err := systemPrimeOTA(ctx, rand, device, rpcClient, repo, false); err != nil {
		return fmt.Errorf("OTA from N -> N' failed: %w", err)
	}
	logger.Infof(ctx, "OTA from N -> N' successful in %s", time.Now().Sub(otaTime))
	logger.Infof(ctx, "OTA cycle sucessful in %s", time.Now().Sub(startTime))

	return nil
}

func initializeDevice(
	ctx context.Context,
	device *device.Client,
	build artifacts.Build,
) (*sl4f.Client, error) {
	logger.Infof(ctx, "Initializing device")

	startTime := time.Now()

	var repo *packages.Repository
	var expectedSystemImage *packages.Package
	var err error

	if build != nil {
		ffx, err := c.deviceConfig.FFXTool()
		if err != nil {
			return nil, fmt.Errorf("error getting FFXTool: %w", err)
		}
		// We don't need to prefetch all the blobs, since we only use a subset of
		// packages from the repository, like run, sl4f.
		repo, err = build.GetPackageRepository(ctx, artifacts.LazilyFetchBlobs, ffx)
		if err != nil {
			return nil, fmt.Errorf("error getting downgrade repository: %w", err)
		}

		updatePackage, err := repo.OpenUpdatePackage(ctx, "update/0")
		if err != nil {
			return nil, err
		}

		systemImage, err := updatePackage.OpenPackage(ctx, "system_image/0")
		if err != nil {
			return nil, fmt.Errorf("error extracting expected system image merkle: %w", err)
		}
		expectedSystemImage = &systemImage
	}

	if err := script.RunScript(ctx, device, repo, nil, c.beforeInitScript); err != nil {
		return nil, fmt.Errorf("failed to run before-init-script: %w", err)
	}

	if build != nil {
		// Only pave if the device is not running the expected version.
		upToDate, err := check.IsDeviceUpToDate(ctx, device, *expectedSystemImage)
		if err != nil {
			return nil, fmt.Errorf("failed to check if up to date during initialization: %w", err)
		}
		if upToDate {
			logger.Infof(ctx, "device already up to date")
		} else {
			if c.useFlash {
				if err := flash.FlashDevice(ctx, device, build); err != nil {
					return nil, fmt.Errorf("failed to flash device during initialization: %w", err)
				}
			} else {
				if err := pave.PaveDevice(ctx, device, build); err != nil {
					return nil, fmt.Errorf("failed to pave device during initialization: %w", err)
				}
			}
		}
	}

	// Creating a sl4f.Client requires knowing the build currently running
	// on the device, which not all test cases know during start. Store the
	// one true client here, and pass around pointers to the various
	// functions that may use it or device.Client to interact with the
	// target. All OTA attempts must first Close and nil out an existing
	// rpcClient and replace it with a new one after reboot. The final
	// rpcClient, if present, will be closed by the defer here.
	var rpcClient *sl4f.Client
	var expectedConfig *sl4f.Configuration
	if build != nil {
		rpcClient, err = device.StartRpcSession(ctx, repo)
		if err != nil {
			return nil, fmt.Errorf("unable to connect to sl4f after pave: %w", err)
		}

		// We always boot into the A partition after a pave.
		config := sl4f.ConfigurationA
		expectedConfig = &config
	}

	if err := check.ValidateDevice(
		ctx,
		device,
		rpcClient,
		expectedSystemImage,
		expectedConfig,
		true,
	); err != nil {
		if rpcClient != nil {
			rpcClient.Close()
		}
		return nil, fmt.Errorf("failed to validate during initialization: %w", err)
	}

	if err := script.RunScript(ctx, device, repo, &rpcClient, c.afterInitScript); err != nil {
		return nil, fmt.Errorf("failed to run after-init-script: %w", err)
	}

	logger.Infof(ctx, "initialization successful in %s", time.Now().Sub(startTime))

	return rpcClient, nil
}

func systemOTA(
	ctx context.Context,
	rand *rand.Rand,
	device *device.Client,
	rpcClient **sl4f.Client,
	repo *packages.Repository,
	checkABR bool,
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
		rpcClient,
		repo,
		updatePackage,
		"ota-test-system_image/0",
		"ota-test-update/0",
		checkABR,
		checkForUnknownFirmware,
	)
}

func systemPrimeOTA(
	ctx context.Context,
	rand *rand.Rand,
	device *device.Client,
	rpcClient **sl4f.Client,
	repo *packages.Repository,
	checkABR bool,
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

	srcSystemImage, err := srcUpdate.OpenPackage(ctx, "system_image/0")
	if err != nil {
		return fmt.Errorf(
			"failed to open system_image/0 from %s update package: %w",
			srcUpdate.Path(),
			err,
		)
	}

	dstSystemImagePath := "system_image_prime/0"
	dstSystemImage, err := repo.EditPackage(
		ctx,
		srcSystemImage,
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
		c.useNewUpdateFormat,
	)
	if err != nil {
		return fmt.Errorf("failed to create the %q package: %w", dstUpdatePath, err)
	}

	return otaToPackage(
		ctx,
		rand,
		device,
		rpcClient,
		repo,
		dstUpdate,
		"ota-test-system_image_prime2/0",
		"ota-test-update_prime2/0",
		checkABR,
		true,
	)
}

func otaToPackage(
	ctx context.Context,
	rand *rand.Rand,
	device *device.Client,
	rpcClient **sl4f.Client,
	repo *packages.Repository,
	srcUpdate *packages.UpdatePackage,
	dstSystemImagePath string,
	dstUpdatePath string,
	checkABR bool,
	checkForUnknownFirmware bool,
) error {
	dstUpdate, dstSystemImage, err := AddRandomFilesToUpdate(
		ctx,
		rand,
		repo,
		srcUpdate,
		dstSystemImagePath,
		dstUpdatePath,
	)
	if err != nil {
		return fmt.Errorf("failed to create update package %s: %w", dstUpdatePath, err)
	}

	dstUpdatePackageUrl := fmt.Sprintf(
		"fuchsia-pkg://fuchsia.com/%s?hash=%s",
		dstUpdatePath,
		dstUpdate.Merkle(),
	)
	logger.Infof(ctx, "Generated update package %s", dstUpdatePackageUrl)

	expectedConfig, err := check.DetermineTargetABRConfig(ctx, *rpcClient)
	if err != nil {
		return fmt.Errorf("error determining target config: %w", err)
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

	u, err := c.installerConfig.Updater(
		repo,
		dstUpdatePackageUrl,
		checkForUnknownFirmware,
		c.useNewUpdateFormat,
	)
	if err != nil {
		return fmt.Errorf("failed to create updater: %w", err)
	}

	if err := u.Update(ctx, device); err != nil {
		return fmt.Errorf("failed to download OTA: %w", err)
	}

	logger.Infof(ctx, "Validating device")

	// Disconnect from sl4f since we rebooted the device.
	//
	// FIXME(47145) To avoid fxbug.dev/47145, we need to delay
	// disconnecting from sl4f until after we reboot the device. Otherwise
	// we risk leaving the ssh session in a bad state.
	if *rpcClient != nil {
		(*rpcClient).Close()
		*rpcClient = nil
	}

	*rpcClient, err = device.StartRpcSession(ctx, repo)
	if err != nil {
		return fmt.Errorf("unable to connect to sl4f after OTA: %w", err)
	}
	if err := check.ValidateDevice(
		ctx,
		device,
		*rpcClient,
		&dstSystemImage,
		expectedConfig,
		checkABR,
	); err != nil {
		return fmt.Errorf("failed to validate after OTA: %w", err)
	}

	if err := script.RunScript(ctx, device, repo, rpcClient, c.afterTestScript); err != nil {
		return fmt.Errorf("failed to run test script after OTA: %w", err)
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
	repo *packages.Repository,
	srcUpdate *packages.UpdatePackage,
	dstSystemImagePath string,
	dstUpdatePath string,
) (*packages.UpdatePackage, packages.Package, error) {
	srcSystemImage, err := srcUpdate.OpenPackage(ctx, "system_image/0")
	if err != nil {
		return nil, packages.Package{}, fmt.Errorf("error extracting expected system image merkle: %w", err)
	}

	otaSize, err := srcUpdate.OtaMaxNeededSize(ctx)
	if err != nil {
		return nil, packages.Package{}, fmt.Errorf("failed to compute size of the OTA: %w", err)
	}
	logger.Infof(ctx, "OTA size of %s: %d", srcUpdate.Path(), otaSize)

	updateImagesSize, err := srcUpdate.UpdateAndImagesSize(ctx)
	if err != nil {
		return nil, packages.Package{}, fmt.Errorf("failed to compute size of the update images: %w", err)
	}
	logger.Infof(ctx, "Update images size of %s: %d", srcUpdate.Path(), updateImagesSize)

	systemImageSize, err := srcUpdate.UpdateAndSystemImageSize(ctx)
	if err != nil {
		return nil, packages.Package{}, fmt.Errorf("failed to compute size of the system image: %w", err)
	}
	logger.Infof(ctx, "System image size of %s: %d", srcUpdate.Path(), systemImageSize)

	if c.maxOtaSize == 0 {
		// We just want to add a random file or so to the update package so
		// it's unique.
		dstUpdate, dstSystemImage, _, err := GenerateUpdatePackageWithRandomFiles(
			ctx,
			rand,
			repo,
			srcSystemImage,
			srcUpdate,
			dstSystemImagePath,
			dstUpdatePath,
			blobBlockSize,
		)

		return dstUpdate, dstSystemImage, err
	} else {
		// Otherwise, we want to fill out the update package until it's
		// approximately full.

		if c.maxOtaSize < otaSize {
			return nil, packages.Package{}, fmt.Errorf(
				"max OTA size %d is smaller than the size of the OTA %d",
				c.maxOtaSize,
				otaSize,
			)
		}

		// We'll keep generating update packages with random files until we get
		// one that's approximately the same size as the max OTA size.
		bytesToAdd := c.maxOtaSize - otaSize
		for bytesToAdd > 0 {
			dstUpdate, dstSystemImage, otaSize, err := GenerateUpdatePackageWithRandomFiles(
				ctx,
				rand,
				repo,
				srcSystemImage,
				srcUpdate,
				dstSystemImagePath,
				dstUpdatePath,
				bytesToAdd,
			)
			if err != nil {
				return nil, packages.Package{}, fmt.Errorf(
					"failed to generate update package %s of size %d: %w",
					dstUpdatePath,
					bytesToAdd,
					err,
				)
			}

			if c.maxOtaSize < otaSize {
				logger.Warningf(
					ctx,
					"OTA size %d is bigger than %d, trying again",
					otaSize,
					c.maxOtaSize,
				)

				// Shrink the OTA size by the amount we overshot by.
				bytesToAdd -= otaSize - c.maxOtaSize
			} else {
				logger.Infof(
					ctx,
					"Accepting update package %s, OTA size %d is smaller than %d",
					dstUpdate.Path(),
					otaSize,
					c.maxOtaSize,
				)
				return dstUpdate, dstSystemImage, nil
			}
		}

		return nil, packages.Package{}, fmt.Errorf(
			"failed to generate update package less than %d",
			c.maxOtaSize,
		)
	}
}

// GenerateRandomFiles will create a number of files that sum up to
// `bytesToAdd` random bytes, each which will be less than 10MiB.
func GenerateUpdatePackageWithRandomFiles(
	ctx context.Context,
	rand *rand.Rand,
	repo *packages.Repository,
	srcSystemImage packages.Package,
	srcUpdate *packages.UpdatePackage,
	dstSystemImagePath string,
	dstUpdatePath string,
	bytesToAdd int64,
) (*packages.UpdatePackage, packages.Package, int64, error) {
	logger.Infof(
		ctx,
		"Trying to insert random files up to %d bytes into the system image %s as %s",
		bytesToAdd,
		srcSystemImage.Path(),
		dstSystemImagePath,
	)

	avbTool, err := c.installerConfig.AVBTool()
	if err != nil {
		return nil, packages.Package{}, 0, fmt.Errorf("failed to intialize AVBTool: %w", err)
	}

	zbiTool, err := c.installerConfig.ZBITool()
	if err != nil {
		return nil, packages.Package{}, 0, fmt.Errorf("failed to initialize ZBITool: %w", err)
	}

	dstSystemImage, err := repo.EditPackage(
		ctx,
		srcSystemImage,
		dstSystemImagePath,
		func(tempDir string) error {
			return GenerateRandomFiles(ctx, rand, tempDir, bytesToAdd)
		},
	)
	if err != nil {
		return nil, packages.Package{}, 0, err
	}

	dstUpdate, err := srcUpdate.EditUpdatePackageWithNewSystemImage(
		ctx,
		avbTool,
		zbiTool,
		"fuchsia.com",
		dstSystemImage,
		dstUpdatePath,
		c.bootfsCompression,
		c.useNewUpdateFormat,
	)
	if err != nil {
		return nil, packages.Package{}, 0, fmt.Errorf(
			"failed to create the %q package: %w",
			dstUpdatePath,
			err,
		)
	}

	otaSize, err := dstUpdate.OtaMaxNeededSize(ctx)
	if err != nil {
		return nil, packages.Package{}, 0, fmt.Errorf("failed to compute size of the OTA: %w", err)
	}
	logger.Infof(
		ctx,
		"Created update package %s with OTA size %d",
		dstUpdate.Path(),
		otaSize,
	)

	updateImagesSize, err := dstUpdate.UpdateAndImagesSize(ctx)
	if err != nil {
		return nil, packages.Package{}, 0, fmt.Errorf("failed to compute size of the update images: %w", err)
	}
	logger.Infof(
		ctx,
		"Created update package %s with update images size: %d",
		dstUpdate.Path(),
		updateImagesSize,
	)

	systemImageSize, err := dstUpdate.UpdateAndSystemImageSize(ctx)
	if err != nil {
		return nil, packages.Package{}, 0, fmt.Errorf("failed to compute size of the system image: %w", err)
	}
	logger.Infof(
		ctx,
		"Created update package %s with system image size: %d",
		dstUpdate.Path(),
		systemImageSize,
	)

	return dstUpdate, dstSystemImage, otaSize, nil
}

// GenerateRandomFiles will create a number of files that sum up to
// `bytesToAdd` random bytes, each which will be less than 10MiB.
func GenerateRandomFiles(
	ctx context.Context,
	rand *rand.Rand,
	tempDir string,
	bytesToAdd int64,
) error {
	otaTestDir := filepath.Join(tempDir, "ota-test")
	if err := os.Mkdir(otaTestDir, 0700); err != nil {
		return fmt.Errorf("failed to create %s: %w", otaTestDir, err)
	}

	index := 0
	bytes := [blobBlockSize]byte{}

	const maxBlobSize = blobBlockSize * 1000

	for bytesToAdd > 0 {
		// Create blobs up to 4MiB.
		var blobSize int64
		if maxBlobSize < bytesToAdd {
			blobSize = maxBlobSize
		} else {
			blobSize = bytesToAdd
		}
		bytesToAdd -= blobSize

		blobPath := filepath.Join(otaTestDir, fmt.Sprintf("ota-test-file-%06d", index))
		f, err := os.Create(blobPath)
		if err != nil {
			return fmt.Errorf("failed to create %s: %w", blobPath, err)
		}
		defer f.Close()

		for blobSize > 0 {
			var n int64
			if blobSize < blobBlockSize {
				n = blobSize
			} else {
				n = blobBlockSize
			}
			blobSize -= n

			if _, err := rand.Read(bytes[:n]); err != nil {
				return fmt.Errorf("failed to read %d random bytes: %w", n, err)
			}

			if _, err := f.Write(bytes[:n]); err != nil {
				return fmt.Errorf("failed to write random bytes to %s: %w", blobPath, err)
			}
		}

		index += 1
	}

	return nil
}
