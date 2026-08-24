# SDK API documentation bundles

Each release artifact documents one KeyOS API version. SDK and KeyOS versions are independent: SDK
`1.0.0` can document KeyOS `1.4.0`. An app developer chooses a hosted KeyOS snapshot that matches
the app's `minKeyosVersion`.

## Source of truth

[`sdk/sdk-build.toml`](../sdk/sdk-build.toml) owns the SDK version and API contents:

- `[[sdk.api_crates]]` is the only allowlist for crates copied into the SDK API surface and rendered
  in Rustdoc. `server` is an ordinary entry. `bt` is forbidden and must not appear in this list or
  in `[[copy]]`.
- `[docs].guide_source` selects the SDK guide source.

The generated docs version always comes from KeyOS-dev's canonical KeyOS version. Historical
versions and their release checksums are selected in Docs-Site, because they control the hosted
deployment rather than the SDK build.

### Temporary QuantumLink compatibility placeholder

The current `quantum-link` API exposes `bt::BluetoothError`. Until QuantumLink v2 removes that
dependency, the SDK builder stages an internal crate named `bt` containing only that error type. It
copies `api/bt/src/error.rs` from the selected KeyOS source and supplies wire-compatible GPIO/SPI
payload enums. It does not contain `BluetoothApi`, Bluetooth messages, permissions, or transport
code, and it is never included in the documentation allowlist. The staging code and fixed wrapper
live under `sdk/xtask/assets/bt-placeholder` rather than `sdk-build.toml` so the public crate list
remains unambiguous.

## SDK API surface

The generated documentation contains only the APIs that third-party developers can use now. The
generator resolves each function's permission policy from the service manifests through
`Message::required_signature` and omits functions that require a Foundation signature or a
permission that is not user-grantable. It also removes the corresponding navigation entries and any
crate whose documented API is entirely unavailable to third-party applications.

This is a single SDK documentation surface, not a switch between public and internal APIs. A crate
appears only when it is in the `sdk.api_crates` allowlist and has a usable API; changing its exposure
later requires reviewing both the allowlist and its permissions.

## Build and package

```text
# Build without publishing:
just docs

# Complete publishing workflow (packages immediately before publishing):
just docs-publish [RELEASE_TAG] [--dry-run] [--replace]

# Equivalent explicit steps; do not run the publisher against an older package:
just docs-package
nix develop .#build --command cargo xtask docs-publish [RELEASE_TAG] [--dry-run] [--replace]

# Local preview:
just docs-open
```

`just docs-publish` is the recommended publishing command. It packages the docs and then invokes the
Rust publisher. The raw `cargo xtask docs-publish` command is only the second step of the explicit
recipe above: it verifies and uploads the ZIP and checksum already on disk, but does not rebuild
them. The publisher refuses to replace an existing asset by default. Pass `--dry-run` to verify
without uploading or `--replace` to overwrite both assets explicitly. The release tag defaults to
the KeyOS version; pass it only when the storage tag differs. When KeyOS-Releases-private has a matching
draft—by its tag, or by its release title when it is untagged (for example, because its version
branch has the same name as its eventual tag)—the publisher uses that draft's release ID. It can
validate and upload assets to the draft without creating or publishing the tag.

The `docs`, `docs-package`, and `docs-publish` recipes enter the repository's `.#build` Nix shell
themselves. That shell supplies the custom `armv7a-unknown-xous-elf` target, so these recipes work
from a normal host shell as well as from an existing development shell.

`just docs-open` builds the bundle and opens its static `index.html` through the branch's
`foundation` CLI. It does not start a server.

The commands produce:

```text
target/sdk-docs/api/
  index.html
  bundle-manifest.json
  bundle-manifest.js
  version-selector.js
  v<KeyOS version>/...

target/keyos-sdk-docs-v<KeyOS version>.zip
target/keyos-sdk-docs-v<KeyOS version>.zip.sha256
```

The manifest records the independent SDK and KeyOS versions, source and generator revisions, included
crate set, and tree hash. The ZIP is self-contained and contains exactly that KeyOS snapshot. ZIP entries are
sorted, use normalized permissions and timestamps, and are reproducible for identical content.

## Release and deployment flow

1. Run `nix develop .#build --command just docs-publish [RELEASE_TAG]` in KeyOS-dev.
2. Run the printed `docs:add` command in Docs-Site. It fetches the published checksum and adds the
   KeyOS version to Docs-Site's checked-in release catalog.
3. Review and merge the Docs-Site pull request. Its main-branch deployment downloads every catalog
   entry and assembles the version selector site from those independent ZIPs.

KeyOS-Releases-private is the current artifact store. The firmware release API can replace it without
changing the per-version bundle format or the independent SDK/KeyOS version identities.
Because that repository is private, every manual or automated asset download must authenticate with
a GitHub token that has read access to `Foundation-Devices/KeyOS-Releases-private`. A repository-scoped
`GITHUB_TOKEN` from Docs-Site cannot read this sibling private repository. Docs-Site does not yet pass
a suitable cross-repository token to its downloader, so deployment from the private store remains
blocked until that fetcher and its workflows are updated.

The SDK builder copies its one generated snapshot into `docs/api`. `foundation docs [SDK_VERSION]`
opens those installed bytes; pass `--url` to print their `file://` URL instead. `SDK_VERSION` selects
the installed SDK bundle.
