# ============================================================================
# scripts/quant/win_e8.ps1 —— E8：实时防护（火绒）对写路径的影响
#
# 口径（先写死，测后不改）：
#   场景：1 GiB、--piece-conns 1 与 16、目标盘 D:（USB 外置 HDD）、每档 3 轮
#   变量：火绒实时防护 开 / 关（⚠️ 关防护只能人工在火绒界面操作，本脚本**不改注册表、不动服务**）
#   量：墙钟、Avg. Disk sec/Write（下载窗口内的均值与 p95）、写放大（与 E2 同一套 raw 计数器积分）
#   判据：两态墙钟中位差 > 20% ⇒ 显著，`#8` 验收须写明"实时防护开着"；≤ 20% ⇒ 不显著
#
# 为什么单独写一个而不是直接调 win_round.ps1：
#   E2 的 CSV 列已经固定（跨轮可比），E8 要多三样 —— 防护状态、延迟分位、以及"状态"列；
#   加列会让旧 CSV 的表头不一致。所以复制积分逻辑、保持 win_round.ps1 一字不动。
#
# 用法：
#   pwsh -NoProfile -File scripts/quant/win_e8.ps1 -ExePath C:\quant-target\herta\release\mc-downloader.exe
#   pwsh -NoProfile -File scripts/quant/win_e8.ps1 -ExePath ... -State off   # 人工关掉防护后跑这一条
# ============================================================================
param(
    [Parameter(Mandatory = $true)][string]$ExePath,
    [int]$Rounds = 3,
    [int[]]$Conns = @(1, 16),
    [string]$Url = 'http://127.0.0.1:8899/q1g.bin',
    [string]$Name = 'q1g.bin',
    [string]$SrcDir = 'C:\quant-src',
    [string]$TargetRoot = 'D:\quant-target',
    [string]$State = '',
    [string]$Csv = 'scripts\quant\raw\e8-av.csv',
    [int]$BaselineSecs = 5,
    [double]$SettleQuietMBps = 1.0,
    [int]$SettleMaxSecs = 90,
    [int]$Timeout = 30,
    [string]$DiskInstance = '2 D:',
    [string]$LogicalInstance = 'D:'
)

$ErrorActionPreference = 'Stop'
$repo = Split-Path (Split-Path $PSScriptRoot -Parent) -Parent
$csvPath = if ([System.IO.Path]::IsPathRooted($Csv)) { $Csv } else { Join-Path $repo $Csv }
New-Item -ItemType Directory -Force -Path (Split-Path $csvPath) | Out-Null

$srcFile = Join-Path $SrcDir $Name
if (-not (Test-Path $srcFile)) { throw "源文件缺失: $srcFile" }
$FileSize = (Get-Item $srcFile).Length

$physCls = 'Win32_PerfRawData_PerfDisk_PhysicalDisk'
$logCls = 'Win32_PerfRawData_PerfDisk_LogicalDisk'
$fmtCls = 'Win32_PerfFormattedData_PerfDisk_PhysicalDisk'

# ---- 防护状态（只读检测：服务 + 进程 + 版本；不改任何东西）----
function Get-AvProbe {
    $svc = Get-Service -Name 'HipsDaemon' -ErrorAction SilentlyContinue
    $procs = @(Get-Process -Name 'HipsDaemon', 'HipsTray', 'usysdiag' -ErrorAction SilentlyContinue |
        Select-Object -ExpandProperty Name -Unique)
    $exe = 'C:\Program Files\Huorong\Sysdiag\bin\HipsDaemon.exe'
    $ver = if (Test-Path $exe) { (Get-Item $exe).VersionInfo.FileVersion } else { '' }
    $sec = @(Get-CimInstance -Namespace 'root/SecurityCenter2' -Class AntiVirusProduct -ErrorAction SilentlyContinue |
        Select-Object -ExpandProperty displayName)
    [pscustomobject]@{
        service = if ($svc) { $svc.Status } else { 'absent' }
        procs   = ($procs -join '+')
        version = $ver
        seccenter2 = if ($sec.Count) { $sec -join '+' } else { 'none' }
    }
}

$av = Get-AvProbe
$stateLabel = if ($State) { $State } else {
    if ($av.service -eq 'Running') { 'on' } elseif ($av.service -eq 'Stopped') { 'off' } else { 'unknown' }
}
Write-Host "防护状态: $stateLabel (service=$($av.service) procs=$($av.procs) v=$($av.version) seccenter2=$($av.seccenter2))"

$rows = @()
foreach ($pc in $Conns) {
    for ($r = 1; $r -le $Rounds; $r++) {
        if (Test-Path $TargetRoot) { Remove-Item -Recurse -Force $TargetRoot }
        New-Item -ItemType Directory -Force -Path $TargetRoot | Out-Null

        # 基线（空闲写速率，用于扣背景）
        $b0 = Get-CimInstance $physCls -Filter "Name='$DiskInstance'"
        $bw = [System.Diagnostics.Stopwatch]::StartNew()
        Start-Sleep -Seconds $BaselineSecs
        $bw.Stop()
        $b1 = Get-CimInstance $physCls -Filter "Name='$DiskInstance'"
        $baseBps = ($b1.DiskWriteBytesPersec - $b0.DiskWriteBytesPersec) / $bw.Elapsed.TotalSeconds

        $outLog = Join-Path $env:TEMP "mcd-e8-c$pc-r$r.out"
        $errLog = Join-Path $env:TEMP "mcd-e8-c$pc-r$r.err"
        $argv = @('fetch', $Url, '-d', $TargetRoot, '-n', $Name, '--piece-conns', "$pc",
            '--timeout', "$Timeout", '--retry', '3', '--force')

        $p0 = Get-CimInstance $physCls -Filter "Name='$DiskInstance'"
        $l0 = Get-CimInstance $logCls -Filter "Name='$LogicalInstance'"
        $latFile = Join-Path $env:TEMP "e8lat-c$pc-r$r.csv"
        Remove-Item $latFile -Force -ErrorAction SilentlyContinue
        # 计数器名里有空格（"Avg. Disk sec/Write"）⇒ Start-Process 的 -ArgumentList 不做引号化，
        # 必须自己带上双引号，否则 typeperf 拿到两个参数、立刻退出（隐藏窗口时还看不到报错）。
        $counter = '"\PhysicalDisk(' + $DiskInstance + ')\Avg. Disk sec/Write"'
        $latProc = Start-Process -FilePath typeperf -ArgumentList @(
            $counter, '-si', '1', '-sc', '900', '-f', 'CSV', '-o', $latFile
        ) -NoNewWindow -PassThru
        Start-Sleep -Milliseconds 300
        $tWallStart = Get-Date
        $wp = [System.Diagnostics.Stopwatch]::StartNew()
        $proc = Start-Process -FilePath $ExePath -ArgumentList $argv -NoNewWindow -PassThru `
            -RedirectStandardOutput $outLog -RedirectStandardError $errLog

        # 采样循环只读 raw 计数器（算字节）。延迟用 PDH（typeperf）另起一个采样器：
        #   Win32_PerfFormattedData 的 AvgDisksecPerWrite 在本机恒为 0（实测 11-27 个样本全是 0），
        #   而用 raw 的 AvgDisksecPerWrite / _Base 手算又和 perfmon 对不上（raw 分子不是 100ns 计数，
        #   实测 Δraw/Δbase/1e7 = 6.47 ms vs typeperf 0.4 ms）。typeperf 走的就是 perfmon 的同一条
        #   PDH 路径，最可信，所以用一个子进程按 1 s 采，按时间窗归到本轮下载窗口里。
        while (-not $proc.HasExited) { Start-Sleep -Milliseconds 400 }
        $proc.WaitForExit()
        $wp.Stop()
        $dlSecs = $wp.Elapsed.TotalSeconds
        $p1 = Get-CimInstance $physCls -Filter "Name='$DiskInstance'"
        $l1 = Get-CimInstance $logCls -Filter "Name='$LogicalInstance'"
        $fmtEnd = Get-CimInstance $fmtCls -Filter "Name='$DiskInstance'"

        # 沉降
        $sw = [System.Diagnostics.Stopwatch]::StartNew()
        $quiet = 0
        $prev = $p1.DiskWriteBytesPersec
        while ($sw.Elapsed.TotalSeconds -lt $SettleMaxSecs) {
            Start-Sleep -Seconds 1
            $cur = Get-CimInstance $physCls -Filter "Name='$DiskInstance'"
            $rate = ($cur.DiskWriteBytesPersec - $prev) / 1.0
            $prev = $cur.DiskWriteBytesPersec
            if ($rate -lt ($SettleQuietMBps * 1MB)) { $quiet++ } else { $quiet = 0 }
            if ($quiet -ge 3) { break }
        }
        $sw.Stop()
        $p2 = Get-CimInstance $physCls -Filter "Name='$DiskInstance'"
        $l2 = Get-CimInstance $logCls -Filter "Name='$LogicalInstance'"

        $window = $dlSecs + $sw.Elapsed.TotalSeconds
        $physBytes = [double]($p2.DiskWriteBytesPersec - $p0.DiskWriteBytesPersec)
        $logBytes = [double]($l2.DiskWriteBytesPersec - $l0.DiskWriteBytesPersec)
        $physNet = $physBytes - $baseBps * $window
        $logNet = $logBytes - $baseBps * $window

        $tWallEnd = Get-Date
        Stop-Process -Id $latProc.Id -Force -ErrorAction SilentlyContinue
        Start-Sleep -Milliseconds 200
        # 只取落在 [下载开始, 沉降结束] 窗口内的样本
        $lat = New-Object System.Collections.Generic.List[double]
        if (Test-Path $latFile) {
            foreach ($line in Get-Content $latFile) {
                $m = [regex]::Match($line, '^"(\d+/\d+/\d+ [\d:.]+)","\s*([\d.eE+-]+)?\s*"')
                if (-not $m.Success) { continue }
                $ts = [datetime]::ParseExact($m.Groups[1].Value, 'M/d/yyyy H:mm:ss.fff', $null)
                if (-not $m.Groups[2].Success -or -not $m.Groups[2].Value) { continue }
                if ($ts -ge $tWallStart -and $ts -le $tWallEnd) { $lat.Add([double]$m.Groups[2].Value * 1000.0) }
            }
        }
        $sorted = @($lat | Sort-Object)
        $latMean = if ($sorted.Count) { ($sorted | Measure-Object -Average).Average } else { $null }
        $latP95 = if ($sorted.Count) { $sorted[[Math]::Min($sorted.Count - 1, [int][Math]::Floor(0.95 * $sorted.Count))] } else { $null }

        $landed = Join-Path $TargetRoot $Name
        $outText = if (Test-Path $outLog) { Get-Content $outLog -Raw } else { '' }
        $pieceLine = ([regex]::Match($outText, '\[\*\] 分片：([^\r\n]*)')).Groups[1].Value
        $row = [pscustomobject]@{
            state         = $stateLabel
            av_service    = $av.service
            av_procs      = $av.procs
            conns         = $pc
            piece_line    = $pieceLine
            round         = $r
            exit_code     = $proc.ExitCode
            landed_bytes  = if (Test-Path $landed) { (Get-Item $landed).Length } else { -1 }
            wall_s        = [math]::Round($dlSecs, 3)
            settle_s      = [math]::Round($sw.Elapsed.TotalSeconds, 3)
            mbps          = [math]::Round($FileSize / $dlSecs / 1MB, 2)
            phys_bytes    = [math]::Round($physBytes, 0)
            phys_wa_raw   = [math]::Round($physBytes / $FileSize, 4)
            phys_wa_net   = [math]::Round($physNet / $FileSize, 4)
            log_wa_net    = [math]::Round($logNet / $FileSize, 4)
            lat_ms_mean   = if ($null -ne $latMean) { [math]::Round($latMean, 3) } else { -1 }
            lat_ms_p95    = if ($null -ne $latP95) { [math]::Round($latP95, 3) } else { -1 }
            lat_samples   = $sorted.Count
            split_io      = ($p2.SplitIOPerSec - $p0.SplitIOPerSec)
            write_ops     = ($p2.DiskWritesPersec - $p0.DiskWritesPersec)
            base_phys_mbps = [math]::Round($baseBps / 1MB, 3)
            pct_wr_time   = $fmtEnd.PercentDiskWriteTime
            out_tail      = ((Get-Content $outLog -Raw -ErrorAction SilentlyContinue) -split "`n" |
                Where-Object { $_.Trim() } | Select-Object -Last 1)
        }
        $rows += $row
        $row | Format-List | Out-String | Write-Host
    }
}

$rows | Export-Csv -Path $csvPath -NoTypeInformation -Append -Encoding utf8
Write-Host "CSV -> $csvPath (+$($rows.Count) rows)"
