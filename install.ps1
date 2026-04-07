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
    } catch [System.UnauthorizedAccessException] {
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
    $matchingProcessIds = New-Object System.Collections.Generic.List[int]

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
            $matchingProcessIds.Add([int]$process.ProcessId)
        }
    }

    return $matchingProcessIds.ToArray()
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

    foreach ($processId in $processIds) {
        Stop-Process -Id $processId -Force -ErrorAction SilentlyContinue
        Wait-Process -Id $processId -Timeout 15 -ErrorAction SilentlyContinue
    }

    return @((Get-TargetProcessIds -BinaryPath $BinaryPath)).Count -eq 0
}

function ConvertTo-CanonicalPathEntry {
    param(
        [Parameter(Mandatory)]
        [string]$Path
    )

    if ([string]::IsNullOrWhiteSpace($Path)) {
        return $null
    }

    $expandedPath = [Environment]::ExpandEnvironmentVariables($Path.Trim())
    return (Resolve-CanonicalPath -Path $expandedPath)
}

function ConvertTo-CanonicalPathEntrySafe {
    param(
        [Parameter(Mandatory)]
        [string]$Path
    )

    try {
        return ConvertTo-CanonicalPathEntry -Path $Path
    } catch {
        return $null
    }
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
        Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;

namespace Tempyr.Win32 {
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

    $normalizedTarget = ConvertTo-CanonicalPathEntry -Path $PathToAdd
    $userPath = [Environment]::GetEnvironmentVariable("Path", "User")
    $machinePath = [Environment]::GetEnvironmentVariable("Path", "Machine")
    $persistedEntries = @()
    foreach ($scopePath in @($userPath, $machinePath)) {
        if (-not [string]::IsNullOrWhiteSpace($scopePath)) {
            $persistedEntries += $scopePath.Split(';', [System.StringSplitOptions]::RemoveEmptyEntries)
        }
    }

    $persistedPathContainsTarget = $false
    foreach ($entry in $persistedEntries) {
        $normalizedEntry = ConvertTo-CanonicalPathEntrySafe -Path $entry
        if ($normalizedEntry -and [string]::Equals($normalizedEntry, $normalizedTarget, [System.StringComparison]::OrdinalIgnoreCase)) {
            $persistedPathContainsTarget = $true
            break
        }
    }

    $currentProcessPathContainsTarget = $false
    foreach ($entry in ($env:Path -split ';')) {
        $normalizedEntry = ConvertTo-CanonicalPathEntrySafe -Path $entry
        if ($normalizedEntry -and [string]::Equals($normalizedEntry, $normalizedTarget, [System.StringComparison]::OrdinalIgnoreCase)) {
            $currentProcessPathContainsTarget = $true
            break
        }
    }

    if ($persistedPathContainsTarget) {
        if (-not $currentProcessPathContainsTarget) {
            $env:Path = "$normalizedTarget;$env:Path"
        }
        return $false
    }

    $updatedPath = if ([string]::IsNullOrWhiteSpace($userPath)) {
        $normalizedTarget
    } else {
        "$($userPath.TrimEnd(';'));$normalizedTarget"
    }

    [Environment]::SetEnvironmentVariable("Path", $updatedPath, "User")
    if (-not $currentProcessPathContainsTarget) {
        $env:Path = "$normalizedTarget;$env:Path"
    }
    Broadcast-EnvironmentChange
    return $true
}

function Get-CargoInstallFailureMessage {
    param(
        [Parameter(Mandatory)]
        [pscustomobject]$InstallResult
    )

    $message = "cargo install failed with exit code $($InstallResult.ExitCode)."
    if (-not [string]::IsNullOrWhiteSpace($InstallResult.Output)) {
        $output = $InstallResult.Output.TrimEnd()
        if (-not [string]::IsNullOrWhiteSpace($output)) {
            $message = "$message`n$output"
        }
    }

    return $message
}

function ConvertTo-WindowsArgument {
    param(
        [AllowEmptyString()]
        [string]$Value
    )

    if ([string]::IsNullOrEmpty($Value)) {
        return '""'
    }

    if ($Value -notmatch '[\s"]') {
        return $Value
    }

    $builder = New-Object System.Text.StringBuilder
    [void]$builder.Append('"')
    $backslashCount = 0

    foreach ($char in $Value.ToCharArray()) {
        if ($char -eq '\') {
            $backslashCount += 1
            continue
        }

        if ($char -eq '"') {
            if ($backslashCount -gt 0) {
                [void]$builder.Append(('\' * ($backslashCount * 2)))
                $backslashCount = 0
            }

            [void]$builder.Append('\"')
            continue
        }

        if ($backslashCount -gt 0) {
            [void]$builder.Append(('\' * $backslashCount))
            $backslashCount = 0
        }

        [void]$builder.Append($char)
    }

    if ($backslashCount -gt 0) {
        [void]$builder.Append(('\' * ($backslashCount * 2)))
    }

    [void]$builder.Append('"')
    return $builder.ToString()
}

function Invoke-CargoInstall {
    param(
        [Parameter(Mandatory)]
        [string]$CratePath,
        [Parameter(Mandatory)]
        [string]$InstallRootPath
    )

    $cargoExe = (Get-Command cargo -ErrorAction Stop).Source
    $cargoArgs = @(
        "install",
        "--path", $CratePath,
        "--root", $InstallRootPath,
        "--locked",
        "--force",
        "--bin", "tempyr"
    )
    $stdoutFile = New-TemporaryFile
    $stderrFile = New-TemporaryFile
    try {
        # Windows PowerShell 5.1 turns native stderr into a terminating NativeCommandError
        # when ErrorActionPreference=Stop, even for cargo's normal progress output.
        $argumentLine = ($cargoArgs | ForEach-Object { ConvertTo-WindowsArgument -Value "$_" }) -join ' '
        $process = Start-Process `
            -FilePath $cargoExe `
            -ArgumentList $argumentLine `
            -WorkingDirectory $CratePath `
            -RedirectStandardOutput $stdoutFile.FullName `
            -RedirectStandardError $stderrFile.FullName `
            -NoNewWindow `
            -PassThru `
            -Wait
        $exitCode = $process.ExitCode

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
            ExitCode = $exitCode
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
    throw (Get-CargoInstallFailureMessage -InstallResult $installResult)
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
        Write-Host "$binDir is already present in PATH."
    }
}
