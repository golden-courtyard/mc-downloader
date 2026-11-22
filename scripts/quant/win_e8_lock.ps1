# ============================================================================
# scripts/quant/win_e8_lock.ps1 —— E8b：实时防护会不会锁文件 / 干扰 rename+delete
#
# 为什么这个实验比 1 GiB 墙钟更快出结论：
#   杀软的实时防护典型形态不是"拖慢每个写"，而是"文件关闭后先扫一遍再放行"——
#   表现就是 close 之后立刻 rename/delete/exclusive-open 失败（os error 32 / 5），
#   或者要等几十到几百毫秒才成功。这个实验直接量"等多久才放行"，比墙钟差灵敏得多。
#
# 三个测试：
#   A. 合成 100 MB（P/Invoke 之外只用 .NET FileStream，1 MiB 缓冲写）：Close 之后
#      循环尝试 ①独占打开（FileShare.None）②重命名 ③删除，记录每次尝试的耗时与错误码。
#   B. 下载进行中：.part 存在期间，重复尝试"独占打开"与"共享读取 1 MiB"，
#      看是否出现 os error 32（共享冲突）——**不重命名、不删除**，避免破坏下载。
#   C. 下载刚结束：对最终文件做 A 的三步，量"下载器关文件之后"有没有额外的放行延迟。
#
# 判据（写死）：出现任何 os error 32/5 ⇒ 记为"实时防护会干扰写路径"。
#
# ⚠ 这个脚本会**反复重命名与删除自己创建的文件**，静态/行为启发式很容易把它判成可疑：
#   它只碰两个自己建的路径（$Dir\lock100.bin 与 $Dir\dl\q1g.bin），不碰任何用户文件、
#   不联网（除 127.0.0.1 的本地假源）、不改注册表、不碰服务。若杀软报警，按这两条路径核对即可。
#
# 用法：
#   pwsh -NoProfile -File scripts/quant/win_e8_lock.ps1 -ExePath C:\quant-target\herta\release\mc-downloader.exe
# 产出：scripts/quant/raw/e8-lock.csv
# ============================================================================
param(
    [Parameter(Mandatory = $true)][string]$ExePath,
    [string]$Url = 'http://127.0.0.1:8899/q1g.bin',
    [string]$Name = 'q1g.bin',
    [string]$Dir = 'D:\quant-av',
    [int]$BigMiB = 100,
    [int]$Attempts = 20,
    [int]$AttemptSleepMs = 50,
    [string]$Csv = 'scripts\quant\raw\e8-lock.csv'
)

$ErrorActionPreference = 'Stop'
$repo = Split-Path (Split-Path $PSScriptRoot -Parent) -Parent
$csvPath = if ([System.IO.Path]::IsPathRooted($Csv)) { $Csv } else { Join-Path $repo $Csv }
New-Item -ItemType Directory -Force -Path (Split-Path $csvPath) | Out-Null
New-Item -ItemType Directory -Force -Path $Dir | Out-Null

$rows = New-Object System.Collections.Generic.List[object]
function Add-Row($test, $attempt, $op, $ok, $errno, $ms, $note) {
    $rows.Add([pscustomobject]@{
            test = $test; attempt = $attempt; op = $op; ok = [int]$ok
            errno = $errno; elapsed_ms = [math]::Round($ms, 3); note = $note
        })
}

function Invoke-Probe {  # Invoke-Probe <test> <attempt> <path>
    param($test, $attempt, $path)

    # ① 独占打开（任何其他句柄在场都会失败：32）
    $t0 = [System.Diagnostics.Stopwatch]::StartNew()
    try {
        $fs = [System.IO.File]::Open($path, 'Open', 'ReadWrite', 'None')
        $fs.Dispose()
        Add-Row $test $attempt 'exclusive_open' $true 0 $t0.Elapsed.TotalMilliseconds ''
    } catch {
        Add-Row $test $attempt 'exclusive_open' $false $_.Exception.HResult $t0.Elapsed.TotalMilliseconds $_.Exception.Message
    }

    # ② 重命名（换名不改内容，杀软挂句柄时会失败）
    $tmp = "$path.renamed"
    $t0 = [System.Diagnostics.Stopwatch]::StartNew()
    try {
        [System.IO.File]::Move($path, $tmp)
        [System.IO.File]::Move($tmp, $path)
        Add-Row $test $attempt 'rename_roundtrip' $true 0 $t0.Elapsed.TotalMilliseconds ''
    } catch {
        Add-Row $test $attempt 'rename_roundtrip' $false $_.Exception.HResult $t0.Elapsed.TotalMilliseconds $_.Exception.Message
        if (Test-Path $tmp) { try { [System.IO.File]::Move($tmp, $path) } catch { } }
    }
}

# ---------------- A. 合成 100 MB ----------------
$big = Join-Path $Dir 'lock100.bin'
Remove-Item $big, "$big.renamed" -Force -ErrorAction SilentlyContinue
$buf = New-Object byte[] (1MB)
$fs = [System.IO.File]::Open($big, 'Create', 'Write', 'None')
$sw = [System.Diagnostics.Stopwatch]::StartNew()
for ($i = 0; $i -lt $BigMiB; $i++) { $fs.Write($buf, 0, $buf.Length) }
$fs.Flush($true)
$fs.Dispose()
$sw.Stop()
Write-Host ("A: 写入 {0} MiB 用 {1:N2}s ({2:N1} MB/s)" -f $BigMiB, $sw.Elapsed.TotalSeconds,
    ($BigMiB / $sw.Elapsed.TotalSeconds))
Add-Row 'A-write100mb' 0 'write' $true 0 $sw.Elapsed.TotalMilliseconds "$BigMiB MiB sequential"

# close 之后立刻探（这是"放行延迟"最敏感的窗口）
Invoke-Probe 'A-immediately-after-close' 1 $big
Start-Sleep -Milliseconds $AttemptSleepMs
Invoke-Probe 'A-immediately-after-close' 2 $big

# 删除
$t0 = [System.Diagnostics.Stopwatch]::StartNew()
try {
    [System.IO.File]::Delete($big)
    Add-Row 'A-delete' 1 'delete' $true 0 $t0.Elapsed.TotalMilliseconds ''
} catch {
    Add-Row 'A-delete' 1 'delete' $false $_.Exception.HResult $t0.Elapsed.TotalMilliseconds $_.Exception.Message
}

# ---------------- B. 下载进行中探 .part ----------------
$target = Join-Path $Dir 'dl'
if (Test-Path $target) { Remove-Item -Recurse -Force $target }
New-Item -ItemType Directory -Force -Path $target | Out-Null
$outLog = Join-Path $env:TEMP 'mcd-e8lock.out'
$proc = Start-Process -FilePath $ExePath -ArgumentList @('fetch', $Url, '-d', $target, '-n', $Name,
    '--piece-conns', '16', '--timeout', '30', '--retry', '3', '--force') `
    -NoNewWindow -PassThru -RedirectStandardOutput $outLog -RedirectStandardError (Join-Path $env:TEMP 'mcd-e8lock.err')

$part = Join-Path $target "$Name.part"
$deadline = (Get-Date).AddSeconds(20)
while (-not (Test-Path $part) -and (Get-Date) -lt $deadline -and -not $proc.HasExited) { Start-Sleep -Milliseconds 50 }
$hit = 0
for ($i = 1; $i -le $Attempts -and -not $proc.HasExited; $i++) {
    $t0 = [System.Diagnostics.Stopwatch]::StartNew()
    try {
        $s = [System.IO.File]::Open($part, 'Open', 'Read', 'ReadWrite')
        $b = New-Object byte[] (1MB)
        $n = $s.Read($b, 0, $b.Length)
        $s.Dispose()
        Add-Row 'B-during-download' $i 'shared_read_1mb' $true 0 $t0.Elapsed.TotalMilliseconds "read=$n"
        $hit++
    } catch {
        Add-Row 'B-during-download' $i 'shared_read_1mb' $false $_.Exception.HResult $t0.Elapsed.TotalMilliseconds $_.Exception.Message
    }
    $t0 = [System.Diagnostics.Stopwatch]::StartNew()
    try {
        $s = [System.IO.File]::Open($part, 'Open', 'ReadWrite', 'None')
        $s.Dispose()
        Add-Row 'B-during-download' $i 'exclusive_open' $true 0 $t0.Elapsed.TotalMilliseconds ''
    } catch {
        Add-Row 'B-during-download' $i 'exclusive_open' $false $_.Exception.HResult $t0.Elapsed.TotalMilliseconds $_.Exception.Message
    }
    Start-Sleep -Milliseconds $AttemptSleepMs
}
$proc.WaitForExit()
$landed = Join-Path $target $Name
$landedLen = -1
if (Test-Path $landed) { $landedLen = (Get-Item $landed).Length }
Write-Host ("B: 尝试 {0} 轮，成功读到 .part {1} 次；下载 exit={2} landed={3}" -f $Attempts, $hit,
    $proc.ExitCode, $landedLen)

# ---------------- C. 下载刚结束 ----------------
if (Test-Path $landed) {
    Invoke-Probe 'C-just-after-download' 1 $landed
    $t0 = [System.Diagnostics.Stopwatch]::StartNew()
    try {
        [System.IO.File]::Delete($landed)
        Add-Row 'C-delete' 1 'delete' $true 0 $t0.Elapsed.TotalMilliseconds ''
    } catch {
        Add-Row 'C-delete' 1 'delete' $false $_.Exception.HResult $t0.Elapsed.TotalMilliseconds $_.Exception.Message
    }
}

$rows | Export-Csv -Path $csvPath -NoTypeInformation -Append -Encoding utf8
# 汇总
$fail32 = @($rows | Where-Object { $_.ok -eq 0 -and ($_.errno -eq -2147024864 -or $_.note -match '32|5') })
Write-Host ""
Write-Host "=== 汇总 ==="
$rows | Group-Object test, op | ForEach-Object {
    $g = $_.Group
    $bad = @($g | Where-Object { $_.ok -eq 0 }).Count
    Write-Host ("{0,-46} n={1,-3} 失败={2,-3} 中位耗时={3:N2}ms" -f $_.Name, $g.Count, $bad,
        (($g | Measure-Object elapsed_ms -Average).Average))
}
Write-Host ""
Write-Host ("出现 os error 32/5 的行数: {0}" -f $fail32.Count)
$rows | Where-Object { $_.ok -eq 0 } | Select-Object -First 12 | Format-Table -Auto
Write-Host "CSV -> $csvPath"
