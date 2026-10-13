#requires -Version 5.1
[CmdletBinding()]
param([switch] $VirtualDisk)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$writer = Join-Path $PSScriptRoot 'write-usb.ps1'
$tokens = $null
$parseErrors = $null
$null = [Management.Automation.Language.Parser]::ParseFile($writer, [ref]$tokens, [ref]$parseErrors)
if ($parseErrors.Count) { throw ($parseErrors | Out-String) }
. $writer
Initialize-UsbWriter

function Assert-True {
    param([bool] $Condition, [string] $Message)
    if (-not $Condition) { throw $Message }
}

function Assert-Fails {
    param([scriptblock] $Action, [string] $Pattern = '.')
    $failure = $null
    try { & $Action | Out-Null } catch { $failure = $_ }
    Assert-True ($null -ne $failure -and $failure.ToString() -match $Pattern) "Expected failure: $Pattern"
}

function New-TestDisk {
    return [pscustomobject]@{
        Number = 3; FriendlyName = 'Test USB'; SerialNumber = 'test-serial';
        BusType = 'USB'; IsBoot = $false; IsSystem = $false; IsOffline = $false;
        IsReadOnly = $false; Size = 64MB; LogicalSectorSize = 512;
        Path = 'not-a-real-device'; UniqueId = 'test-id'; PartitionStyle = 'RAW'
    }
}

$directory = Join-Path ([IO.Path]::GetTempPath()) ('osel-usb-test-' + [guid]::NewGuid())
$null = New-Item -ItemType Directory -Path $directory
try {
    $good = New-TestDisk
    Assert-UsbTarget $good 512
    foreach ($case in @(
        @{BusType = 'SATA'}, @{BusType = 'NVMe'}, @{IsBoot = $true}, @{IsSystem = $true},
        @{IsOffline = $true}, @{IsReadOnly = $true}, @{Size = 256},
        @{LogicalSectorSize = 4096}, @{UniqueId = ''}, @{Path = ''}
    )) {
        $bad = New-TestDisk
        foreach ($key in $case.Keys) { $bad.$key = $case[$key] }
        Assert-Fails { Assert-UsbTarget $bad 512 }
    }
    Write-Host 'Target guards OK: internal, Windows, read-only, offline, size, sector, identity'

    $imagePath = Join-Path $directory 'OSel.bios.img'
    $checksums = Join-Path $directory 'SHA256SUMS'
    $bytes = New-Object byte[] (2MB + 512)
    (New-Object Random 123).NextBytes($bytes)
    [IO.File]::WriteAllBytes($imagePath, $bytes)
    $hash = (Get-FileHash -LiteralPath $imagePath -Algorithm SHA256).Hash.ToLowerInvariant()
    "$hash  OSel.bios.img" | Set-Content -LiteralPath $checksums -Encoding Ascii
    Assert-True ((Get-ImageChecksum $checksums 'OSel.bios.img') -eq $hash) 'Checksum parse'
    Assert-Fails { Get-ImageChecksum $checksums 'OSel.uefi.img' }
    "$hash *OSel.bios.img" | Add-Content -LiteralPath $checksums -Encoding Ascii
    Assert-Fails { Get-ImageChecksum $checksums 'OSel.bios.img' }
    "$hash *OSel.bios.img" | Set-Content -LiteralPath $checksums -Encoding Ascii

    # File-backed streams exercise the same exact-length write and readback loop.
    $destination = Join-Path $directory 'destination.bin'
    $tail = New-Object byte[] ($bytes.Length + 512)
    for ($i = $bytes.Length; $i -lt $tail.Length; $i++) { $tail[$i] = 204 }
    [IO.File]::WriteAllBytes($destination, $tail)
    $inputStream = [IO.File]::OpenRead($imagePath)
    $outputStream = [IO.File]::Open($destination, [IO.FileMode]::Open, [IO.FileAccess]::ReadWrite)
    try {
        [OSel.UsbWriter]::CopyAndVerify($inputStream, $outputStream, $hash)
        Assert-True ($outputStream.Length -eq $tail.Length) 'Writer truncated the destination'
        $outputStream.Position = $bytes.Length
        Assert-True ($outputStream.ReadByte() -eq 204) 'Writer touched bytes after the image'
        Assert-Fails { [OSel.UsbWriter]::CopyAndVerify($inputStream, $outputStream, ('0' * 64)) } 'SHA-256'
        $unaligned = New-Object IO.MemoryStream (,[byte[]](1, 2, 3))
        try {
            Assert-Fails { [OSel.UsbWriter]::CopyAndVerify($unaligned, $outputStream, $hash) } '512'
        } finally { $unaligned.Dispose() }
    } finally { $inputStream.Dispose(); $outputStream.Dispose() }
    Assert-Fails { [OSel.UsbWriter]::OpenDisk($destination, 3) }
    Write-Host 'Copy OK: multi-buffer image, sector tail, disk readback, mismatch refusal'

    # Mock only discovery and confirmation. -WhatIf/cancel/changed identity must
    # stop before native OpenDisk, whose deliberately invalid test path would fail.
    function Assert-WindowsAdministrator { }
    $script:discoveryCalls = 0
    $script:changeIdentity = $false
    function Get-Disk {
        param($Number)
        $script:discoveryCalls++
        $found = New-TestDisk
        if ($script:changeIdentity -and $script:discoveryCalls -gt 1) { $found.UniqueId = 'other-disk' }
        return $found
    }
    function Read-Host { throw 'WhatIf must not prompt' }
    Write-OSelUsb -ImagePath $imagePath -DiskNumber 3 -WhatIf | Out-Null
    function Read-Host { return 'cancel' }
    Assert-Fails { Write-OSelUsb -ImagePath $imagePath -DiskNumber 3 } 'Подтверждение'
    function Read-Host { return 'ERASE 3' }
    $script:discoveryCalls = 0
    $script:changeIdentity = $true
    Assert-Fails { Write-OSelUsb -ImagePath $imagePath -DiskNumber 3 } 'изменился'
    ('0' * 64) + '  OSel.bios.img' | Set-Content -LiteralPath $checksums -Encoding Ascii
    $script:discoveryCalls = 0
    Assert-Fails { Write-OSelUsb -ImagePath $imagePath -DiskNumber 3 } 'SHA-256'
    Assert-True ($script:discoveryCalls -eq 0) 'Invalid image reached disk discovery'
    Remove-Item Function:\Get-Disk
    Remove-Item Function:\Read-Host
    Write-Host 'Entry point OK: WhatIf, cancellation, unplug/replacement, wrong checksum'

    if ($VirtualDisk) {
        # A disposable VHD exercises real Win32 disk/volume APIs on CI, without USB
        # hardware. Production USB guards remain unchanged and are tested above.
        $vhdPath = Join-Path $directory 'test.vhd'
        $diskpartScript = Join-Path $directory 'create-vhd.txt'
        "create vdisk file=`"$vhdPath`" maximum=64 type=fixed" |
            Set-Content -LiteralPath $diskpartScript -Encoding Ascii
        diskpart.exe /s $diskpartScript
        if ($LASTEXITCODE -ne 0 -or -not (Test-Path -LiteralPath $vhdPath)) { throw 'VHD creation failed' }
        $null = Mount-DiskImage -ImagePath $vhdPath
        $raw = $null
        $source = $null
        $volumeHandle = $null
        try {
            $virtual = Get-DiskImage -ImagePath $vhdPath | Get-Disk
            Assert-True ($virtual.BusType -eq 'File Backed Virtual' -or
                $virtual.BusType -eq 'FileBackedVirtual') 'Test target is not a file-backed VHD'
            Assert-True (-not $virtual.IsBoot -and -not $virtual.IsSystem) 'VHD is a Windows disk'
            $null = Initialize-Disk -Number $virtual.Number -PartitionStyle MBR
            $partition = New-Partition -DiskNumber $virtual.Number -UseMaximumSize -AssignDriveLetter
            $null = $partition | Format-Volume -FileSystem FAT32 -Confirm:$false
            $partition = Get-Partition -DiskNumber $virtual.Number
            $guid = @($partition.AccessPaths | Where-Object { $_ -match '^\\\\\?\\Volume\{[^}]+\}\\$' })
            Assert-True ($guid.Count -eq 1) 'Expected one VHD volume GUID'
            $busyPath = "$($partition.DriveLetter):\busy.txt"
            'Keep this volume busy' | Set-Content -LiteralPath $busyPath
            $busy = [IO.File]::OpenRead($busyPath)
            try { Assert-Fails { [OSel.UsbWriter]::LockVolume($guid[0]) } 'lock' }
            finally { $busy.Dispose() }
            $raw = [OSel.UsbWriter]::OpenDisk($virtual.Path, $virtual.Number)
            $volumeHandle = [OSel.UsbWriter]::LockVolume($guid[0])
            $source = [IO.File]::OpenRead($imagePath)
            [OSel.UsbWriter]::CopyAndVerify($source, $raw, $hash)
            Write-Host 'Native Windows disk OK: busy volume refused, lock, dismount, write, flush, readback'
        } finally {
            if ($source) { $source.Dispose() }
            if ($raw) { $raw.Dispose() }
            if ($volumeHandle) { $volumeHandle.Dispose() }
            Dismount-DiskImage -ImagePath $vhdPath
        }
    }
    Write-Host 'All Windows USB writer checks passed'
} finally {
    Remove-Item -LiteralPath $directory -Recurse -Force
}
