# Builds the unsigned Windows NSIS installer into target\release\bundle\nsis\.
# Build only: running the installer replaces the TokenLens installed for this
# user, so leave that step to the user.
$ErrorActionPreference = 'Stop'

$repo = Split-Path -Parent $PSScriptRoot
$cargo = (Get-Command cargo).Source
$cargoHome = if ($env:CARGO_HOME) { $env:CARGO_HOME } else { Split-Path -Parent (Split-Path -Parent $cargo) }

# Panic locations would otherwise embed this machine's paths (and user name).
# The last matching prefix wins, so the repo maps to `tokenlens`, the Cargo
# cache to `cargo` and anything else under the home folder to `~`.
# The encoded form keeps paths intact even if they contain spaces.
$env:CARGO_ENCODED_RUSTFLAGS = @(
  "--remap-path-prefix=$HOME=~",
  "--remap-path-prefix=$cargoHome=cargo",
  "--remap-path-prefix=$repo=tokenlens"
) -join [char]0x1f

Push-Location (Join-Path $repo 'app')
try {
  npm run tauri build -- --bundles nsis
  if ($LASTEXITCODE -ne 0) { throw "tauri build failed ($LASTEXITCODE)" }
} finally {
  Pop-Location
}

# Search the app binary for the home path and user name, as ASCII and UTF-16.
$binary = Join-Path $repo 'target\release\tokenlens-app.exe'
$latin1 = [Text.Encoding]::GetEncoding(28591)
$text = $latin1.GetString([IO.File]::ReadAllBytes($binary))
$leaks = @()
foreach ($needle in @($HOME, $env:USERNAME)) {
  foreach ($encoding in @([Text.Encoding]::ASCII, [Text.Encoding]::Unicode)) {
    $pattern = $latin1.GetString($encoding.GetBytes($needle))
    if ($text.IndexOf($pattern, [StringComparison]::OrdinalIgnoreCase) -ge 0) {
      $leaks += "$needle ($($encoding.EncodingName))"
    }
  }
}
if ($leaks.Count -gt 0) {
  throw "The app binary still contains: $($leaks -join ', ')"
}

$installer = Get-ChildItem (Join-Path $repo 'target\release\bundle\nsis') -Filter '*-setup.exe' |
  Sort-Object LastWriteTime -Descending | Select-Object -First 1
'{0}  {1:N1} MB  {2}' -f $installer.FullName, ($installer.Length / 1MB), $installer.LastWriteTime
'Installer ready; no home path or user name inside the app binary.'
