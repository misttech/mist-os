// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package flasher

import (
	"context"
	"io"
	"os"

	"go.fuchsia.dev/fuchsia/src/testing/host-target-testing/ffx"
	"golang.org/x/crypto/ssh"
)

type BuildFlasher struct {
	Ffx           *ffx.FFXTool
	FlashManifest string
	usePB         bool
	sshPublicKey  ssh.PublicKey
	target        string
}

type Flasher interface {
	Flash(ctx context.Context) error
	SetTarget(ctx context.Context, target string) error
	Close() error
}

// NewBuildFlasher constructs a new flasher that uses `ffx` as the FFXTool used
// to flash a device using flash.json located at `flashManifest`. Also accepts a
// number of optional parameters.
func NewBuildFlasher(ffx *ffx.FFXTool, flashManifest string, usePB bool, options ...BuildFlasherOption) (*BuildFlasher, error) {
	p := &BuildFlasher{
		Ffx:           ffx,
		FlashManifest: flashManifest,
		usePB:         usePB,
	}

	for _, opt := range options {
		if err := opt(p); err != nil {
			return nil, err
		}
	}

	return p, nil
}

type BuildFlasherOption func(p *BuildFlasher) error

// Sets the SSH public key that the Flasher will bake into the device as an
// authorized key.
func SSHPublicKey(publicKey ssh.PublicKey) BuildFlasherOption {
	return func(p *BuildFlasher) error {
		p.sshPublicKey = publicKey
		return nil
	}
}

// Send stdout from the ffx target flash scripts to `writer`. Defaults to the parent
// stdout.
func Stdout(writer io.Writer) BuildFlasherOption {
	return func(p *BuildFlasher) error {
		p.Ffx.SetStdout(writer)
		return nil
	}
}

// Close cleans up the resources associated with the flasher.
func (p *BuildFlasher) Close() error {
	return p.Ffx.Close()
}

// SetTarget sets the target to flash.
func (p *BuildFlasher) SetTarget(ctx context.Context, target string) error {
	p.target = target
	return p.Ffx.TargetAdd(ctx, target)
}

// Flash a device with flash.json manifest.
func (p *BuildFlasher) Flash(ctx context.Context) error {
	flasherArgs := []string{}

	// Write out the public key's authorized keys.
	if p.sshPublicKey != nil {
		authorizedKeys, err := os.CreateTemp("", "")
		if err != nil {
			return err
		}
		defer os.Remove(authorizedKeys.Name())

		if _, err := authorizedKeys.Write(ssh.MarshalAuthorizedKey(p.sshPublicKey)); err != nil {
			return err
		}

		if err := authorizedKeys.Close(); err != nil {
			return err
		}

		flasherArgs = append(flasherArgs, "--authorized-keys", authorizedKeys.Name())
	}
	if p.usePB {
		flasherArgs = append(flasherArgs, "--product-bundle")
	}
	return p.Ffx.Flash(ctx, p.target, p.FlashManifest, flasherArgs...)
}
