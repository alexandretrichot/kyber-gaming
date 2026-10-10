# CI: Kyber builds

[Kyber](https://gitlab.com/kyber.stream/apps/kyber-desktop) publishes no Linux or macOS binaries, so
`.github/workflows/kyber.yml` builds it from the pinned submodule `third_party/kyber-desktop`.

## What is built

| Job | Runner | Output |
| --- | --- | --- |
| `linux` | `ubuntu-24.04` | `kyber-linux-x86_64.tar.bz2` (server + client, upstream's `build-linux.sh -p`) |
| `macos` (`arm64`, `x86_64`) | `macos-15` / `macos-15-intel` (native builds) | `Kyber-Client-macos-<arch>.app.zip` |
| `runtime-image` | `ubuntu-24.04` | `ghcr.io/<owner>/kg-runtime` |
| `release` | `ubuntu-24.04` | GitHub Release with all of the above |

The macOS zip is upstream's `Kyber-Client.app.zip` renamed per arch so both can live in one release.
Cross-building `x86_64` on Apple Silicon (`build-macos.sh -a x86_64`) was tried and fails: FFmpeg's
configure picks up the host's arm64 Homebrew `liblzma`. `macos-15-intel` is GitHub's last
Intel image; once it is retired, the `x86_64` client needs a Homebrew-free cross-build or must be dropped.

The app is unsigned: Gatekeeper needs `xattr -dr com.apple.quarantine Kyber-Client.app` after unzipping.

Windows is not built: the client targets are macOS only for now. Upstream cross-compiles it from
Linux with mingw (`build-win32.sh -d -w` then `-s -p -w`, Rust target `x86_64-pc-windows-gnu`).

## Where artifacts land

- **Every push to `main` and every PR**: workflow artifacts `kyber-linux-x86_64`, `kyber-macos-arm64`,
  `kyber-macos-x86_64` (kept 90 days by default).
- **Push to `main`**: `ghcr.io/<owner>/kg-runtime:sha-<short>`.
- **Tag `v*`**: GitHub Release `<tag>` with the three packages, `ghcr.io/<owner>/kg-runtime:<tag>`.
  The release notes name the exact Kyber commit and link to its source (AGPLv3).

## kg-runtime image

`packaging/kg-runtime/Dockerfile` is a `FROM scratch` image with a single layer: the extracted Linux
package under `/opt/kg/kyber/` (`bin/`, `lib/`, `share/`, run scripts). It holds no OS and is never run.
The VM agent pulls it and extracts the layers onto the VM. Future layers must stay additive (no
deletions, so no whiteout files). It is pushed without provenance/SBOM attestations, so the tag
resolves to a plain `linux/amd64` manifest. The Kyber commit is in the `org.kyber-gaming.kyber.rev` label.

## Bumping the Kyber pin

```bash
git -C third_party/kyber-desktop fetch origin
git -C third_party/kyber-desktop checkout <commit-or-tag>
git submodule update --init --recursive
git add third_party/kyber-desktop
```

Check upstream's `CHANGELOG.md` and `.gitlab-ci.yml` for toolchain changes (Rust version, `cargo-c`,
meson, system packages) and update the `env` block of the workflow to match. The first run after a
bump rebuilds all native deps.

## Cache strategy

FFmpeg, VLC, txproto and the other native deps dominate build time and only change with the pin.
Each platform job is split in two upstream steps:

1. `build-*.sh -d`: native deps only, into `$GITHUB_WORKSPACE/rootfs`. Skipped on cache hit.
2. `build-*.sh -s -p`: Kyber's Rust app and the package, against the cached `rootfs`.

`rootfs` is cached with `actions/cache/restore` + `actions/cache/save`, saved right after step 1 so
that a failing app build still keeps the deps. The key is
`kyber-deps-v<DEPS_CACHE_VERSION>-<platform>-<hash>`, where `<hash>` covers the kyber-desktop commit,
every recursive submodule commit and the absolute `rootfs` path (it ends up in `.pc` files and macOS
install names, so a renamed repo must rebuild). Cargo's registry and `target/` are cached separately
under the same hash, which makes step 2 incremental.

A PR that does not move the pin restores everything. To force a rebuild without moving the pin
(e.g. a broken cache, a runner image change), bump `DEPS_CACHE_VERSION`.

GitHub caps caches at 10 GB per repository and evicts the least recently used. One pin uses 3 deps
caches plus 3 cargo caches.

## Expected durations

| Job | Cold (deps rebuilt) | Warm (pin unchanged) |
| --- | --- | --- |
| `linux` | 45-60 min | ~10 min |
| `macos` (each) | 35-50 min | ~8 min |
