# 版本号 vs 决策记录：描述统计与图（R）
#
# 输入：docs/reports/raw/versioning-vs-notes.csv（由 scripts/quant/versioning_vs_notes.py 生成）
# 输出：摘要打到 stdout；图存 docs/reports/figs/fig-versioning-notes.png
#
# 口径与 Python 端一致：**剔除第一行**（仓库根到首个标签，包含整段历史，不是一次发布）。

suppressPackageStartupMessages({
  library(ggplot2)
})

# 仓库根：默认取当前工作目录；也可以作为第一个参数传进来
args <- commandArgs(trailingOnly = TRUE)
root <- if (length(args) >= 1) args[1] else getwd()
if (!file.exists(file.path(root, "Cargo.toml"))) {
  stop("请在仓库根目录运行，或把仓库根作为第一个参数传入：Rscript ... <repo-root>")
}

csv <- file.path(root, "docs/reports/raw/versioning-vs-notes.csv")
stopifnot(file.exists(csv))
d <- read.csv(csv, stringsAsFactors = FALSE)

# 与 Python 端同一条口径：第一行是 repo root → 首个 tag 的桶
d <- d[-1, ]

num <- function(x) as.numeric(ifelse(x == "", NA, x))

d$commits <- num(d$commits)
d$impl <- num(d$add_implemented)
d$prop <- num(d$add_proposed)
d$trans <- num(d$transitions)
d$impl_per_100 <- num(d$impl_per_100_commits)

seg <- ifelse(grepl("-alpha\\.", d$tag), "alpha", ifelse(grepl("-rc\\.", d$tag), "rc", "stable"))
d$seg <- factor(seg, levels = c("alpha", "rc", "stable"))

cat("== 区间数与分布（剔除 root 桶）==\n")
cat(sprintf("n = %d\n", nrow(d)))
for (k in c("commits", "impl", "prop", "trans")) {
  v <- d[[k]]
  cat(sprintf("%-8s median=%7.1f  mean=%7.1f  max=%7.0f\n", k, median(v), mean(v), max(v)))
}

cat("\n== 按 bump_kind（中位 implemented / 中位提交）==\n")
agg <- aggregate(cbind(impl, commits) ~ bump_kind, data = d, FUN = function(x) median(x, na.rm = TRUE))
agg$n <- as.vector(table(d$bump_kind)[agg$bump_kind])
print(agg[order(-agg$n), ], row.names = FALSE)

cat("\n== alpha vs rc：implemented 的类别构成（份额 %，括号内为 bootstrap 95% CI）==\n")
boot_share <- function(sub, col, B = 2000) {
  tot <- sum(sub$impl, na.rm = TRUE)
  if (tot == 0) return(c(NA, NA, NA))
  x <- sub[[col]]
  s <- replicate(B, {
    i <- sample(nrow(sub), replace = TRUE)
    100 * sum(x[i], na.rm = TRUE) / sum(sub$impl[i], na.rm = TRUE)
  })
  c(100 * sum(x, na.rm = TRUE) / tot, quantile(s, 0.025), quantile(s, 0.975))
}

for (g in c("alpha", "rc")) {
  sub <- d[d$seg == g, ]
  cat(sprintf("\n[%s] n=%d  implemented=%d\n", g, nrow(sub), sum(sub$impl, na.rm = TRUE)))
  for (col in c("impl_feature", "impl_bugfix", "impl_arch", "impl_process")) {
    r <- boot_share(sub, col)
    cat(sprintf("  %-12s %5.1f%%  [%5.1f, %5.1f]\n", col, r[1], r[2], r[3]))
  }
}

cat("\n== 作图 ==\n")
dir.create(file.path(root, "docs/reports/figs"), showWarnings = FALSE, recursive = TRUE)
p1 <- ggplot(d, aes(x = commits, y = impl, colour = seg)) +
  geom_point(size = 2, alpha = 0.8) +
  geom_smooth(method = "lm", se = FALSE, linewidth = 0.7) +
  scale_x_log10() +
  scale_y_log10() +
  labs(
    title = "每次发布的决策记录产出",
    subtitle = "43 个标签区间（剔除 repo root 桶）；双对数",
    x = "提交数（log）", y = "新增 implemented notes（log）", colour = "段"
  ) +
  theme_minimal(base_size = 11)

p2 <- ggplot(d, aes(x = seg, y = impl, fill = seg)) +
  geom_boxplot(alpha = 0.7, outlier.shape = NA) +
  geom_jitter(width = 0.12, size = 1.4, alpha = 0.6) +
  labs(title = "alpha 段 vs rc 段的批量", x = NULL, y = "每区间新增 implemented") +
  theme_minimal(base_size = 11) +
  theme(legend.position = "none")

png(file.path(root, "docs/reports/figs/fig-versioning-notes.png"), width = 1600, height = 700, res = 130)
gridExtra_ok <- requireNamespace("gridExtra", quietly = TRUE)
if (gridExtra_ok) {
  gridExtra::grid.arrange(p1, p2, ncol = 2)
} else {
  print(p1)
}
dev.off()
cat("图: docs/reports/figs/fig-versioning-notes.png\n")
