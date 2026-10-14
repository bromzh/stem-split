<#
.SYNOPSIS
Builds the release and packs it into dist\stem-split-<version>-windows-x64.zip.

.DESCRIPTION
The archive holds everything the application needs on a Windows machine that
has neither Rust nor Visual Studio:

  stem-split.exe      the application; ONNX Runtime is linked into it
  DirectML.dll        GPU acceleration; the copy shipped with Windows 10 is too old
  vcruntime140*.dll   the Visual C++ runtime ONNX Runtime was built against,
  msvcp140*.dll       placed next to the application so nothing has to be installed
  README.txt          what a user needs to know

The runtime libraries are taken from the redistributable folder of a Visual
Studio or Build Tools installation, which is where Microsoft's licence allows
them to be copied from. If several versions are installed, the newest is used.

.EXAMPLE
powershell -ExecutionPolicy Bypass -File tools\package.ps1
#>

$ErrorActionPreference = 'Stop'

$root = Split-Path -Parent $PSScriptRoot
$runtimeLibraries = 'vcruntime140.dll', 'vcruntime140_1.dll', 'msvcp140.dll', 'msvcp140_1.dll'

function Find-RuntimeFolder {
    $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
    if (-not (Test-Path $vswhere)) {
        throw "vswhere.exe was not found; Visual Studio or its Build Tools are required to package a release."
    }
    $installs = & $vswhere -all -products * -property installationPath
    if (-not $installs) {
        throw "No Visual Studio installation was found."
    }
    # The runtime must be at least as new as the compiler the application was
    # built with, so the newest one on the machine is taken: the folder
    # ...\MSVC\<version>\x64\Microsoft.VC*.CRT with the highest version.
    $folder = $installs |
        ForEach-Object { Get-ChildItem (Join-Path $_ 'VC\Redist\MSVC\*\x64\Microsoft.VC*.CRT') -Directory -ErrorAction SilentlyContinue } |
        Sort-Object { [version]$_.Parent.Parent.Name } |
        Select-Object -Last 1
    if (-not $folder) {
        throw "No Visual C++ redistributable folder was found in the Visual Studio installations."
    }
    return $folder.FullName
}

Push-Location $root
try {
    cargo build --release -p stem-split
    if ($LASTEXITCODE -ne 0) {
        throw "The build failed."
    }

    $metadata = cargo metadata --no-deps --format-version 1 | ConvertFrom-Json
    $version = ($metadata.packages | Where-Object { $_.name -eq 'stem-split' }).version
    $name = "stem-split-$version-windows-x64"
    $stage = Join-Path $root "dist\$name"
    $archive = Join-Path $root "dist\$name.zip"

    if (Test-Path $stage) { Remove-Item -Recurse -Force $stage }
    if (Test-Path $archive) { Remove-Item -Force $archive }
    New-Item -ItemType Directory -Force $stage | Out-Null

    Copy-Item (Join-Path $root 'target\release\stem-split.exe') $stage
    Copy-Item (Join-Path $root 'target\release\DirectML.dll') $stage
    $runtime = Find-RuntimeFolder
    foreach ($library in $runtimeLibraries) {
        Copy-Item (Join-Path $runtime $library) $stage
    }
    Copy-Item (Join-Path $root 'tools\release-readme.txt') (Join-Path $stage 'README.txt')

    Compress-Archive -Path $stage -DestinationPath $archive

    Write-Output "Runtime libraries from: $runtime"
    Write-Output "Contents:"
    Get-ChildItem $stage | ForEach-Object { Write-Output ("  {0,12:N0}  {1}" -f $_.Length, $_.Name) }
    Write-Output ("Archive: {0} ({1:N1} MB)" -f $archive, ((Get-Item $archive).Length / 1MB))
}
finally {
    Pop-Location
}
