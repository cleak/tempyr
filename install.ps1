[CmdletBinding()]
param(
    [string]$InstallRoot = $(if ($env:TEMPYR_INSTALL_ROOT) {
            $env:TEMPYR_INSTALL_ROOT
        } else {
            Join-Path $env:LOCALAPPDATA "Tempyr"
        }),
    [switch]$NoPathUpdate
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function Resolve-CanonicalPath {
    param(
        [Parameter(Mandatory)]
        [string]$Path
    )

    if (Test-Path -LiteralPath $Path) {
        return (Get-Item -LiteralPath $Path -ErrorAction Stop).FullName.TrimEnd('\')
    }

    return [System.IO.Path]::GetFullPath($Path).TrimEnd('\')
}

function Test-FileLocked {
    param(
        [Parameter(Mandatory)]
        [string]$Path
    )

    if (-not (Test-Path -LiteralPath $Path)) {
        return $false
    }

    try {
        $stream = [System.IO.File]::Open(
            $Path,
            [System.IO.FileMode]::Open,
            [System.IO.FileAccess]::ReadWrite,
            [System.IO.FileShare]::None
        )
        $stream.Dispose()
        return $false
    } catch [System.IO.IOException] {
        return $true
    }
}

function Get-TargetProcessIds {
    param(
        [Parameter(Mandatory)]
        [string]$BinaryPath
    )

    $targetPath = Resolve-CanonicalPath -Path $BinaryPath
    $matches = New-Object System.Collections.Generic.List[int]

    foreach ($process in Get-CimInstance Win32_Process -Filter "Name='tempyr.exe'") {
        if (-not $process.ExecutablePath) {
            continue
        }

        try {
            $candidate = Resolve-CanonicalPath -Path $process.ExecutablePath
        } catch {
            continue
        }

        if ([string]::Equals($candidate, $targetPath, [System.StringComparison]::OrdinalIgnoreCase)) {
            $matches.Add([int]$process.ProcessId)
        }
    }

    return $matches.ToArray()
}

function Stop-TargetProcesses {
    param(
        [Parameter(Mandatory)]
        [string]$BinaryPath
    )

    $processIds = @(Get-TargetProcessIds -BinaryPath $BinaryPath)
    if ($processIds.Count -eq 0) {
        return $false
    }

    Write-Host "Detected a locked Tempyr install at $BinaryPath. Stopping matching processes: $($processIds -join ', ')"
    Stop-Process -Id $processIds -Force

    foreach ($processId in $processIds) {
        try {
            Wait-Process -Id $processId -Timeout 15 -ErrorAction Stop
        } catch {
        }
    }

    return $true
}

function Normalize-PathEntry {
    param(
        [Parameter(Mandatory)]
        [string]$Path
    )

    if ([string]::IsNullOrWhiteSpace($Path)) {
        return $null
    }

    return (Resolve-CanonicalPath -Path $Path)
}

function Output-IndicatesLockError {
    param(
        [Parameter(Mandatory)]
        [string]$Output
    )

    return $Output -match "being used by another process|cannot access the file|Access is denied"
}

function Broadcast-EnvironmentChange {
    if (-not ("Tempyr.Win32.NativeMethods" -as [type])) {
        Add-Type -Namespace Tempyr.Win32 -Name NativeMethods -MemberDefinition @"
using System;
using System.Runtime.InteropServices;

public static class NativeMethods {
    [DllImport("user32.dll", CharSet = CharSet.Auto, SetLastError = true)]
    public static extern IntPtr SendMessageTimeout(
        IntPtr hWnd,
        int Msg,
        IntPtr wParam,
        string lParam,
        int fuFlags,
        int uTimeout,
        out IntPtr lpdwResult);
}
"@
    }

    $result = [IntPtr]::Zero
    [void][Tempyr.Win32.NativeMethods]::SendMessageTimeout(
        [IntPtr]0xffff,
        0x1A,
        [IntPtr]::Zero,
        "Environment",
        0x0002,
        5000,
        [ref]$result
    )
}

function Ensure-UserPathContains {
    param(
        [Parameter(Mandatory)]
        [string]$PathToAdd
    )

    $normalizedTarget = Normalize-PathEntry -Path $PathToAdd
    $userPath = [Environment]::GetEnvironmentVariable("Path", "User")
    $entries = @()
    if (-not [string]::IsNullOrWhiteSpace($userPath)) {
        $entries = $userPath.Split(';', [System.StringSplitOptions]::RemoveEmptyEntries)
    }

    foreach ($entry in $entries) {
        $normalizedEntry = Normalize-PathEntry -Path $entry
        if (
            $normalizedEntry -and
            [string]::Equals($normalizedEntry, $normalizedTarget, [System.StringComparison]::OrdinalIgnoreCase)
        ) {
            if (-not (($env:Path -split ';') | Where-Object {
                        [string]::Equals(
                            (Normalize-PathEntry -Path $_),
                            $normalizedTarget,
                            [System.StringComparison]::OrdinalIgnoreCase
                        )
                    })) {
                $env:Path = "$normalizedTarget;$env:Path"
            }
            return $false
        }
    }

    $updatedPath = if ([string]::IsNullOrWhiteSpace($userPath)) {
        $normalizedTarget
    } else {
        "$($userPath.TrimEnd(';'));$normalizedTarget"
    }

    [Environment]::SetEnvironmentVariable("Path", $updatedPath, "User")
    $env:Path = "$normalizedTarget;$env:Path"
    Broadcast-EnvironmentChange
    return $true
}

function Invoke-CargoInstall {
    param(
        [Parameter(Mandatory)]
        [string]$CratePath,
        [Parameter(Mandatory)]
        [string]$InstallRootPath
    )

    $cargoExe = (Get-Command cargo -ErrorAction Stop).Source
    $stdoutFile = New-TemporaryFile
    $stderrFile = New-TemporaryFile
    try {
        $process = Start-Process `
            -FilePath $cargoExe `
            -ArgumentList @(
                "install",
                "--path", $CratePath,
                "--root", $InstallRootPath,
                "--locked",
                "--force",
                "--bin", "tempyr"
            ) `
            -NoNewWindow `
            -Wait `
            -PassThru `
            -RedirectStandardOutput $stdoutFile.FullName `
            -RedirectStandardError $stderrFile.FullName

        $stdoutLines = @()
        $stderrLines = @()
        if ((Get-Item -LiteralPath $stdoutFile.FullName).Length -gt 0) {
            $stdoutLines = @(Get-Content -LiteralPath $stdoutFile.FullName)
        }
        if ((Get-Item -LiteralPath $stderrFile.FullName).Length -gt 0) {
            $stderrLines = @(Get-Content -LiteralPath $stderrFile.FullName)
        }

        foreach ($line in ($stderrLines + $stdoutLines)) {
            Write-Host $line
        }

        $output = ($stderrLines + $stdoutLines) -join [Environment]::NewLine
        return [pscustomobject]@{
            ExitCode = $process.ExitCode
            Output   = $output
        }
    } finally {
        Remove-Item -LiteralPath $stdoutFile.FullName -Force -ErrorAction SilentlyContinue
        Remove-Item -LiteralPath $stderrFile.FullName -Force -ErrorAction SilentlyContinue
    }
}

if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    throw "cargo is required but was not found in PATH."
}

$scriptRoot = Split-Path -Parent $MyInvocation.MyCommand.Path
$cratePath = Join-Path $scriptRoot "crates/tempyr-cli"
$binDir = Join-Path $InstallRoot "bin"
$targetBinary = Join-Path $binDir "tempyr.exe"

if (-not (Test-Path -LiteralPath (Join-Path $cratePath "Cargo.toml"))) {
    throw "Could not find crates/tempyr-cli/Cargo.toml relative to $scriptRoot."
}

$installResult = Invoke-CargoInstall -CratePath $cratePath -InstallRootPath $InstallRoot
if ($installResult.ExitCode -ne 0) {
    if (
        (Test-FileLocked -Path $targetBinary) -and
        (Output-IndicatesLockError -Output $installResult.Output) -and
        (Stop-TargetProcesses -BinaryPath $targetBinary)
    ) {
        Write-Host "Retrying cargo install after stopping matching Tempyr processes..."
        $installResult = Invoke-CargoInstall -CratePath $cratePath -InstallRootPath $InstallRoot
    }
}

if ($installResult.ExitCode -ne 0) {
    throw "cargo install failed."
}

if (-not $NoPathUpdate) {
    $addedToPath = Ensure-UserPathContains -PathToAdd $binDir
}

Write-Host ""
Write-Host "Tempyr installed to $targetBinary"
if (-not $NoPathUpdate) {
    if ($addedToPath) {
        Write-Host "Added $binDir to the user PATH."
    } else {
        Write-Host "$binDir is already present in the user PATH."
    }
}
