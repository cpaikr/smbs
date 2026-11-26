# Install or recover the executable

Read the canonical [smbs installation guide](https://github.com/cpaikr/smbs/blob/main/README.md#install)
for supported platforms, the one-line installers, the install location,
PATH, verification, and upgrades. Follow its branch for the user's operating
system. Installation facts live there, so this package does not keep a second
installer recipe.

Binaries come from [public GitHub Releases](https://github.com/cpaikr/smbs/releases)
without login, for Linux x64 and arm64 (glibc 2.34+), Apple-silicon macOS,
and Windows x64. On other platforms, report that no prebuilt binary exists
instead of inventing an install command. If the guide or release cannot be
reached, report the unavailable resource.

When the user has authorized installation or updates, run the guide's
installer yourself. It verifies the download's SHA-256 and needs no browser
or login. An update notice alone is not an upgrade request.

- For an authorized update, run `smbs upgrade`. If it reports
  `unmanaged_installation`, the binary was not installed by the installer, or
  was installed before 0.3.0. Run the installer once, with the same
  `SMBS_INSTALL_DIR` if one was used originally, and later updates can use
  `smbs upgrade`. If it still reports `unmanaged_installation`, another `smbs`
  may come first on PATH: compare `command -v smbs` (PowerShell:
  `(Get-Command smbs).Source`) with the installed path. Then put the install
  directory earlier on PATH, or remove the other copy.
- `smbs upgrade --check` and `smbs version --check` report availability
  without changing anything.

Verify with `smbs --version` and `smbs --help`, first through the installed
file's full path, then by name in the user's shell. The installer prints how
to add the install directory to PATH when it is missing; a new terminal may be
needed before the name resolves.
