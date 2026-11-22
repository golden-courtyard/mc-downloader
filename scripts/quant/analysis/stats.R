#!/usr/bin/env Rscript
# ============================================================================
# scripts/quant/analysis/stats.R —— 双路径之二（R 4.6.1，独立实现）
#
# 与 fit.wls 读**同一份** scripts/quant/raw/fit-input.csv，拟**同一个**模型：
#     T = a·B + b·H      a = 1/BW, b = t_hop,  p_min = 10·t_hop·BW
# 差别在执行路径（R 用 lm() 的正规方程 + 手写 bootstrap），所以两条路给出同一组
# 数字，才说明结论不依赖某个工具的实现细节。
#
# 另外三件 R 独有的活（派发单要求）：
#   ① bootstrap CI（按运行重采样，n=18 很小，必须给区间而不是只给点估计）
#   ② P50/P95/P99（分片响应服务时间的尾部，P99/P50 是"变异性"的判据）
#   ③ 二项检验（E5 的重试是否同刻成簇）
#
# 跑法：Rscript scripts/quant/analysis/stats.R
# ============================================================================

suppressPackageStartupMessages({
  library(stats)
})

root <- normalizePath(getwd(), mustWork = TRUE)
if (!dir.exists(file.path(root, "scripts", "quant"))) {
  stop("请在仓库根目录运行：cd mc-downloader && Rscript scripts/quant/analysis/stats.R")
}
raw <- file.path(root, "scripts", "quant", "raw")
out <- file.path(root, "scripts", "quant", "analysis", "out-r.csv")

d <- read.csv(file.path(raw, "fit-input.csv"), stringsAsFactors = FALSE)
stopifnot(all(c("group", "conns", "bytes", "secs", "hops_H") %in% names(d)))

fit_group <- function(g) {
  x <- d[d$group == g, ]
  m <- lm(secs ~ 0 + bytes + hops_H, data = x)   # 0 = 无截距（口径钉死）
  a <- unname(coef(m)["bytes"])
  b <- unname(coef(m)["hops_H"])
  bw <- 1 / a
  thop <- b
  # 无截距的 R²：以 0 为中心（SST = Σy²）
  r2 <- 1 - sum(resid(m)^2) / sum(x$secs^2)
  list(model = m, n = nrow(x), a = a, b = b, BW = bw, t_hop = thop,
       p_min = 10 * thop * bw, R2 = r2)
}

boot_ci <- function(g, B = 2000, seed = 20261122) {
  set.seed(seed)
  x <- d[d$group == g, ]
  n <- nrow(x)
  aa <- bb <- numeric(B)
  for (i in seq_len(B)) {
    idx <- sample.int(n, n, replace = TRUE)
    s <- x[idx, ]
    m <- tryCatch(lm(secs ~ 0 + bytes + hops_H, data = s), error = function(e) NULL)
    if (is.null(m)) { aa[i] <- NA; bb[i] <- NA; next }
    cf <- coef(m)
    aa[i] <- unname(cf["bytes"]); bb[i] <- unname(cf["hops_H"])
  }
  q <- function(v) quantile(v, c(0.025, 0.975), na.rm = TRUE)
  list(BW = 1 / q(aa), t_hop = q(bb) * 1000, p_min = q(10 * bb / aa) / 1048576)
}

knee_conns <- function(g) {
  x <- d[d$group == g, ]
  med <- tapply(x$mbps, x$conns, median)
  thr <- 0.95 * max(med)
  as.numeric(names(med)[which(med >= thr)[1]])
}

res <- lapply(c("e1-unlimited", "e1-netem400"), function(g) {
  f <- fit_group(g); ci <- boot_ci(g)
  data.frame(group = g, n = f$n,
             a_s_per_byte = f$a, b_s_per_hop = f$b,
             BW_MBps = f$BW / 1048576, t_hop_ms = f$t_hop * 1000,
             p_min_MB = f$p_min / 1048576, R2 = f$R2,
             BW_lo = ci$BW[1] / 1048576, BW_hi = ci$BW[2] / 1048576,
             t_hop_lo = ci$t_hop[1], t_hop_hi = ci$t_hop[2],
             p_min_lo = ci$p_min[1], p_min_hi = ci$p_min[2],
             knee_conns = knee_conns(g))
})
resdf <- do.call(rbind, res)
write.csv(resdf, out, row.names = FALSE)
cat("=== R path (stats.R) ===\n"); print(resdf)

# ---- ② 分片响应服务时间的 P50/P95/P99（尾部判据） ----
pt <- read.csv(file.path(raw, "piece-times.csv"))
big <- pt[pt$pieces == 64, ]                     # 只取 1 GiB 的轮次，片大小/片数一致
q <- function(v, p) as.numeric(quantile(v, p))
tail <- do.call(rbind, lapply(sort(unique(big$concurrency)), function(c) {
  v <- big$p50_secs[big$concurrency == c]
  data.frame(concurrency = c, bursts = length(v),
             P50 = q(v, .50), P95 = q(v, .95), P99 = q(v, .99),
             P99_over_P50 = q(v, .99) / q(v, .50))
}))
cat("\n=== piece service time percentiles (1 GiB runs, server-side) ===\n"); print(tail)
write.csv(tail, file.path(root, "scripts", "quant", "analysis", "out-r-percentiles.csv"), row.names = FALSE)

# ---- ③ 二项检验：E5 的重试是不是「同刻成簇」 ----
# 零假设：重试时刻在退避窗口内均匀抖动 ⇒ 一波 4 个请求落在 200 ms 内的概率 ≈ 0.2/窗口
# 观测：4 波里 3 波 spread < 200 ms（实测 4.1 / 169.7 / 6.5 / 4.7 ms）
w <- read.csv(file.path(raw, "e5-waves.csv"))
win <- c(0, 2.148, 3.191, 5.030)          # 每波拿到的退避窗口（实测）——
# 保守取「名义退避 1/2/4 s」而不是实测窗口，避免把测量误差当收益
nominal <- c(1, 1, 2, 4)
clustered <- w$spread_ms < 200
p_null <- mean(200 / (nominal * 1000))     # 单波在零假设下被判为「同刻」的概率
k <- sum(clustered); n <- length(clustered)
bt <- binom.test(k, n, p = p_null, alternative = "greater")
cat("\n=== E5 cluster test (binomial, null = uniform jitter within backoff window) ===\n")
cat(sprintf("waves=%d clustered=%d p_null=%.3f  p-value=%.3g  CI=[%.3f,1]\n",
            n, k, p_null, bt$p.value, bt$conf.int[1]))

# ---- 落一份给报告直接引用的汇总 ----
sink(file.path(root, "scripts", "quant", "analysis", "out-r-summary.txt"))
cat("R fit (T = a*B + b*H, no intercept)\n"); print(resdf)
cat("\nPiece-level percentiles (1 GiB runs)\n"); print(tail)
cat(sprintf("\nE5 clustering: %d/%d waves within 200 ms; binomial p=%.3g (null p=%.3f)\n",
            k, n, bt$p.value, p_null))
sink()
cat("\nwrote:", out, "\n")
