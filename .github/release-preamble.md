## Install (macOS)

Universal binary — runs on both Apple Silicon and Intel.

```sh
tar -xzf diffident-*-macos-universal.tar.gz
xattr -d com.apple.quarantine diffident   # required: see below
./diffident
```

> **The `xattr` step is not optional.** This binary is unsigned and un-notarized, so
> Gatekeeper will refuse to run it with *"cannot be opened because the developer cannot be
> verified"*. Removing the quarantine attribute is what allows it to launch. Signing
> requires a paid Apple Developer certificate.

Verify the download against the published checksum:

```sh
shasum -a 256 -c diffident-*-macos-universal.tar.gz.sha256
```

This ships as a bare binary, not an `.app` bundle — it opens a window, but has no custom
Dock icon and won't appear in Launchpad.

---
