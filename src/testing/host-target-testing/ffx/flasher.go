// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package ffx

import (
	"context"
	"os"

	"golang.org/x/crypto/ssh"
)

type Flasher struct {
	ffx               *FFXTool
	sshPublicKey      ssh.PublicKey
	target            string
	manifestPath      string
	productBundlePath string
}

// NewFlasher constructs a new flasher that uses `ffx` as the FFXTool used
// to flash a device using flash.json located at `path`. Also accepts a
// number of optional parameters.
func newFlasher(ffx *FFXTool) *Flasher {
	return &Flasher{
		ffx: ffx,
	}
}

// Sets the SSH public key that the Flasher will bake into the device as an
// authorized key.
func (f *Flasher) SetSSHPublicKey(publicKey ssh.PublicKey) {
	f.sshPublicKey = publicKey
}

func (f *Flasher) SetTarget(target string) {
	f.target = target
}

func (f *Flasher) SetManifest(manifestPath string) {
	f.manifestPath = manifestPath
}

func (f *Flasher) SetProductBundle(productBundlePath string) {
	f.productBundlePath = productBundlePath
}

// Flash a device with flash.json manifest.
func (f *Flasher) Flash(ctx context.Context) (string, error) {
	args := []string{}

	if f.target != "" {
		args = append(args, "--target", f.target)
	}

	args = append(args, "--config",
		"{\"ffx\": {\"fastboot\": {\"inline_target\": true, "+
			"\"flash\":{\"timeout_rate\":4}}}}",
	)
	args = append(args, "target", "flash")
	args = append(args, "--config", "{\"ffx\": {\"fastboot\": {\"inline_target\": true}}}")

	// Write out the public key's authorized keys.
	if f.sshPublicKey != nil {
		authorizedKeys, err := os.CreateTemp("", "")
		if err != nil {
			return "", err
		}
		defer os.Remove(authorizedKeys.Name())

		if _, err := authorizedKeys.Write(ssh.MarshalAuthorizedKey(f.sshPublicKey)); err != nil {
			return "", err
		}

		if err := authorizedKeys.Close(); err != nil {
			return "", err
		}

		args = append(args, "--authorized-keys", authorizedKeys.Name())
	}

	if f.productBundlePath != "" {
		args = append(args, "--product-bundle", f.productBundlePath)
	}

	if f.manifestPath != "" {
		args = append(args, "--manifest", f.manifestPath)
	}

	stdout, err := f.ffx.runFFXCmd(ctx, args...)
	if err != nil {
		return "", err
	}

	return string(stdout), nil
}
