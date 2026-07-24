param(
    [Parameter(Mandatory = $false)]
    [string]$BinaryPattern = "target/debug/deps/app_lib-*.exe"
)

$ErrorActionPreference = "Stop"

$binary = Get-ChildItem -Path $BinaryPattern -File |
    Sort-Object LastWriteTimeUtc -Descending |
    Select-Object -First 1

if ($null -eq $binary) {
    Write-Warning "No Windows test binary matched '$BinaryPattern'."
    exit 0
}

$vswhere = Join-Path ${env:ProgramFiles(x86)} "Microsoft Visual Studio/Installer/vswhere.exe"
if (-not (Test-Path -LiteralPath $vswhere -PathType Leaf)) {
    Write-Warning "vswhere.exe is unavailable; cannot inspect PE imports."
    exit 0
}

$visualStudio = (& $vswhere `
    -latest `
    -products * `
    -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 `
    -property installationPath).Trim()

if ([string]::IsNullOrWhiteSpace($visualStudio)) {
    Write-Warning "Visual Studio C++ tools are unavailable; cannot inspect PE imports."
    exit 0
}

$toolsRoot = Join-Path $visualStudio "VC/Tools/MSVC"
$toolsVersion = Get-ChildItem -LiteralPath $toolsRoot -Directory |
    Sort-Object Name -Descending |
    Select-Object -First 1
$dumpbin = if ($null -eq $toolsVersion) {
    $null
} else {
    Join-Path $toolsVersion.FullName "bin/Hostx64/x64/dumpbin.exe"
}

if ($null -eq $dumpbin -or -not (Test-Path -LiteralPath $dumpbin -PathType Leaf)) {
    Write-Warning "dumpbin.exe is unavailable; cannot inspect PE imports."
    exit 0
}

Write-Output "::group::Windows test executable imports"
Write-Output "Binary: $($binary.FullName)"
& $dumpbin /nologo /imports $binary.FullName
$dumpbinExit = $LASTEXITCODE
Write-Output "::endgroup::"

if ($dumpbinExit -ne 0) {
    Write-Warning "dumpbin.exe exited with code $dumpbinExit."
}

$expectedSystemExports = @(
    @{ Dll = "advapi32.dll"; Symbol = "ConvertStringSecurityDescriptorToSecurityDescriptorW" },
    @{ Dll = "advapi32.dll"; Symbol = "OpenProcessToken" },
    @{ Dll = "advapi32.dll"; Symbol = "RegOpenKeyExW" },
    @{ Dll = "advapi32.dll"; Symbol = "RegQueryValueExW" },
    @{ Dll = "advapi32.dll"; Symbol = "RegSetValueExW" },
    @{ Dll = "advapi32.dll"; Symbol = "SetFileSecurityW" },
    @{ Dll = "kernel32.dll"; Symbol = "GetNamedPipeClientProcessId" },
    @{ Dll = "kernel32.dll"; Symbol = "MoveFileExW" },
    @{ Dll = "user32.dll"; Symbol = "SendMessageTimeoutW" }
)

Write-Output "::group::Phase 3 Win32 export availability"
foreach ($expected in $expectedSystemExports) {
    $handle = [System.Runtime.InteropServices.NativeLibrary]::Load($expected.Dll)
    try {
        $address = [IntPtr]::Zero
        $available = [System.Runtime.InteropServices.NativeLibrary]::TryGetExport(
            $handle,
            $expected.Symbol,
            [ref]$address
        )
        Write-Output "$($expected.Dll)!$($expected.Symbol): $available"
    } finally {
        [System.Runtime.InteropServices.NativeLibrary]::Free($handle)
    }
}
Write-Output "::endgroup::"

exit 0
