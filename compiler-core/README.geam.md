<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- SPDX-FileCopyrightText: 2026 The Gleam contributors -->

# Gleam compiler component for Geam

This crate republishes a Rust component from the
[Gleam compiler](https://github.com/gleam-lang/gleam) so that
[Geam](https://github.com/panarch/geam) can use the compiler through crates.io.

Implementation source follows the recorded upstream tag. The mirror changes
Cargo metadata and keeps language- and protocol-visible Gleam version strings at
the upstream release while the package version also records the Geam packaging
revision.

This is not a general-purpose fork of the Gleam compiler. Bugs and language
changes belong upstream unless they are specific to this packaging boundary.
