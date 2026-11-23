# 定量分析怎么做（工具分工）

照 `Academic/research/bid-info` 的做法：**每个工具只做它最擅长的那一段**，中间用 CSV 交接。
这样每一步都能单独重跑，出了问题也知道该换哪个工具。

## 一、分工

| 段 | 工具 | 放哪 | 为什么是它 |
|---|---|---|---|
| **取数与清洗** | Python | `scripts/quant/*.py` | 抓 git / 起服务端 / 解析日志，标准库就够，跨平台 |
| **统计与作图** | **R** | `scripts/quant/analysis/*.R` | 描述统计、bootstrap、混合效应、分位回归都是它的主场；`ggplot2` 出图无需手工调 |
| **关键数字交叉验算** | **Wolfram** | `scripts/quant/analysis/*.wls` | 只用来"换个引擎把同一批数再算一遍"；不做主力 |
| **成品** | Markdown | `docs/reports/*.md` | 报告与结论；图放 `docs/reports/figs/` |
| **口径** | Markdown 表格 | 报告第一节 | 先写后测，事后不改判据 |

**不用 MATLAB**：本机那份是盗版，不方便也不该在交付链路里。数值/统计需求用 R，符号推导用 Wolfram。

## 二、目录

```
scripts/quant/
  *_py / *.ps1 / *.sh      现有脚本（历史布局，仍然有效）：造故障、测速、解析日志
  analysis/*.R             R：统计 + 图
  analysis/*.wls           Wolfram：交叉验算
  raw/                     原始产物（CSV / 日志），报告引用的都在这里
docs/reports/
  *.md                     报告
  raw/*.csv                报告直接引用的数（可回溯）
  figs/*.png               图
```

工具选型、R 离线文档怎么查、还有哪些包没用上：见 [`docs/dev/quant-toolbox.md`](../../docs/dev/quant-toolbox.md)。

## 三、三步交付（缺一步就不算做完）

```bash
# 1) 取数：把外部事实变成 CSV（可重复跑）
python scripts/quant/<extract>.py

# 2) 统计与图：R 读 CSV，输出摘要 + PNG
Rscript scripts/quant/analysis/<name>.R

# 3) 交叉验算：换 Wolfram 把关键数字再算一遍，与 R 的结果并排贴进报告
wolframscript -file scripts/quant/analysis/<name>.wls
```

## 四、规矩（与 `AGENTS.md` §三 一致）

1. **口径先写、后测**：报告第一节先写"要证明什么、什么算失败"，然后才跑。
2. **每个数可回溯**：`raw CSV → 脚本 → 图/数`，报告里的图必须能由这两个脚本重新生成。
3. **没差异、被证伪、没跑，都要写**：那和"有收益"一样是交付。
4. **两个引擎允许不一致**：不一致时**先查口径**（是不是读的是同一列、有没有剔掉同一个桶），
   不是先改数。
