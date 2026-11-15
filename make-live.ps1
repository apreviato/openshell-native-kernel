<#
.SYNOPSIS
Build the OpenShell Native UEFI live ISO, write it to a USB stick and verify it.
.EXAMPLE
  .\make-live.ps1 -BuildOnly
.EXAMPLE
  .\make-live.ps1 -Iso .\out\native-example\openshell-native-live-2.1.0-amd64.iso -DiskNumber 2
.EXAMPLE
  .\make-live.ps1 -Bundle ..\openshell\apps\desktop\dist-bundle\openshell-desktop -InstallImage
#>
[CmdletBinding(SupportsShouldProcess = $true)]
param(
    [string]$OpenShellDir = (Join-Path $PSScriptRoot '..\openshell'),
    [string]$Bundle,
    [string]$Iso,
    [ValidateRange(-1, 2147483647)][int]$DiskNumber = -1,
    [switch]$InstallImage,
    [switch]$BuildOnly
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$writeUsb = -not $BuildOnly -and -not $InstallImage
if ($Iso -and ($Bundle -or $InstallImage -or $BuildOnly)) { throw 'Use -Iso only to write an existing live ISO.' }
if (-not $writeUsb -and $DiskNumber -ge 0) { throw '-DiskNumber applies only to live USB creation.' }
$action = if ($InstallImage) { 'install-image' } else { 'live-iso' }
if ($WhatIfPreference) {
    if ($writeUsb) { Write-Host '[plan] Select and confirm a USB disk before building, then write and verify it.' }
    if (-not $Iso) { Write-Host "[plan] Build and verify $action in $PSScriptRoot\out." }
    return
}

if ($writeUsb) {
    $principal = New-Object Security.Principal.WindowsPrincipal([Security.Principal.WindowsIdentity]::GetCurrent())
    if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
        throw 'Run PowerShell as administrator to create a live USB, or use -BuildOnly.'
    }
    . (Join-Path $PSScriptRoot 'tools\build\usb.ps1')
    $minimumSize = if ($Iso) { (Get-Item -LiteralPath $Iso).Length } else { 0 }
    $target = Confirm-OpenShellNativeUsb -DiskNumber $DiskNumber -MinimumSize $minimumSize
    if (-not $PSCmdlet.ShouldProcess("USB disk $($target.Number)", 'Write and verify OpenShell Native')) { return }
} elseif (-not $PSCmdlet.ShouldProcess($PSScriptRoot, "Build and verify $action")) { return }

if (-not $Iso -and -not $Bundle) {
    $OpenShellDir = (Resolve-Path -LiteralPath $OpenShellDir).Path
    $pnpm = Get-Command pnpm.cmd -ErrorAction SilentlyContinue
    $managerArgs = @()
    if ($pnpm) {
        $manager = $pnpm.Source
    } else {
        $manager = (Get-Command npx.cmd -ErrorAction Stop).Source
        $managerArgs = @('-y', 'pnpm@10.8.1')
    }
    Push-Location -LiteralPath $OpenShellDir
    try {
        & $manager @managerArgs install --frozen-lockfile --config.confirmModulesPurge=false --filter '@openshell/desktop...' --filter '@openshell/web...'
        if ($LASTEXITCODE -ne 0) { throw 'Desktop dependency installation failed.' }
        & $manager @managerArgs --filter '@openshell/desktop' bundle
        if ($LASTEXITCODE -ne 0) { throw 'Desktop bundle build failed.' }
    } finally { Pop-Location }
    $Bundle = Join-Path $OpenShellDir 'apps\desktop\dist-bundle\openshell-desktop'
}

$media = $Iso
if (-not $Iso) {
    $Bundle = (Resolve-Path -LiteralPath $Bundle).Path
    $node = (Get-Command node -ErrorAction Stop).Source
    $runner = Join-Path $PSScriptRoot 'tools\build\native.mjs'
    & $node $runner $action $Bundle | ForEach-Object {
        Write-Host $_
        if ($_ -match '^Media: (.+)$') { $media = $Matches[1] }
    }
    if ($LASTEXITCODE -ne 0 -or -not $media) { throw 'OpenShell Native media build failed.' }
}

$media = (Resolve-Path -LiteralPath $media).Path
$expected = ((Get-Content -LiteralPath "$media.sha256" -TotalCount 1) -split '\s+')[0]
if ($expected -notmatch '^[0-9a-fA-F]{64}$') { throw "Invalid SHA-256 in $media.sha256" }
$secureBoot = @'
Secure Boot: the stick starts like OpenShell Live's (Debian's shim and GRUB). The first time on a
computer with Secure Boot on, enroll OpenShell Native's key once: in the boot menu choose
"More options > Enroll the OpenShell Native key", then in MokManager Enroll key from disk >
ENROLL_THIS_KEY_IN_MOKMANAGER.cer > Continue > Yes, and reboot.
'@
if ($writeUsb) {
    Write-OpenShellNativeUsb -Iso $media -ExpectedHash $expected -Target $target
    Write-Host $secureBoot
    return
}
if ((Get-FileHash -LiteralPath $media -Algorithm SHA256).Hash -ne $expected) {
    throw "SHA-256 verification failed for $media"
}
Write-Host "Verified image: $media"
Write-Host $secureBoot
