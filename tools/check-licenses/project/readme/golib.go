// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package readme

import (
	"fmt"
	"path/filepath"
	"strings"
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
	directoryContents, err := listFilesRecursive(path)
	if err != nil {
		return nil, err
	}
	for _, item := range directoryContents {
		lower := strings.ToLower(item)
		// In practice, all license files for golibs either have "COPYING"
		// or "license" in their name.
		if !(strings.Contains(lower, "licen") ||
			strings.Contains(lower, "copying")) {
			continue
		}

		// There are some instances of go source files that fit the above
		// criteria. Skip those files.
		ext := filepath.Ext(item)
		if ext == ".go" || ext == ".tmpl" || strings.Contains(lower, "template") {
			continue
		}

		licenseUrl, err := getDartLicenseURL(&b, remainder, item)
		if err != nil {
			return nil, err
		}

		b.addLicense(item, licenseUrl, singleLicenseFile)
	}

	return NewReadme(strings.NewReader(b.build()))
}

func getDartLicenseURL(b *builder, remainder, path string) (string, error) {
	switch {
	// pkg.go.dev/*
	case b.parent == "google.golang.org":
		fallthrough
	case b.grandparent == "golang.org":
		fallthrough
	case b.grandparent == "gonum.org":
		fallthrough
	case b.grandparent == "gopkg.in":
		fallthrough
	case b.grandparent == "go.uber.org":
		fallthrough
	case b.grandparent == "gvisor.dev":
		fallthrough
	case b.dir == "go.opencensus.io":
		fallthrough
	case b.parent == "cloud.google.com":
		return fmt.Sprintf("https://pkg.go.dev/%s?tab=licenses", remainder), nil

	// github.com/*
	case remainder == "github.com/googleapis/gax-go/v2":
		fallthrough
	case b.grandparent == "github.com":
		return fmt.Sprintf("%s/blob/master/%s", b.url, path), nil

	// Unknown
	default:
		return "", fmt.Errorf("Unknown golib URL for package %s", path)
	}
}
