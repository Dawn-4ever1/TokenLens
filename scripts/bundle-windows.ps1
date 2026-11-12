# Builds the Windows NSIS installer (no paid OS code signing) under
# CARGO_TARGET_DIR\release\bundle\nsis, or target\release\bundle\nsis.
# Build only: running the installer replaces the TokenLens installed for this
# user, so leave that step to the user.
$ErrorActionPreference = 'Stop'

$repo = Split-Path -Parent $PSScriptRoot
$target = if (-not $env:CARGO_TARGET_DIR) { Join-Path $repo 'target' } elseif ([IO.Path]::IsPathRooted($env:CARGO_TARGET_DIR)) { [IO.Path]::GetFullPath($env:CARGO_TARGET_DIR) } else { [IO.Path]::GetFullPath((Join-Path $repo $env:CARGO_TARGET_DIR)) }
$env:CARGO_TARGET_DIR = $target
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

$updater = $env:TOKENLENS_UPDATER_ARTIFACTS -eq '1'
if ($updater -and -not $env:TAURI_SIGNING_PRIVATE_KEY) {
  throw 'Updater artifacts require TAURI_SIGNING_PRIVATE_KEY.'
}
# A temporary config avoids shell quoting differences and never contains a key.
$override = Join-Path ([IO.Path]::GetTempPath()) ("tokenlens-bundle-{0}.json" -f [guid]::NewGuid())
[IO.File]::WriteAllText($override, (@{ bundle = @{ createUpdaterArtifacts = $updater } } | ConvertTo-Json -Compress))
Push-Location (Join-Path $repo 'app')
try {
  npm run tauri build -- --bundles nsis --config $override -- -j 4
  if ($LASTEXITCODE -ne 0) { throw "tauri build failed ($LASTEXITCODE)" }
} finally {
  Pop-Location
  Remove-Item -LiteralPath $override -Force
}

# Search the app binary for the home path and user name, as ASCII and UTF-16.
$binary = Join-Path $target 'release\tokenlens-app.exe'
$latin1 = [Text.Encoding]::GetEncoding(28591)
$text = $latin1.GetString([IO.File]::ReadAllBytes($binary))
$leaks = @()
foreach ($entry in @(@{ Label = 'home path'; Value = $HOME }, @{ Label = 'user name'; Value = $env:USERNAME })) {
  $needle = $entry.Value
  foreach ($encoding in @([Text.Encoding]::ASCII, [Text.Encoding]::Unicode)) {
    $pattern = $latin1.GetString($encoding.GetBytes($needle))
    if ($text.IndexOf($pattern, [StringComparison]::OrdinalIgnoreCase) -ge 0) {
      $leaks += "$($entry.Label) ($($encoding.EncodingName))"
    }
  }
}
if ($leaks.Count -gt 0) {
  throw "The app binary still contains: $($leaks -join ', ')"
}

$installer = Get-ChildItem (Join-Path $target 'release\bundle\nsis') -Filter '*-setup.exe' |
  Sort-Object LastWriteTime -Descending | Select-Object -First 1
if (-not $installer) { throw 'Installer is missing.' }
if ($updater -and -not (Test-Path -LiteralPath "$($installer.FullName).sig" -PathType Leaf)) {
  throw 'Updater signature is missing.'
}
'{0}  {1:N1} MB  {2}' -f $installer.FullName, ($installer.Length / 1MB), $installer.LastWriteTime
'Installer ready; no home path or user name inside the app binary.'
