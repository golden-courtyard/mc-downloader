# ============================================================================
# scripts/quant/win_round.ps1 —— Windows 侧「一次下载 + 物理磁盘计数器积分」测量器
#
# 为什么用 raw 计数器而不是 Win32_PerfFormattedData 的 *PerSec：
#   Win32_PerfFormattedData_* 的 "xxxPerSec" 是 WMI 自己按它自己的采样间隔算的**速率**，
#   我们外部再按时钟积分会踩到两个坑：① 间隔对不齐（同一底层样本被读两次或漏一次）；
#   ② provider 的实际刷新周期与我们的 sleep 无关。
#   Win32_PerfRawData_* 的 "xxxPersec" 字段是**单调累加的字节总数**（BULK_COUNT 类型），
#   两次读数之差就是该窗口里真实下发的字节数 —— 不需要积分，也没有间隔误差。
#   本脚本因此：raw 差值 = 物力量，FormattedData 只用来取瞬时速率/延迟做旁证。
#
# 写放大口径（**先定后测，不事后改判据**）：
#   WA = (窗口内物理磁盘写字节 - 基线外推字节) / 文件大小
#   窗口 = 从启动 fetch 到「沉降结束」；沉降结束 = 连续 3 个 1s 样本 < SettleQuietMBps
#   或到达 SettleMaxSecs（USB 盘的写回缓存会把写摊到下载结束之后，不等到沉降就少算）。
#   基线 = 本轮开始前 BaselineSecs 秒的空闲速率外推（本机有后台进程在写 D:，
#   不扣基线会把噪声算成放大）。这两个数都原样记进 CSV。
#
# 用法：
#   pwsh -File win_round.ps1 -Url http://127.0.0.1:8899/q1g.bin -Name q1g.bin `
#        -PieceConns 16 -Label e2 -Rounds 3 -CsvPath raw\e2-windows.csv
# ============================================================================
param(
    [Parameter(Mandatory = $true)][string]$Url,
    [Parameter(Mandatory = $true)][string]$Name,
    [Parameter(Mandatory = $true)][int]$PieceConns,
    [Parameter(Mandatory = $true)][string]$Label,
    [string]$TargetRoot = 'D:\quant-target',
    [string]$CsvPath = '',
    [int]$Rounds = 3,
    [int]$BaselineSecs = 5,
    [double]$SettleQuietMBps = 1.0,
    [int]$SettleMaxSecs = 90,
    [double]$SampleSecs = 0.4,
    [string]$ExePath = 'D:\Games\mc-downloader\target\release\mc-downloader.exe',
    [int]$Timeout = 30,
    [int]$Retry = 3,
    [string]$Adaptive = '',
    [switch]$Prealloc,
    [string]$DiskInstance = '2 D:',
    [string]$LogicalInstance = 'D:'
)

$ErrorActionPreference = 'Stop'
$repo = 'D:\Games\mc-downloader'
if (-not $CsvPath) { $CsvPath = Join-Path $repo 'scripts\quant\raw\win-raw.csv' }
if (-not [System.IO.Path]::IsPathRooted($CsvPath)) { $CsvPath = Join-Path $repo $CsvPath }
New-Item -ItemType Directory -Force -Path (Split-Path $CsvPath) | Out-Null

function Get-Raw([string]$cls, [string]$inst) {
    Get-CimInstance $cls -Filter "Name='$inst'"
}
$physCls = 'Win32_PerfRawData_PerfDisk_PhysicalDisk'
$logCls = 'Win32_PerfRawData_PerfDisk_LogicalDisk'
$physFmtCls = 'Win32_PerfFormattedData_PerfDisk_PhysicalDisk'

$srcFile = Join-Path 'C:\quant-src' $Name
if (-not (Test-Path $srcFile)) { throw "源文件缺失: $srcFile" }
$FileSize = (Get-Item $srcFile).Length

$rows = @()
for ($r = 1; $r -le $Rounds; $r++) {
    # ① 清场：删掉上一轮产物（删除自身的元数据写发生在基线窗口之前）
    if (Test-Path $TargetRoot) { Remove-Item -Recurse -Force $TargetRoot }
    New-Item -ItemType Directory -Force -Path $TargetRoot | Out-Null

    # ①b E6 的「先顺序写零一遍」臂：按现状代码的 .part 命名预先铺满 1 GiB 零字节，
    #    让 one_split 的 `if len != size { set_len }` 短路（len 已经等于 size），
    #    于是后续随机写落在一个 **VDL 已铺满** 的文件里 —— 与稀疏 set_len 形成 A/B。
    $preallocSecs = 0.0
    if ($Prealloc) {
        $partPath = Join-Path $TargetRoot ($Name + '.part')
        $swP = [System.Diagnostics.Stopwatch]::StartNew()
        $buf = New-Object byte[] (4 * 1024 * 1024)
        $fs = [System.IO.File]::Open($partPath, 'Create', 'Write', 'None')
        try {
            $left = $FileSize
            while ($left -gt 0) {
                $n = [int][Math]::Min($buf.Length, $left)
                $fs.Write($buf, 0, $n)
                $left -= $n
            }
            $fs.Flush($true)          # 真落盘：不 Flush 就等于把写推迟到下载窗口里
        } finally { $fs.Dispose() }
        $swP.Stop()
        $preallocSecs = $swP.Elapsed.TotalSeconds
    }

    # ② 基线：空闲速率外推用的系数
    $b0 = Get-Raw $physCls $DiskInstance; $l0 = Get-Raw $logCls $LogicalInstance
    $bw = [System.Diagnostics.Stopwatch]::StartNew()
    Start-Sleep -Seconds $BaselineSecs
    $bw.Stop()
    $b1 = Get-Raw $physCls $DiskInstance; $l1 = Get-Raw $logCls $LogicalInstance
    $baseSecs = $bw.Elapsed.TotalSeconds
    $basePhysBps = ($b1.DiskWriteBytesPersec - $b0.DiskWriteBytesPersec) / $baseSecs
    $baseLogBps = ($l1.DiskWriteBytesPersec - $l0.DiskWriteBytesPersec) / $baseSecs

    # ③ 启动 fetch
    $outLog = Join-Path $env:TEMP "mcd-$Label-c$PieceConns-r$r.out"
    $errLog = Join-Path $env:TEMP "mcd-$Label-c$PieceConns-r$r.err"
    $argv = @('fetch', $Url, '-d', $TargetRoot, '-n', $Name, '--piece-conns', "$PieceConns",
        '--timeout', "$Timeout", '--retry', "$Retry", '--force')
    if ($Adaptive) { $argv += @('--adaptive', $Adaptive) }

    $p0 = Get-Raw $physCls $DiskInstance; $pl0 = Get-Raw $logCls $LogicalInstance
    $p0Split = $p0.SplitIOPerSec; $p0Writes = $p0.DiskWritesPersec
    $wp = [System.Diagnostics.Stopwatch]::StartNew()
    $proc = Start-Process -FilePath $ExePath -ArgumentList $argv -NoNewWindow -PassThru `
        -RedirectStandardOutput $outLog -RedirectStandardError $errLog
    $samples = @()
    while (-not $proc.HasExited) {
        $s = Get-Raw $physCls $DiskInstance
        $samples += , (@($wp.Elapsed.TotalSeconds, $s.DiskWriteBytesPersec, $s.SplitIOPerSec))
        Start-Sleep -Milliseconds ([int]($SampleSecs * 1000))
    }
    $proc.WaitForExit()
    $wp.Stop()
    $dlSecs = $wp.Elapsed.TotalSeconds
    $p1 = Get-Raw $physCls $DiskInstance; $pl1 = Get-Raw $logCls $LogicalInstance
    $fmtNow = Get-CimInstance $physFmtCls -Filter "Name='$DiskInstance'"

    # ④ 沉降：等到写回缓存把尾巴吐完（或到上限）
    $settleStop = [System.Diagnostics.Stopwatch]::StartNew()
    $quiet = 0
    $prevB = $p1.DiskWriteBytesPersec
    while ($settleStop.Elapsed.TotalSeconds -lt $SettleMaxSecs) {
        Start-Sleep -Seconds 1
        $cur = Get-Raw $physCls $DiskInstance
        $rate = ($cur.DiskWriteBytesPersec - $prevB) / 1.0
        $prevB = $cur.DiskWriteBytesPersec
        if ($rate -lt ($SettleQuietMBps * 1MB)) { $quiet++ } else { $quiet = 0 }
        if ($quiet -ge 3) { break }
    }
    $settleStop.Stop()
    $p2 = Get-Raw $physCls $DiskInstance; $pl2 = Get-Raw $logCls $LogicalInstance

    $windowSecs = ($wp.Elapsed.TotalSeconds + $settleStop.Elapsed.TotalSeconds)
    $physBytes = [double]($p2.DiskWriteBytesPersec - $p0.DiskWriteBytesPersec)
    $logBytes = [double]($pl2.DiskWriteBytesPersec - $pl0.DiskWriteBytesPersec)
    $basePhysBytes = $basePhysBps * $windowSecs
    $baseLogBytes = $baseLogBps * $windowSecs
    $physNet = $physBytes - $basePhysBytes
    $logNet = $logBytes - $baseLogBytes

    $landed = Join-Path $TargetRoot $Name
    $landedLen = if (Test-Path $landed) { (Get-Item $landed).Length } else { -1 }
    $stdout = if (Test-Path $outLog) { (Get-Content $outLog -Raw) } else { '' }
    $stderr = if (Test-Path $errLog) { (Get-Content $errLog -Raw) } else { '' }

    # ⑤ extent 数（= Windows 的 filefrag；defrag /A 需要管理员，本会话提不了权）
    $ext = $null
    if (Test-Path $landed) {
        try {
            $ext = (python (Join-Path $repo 'scripts\quant\win_extents.py') $landed | ConvertFrom-Json)
        } catch { $ext = $null }
    }

    $row = [pscustomobject]@{
        label          = $Label
        piece_conns    = $PieceConns
        round          = $r
        file           = $Name
        file_bytes     = $FileSize
        exit_code      = $proc.ExitCode
        landed_bytes   = $landedLen
        wall_s         = [math]::Round($dlSecs, 3)
        settle_s       = [math]::Round($settleStop.Elapsed.TotalSeconds, 3)
        window_s       = [math]::Round($windowSecs, 3)
        mbps_app       = [math]::Round($FileSize / $dlSecs / 1MB, 2)
        phys_bytes     = [math]::Round($physBytes, 0)
        phys_wa_raw    = [math]::Round($physBytes / $FileSize, 4)
        phys_net_bytes = [math]::Round($physNet, 0)
        phys_wa_net    = [math]::Round($physNet / $FileSize, 4)
        log_bytes      = [math]::Round($logBytes, 0)
        log_wa_net     = [math]::Round($logNet / $FileSize, 4)
        base_phys_mbps = [math]::Round($basePhysBps / 1MB, 3)
        base_log_mbps  = [math]::Round($baseLogBps / 1MB, 3)
        split_io       = ($p2.SplitIOPerSec - $p0Split)
        write_ops      = ($p2.DiskWritesPersec - $p0Writes)
        avg_wr_ms      = [math]::Round([double]$fmtNow.AvgDisksecPerWrite * 1000, 3)
        pct_wr_time    = $fmtNow.PercentDiskWriteTime
        prealloc_s     = [math]::Round($preallocSecs, 3)
        extents        = if ($ext) { $ext.extents } else { -1 }
        ext_clusters   = if ($ext) { $ext.clusters } else { -1 }
        out_tail       = ($stdout -split "`n" | Where-Object { $_.Trim() } | Select-Object -Last 1)
        err_tail       = ($stderr -split "`n" | Where-Object { $_.Trim() } | Select-Object -Last 1)
    }
    $rows += $row
    $row | Format-List | Out-String | Write-Host
}

$rows | Export-Csv -Path $CsvPath -NoTypeInformation -Append -Encoding utf8
Write-Host "CSV -> $CsvPath (+$($rows.Count) rows)"
