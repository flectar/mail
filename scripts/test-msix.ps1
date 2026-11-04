#Requires -Version 5.1
<#
.SYNOPSIS
Installs a Microsoft Store package with a throwaway signature, starts the app
with its package identity, and removes it again.

.DESCRIPTION
Run this only on a disposable machine such as a CI runner: it trusts a new
self-signed certificate for package installs while it runs. The Store signs
uploaded packages itself, so only the tested copy carries this signature.

The checks cover what packaging changes for the app: the package installs
and starts, the app's AppData writes land in the package's private folder,
and removing the package removes that data too.

Windows PowerShell 5.1 runs it, because PowerShell 7 cannot load the Appx
module natively everywhere.

.PARAMETER Package
The unsigned .msix or .msixbundle to test.
#>
param(
    [Parameter(Mandatory)] [string] $Package
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

function Find-SdkTool([string] $Name) {
    $kits = Join-Path ${env:ProgramFiles(x86)} 'Windows Kits\10\bin'
    $architectures = if ($env:PROCESSOR_ARCHITECTURE -eq 'ARM64') { 'arm64', 'x64', 'x86' } else { 'x64', 'x86' }
    $versions = Get-ChildItem $kits -Directory | Where-Object Name -Match '^10\.[0-9.]+$' |
        Sort-Object { [version] $_.Name } -Descending
    foreach ($version in $versions) {
        foreach ($architecture in $architectures) {
            $tool = Join-Path $version.FullName "$architecture\$Name"
            if (Test-Path $tool) { return $tool }
        }
    }
    throw "$Name was not found in a Windows SDK under $kits"
}

function Get-PackageIdentity([string] $Path) {
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $archive = [IO.Compression.ZipFile]::OpenRead($Path)
    try {
        $entry = $archive.GetEntry('AppxManifest.xml')
        if (-not $entry) { $entry = $archive.GetEntry('AppxMetadata/AppxBundleManifest.xml') }
        if (-not $entry) { throw "$Path has no package manifest" }
        $reader = [IO.StreamReader]::new($entry.Open())
        try { [xml] $manifest = $reader.ReadToEnd() } finally { $reader.Dispose() }
    } finally {
        $archive.Dispose()
    }
    return $manifest.DocumentElement.Identity
}

# Start a packaged app by its application user model ID, as Start does. This
# gives the process its package identity, which running the executable from
# the install folder would not.
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;

public static class PackagedApp
{
    [ComImport, Guid("2e941141-7f97-4756-ba1d-9decde894a3d"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    private interface IApplicationActivationManager
    {
        [PreserveSig]
        int ActivateApplication([MarshalAs(UnmanagedType.LPWStr)] string appUserModelId,
            [MarshalAs(UnmanagedType.LPWStr)] string arguments, int options, out uint processId);
    }

    [ComImport, Guid("45BA127D-10A8-46EA-8AB7-56EA9078943C")]
    private class ApplicationActivationManager { }

    public static uint Start(string appUserModelId)
    {
        var manager = (IApplicationActivationManager)new ApplicationActivationManager();
        uint processId;
        Marshal.ThrowExceptionForHR(manager.ActivateApplication(appUserModelId, null, 0, out processId));
        return processId;
    }
}
'@

$Package = (Resolve-Path $Package).Path
$identity = Get-PackageIdentity $Package
$work = Join-Path ([IO.Path]::GetTempPath()) "flectar-msix-test-$([guid]::NewGuid())"
New-Item -ItemType Directory $work | Out-Null
$signed = Join-Path $work (Split-Path $Package -Leaf)
Copy-Item $Package $signed

# The signing certificate's subject must equal the manifest's publisher.
$certificate = New-SelfSignedCertificate -Type Custom -Subject $identity.Publisher `
    -KeyUsage DigitalSignature -FriendlyName 'Flectar Mail package test' `
    -CertStoreLocation 'Cert:\CurrentUser\My' `
    -TextExtension @('2.5.29.37={text}1.3.6.1.5.5.7.3.3', '2.5.29.19={text}')
$trustedCertificate = "Cert:\LocalMachine\TrustedPeople\$($certificate.Thumbprint)"
$installed = $null
$processId = $null
try {
    $certificateFile = Join-Path $work 'package-test.cer'
    Export-Certificate -Cert $certificate -FilePath $certificateFile | Out-Null
    Import-Certificate -FilePath $certificateFile -CertStoreLocation 'Cert:\LocalMachine\TrustedPeople' | Out-Null
    & (Find-SdkTool 'signtool.exe') sign /fd SHA256 /sha1 $certificate.Thumbprint $signed
    if ($LASTEXITCODE -ne 0) { throw "signtool failed with exit code $LASTEXITCODE" }

    Add-AppxPackage -Path $signed
    $installed = Get-AppxPackage -Name $identity.Name
    if (-not $installed) { throw "$($identity.Name) is not installed after Add-AppxPackage" }
    $installed | Format-List Name, PackageFullName, Architecture, Version, InstallLocation

    $packageData = Join-Path $env:LOCALAPPDATA "Packages\$($installed.PackageFamilyName)"
    $database = Join-Path $packageData 'LocalCache\Roaming\flectar-mail\flectar-mail.db'
    $unpackagedData = Join-Path $env:APPDATA 'flectar-mail'
    $hadUnpackagedData = Test-Path $unpackagedData

    $processId = [PackagedApp]::Start("$($installed.PackageFamilyName)!FlectarMail")
    $deadline = (Get-Date).AddSeconds(90)
    while (-not (Test-Path $database)) {
        if (-not (Get-Process -Id $processId -ErrorAction SilentlyContinue)) {
            throw 'The packaged app exited during startup'
        }
        if ((Get-Date) -gt $deadline) { throw "The packaged app did not create $database" }
        Start-Sleep -Milliseconds 500
    }
    $process = Get-Process -Id $processId
    if (-not $process.Path.StartsWith($installed.InstallLocation, [StringComparison]::OrdinalIgnoreCase)) {
        throw "Process $processId runs $($process.Path), not the packaged executable"
    }
    if (-not $hadUnpackagedData -and (Test-Path $unpackagedData)) {
        throw "The packaged app wrote $unpackagedData instead of its package folder"
    }
    Start-Sleep -Seconds 5
    if ($process.HasExited) { throw "The packaged app exited with code $($process.ExitCode) after startup" }
    Write-Host "The packaged app is running and keeps its data in $packageData"
} finally {
    if ($processId) {
        Stop-Process -Id $processId -Force -ErrorAction SilentlyContinue
        Wait-Process -Id $processId -Timeout 30 -ErrorAction SilentlyContinue
    }
    if ($installed) { Remove-AppxPackage -Package $installed.PackageFullName }
    Remove-Item $trustedCertificate -ErrorAction SilentlyContinue
    Remove-Item "Cert:\CurrentUser\My\$($certificate.Thumbprint)" -ErrorAction SilentlyContinue
    Remove-Item $work -Recurse -Force -ErrorAction SilentlyContinue
}

if (Get-AppxPackage -Name $identity.Name) { throw "$($identity.Name) is still installed" }
if (Test-Path $packageData) { throw "Removing the package left $packageData behind" }
Write-Host "Installed, started and removed $(Split-Path $Package -Leaf)"
