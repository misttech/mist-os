// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package readme

import (
	"fmt"
	"io/ioutil"
	"path/filepath"
	"strings"
)

const (
	// This project doesn't have a LICENSE file, and so must rely on the
	// license file in the parent directory.
	golibCloudInternalRoot        = "third_party/golibs/vendor/cloud.google.com/go/internal"
	golibCloudInternalLicenseFile = "../LICENSE"
	golibCloudInternalLicenseURL  = "https://fuchsia.googlesource.com/fuchsia/+/refs/heads/main/third_party/golibs/vendor/cloud.google.com/go/LICENSE"

	golibCustomReadme = "tools/check-licenses/assets/readmes/"
)

type golibReadmeBuilder struct {
	url         string
	dir         string
	parent      string
	grandparent string
}

// Create an in-memory representation of a new README.fuchsia file
// by inferring info about the given go library from it's location in the repo.
func NewGolibReadme(path string) (*Readme, error) {
	var b builder
	var remainder string

	b.setPath(path)
	b.setName(filepath.Base(path))

	cut := "third_party/golibs/vendor/"
	if _, after, found := strings.Cut(path, cut); found {
		remainder = after
	}
	b.setURL(fmt.Sprintf("https://%s", remainder))

	// Find all license files for this project.
	// They should all live in the root directory of this project.
	directoryContents, err := ioutil.ReadDir(path)
	if err != nil {
		return nil, err
	}
	for _, item := range directoryContents {
		lower := strings.ToLower(item.Name())
		// In practice, all license files for golibs either have "COPYING"
		// or "license" in their name.
		if !(strings.Contains(lower, "licen") ||
			strings.Contains(lower, "copying")) {
			continue
		}

		// There are some instances of go source files that fit the above
		// criteria. Skip those files.
		ext := filepath.Ext(item.Name())
		if ext == ".go" || ext == ".tmpl" || strings.Contains(lower, "template") {
			continue
		}

		licenseUrl, err := getGolibLicenseURL(&b, remainder, item.Name())
		if err != nil {
			return nil, err
		}

		b.addLicense(item.Name(), licenseUrl, singleLicenseFile)
	}

	if path == golibCloudInternalRoot {
		b.addLicense(golibCloudInternalLicenseFile, golibCloudInternalLicenseURL, singleLicenseFile)
	}

	customReadmePath := filepath.Join(golibCustomReadme, path)
	return NewReadme(strings.NewReader(b.build()), path, customReadmePath)
}

func getGolibLicenseURL(b *builder, remainder, path string) (string, error) {
	switch {
	// pkg.go.dev/*
	case b.parent == "google.golang.org",
		b.grandparent == "golang.org",
		b.grandparent == "gonum.org",
		b.grandparent == "gopkg.in",
		b.parent == "go.uber.org",
		b.parent == "gvisor.dev",
		b.dir == "go.opencensus.io",
		b.parent == "cloud.google.com",
		b.parent == "gopkg.in",
		b.grandparent == "cloud.google.com",
		b.grandparent == "honnef.co":
		return fmt.Sprintf("https://pkg.go.dev/%s?tab=licenses", remainder), nil

	// github.com/*
	case remainder == "github.com/googleapis/gax-go/v2",
		b.grandparent == "github.com":
		return fmt.Sprintf("%s/blob/master/%s", b.url, path), nil

	// Unknown
	default:
		return "", fmt.Errorf("Unknown golib URL for package %s", remainder)
	}
}
