# USB selection follows OpenShellLive's usb-target.ps1. The writer keeps the
# verified source and the same raw device handle open through readback, and
# writes the partition table last (Copy-OpenShellNativeUsbImage).
function Get-OpenShellNativeUsb {
    param([int]$DiskNumber, [uint64]$MinimumSize)
    $disk = Get-Disk -Number $DiskNumber -ErrorAction Stop
    if ($disk.IsBoot -or $disk.IsSystem -or $disk.BusType -ne 'USB' -or
        $disk.IsOffline -or $disk.IsReadOnly -or $disk.LogicalSectorSize -ne 512 -or
        $disk.Size -lt $MinimumSize -or [string]::IsNullOrWhiteSpace([string]$disk.UniqueId)) {
        throw "Disk $DiskNumber is ineligible. Use a writable USB disk with a stable ID, 512-byte sectors and room for the image."
    }
    return $disk
}

function Confirm-OpenShellNativeUsb {
    param([int]$DiskNumber = -1, [uint64]$MinimumSize)
    if ($DiskNumber -lt 0) {
        $candidates = @(Get-Disk | Where-Object {
            $_.BusType -eq 'USB' -and -not $_.IsBoot -and -not $_.IsSystem -and
            -not $_.IsOffline -and -not $_.IsReadOnly -and $_.LogicalSectorSize -eq 512 -and
            $_.Size -ge $MinimumSize -and -not [string]::IsNullOrWhiteSpace([string]$_.UniqueId)
        })
        if ($candidates.Count -eq 0) { throw 'No eligible USB disk found. Connect the stick and run again.' }
        $candidates | Format-Table Number, FriendlyName, @{n='Size (GB)'; e={[math]::Round($_.Size / 1GB, 1)}} -AutoSize | Out-Host
        $choice = Read-Host 'USB disk number to use'
        if (-not [int]::TryParse($choice, [ref]$DiskNumber) -or $DiskNumber -lt 0) { throw 'Invalid USB disk number.' }
    }
    $disk = Get-OpenShellNativeUsb -DiskNumber $DiskNumber -MinimumSize $MinimumSize
    Write-Host ("All data on disk {0} ({1}, {2:N1} GB) will be erased." -f $DiskNumber, $disk.FriendlyName, ($disk.Size / 1GB)) -ForegroundColor Yellow
    if ((Read-Host "Type $DiskNumber to confirm the target").Trim() -ne "$DiskNumber") { throw 'Disk confirmation did not match.' }
    return [pscustomobject]@{
        Number = [int]$disk.Number
        UniqueId = [string]$disk.UniqueId
        SerialNumber = ([string]$disk.SerialNumber).Trim()
        Size = [uint64]$disk.Size
    }
}

function Assert-OpenShellNativeUsb {
    param([psobject]$Target, [uint64]$MinimumSize)
    $disk = Get-OpenShellNativeUsb -DiskNumber $Target.Number -MinimumSize $MinimumSize
    if ($disk.UniqueId -ne $Target.UniqueId -or ([string]$disk.SerialNumber).Trim() -ne $Target.SerialNumber -or
        [uint64]$disk.Size -ne $Target.Size) {
        throw 'The selected USB disk changed. Run again to confirm the target.'
    }
    return $disk
}

# Copies bytes From to To of the image to the same place on the stick, or
# with -Verify reads them back from both: returns their SHA-256 values.
function Invoke-OpenShellNativeUsbRange {
    param([IO.Stream]$Source, [IO.Stream]$Device, [long]$From, [long]$To, [string]$Activity, [switch]$Verify)
    $buffer = New-Object byte[] 4MB
    $sourceHash = [Security.Cryptography.SHA256]::Create()
    $deviceHash = [Security.Cryptography.SHA256]::Create()
    try {
        $Source.Position = $From
        [void]$Device.Seek($From, [IO.SeekOrigin]::Begin)
        $processed = $From
        while ($processed -lt $To) {
            $count = [int][Math]::Min($buffer.Length, $To - $processed)
            $received = 0
            while ($received -lt $count) {
                $read = $Source.Read($buffer, $received, $count - $received)
                if ($read -le 0) { throw "Short image read at $processed bytes." }
                $received += $read
            }
            if ($Verify) {
                [void]$sourceHash.TransformBlock($buffer, 0, $count, $buffer, 0)
                $received = 0
                while ($received -lt $count) {
                    $read = $Device.Read($buffer, $received, $count - $received)
                    if ($read -le 0) { throw "Short USB read at $processed bytes." }
                    $received += $read
                }
                [void]$deviceHash.TransformBlock($buffer, 0, $count, $buffer, 0)
            } else {
                $Device.Write($buffer, 0, $count)
            }
            $processed += $count
            Write-Progress -Activity $Activity -PercentComplete ([int](100 * ($processed - $From) / [Math]::Max(1, $To - $From)))
        }
        if (-not $Verify) { $Device.Flush($true) }
        [void]$sourceHash.TransformFinalBlock([byte[]]@(), 0, 0)
        [void]$deviceHash.TransformFinalBlock([byte[]]@(), 0, 0)
        return @([BitConverter]::ToString($sourceHash.Hash), [BitConverter]::ToString($deviceHash.Hash))
    } finally {
        Write-Progress -Activity $Activity -Completed
        $sourceHash.Dispose()
        $deviceHash.Dispose()
    }
}

# The image's first 4 MiB hold its partition table and go last: until then
# the stick has none, so Windows mounts nothing on it while the rest is
# written and read back. Once Windows sees the EFI partition it mounts it and
# writes there ("System Volume Information"), which a read-back after it
# would take for a bad write. Each part is checked against the image (whose
# SHA-256 was checked whole before) before the next is written.
function Copy-OpenShellNativeUsbImage {
    param([IO.Stream]$Source, [IO.Stream]$Device, [string]$ExpectedHash, [int]$DiskNumber)
    $length = $Source.Length
    $head = [long][Math]::Min(4MB, $length)
    foreach ($part in @(@($head, $length), @(0L, $head))) {
        $from, $to = $part
        if ($from -ge $to) { continue }
        [void](Invoke-OpenShellNativeUsbRange -Source $Source -Device $Device -From $from -To $to `
            -Activity "Writing OpenShell Native on disk $DiskNumber")
        $sums = Invoke-OpenShellNativeUsbRange -Source $Source -Device $Device -From $from -To $to `
            -Activity "Verifying OpenShell Native on disk $DiskNumber" -Verify
        if ($sums[0] -ne $sums[1]) {
            throw "USB verification failed on disk $DiskNumber (bytes $from to $to)."
        }
    }
}

function Write-OpenShellNativeUsb {
    param([string]$Iso, [string]$ExpectedHash, [psobject]$Target)
    $number = $Target.Number
    $source = [IO.File]::OpenRead($Iso)
    $device = $null
    $hash = [Security.Cryptography.SHA256]::Create()
    try {
        $length = $source.Length
        if ($length -lt 34816 -or $length % 2048 -ne 0) { throw 'Invalid ISO image size.' }
        $header = New-Object byte[] 520
        if ($source.Read($header, 0, $header.Length) -ne $header.Length) { throw 'Incomplete ISO header.' }
        $hasEfi = [Text.Encoding]::ASCII.GetString($header, 512, 8) -eq 'EFI PART'
        for ($i = 0; $i -lt 4; $i++) {
            if ($header[450 + 16 * $i] -eq 0xEF) { $hasEfi = $true }
        }
        $magic = New-Object byte[] 5
        $source.Position = 32769
        if ($source.Read($magic, 0, $magic.Length) -ne $magic.Length -or
            [Text.Encoding]::ASCII.GetString($magic) -ne 'CD001' -or
            $header[510] -ne 0x55 -or $header[511] -ne 0xAA -or -not $hasEfi) {
            throw 'The image must be a hybrid ISO with a UEFI boot partition.'
        }
        $source.Position = 0
        $actualHash = [BitConverter]::ToString($hash.ComputeHash($source)).Replace('-', '')
        if ($actualHash -ne $ExpectedHash) { throw "SHA-256 verification failed for $Iso" }
        $source.Position = 0

        $root = [IO.Path]::GetPathRoot($Iso).TrimEnd('\').TrimEnd(':')
        if ($root -match '^[A-Za-z]$') {
            $partition = Get-Partition -DriveLetter $root -ErrorAction Stop
            if ($partition.DiskNumber -eq $number) { throw 'Move the ISO off the selected USB disk before writing.' }
        }

        $disk = Assert-OpenShellNativeUsb -Target $Target -MinimumSize $length
        if ($disk.PartitionStyle -ne 'RAW') {
            Clear-Disk -InputObject $disk -RemoveData -RemoveOEM -Confirm:$false -ErrorAction Stop
            Start-Sleep -Seconds 2
        }
        [void](Assert-OpenShellNativeUsb -Target $Target -MinimumSize $length)
        $device = [IO.FileStream]::new("\\.\PhysicalDrive$number", [IO.FileMode]::Open,
            [IO.FileAccess]::ReadWrite, [IO.FileShare]::ReadWrite, 512, [IO.FileOptions]::WriteThrough)
        [void](Assert-OpenShellNativeUsb -Target $Target -MinimumSize $length)
        Copy-OpenShellNativeUsbImage -Source $source -Device $device -ExpectedHash $ExpectedHash -DiskNumber $number
    } finally {
        if ($device) { $device.Dispose() }
        $source.Dispose()
        $hash.Dispose()
    }
    Update-Disk -Number $number -ErrorAction SilentlyContinue
    Write-Host "Verified OpenShell Native live USB on disk $number."
}
