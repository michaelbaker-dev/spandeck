# Install the latest spandeck release on Windows (x64). The Windows form of install.sh.
#   irm https://raw.githubusercontent.com/michaelbaker-dev/spandeck/main/scripts/install.ps1 | iex
# Options (environment): SPANDECK_VERSION=v0.4.27   SPANDECK_BIN_DIR=<folder>
#   SPANDECK_RELEASES=<address of a copy of the release page>
#   SPANDECK_ARCHIVE=<a release .zip already on this machine>, with SPANDECK_VERSION
#     and SPANDECK_ARCHIVE_SHA256=<its SHA-256>: nothing is downloaded
#   SPANDECK_NO_SERVICE=1   do not set spandeck to start when you log on
#   SPANDECK_NO_PATH=1      do not add the folder to your PATH
# Works in Windows PowerShell 5.1 and PowerShell 7. Needs no administrator.
$ErrorActionPreference = 'Stop'
# Windows PowerShell 5.1 can default to TLS 1.0, which GitHub refuses.
[Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
# The progress bar makes Invoke-WebRequest many times slower in 5.1.
$ProgressPreference = 'SilentlyContinue'

$repo = 'michaelbaker-dev/spandeck'
$releases = if ($env:SPANDECK_RELEASES) { $env:SPANDECK_RELEASES.TrimEnd('/') } else { "https://github.com/$repo/releases" }
$binDir = if ($env:SPANDECK_BIN_DIR) { $env:SPANDECK_BIN_DIR } else { Join-Path $env:LOCALAPPDATA 'Programs\spandeck' }

# PROCESSOR_ARCHITEW6432 is set when a 32-bit PowerShell runs on 64-bit Windows.
$arch = if ($env:PROCESSOR_ARCHITEW6432) { $env:PROCESSOR_ARCHITEW6432 } else { $env:PROCESSOR_ARCHITECTURE }
if ($arch -ne 'AMD64') {
    throw "There is no spandeck release for this processor ($arch). Windows releases are x64 only."
}
$target = 'x86_64-pc-windows-msvc'

# "The release could not be downloaded" begins every error of that kind: a
# station that is adding this machine then sends the release itself.
$version = $env:SPANDECK_VERSION
$archive = $env:SPANDECK_ARCHIVE
if ($archive -and (-not $version -or -not (Test-Path $archive) -or "$($env:SPANDECK_ARCHIVE_SHA256)".Length -ne 64)) {
    throw 'SPANDECK_ARCHIVE needs the file, SPANDECK_VERSION and SPANDECK_ARCHIVE_SHA256.'
}
if (-not $version) {
    # The "latest" page redirects to the newest release's tag.
    try {
        $request = [Net.WebRequest]::Create("$releases/latest")
        $request.Method = 'HEAD'
        $response = $request.GetResponse()
        $version = ($response.ResponseUri.AbsolutePath -split '/')[-1]
        $response.Close()
    }
    catch { throw "The release could not be downloaded: $releases did not answer. $_" }
}
if ($version -notmatch '^v[0-9A-Za-z.\-]+$') {
    throw "Could not find the latest release of $repo."
}

$name = "spandeck-$version-$target"
$url = "$releases/download/$version/$name.zip"
$tmp = Join-Path ([IO.Path]::GetTempPath()) ("spandeck-install-" + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $tmp | Out-Null
try {
    if ($archive) {
        Write-Host "Installing $name from $archive"
        $zip = $archive
        $want = "$($env:SPANDECK_ARCHIVE_SHA256)".ToLower()
    }
    else {
        Write-Host "Downloading $name"
        $zip = Join-Path $tmp "$name.zip"
        try {
            Invoke-WebRequest -UseBasicParsing -Uri $url -OutFile $zip
            $published = (Invoke-WebRequest -UseBasicParsing -Uri "$url.sha256").Content
        }
        catch { throw "The release could not be downloaded: $_" }
        if ($published -is [byte[]]) { $published = [Text.Encoding]::ASCII.GetString($published) }
        $want = ($published.Trim() -split '\s+')[0].ToLower()
    }
    # .NET directly, not Get-FileHash or Expand-Archive: those two are script
    # modules, and where scripts are restricted (seen on a CI machine)
    # PowerShell cannot load them and says the command does not exist.
    $sha = [System.Security.Cryptography.SHA256]::Create()
    $stream = [System.IO.File]::OpenRead($zip)
    try { $got = ([System.BitConverter]::ToString($sha.ComputeHash($stream)) -replace '-', '').ToLower() } finally { $stream.Dispose(); $sha.Dispose() }
    if ($want.Length -ne 64 -or $want -ne $got) {
        if ($archive) { throw "$archive does not have the SHA-256 given for it. Nothing was installed." }
        throw 'The release could not be downloaded: the download does not match its published checksum. Nothing was installed.'
    }
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    [System.IO.Compression.ZipFile]::ExtractToDirectory($zip, $tmp)

    New-Item -ItemType Directory -Path $binDir -Force | Out-Null
    foreach ($exe in 'spandeck.exe', 'sd.exe', 'spandeckd.exe', 'spandeck-pane-host.exe') {
        $from = Join-Path (Join-Path $tmp $name) $exe
        if (-not (Test-Path $from)) { continue }
        $to = Join-Path $binDir $exe
        # A program that is running cannot be written over, but it can be
        # renamed: the running daemon keeps its old file until it is upgraded,
        # and the pane host keeps its own for as long as it holds a pane.
        if (Test-Path $to) {
            $old = "$to.old"
            Remove-Item -Path $old -Force -ErrorAction SilentlyContinue
            Rename-Item -Path $to -NewName ([IO.Path]::GetFileName($old))
        }
        Copy-Item -Path $from -Destination $to
        # Freshly downloaded programs carry a mark that makes Windows ask
        # before running them; these were just checked against their checksum.
        Unblock-File -Path $to -ErrorAction SilentlyContinue
        Remove-Item -Path "$to.old" -Force -ErrorAction SilentlyContinue
    }
    Write-Host "Installed spandeck $version to $binDir (checksum verified)"
}
finally {
    Remove-Item -Path $tmp -Recurse -Force -ErrorAction SilentlyContinue
}

# The user's own PATH (no administrator needed). New terminals pick it up.
$userPath = if ($env:SPANDECK_NO_PATH) { $binDir } else { [Environment]::GetEnvironmentVariable('Path', 'User') }
$entries = @()
if ($userPath) { $entries = $userPath -split ';' | Where-Object { $_ } }
if (-not ($entries | Where-Object { $_.TrimEnd('\') -ieq $binDir.TrimEnd('\') })) {
    [Environment]::SetEnvironmentVariable('Path', (($entries + $binDir) -join ';'), 'User')
    Write-Host "Added $binDir to your PATH. Open a new terminal to use the spandeck command."
}
if (-not (($env:Path -split ';') | Where-Object { $_.TrimEnd('\') -ieq $binDir.TrimEnd('\') })) {
    $env:Path = "$env:Path;$binDir"
}

if ($env:SPANDECK_NO_SERVICE) {
    Write-Host 'Next:  spandeck service --install   then   spandeck doctor'
    return
}
# Start at log on: a Scheduled Task for this user. `service --install` writes
# the script that registers it and prints nothing else we need.
$spandeck = Join-Path $binDir 'spandeck.exe'
& $spandeck service --install | Out-Null
if ($LASTEXITCODE -ne 0) { throw 'spandeck service --install failed.' }
$task = Join-Path (Join-Path $env:APPDATA 'spandeck') 'start-at-login.ps1'
# Run in its own PowerShell: the default policy on Windows does not let a
# script file run, and this changes it for that one process only.
& powershell.exe -NoProfile -ExecutionPolicy Bypass -File $task
if ($LASTEXITCODE -eq 0) {
    Write-Host 'spandeck starts when you log on, and is starting now.'
}
else {
    Write-Host 'spandeck is installed, but could not be set to start when you log on.'
    Write-Host 'Start it yourself with:  spandeck daemon'
}
Write-Host 'Next:  spandeck doctor'
