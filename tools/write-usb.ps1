#requires -Version 5.1
<#
.SYNOPSIS
Записывает образ OSel на USB-диск и проверяет результат без сторонних программ.
.DESCRIPTION
Windows PowerShell 5.1, права администратора, образ .img и SHA256SUMS.
-WhatIf проверяет образ и выбор диска без записи. Подтверждение ERASE N обязательно.
#>
[CmdletBinding(SupportsShouldProcess = $true)]
param(
    [string] $ImagePath,
    [int] $DiskNumber,
    [string] $ChecksumsPath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Assert-WindowsAdministrator {
    if ([Environment]::OSVersion.Platform -ne [PlatformID]::Win32NT) {
        throw 'Скрипт работает только в Windows.'
    }
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    $principal = New-Object Security.Principal.WindowsPrincipal($identity)
    if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
        throw 'Откройте Windows PowerShell от имени администратора.'
    }
}

function Assert-UsbTarget {
    param($Disk, [long] $ImageLength)
    if ($Disk.BusType -ne 'USB' -or $Disk.IsBoot -or $Disk.IsSystem) {
        throw 'Разрешён только USB-диск без загрузочной или системной Windows.'
    }
    if ($Disk.IsOffline -or $Disk.IsReadOnly) {
        throw 'USB-диск отключён или защищён от записи.'
    }
    if ($Disk.Size -lt $ImageLength) {
        throw 'Образ не помещается на USB-диск.'
    }
    if ($Disk.LogicalSectorSize -ne 512) {
        throw 'Образы OSel требуют диск с логическим сектором 512 байт.'
    }
    if ([string]::IsNullOrWhiteSpace($Disk.Path) -or
        [string]::IsNullOrWhiteSpace($Disk.UniqueId)) {
        throw 'Не удалось определить постоянный идентификатор USB-диска.'
    }
}

function Get-ImageChecksum {
    param([string] $Path, [string] $FileName)
    $matchesForImage = @(
        foreach ($line in Get-Content -LiteralPath $Path) {
            if ($line -match '^([0-9a-fA-F]{64}) [ *](.+)$' -and $Matches[2] -ceq $FileName) {
                $Matches[1].ToLowerInvariant()
            }
        }
    )
    if ($matchesForImage.Count -ne 1) {
        throw "В SHA256SUMS должна быть ровно одна строка для $FileName."
    }
    return $matchesForImage[0]
}

function Initialize-UsbWriter {
    if ('OSel.UsbWriter' -as [type]) { return }
    # Only built-in .NET and Win32 APIs; compatible with Windows PowerShell 5.1.
    Add-Type -TypeDefinition @'
using System;
using System.ComponentModel;
using System.IO;
using System.Runtime.InteropServices;
using System.Security.Cryptography;
using Microsoft.Win32.SafeHandles;

namespace OSel {
    public static class UsbWriter {
        [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
        private static extern SafeFileHandle CreateFile(string name, uint access,
            uint share, IntPtr security, uint creation, uint flags, IntPtr template);

        [DllImport("kernel32.dll", SetLastError = true)]
        private static extern bool DeviceIoControl(SafeFileHandle handle, uint code,
            IntPtr input, uint inputSize, byte[] output, uint outputSize,
            out uint returned, IntPtr overlapped);

        private static SafeFileHandle Open(string path) {
            // OPEN_EXISTING, GENERIC_READ | GENERIC_WRITE, FILE_SHARE_READ | WRITE.
            SafeFileHandle handle = CreateFile(path, 0xC0000000, 3,
                IntPtr.Zero, 3, 0, IntPtr.Zero);
            if (handle.IsInvalid) {
                int error = Marshal.GetLastWin32Error();
                handle.Dispose();
                throw new Win32Exception(error, "Cannot open " + path);
            }
            return handle;
        }

        public static FileStream OpenDisk(string path, uint expectedNumber) {
            SafeFileHandle handle = Open(path);
            try {
                byte[] device = new byte[12];
                uint returned;
                // IOCTL_STORAGE_GET_DEVICE_NUMBER: ensure the opened device is our disk.
                if (!DeviceIoControl(handle, 0x2D1080, IntPtr.Zero, 0,
                    device, 12, out returned, IntPtr.Zero))
                    throw new Win32Exception(Marshal.GetLastWin32Error());
                if (returned != 12 || BitConverter.ToUInt32(device, 0) != 7 ||
                    BitConverter.ToUInt32(device, 4) != expectedNumber)
                    throw new IOException("USB disk identity changed; nothing was written.");
                return new FileStream(handle, FileAccess.ReadWrite, 1024 * 1024, false);
            } catch {
                handle.Dispose();
                throw;
            }
        }

        public static SafeFileHandle LockVolume(string path) {
            SafeFileHandle handle = Open(path.TrimEnd('\\'));
            try {
                uint returned;
                // Lock before dismount, and retain the handle until verification ends.
                if (!DeviceIoControl(handle, 0x90018, IntPtr.Zero, 0,
                    null, 0, out returned, IntPtr.Zero))
                    throw new Win32Exception(Marshal.GetLastWin32Error(),
                        "Cannot lock USB volume; close its files and Explorer windows.");
                if (!DeviceIoControl(handle, 0x90020, IntPtr.Zero, 0,
                    null, 0, out returned, IntPtr.Zero))
                    throw new Win32Exception(Marshal.GetLastWin32Error());
                return handle;
            } catch {
                handle.Dispose();
                throw;
            }
        }

        public static string HashImage(Stream image) {
            image.Position = 0;
            using (SHA256 hash = SHA256.Create()) {
                string result = BitConverter.ToString(hash.ComputeHash(image))
                    .Replace("-", "").ToLowerInvariant();
                image.Position = 0;
                return result;
            }
        }

        public static void CopyAndVerify(Stream image, Stream disk, string expectedHash) {
            if (image.Length < 512 || image.Length % 512 != 0)
                throw new IOException("Image size must be a positive multiple of 512.");
            // Keep a single source handle from checksum validation through copying.
            // Never truncate the device or touch bytes beyond the image's length.
            image.Position = 0;
            disk.Position = 0;
            byte[] buffer = new byte[1024 * 1024];
            int count;
            long remaining = image.Length;
            while (remaining > 0) {
                int wanted = (int)Math.Min(buffer.Length, remaining);
                int filled = 0;
                while (filled < wanted) {
                    count = image.Read(buffer, filled, wanted - filled);
                    if (count == 0) throw new EndOfStreamException("Image read ended early.");
                    filled += count;
                }
                disk.Write(buffer, 0, wanted);
                remaining -= wanted;
            }
            FileStream file = disk as FileStream;
            if (file != null) file.Flush(true); else disk.Flush();
            disk.Position = 0;
            remaining = image.Length;
            using (SHA256 hash = SHA256.Create()) {
                while (remaining > 0) {
                    count = disk.Read(buffer, 0, (int)Math.Min(buffer.Length, remaining));
                    if (count == 0) throw new EndOfStreamException("USB readback ended early.");
                    hash.TransformBlock(buffer, 0, count, buffer, 0);
                    remaining -= count;
                }
                hash.TransformFinalBlock(new byte[0], 0, 0);
                string actual = BitConverter.ToString(hash.Hash).Replace("-", "");
                if (!String.Equals(actual, expectedHash, StringComparison.OrdinalIgnoreCase))
                    throw new IOException("USB readback SHA-256 does not match the image.");
            }
        }
    }
}
'@
}

function Write-OSelUsb {
    [CmdletBinding(SupportsShouldProcess = $true)]
    param(
        [Parameter(Mandatory = $true)] [string] $ImagePath,
        [Parameter(Mandatory = $true)] [ValidateRange(0, 2147483647)] [int] $DiskNumber,
        [string] $ChecksumsPath
    )
    Assert-WindowsAdministrator
    $imageFile = Get-Item -LiteralPath $ImagePath
    if ($imageFile -isnot [IO.FileInfo] -or $imageFile.Extension -ne '.img' -or
        $imageFile.Length -lt 512 -or $imageFile.Length % 512 -ne 0) {
        throw 'Нужен обычный файл .img с размером, кратным 512 байтам.'
    }
    if (-not $ChecksumsPath) {
        $ChecksumsPath = Join-Path $imageFile.DirectoryName 'SHA256SUMS'
    }
    $expectedHash = Get-ImageChecksum $ChecksumsPath $imageFile.Name
    Initialize-UsbWriter
    $source = [IO.File]::Open($imageFile.FullName, [IO.FileMode]::Open,
        [IO.FileAccess]::Read, [IO.FileShare]::Read)
    $target = $null
    $locks = New-Object 'System.Collections.Generic.List[Microsoft.Win32.SafeHandles.SafeFileHandle]'
    try {
        if ([OSel.UsbWriter]::HashImage($source) -ne $expectedHash) {
            throw 'SHA-256 образа не совпадает с SHA256SUMS. Запись отменена.'
        }
        # No destructive calls are made until validation and explicit confirmation.
        $disk = Get-Disk -Number $DiskNumber
        Assert-UsbTarget $disk $source.Length
        # Capture volume GUIDs before confirmation so a recycled disk number cannot
        # redirect dismounts to a different device after unplugging the USB drive.
        $volumePaths = @()
        if ($disk.PartitionStyle -ne 'RAW') {
            foreach ($partition in @(Get-Partition -DiskNumber $DiskNumber)) {
                $volumes = @($partition.AccessPaths | Where-Object { $_ -match '^\\\\\?\\Volume\{[^}]+\}\\$' })
                if ($volumes.Count -eq 0 -and [string]$partition.DriveLetter -match '^[A-Za-z]$') {
                    throw 'Не удалось определить том USB-диска. Запись отменена.'
                }
                $volumePaths += $volumes
            }
        }
        $disk | Format-List Number, FriendlyName, SerialNumber, UniqueId, BusType, Size
        if (-not $PSCmdlet.ShouldProcess("USB-диск $DiskNumber ($($disk.FriendlyName))",
            "Записать $($imageFile.Name); удалить содержимое флешки")) { return }
        $confirmation = Read-Host "Все данные флешки будут потеряны. Введите ERASE $DiskNumber"
        if ($confirmation -cne "ERASE $DiskNumber") {
            throw 'Подтверждение не совпало. Запись отменена.'
        }
        $fresh = Get-Disk -Number $DiskNumber
        Assert-UsbTarget $fresh $source.Length
        if ($fresh.Path -ne $disk.Path -or $fresh.UniqueId -ne $disk.UniqueId -or
            $fresh.Size -ne $disk.Size -or $fresh.SerialNumber -ne $disk.SerialNumber) {
            throw 'USB-диск изменился после подтверждения. Запись отменена.'
        }
        # Open the stable device path, not a disk number that unplugging could recycle.
        $target = [OSel.UsbWriter]::OpenDisk($fresh.Path, $DiskNumber)
        foreach ($volume in @($volumePaths | Select-Object -Unique)) {
            $locks.Add([OSel.UsbWriter]::LockVolume($volume))
        }
        Write-Host 'Запись и проверка данных флешки...'
        [OSel.UsbWriter]::CopyAndVerify($source, $target, $expectedHash)
        Write-Host 'Готово: данные прочитаны с флешки, SHA-256 совпадает. Извлеките её безопасно.'
    } finally {
        if ($target) { $target.Dispose() }
        foreach ($handle in $locks) { $handle.Dispose() }
        $source.Dispose()
    }
}

# Dot-sourcing only loads functions, allowing tests without touching a physical disk.
if ($MyInvocation.InvocationName -ne '.') {
    Write-OSelUsb @PSBoundParameters
}
