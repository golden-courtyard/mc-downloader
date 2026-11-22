# ============================================================================
# scripts/quant/win_e5.ps1 —— E5: 恒 500 源下的重试行为（Windows）
#
# 量两件事（判据在报告 §7 里写死）：
#   ① --retry 0：每次运行只发几个请求？（预期 1 个 = 单次尝试即放弃）
#   ② --retry 3 + 4 个分片：16 个请求的时刻分布 —— 同一波的分片是不是"同刻重试"
#      （无 jitter ⇒ 瞬时并发翻倍 ⇒ issue #4 §二 的正反馈风险）
#
# 为什么用 -s 直接给大小：跳过 HEAD 探测，让 4 个分片请求立刻并发起来，
# 否则 HEAD 也会吃 500，测到的是"探测失败"而不是"分片重试"。
#
# 用法：pwsh -NoProfile -File scripts/quant/win_e5.ps1
# ============================================================================
param(
    [string]$ExePath = 'D:\mcd-target\herta\release\mc-downloader.exe',
    [int]$Port = 8898,
    [string]$Raw = 'scripts\quant\raw'
)
$ErrorActionPreference = 'Stop'
$repo = Split-Path (Split-Path $PSScriptRoot -Parent) -Parent
$log = Join-Path $repo "$Raw\e5-server-cluster.log"
New-Item -ItemType Directory -Force -Path (Join-Path $repo $Raw) | Out-Null
$target = 'C:\quant-target-e5'

$srv = Start-Process python -ArgumentList @((Join-Path $repo 'scripts\quant\always500.py'),
        '--port', "$Port", '--log', $log) -WindowStyle Hidden -PassThru
Start-Sleep -Seconds 1
try {
    $rows = @()
    foreach ($r in 1, 2, 3) {
        if (Test-Path $log) { Clear-Content $log }
        $t0 = Get-Date
        $o = & $ExePath fetch "http://127.0.0.1:$Port/f.bin" -d $target -n e5.bin `
            -s 4194304 --piece-conns 1 --retry 0 --force 2>&1
        $rc = $LASTEXITCODE
        $dt = ((Get-Date) - $t0).TotalSeconds
        Start-Sleep -Milliseconds 300
        $n = (Select-String -Path $log -Pattern 'ev=req' -AllMatches).Count
        $rows += [pscustomobject]@{ arm = 'retry0'; round = $r; conns = 1; retry = 0
            reqs = $n; secs = [math]::Round($dt, 2); exit = $rc
            tail = ($o | Where-Object { $_ -match '500|错误|error' } | Select-Object -Last 1) }
    }
    if (Test-Path $log) { Clear-Content $log }
    $t0 = Get-Date
    $o2 = & $ExePath fetch "http://127.0.0.1:$Port/f.bin" -d $target -n e5b.bin `
        -s 67108864 --piece-conns 4 --retry 3 --force 2>&1
    $rc2 = $LASTEXITCODE
    $dt2 = ((Get-Date) - $t0).TotalSeconds
    Start-Sleep -Milliseconds 300
    $n2 = (Select-String -Path $log -Pattern 'ev=req' -AllMatches).Count
    $rows += [pscustomobject]@{ arm = 'retry3-cluster'; round = 1; conns = 4; retry = 3
        reqs = $n2; secs = [math]::Round($dt2, 2); exit = $rc2
        tail = ($o2 | Where-Object { $_ -match '重试|错误|error' } | Select-Object -Last 1) }
    $rows | Format-Table -Auto
    $rows | Export-Csv (Join-Path $repo "$Raw\e5-windows.csv") -NoTypeInformation -Encoding utf8
} finally {
    Stop-Process -Id $srv.Id -Force -ErrorAction SilentlyContinue
    if (Test-Path $log) { Move-Item -Force $log (Join-Path $repo "$Raw\e5-server-cluster.log") }
}
Write-Host "备注：分片响应的耗时尾部统计见 $Raw\piece-times.csv（build_inputs.py 生成）"
