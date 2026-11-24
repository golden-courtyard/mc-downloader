# 检查 proofs/ 是否可编译、是否有 sorry。
#
# 只在改过 proofs/ 时需要跑；CI 不跑（理由见 proofs/PROBE.md「为什么不进 CI」）。
# 判据：lake build 退出 0，且 Proofs/*.lean 里没有 `sorry`。

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
$dir = Join-Path $root "proofs"
Push-Location $dir
try {
    $files = Get-ChildItem -Path (Join-Path $dir "Proofs") -Filter *.lean
    if (-not $files) { throw "proofs/Proofs 下没有 .lean 文件" }

    $out = & lake build 2>&1
    $code = $LASTEXITCODE
    $out | ForEach-Object { $_.ToString() }
    if ($code -ne 0) { throw "lake build 失败（exit $code）" }

    foreach ($f in $files) {
        $hit = Select-String -Path $f.FullName -Pattern "\bsorry\b" -ErrorAction SilentlyContinue
        if ($hit) { throw "发现 sorry：$($f.Name):$($hit.LineNumber)" }
    }

    "{0}/{0} OK（无 sorry）" -f $files.Count
}
finally {
    Pop-Location
}
