// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package readme

import (
	"bufio"
	"context"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"sort"
	"strings"

	"go.fuchsia.dev/fuchsia/tools/check-licenses/file"
)

const (
	singleLicenseFile = "Single License File"
)

var (
	AllReadmes = map[string]*Readme{}

	knownDirectives = map[string]bool{
		"Name":                true,
		"License":             true,
		"License File":        true,
		"License File Format": true,
		"License File URL":    true,
		"Version":             true,
		"Modifications":       true,
		"Local Modifications": true,
		"Description":         true,
		"URL":                 true,
		"Upstream git":        true,
		"Upstream Git":        true,
		"Security Critical":   true,

		// Unused or non-standard
		"Files":                      true,
		"Upstream":                   true,
		"License Android Compatible": true,
		"Versions":                   true,
		"Source":                     true,
		"Short Name":                 true,
		"Git Commit":                 true,
		"Commit":                     true,
		"Revision":                   true,
		"Date":                       true,
		"Deprecated":                 true,

		// DEPRECATED: soft transition
		"check-licenses": true,
		"#License File":  true,
		"LICENSE":        true,
	}
)

// Readme struct follows the format of README.fuchsia files.
// For more info, see the following article:
//
//	https://fuchsia.dev/fuchsia-src/development/source_code/third-party-metadata
type Readme struct {
	Name               string           `json:"name"`
	URL                string           `json:"url"`
	Version            string           `json:"version"`
	Licenses           []*ReadmeLicense `json:"licenseInfo"`
	UpstreamGit        string           `json:"upstreamGit"`
	SecurityCritical   bool             `json:"securityCritical"`
	Description        string           `json:"description"`
	LocalModifications string           `json:"localModifications"`

	// Custom fields for the Fuchsia repository.
	ReadmePath      string `json:"readmePath"`
	RegularFileType file.FileType

	// For generating the license URL of files that aren't
	// explicitly listed in the README.fuchsia file.
	GetLicenseURLForPath func(string) (string, error)

	// For Compliance worksheet
	ShouldBeDisplayed  bool
	SourceCodeIncluded bool

	// Logging
	MalformedLines []string `json:"malformedLines"`
}

// Several directives specify information about a given license file.
// Group them together in this ReadmeLicense data structure.
type ReadmeLicense struct {
	License           string     `json:"license"`
	LicenseFilePath   string     `json:"licenseFilePath"`
	LicenseFile       *file.File `json:"licenseFile"`
	LicenseFileFormat string     `json:"licenseFileFormat"`
	LicenseFileURL    string     `json:"licenseFileURL"`
}

// Create a Readme object from a README.* file on the filesystem.
func NewReadmeFromFile(readmePath string) (*Readme, error) {
	return NewReadmeFromFileCustomLocation(readmePath, readmePath)
}

// Create a Readme object from a README.* file on the filesystem.
// Second parameter is the project root. This is helpful when creating projects
// from README.fuchsia files that are not located in the root directory
// of the project.
func NewReadmeFromFileCustomLocation(readmePath, projectRoot string) (*Readme, error) {
	if _, err := os.Stat(readmePath); os.IsNotExist(err) {
		return nil, err
	}
	if _, err := os.Stat(filepath.Dir(projectRoot)); os.IsNotExist(err) {
		return nil, err
	}
	f, err := os.Open(readmePath)
	if err != nil {
		return nil, fmt.Errorf("newReadme(%s): %w\n", readmePath, err)
	}
	defer f.Close()

	return NewReadme(f, projectRoot)
}

// Create a Readme object from a path on the filesystem.
//
// Certain projects in the repo do not currently (and never will) provide
// a README.fuchsia file. This generates a Readme object by using other
// information about the project.
func NewReadmeCustom(projectRoot string) (*Readme, error) {
	switch {
	case strings.Contains(projectRoot, "dart-pkg"):
		return NewDartPkgReadme(projectRoot)
	case strings.Contains(projectRoot, "golibs") || strings.Contains(projectRoot, "syzkaller"):
		return NewGolibReadme(projectRoot)
	case strings.Contains(projectRoot, "rust_crates"):
		return NewRustCrateReadme(projectRoot)
	default:
		return nil, fmt.Errorf("Custom readme generation for project root [%s] is not supported", projectRoot)
	}
}

// NewReadme creates a new Readme object from an io.Reader.
func NewReadme(r io.Reader, path string) (*Readme, error) {
	if r, ok := AllReadmes[path]; ok {
		return r, nil
	}

	readme := &Readme{
		Licenses:       make([]*ReadmeLicense, 0),
		MalformedLines: make([]string, 0),
		ReadmePath:     path,
	}

	s := bufio.NewScanner(r)
	s.Split(bufio.ScanLines)

	line := ""
	getNextLine := true

	for {
		if getNextLine {
			if !s.Scan() {
				break
			}
		} else {
			getNextLine = true
		}
		line = s.Text()
		if len(strings.TrimSpace(line)) == 0 {
			continue
		}

		directive, value, err := parseReadmeLine(line)
		if err != nil {
			readme.MalformedLines = append(readme.MalformedLines, line)
			continue
		}

		switch directive {
		case "Name":
			readme.Name = value
		case "Source", "URL":
			readme.URL = value
		case "Versions", "Version":
			readme.Version = value
		case "LICENSE", "License":
			readme.ProcessReadmeLicense(&ReadmeLicense{License: value})
		case "License File Format":
			readme.ProcessReadmeLicense(&ReadmeLicense{LicenseFileFormat: value})
		case "License File":
			readme.ProcessReadmeLicense(&ReadmeLicense{LicenseFilePath: value})
		case "License File URL":
			readme.ProcessReadmeLicense(&ReadmeLicense{LicenseFileURL: value})
		case "Upstream git", "Upstream Git":
			readme.UpstreamGit = value
		case "Security Critical":
			readme.SecurityCritical = strings.ToLower(value) == "yes"
		case "Description":
			directive, value, readme.Description, getNextLine = parseReadmeMultiLineDirective(s, value)
		case "Modifications", "Local Modifications":
			directive, value, readme.LocalModifications, getNextLine = parseReadmeMultiLineDirective(s, value)

		// Deprecated but still in use currently
		case "check-licenses":
			// Used to specify license format
			switch value {
			case "license format: multi_license_google":
				readme.ProcessReadmeLicense(&ReadmeLicense{LicenseFileFormat: "Multi License Google"})
			case "license format: multi_license_chromium":
				readme.ProcessReadmeLicense(&ReadmeLicense{LicenseFileFormat: "Multi License Chromium"})
			case "license format: multi_license_flutter":
				readme.ProcessReadmeLicense(&ReadmeLicense{LicenseFileFormat: "Multi License Flutter"})
			case "license format: multi_license_android":
				readme.ProcessReadmeLicense(&ReadmeLicense{LicenseFileFormat: "Multi License Android"})
			case "file format: copyright_header": //do nothing
			default:
				return nil, fmt.Errorf("Unknown deprecated license directive: %s: %s\n", value, path)
			}

		// Unused multi-line directives still need to be processed here.
		case "Deprecated":
			getNextLine = false
			parseReadmeMultiLineDirective(s, value)

		// Empty space is OK
		case "":
			// Do nothing.
		}
	}

	readme.GetLicenseURLForPath = getLicenseURLFunction(path)

	// Loop through all license files that are listed in this Readme.
	for _, l := range readme.Licenses {
		// If this license file does not already have a URL, generate one now.
		if l.LicenseFileURL == "" {
			dir := filepath.Dir(path)
			licenseFilePath := filepath.Join(dir, l.LicenseFilePath)
			licenseURL, err := readme.GetLicenseURLForPath(licenseFilePath)
			if err != nil {
				return nil, err
			}
			l.LicenseFileURL = licenseURL
		}
	}

	AllReadmes[path] = readme
	return readme, nil
}

// License file directives can be listed in any order.
// e.g. URL info may come before or after information about where the file lives on the filesystem.
// We solve this by merging ReadmeLicense structs together that don't have overlapping information.
func (r *Readme) ProcessReadmeLicense(rl *ReadmeLicense) {
	var last *ReadmeLicense
	l := len(r.Licenses)
	if l > 0 {
		last = r.Licenses[l-1]
	}

	switch {
	case l == 0:
	case rl.License != "" && last.License != "":
	case rl.LicenseFileFormat != "" && last.LicenseFileFormat != "":
	case rl.LicenseFileURL != "" && last.LicenseFileURL != "":
	case rl.LicenseFilePath != "" && last.LicenseFilePath != "":
	default:
		last.License = last.License + rl.License
		last.LicenseFileFormat = last.LicenseFileFormat + rl.LicenseFileFormat
		last.LicenseFileURL = last.LicenseFileURL + rl.LicenseFileURL
		last.LicenseFilePath = last.LicenseFilePath + rl.LicenseFilePath
		return
	}
	r.Licenses = append(r.Licenses, rl)
}

// Parse a single line in a README.fuchsia file.
// Find the colon ':', directives are before that, values are after that.
func parseReadmeLine(line string) (string, string, error) {
	colon := strings.Index(line, ":")
	if colon < 0 {
		return "", line, fmt.Errorf("Failed to find ':' in line '%s'", line)
	}
	directive := line[:colon]
	value := strings.TrimSpace(line[colon+1:])

	if _, ok := knownDirectives[directive]; !ok {
		return "", line, fmt.Errorf("Unknown directive '%s'", directive)
	}
	return directive, value, nil
}

// Some directives can span multiple lines (e.g. "Description").
// In this case, keep parsing until we find another directive, or we reach the end of the file.
func parseReadmeMultiLineDirective(s *bufio.Scanner, value string) (string, string, string, bool) {
	var err error
	var b strings.Builder
	var line, directive string

	b.WriteString(fmt.Sprintf("%s\n", value))
	eof := true
	for s.Scan() {
		line = s.Text()
		directive, value, err = parseReadmeLine(line)
		if err == nil {
			eof = false
			break
		} else {
			b.WriteString(fmt.Sprintf("%s\n", line))
		}
	}

	return directive, value, b.String(), eof
}

// README.fuchsia files should exist for all projects in the filesystem, and
// they should specify the location and URLs of each license file in the project.
//
// Many times this isn't the case: either the URL is missing, or the license file
// is entirely missing from the README.fuchsia file.
//
// This callback function supports setting the URL for a given license file
// during directory traversal.
func getLicenseURLFunction(projectRoot string) func(string) (string, error) {
	ctx := context.Background()

	return func(licenseFilePath string) (string, error) {
		licenseFileDir := filepath.Dir(licenseFilePath)
		projectRootParent := filepath.Dir(projectRoot)

		// Get the path to the license file, relative to the root of the project.
		// Include the project name itself in the filepath.
		relPath, err := filepath.Rel(projectRootParent, licenseFilePath)
		if err != nil {
			return "", fmt.Errorf("Failed to get relative path for license URL: %s %s %w\n",
				projectRootParent, licenseFilePath, err)
		}

		gitURL, err := git.GetURL(ctx, licenseFileDir)
		if err != nil {
			return "", fmt.Errorf("Failed to get git URL for project path %s: %w",
				projectRoot, err)
		}

		gitHash, err := git.GetCommitHash(ctx, licenseFileDir)
		if err != nil {
			return "", fmt.Errorf("Failed to get git hash for path %s: %w", licenseFilePath, err)
		}

		url := fmt.Sprintf("%s/+/%s/%s", gitURL, gitHash, licenseFilePath)

		// Projects that are hosted in third_party fuchsia repositories do not
		// have the project name in the URL path after the + sign.
		//
		// TODO: Design a better solution to construct these URLs.
		if gitURL != "https://fuchsia.googlesource.com/fuchsia" {
			url = fmt.Sprintf("%s/+/%s/%s", gitURL, gitHash, relPath)
		}
		return url, nil
	}
}

// Sort the internal fields of the Readme struct, so a readme object can deterministically
// be compared against other readme objects.
func (r *Readme) Sort() {
	sort.Slice(r.Licenses[:], func(i, j int) bool {
		return r.Licenses[i].LicenseFilePath < r.Licenses[j].LicenseFilePath
	})
}
