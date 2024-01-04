// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package updater

import (
	"bufio"
	"bytes"
	"context"
	"errors"
	"fmt"
	"io"
	"net/url"
	"os"
	"regexp"
	"time"

	"go.fuchsia.dev/fuchsia/src/testing/host-target-testing/avb"
	"go.fuchsia.dev/fuchsia/src/testing/host-target-testing/omaha_tool"
	"go.fuchsia.dev/fuchsia/src/testing/host-target-testing/packages"
	"go.fuchsia.dev/fuchsia/src/testing/host-target-testing/zbi"
	"go.fuchsia.dev/fuchsia/tools/lib/logger"

	"golang.org/x/crypto/ssh"
)

const (
	updateErrorSleepTime = 30 * time.Second
)

type client interface {
	ExpectReboot(ctx context.Context, f func() error) error
	Reboot(ctx context.Context) error
	DisconnectionListener() <-chan struct{}
	ServePackageRepository(
		ctx context.Context,
		repo *packages.Repository,
		name string,
		createRewriteRule bool,
		rewritePackages []string) (*packages.Server, error)
	Run(ctx context.Context, command []string, stdout io.Writer, stderr io.Writer) error
}

type Updater interface {
	Update(ctx context.Context, c client) error
}

func checkSyslogForUnknownFirmware(ctx context.Context, c client) error {
	logger.Infof(ctx, "Checking system log for errors")

	// Try to dump logs using the new logger.
	var stdout bytes.Buffer
	if err := c.Run(
		ctx,
		[]string{"log_listener", "--tag", "system-updater", "dump"},
		&stdout,
		os.Stderr,
	); err != nil {
		// Don't bother trying to fall back to the old command if we
		// disconnected from the device.
		var errExitMissing *ssh.ExitMissingError
		if errors.As(err, &errExitMissing) {
			return err
		}

		// Otherwise fall back to the old logger
		stdout = bytes.Buffer{}
		if err := c.Run(
			ctx,
			[]string{"log_listener", "--tag", "system-updater", "--dump_logs", "yes"},
			&stdout,
			os.Stderr,
		); err != nil {
			return err
		}
	}

	re := regexp.MustCompile("skipping unsupported .* type:")

	scanner := bufio.NewScanner(&stdout)
	for scanner.Scan() {
		line := scanner.Text()
		if re.MatchString(line) {
			return fmt.Errorf("System Updater should not have skipped installing firmware: %s", line)
		}
	}

	return nil
}

// SystemUpdateChecker uses `update check-now` to install a package.
type SystemUpdateChecker struct {
	repo                   *packages.Repository
	updatePackageUrl       string
	checkForUnkownFirmware bool
}

func NewSystemUpdateChecker(
	repo *packages.Repository,
	updatePackageUrl string,
	checkForUnkownFirmware bool,
) *SystemUpdateChecker {
	return &SystemUpdateChecker{
		repo:                   repo,
		updatePackageUrl:       updatePackageUrl,
		checkForUnkownFirmware: checkForUnkownFirmware,
	}
}

const (
	// The default fuchsia update package URL.
	defaultUpdatePackageURL = "fuchsia-pkg://fuchsia.com/update/0"
)

func (u *SystemUpdateChecker) Update(ctx context.Context, c client) error {
	// If we're using the default update package url, we can directly update
	// with it.
	if u.updatePackageUrl == defaultUpdatePackageURL {
		return updateCheckNow(ctx, c, u.repo, true, u.checkForUnkownFirmware)
	}

	// Otherwise, copy the repository into a temporary directory, and publish
	// the update package to `fuchsia-pkg://fuchsia.com/update/0`, then update
	// with the temp repository.

	logger.Infof(
		ctx,
		"update package %s isn't default, cloning the repository so it can be made %s",
		u.updatePackageUrl,
		defaultUpdatePackageURL,
	)

	url, err := url.Parse(u.updatePackageUrl)
	if err != nil {
		return fmt.Errorf("invalid update package URL %q: %w", u.updatePackageUrl, err)
	}
	srcUpdatePath := url.Path[1:]

	tempDir, err := os.MkdirTemp("", "")
	if err != nil {
		return fmt.Errorf("failed to create a temp directory: %w", err)
	}
	defer os.RemoveAll(tempDir)

	repo, err := u.repo.CloneIntoDir(ctx, tempDir)
	if err != nil {
		return fmt.Errorf("failed to copy %s into %s: %w", u.repo, tempDir, err)
	}

	srcUpdate, err := repo.OpenUpdatePackage(ctx, srcUpdatePath)
	if err != nil {
		return fmt.Errorf("failed to open %s: %w", srcUpdatePath, err)
	}

	dstUpdatePath := "update/0"
	_, err = srcUpdate.EditContents(ctx, dstUpdatePath, func(tempDir string) error { return nil })
	if err != nil {
		return fmt.Errorf("failed to publish %s: %w", dstUpdatePath, err)
	}

	return updateCheckNow(ctx, c, repo, true, u.checkForUnkownFirmware)
}

func updateCheckNow(
	ctx context.Context,
	c client,
	repo *packages.Repository,
	createRewriteRule bool,
	checkForUnkownFirmware bool,
) error {
	logger.Infof(ctx, "Triggering OTA")

	startTime := time.Now()
	err := c.ExpectReboot(ctx, func() error {
		// Since an update can trigger a reboot, we can run into all
		// sorts of races. The two main ones are:
		//
		//  * the network connection is torn down before we see the
		//    `update` command exited cleanly.
		//  * the system updater service was torn down before the
		//    `update` process, which would show up as the channel to
		//    be closed.
		//
		// In order to avoid this races, we need to:
		//
		//  * assume the ssh connection was closed means the OTA was
		//    probably installed and the device rebooted as normal.
		//  * `update` exiting with a error could be we just lost the
		//    shutdown race. So if we get an `update` error, wait a few
		//    seconds to see if the device disconnects. If so, treat it
		//    like the OTA was successful.

		// We pass createRewriteRule=true for versions of system-update-checker prior to
		// fxrev.dev/504000. Newer versions need to have `update channel set` called below.
		server, err := c.ServePackageRepository(ctx, repo, "trigger-ota", createRewriteRule, nil)
		if err != nil {
			return fmt.Errorf("error setting up server: %w", err)
		}
		defer server.Shutdown(ctx)

		ch := c.DisconnectionListener()

		cmd := []string{
			"/bin/update",
			"channel",
			"set",
			"trigger-ota",
		}
		if err := c.Run(ctx, cmd, os.Stdout, os.Stderr); err != nil {
			logger.Warningf(ctx, "update channel set failed: %v. This probably indicates the device is running an old version of system-update-checker.", err)
		}

		cmd = []string{
			"/bin/update",
			"check-now",
			"--monitor",
		}

		err = c.Run(ctx, cmd, os.Stdout, os.Stderr)
		if err == nil && checkForUnkownFirmware {
			// FIXME(https://fxbug.dev/126805): We wouldn't have to ignore disconnects
			// if we could trigger an update without it automatically rebooting.
			err = checkSyslogForUnknownFirmware(ctx, c)
		}

		if err != nil {
			// If the device rebooted before ssh was able to tell
			// us the command ran, it will tell us the session
			// exited without passing along an exit code. So,
			// ignore that specific error.
			var errExitMissing *ssh.ExitMissingError
			if errors.As(err, &errExitMissing) {
				logger.Warningf(ctx, "disconnected, assuming this was because OTA triggered reboot")
				return nil
			}

			logger.Warningf(ctx, "update errored out, but maybe it lost the race, waiting a moment to see if the device reboots: %v", err)

			// We got an error, but maybe we lost the reboot race.
			// So wait a few moments to see if the device reboots
			// anyway.
			select {
			case <-ch:
				logger.Warningf(ctx, "disconnected, assuming this was because OTA triggered reboot")
				return nil
			case <-time.After(updateErrorSleepTime):
				return fmt.Errorf("failed to trigger OTA: %w", err)
			}
		}
		return nil
	})
	if err != nil {
		return err
	}

	cmd := []string{"/bin/update", "wait-for-commit"}
	if err := c.Run(ctx, cmd, os.Stdout, os.Stderr); err != nil {
		logger.Warningf(ctx, "update wait-for-commit failed: %v", err)
	}

	logger.Infof(ctx, "OTA completed in %s", time.Now().Sub(startTime))

	return nil
}

// SystemUpdater uses the `system-updater` to install a package.
type SystemUpdater struct {
	repo                   *packages.Repository
	updatePackageUrl       string
	checkForUnkownFirmware bool
}

func NewSystemUpdater(repo *packages.Repository, updatePackageUrl string, checkForUnkownFirmware bool) *SystemUpdater {
	return &SystemUpdater{repo: repo, updatePackageUrl: updatePackageUrl, checkForUnkownFirmware: checkForUnkownFirmware}
}

func (u *SystemUpdater) Update(ctx context.Context, c client) error {
	startTime := time.Now()

	url, err := url.Parse(u.updatePackageUrl)
	if err != nil {
		return fmt.Errorf("invalid update package URL %q: %w", u.updatePackageUrl, err)
	}
	pkgPath := url.Path[1:]

	srcUpdate, err := u.repo.OpenUpdatePackage(ctx, pkgPath)
	if err != nil {
		return err
	}

	repoName := "download-ota"
	dstUpdate, err := srcUpdate.RehostUpdatePackage(ctx, repoName, pkgPath)

	logger.Infof(ctx, "published %q as %q to %q", pkgPath, dstUpdate.Merkle(), u.repo)

	server, err := c.ServePackageRepository(ctx, u.repo, repoName, true, nil)
	if err != nil {
		return fmt.Errorf("error setting up server: %w", err)
	}
	defer server.Shutdown(ctx)

	logger.Infof(ctx, "Downloading OTA %q", u.updatePackageUrl)

	cmd := []string{
		"update",
		"force-install",
		"--reboot", "false",
		fmt.Sprintf("%q", u.updatePackageUrl),
	}
	if err := c.Run(ctx, cmd, os.Stdout, os.Stderr); err != nil {
		return fmt.Errorf("failed to run system updater: %w", err)
	}

	logger.Infof(ctx, "OTA successfully downloaded in %s", time.Now().Sub(startTime))

	if err := checkSyslogForUnknownFirmware(ctx, c); err != nil {
		return err
	}

	logger.Infof(ctx, "Rebooting device")
	startTime = time.Now()

	if err = c.Reboot(ctx); err != nil {
		return fmt.Errorf("device failed to reboot after OTA applied: %w", err)
	}

	logger.Infof(ctx, "Reboot complete in %s", time.Now().Sub(startTime))

	cmd = []string{"/bin/update", "wait-for-commit"}
	if err := c.Run(ctx, cmd, os.Stdout, os.Stderr); err != nil {
		logger.Warningf(ctx, "update wait-for-commit failed: %v", err)
	}

	return nil
}

type OmahaUpdater struct {
	repo                        *packages.Repository
	updatePackageURL            *url.URL
	omahaTool                   *omaha_tool.OmahaTool
	avbTool                     *avb.AVBTool
	zbiTool                     *zbi.ZBITool
	workaroundOtaNoRewriteRules bool
	checkForUnkownFirmware      bool
	useNewUpdateFormat          bool
}

func NewOmahaUpdater(
	repo *packages.Repository,
	updatePackageURL string,
	omahaTool *omaha_tool.OmahaTool,
	avbTool *avb.AVBTool,
	zbiTool *zbi.ZBITool,
	workaroundOtaNoRewriteRules bool,
	checkForUnkownFirmware bool,
	useNewUpdateFormat bool,
) (*OmahaUpdater, error) {
	u, err := url.Parse(updatePackageURL)
	if err != nil {
		return nil, fmt.Errorf("invalid update package URL %q: %w", updatePackageURL, err)
	}

	if u.Scheme != "fuchsia-pkg" {
		return nil, fmt.Errorf("scheme must be 'fuchsia-pkg', not %q", u.Scheme)
	}

	if u.Host == "" {
		return nil, fmt.Errorf("update package URL's host must not be empty")
	}

	return &OmahaUpdater{
		repo:                        repo,
		updatePackageURL:            u,
		omahaTool:                   omahaTool,
		avbTool:                     avbTool,
		zbiTool:                     zbiTool,
		workaroundOtaNoRewriteRules: workaroundOtaNoRewriteRules,
		checkForUnkownFirmware:      checkForUnkownFirmware,
		useNewUpdateFormat:          useNewUpdateFormat,
	}, nil
}

func (u *OmahaUpdater) Update(ctx context.Context, c client) error {
	logger.Infof(ctx, "injecting omaha_url into %q", u.updatePackageURL)

	srcUpdate, err := u.repo.OpenUpdatePackage(ctx, u.updatePackageURL.Path[1:])
	if err != nil {
		return err
	}

	logger.Infof(ctx, "source update package merkle for %q is %q", u.updatePackageURL, srcUpdate.Merkle())

	// Create a ZBI with the omaha_url argument.
	destZbi, err := os.CreateTemp("", "omaha_argument.zbi")
	if err != nil {
		return fmt.Errorf("failed to create temp file: %w", err)
	}
	defer os.Remove(destZbi.Name())

	imageArguments := map[string]string{
		"omaha_url":    u.omahaTool.URL(),
		"omaha_app_id": u.omahaTool.Args.AppId,
	}

	logger.Infof(ctx, "Omaha Server URL set in vbmeta to %q", u.omahaTool.URL())

	if err := u.zbiTool.MakeImageArgsZbi(ctx, destZbi.Name(), imageArguments); err != nil {
		return fmt.Errorf("failed to create ZBI: %w", err)
	}

	// Create a vbmeta that includes the ZBI we just created.
	propFiles := map[string]string{
		"zbi": destZbi.Name(),
	}

	repoName := "trigger-ota"

	dstUpdate, err := srcUpdate.EditUpdatePackageWithVBMetaProperties(
		ctx,
		u.avbTool,
		repoName,
		"update_omaha/0",
		propFiles,
		u.useNewUpdateFormat,
	)
	if err != nil {
		return fmt.Errorf("failed to inject vbmeta properties into update package: %w", err)
	}

	logger.Infof(ctx, "Published %q as %q to %q", dstUpdate.Path(), dstUpdate.Merkle(), u.repo)

	omahaPackageURL := fmt.Sprintf(
		"fuchsia-pkg://%s/%s?hash=%s",
		repoName,
		dstUpdate.Path(),
		dstUpdate.Merkle(),
	)

	// Configure the Omaha server with the new omaha package URL.
	if err := u.omahaTool.SetPkgURL(ctx, omahaPackageURL); err != nil {
		return fmt.Errorf("Failed to set Omaha update package: %w", err)
	}

	// Trigger an update
	return updateCheckNow(ctx, c, u.repo, !u.workaroundOtaNoRewriteRules, u.checkForUnkownFirmware)
}
