# e1-throughput 的「运算」层：只读 tidy.csv + config.json，输出 result.txt。
#
# 这里不含数据、不含口径：数据来自 tidy.csv，阈值全部从 config.json 读。
# 每条判据输出 PASS / FAIL —— 不允许"结果与预期基本符合"这种写法。
#
# 用法：Rscript scripts/quant/e1-throughput/calc.R [<repo-root>]

args <- commandArgs(trailingOnly = TRUE)
here <- if (length(args) >= 1) file.path(args[1], "scripts/quant/e1-throughput") else getwd()
cfg_path <- file.path(here, "config.json")
tidy_path <- file.path(here, "out", "tidy.csv")
stopifnot(file.exists(cfg_path), file.exists(tidy_path))

cfg <- jsonlite::fromJSON(cfg_path)
d <- read.csv(tidy_path, stringsAsFactors = FALSE)
d <- d[d$exit_ok == 1, ]

med <- function(c) median(d$mib_per_s[d$conns == c], na.rm = TRUE)
n_of <- function(c) sum(d$conns == c)
ratio <- function(a, b) med(a) / med(b)

lines <- character(0)
add <- function(s) lines <<- c(lines, s)

add("e1-throughput result")
add(sprintf("rows used = %d   groups = %s", nrow(d), paste(sort(unique(d$conns)), collapse = ",")))
add("")
add("  conns    n    median MiB/s")
for (c in sort(unique(d$conns))) {
  add(sprintf("  %5d %4d    %10.2f", c, n_of(c), med(c)))
}
add("")

# ---- 判据（阈值来自 config.json）----
verdict <- function(name, ok, detail) {
  add(sprintf("%-28s %-4s  %s", name, if (isTRUE(ok)) "PASS" else "FAIL", detail))
}

th <- cfg$thresholds
verdict("data/unit-check", all(d$unit_check == "ok"),
        if (all(d$unit_check == "ok")) "mbps 与 bytes/1048576/secs 一致" else "单位不一致，结论作废")

verdict("data/min-runs", all(sapply(sort(unique(d$conns)), function(c) n_of(c) >= cfg$min_runs_per_group)),
        sprintf("每组至少 %d 次", cfg$min_runs_per_group))

verdict("parallel-gain(8 vs 1)", ratio(8, 1) >= th$c8_over_c1,
        sprintf("c8/c1 = %.3f  (阈值 >= %.2f)", ratio(8, 1), th$c8_over_c1))

verdict("c16-over-c4", ratio(16, 4) >= th$c16_over_c4,
        sprintf("c16/c4 = %.3f  (阈值 >= %.2f)", ratio(16, 4), th$c16_over_c4))

verdict("c64-not-worse-than-c16", ratio(64, 16) >= th$c64_over_c16,
        sprintf("c64/c16 = %.3f  (阈值 >= %.2f)", ratio(64, 16), th$c64_over_c16))

out <- file.path(here, "result.txt")
writeLines(lines, out, useBytes = TRUE)
cat(paste(lines, collapse = "\n"), "\n")
cat("\nwrote ", out, "\n", sep = "")
