# 定量分析怎么做（工具分工）

照一份参照的定量研究项目的做法（私有仓，不外链）：**每个工具只做它最擅长的那一段**，中间用 CSV 交接。
这样每一步都能单独重跑，出了问题也知道该换哪个工具。

## 一、分工

| 段 | 工具 | 放哪 | 为什么是它 |
|---|---|---|---|
| **取数与清洗** | Python | `scripts/quant/*.py` | 抓 git / 起服务端 / 解析日志，标准库就够，跨平台 |
| **统计与作图** | **R** | `scripts/quant/analysis/*.R` | 描述统计、bootstrap、混合效应、分位回归都是它的主场；`ggplot2` 出图无需手工调 |
| **解析对象与符号核验** | **Wolfram** | `scripts/quant/analysis/*.wls` | 传递函数/极点/稳定性/噪声增益、符号推导、精确与任意精度；"R 给数，Wolfram 给式" |
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

## 二·五、单个实验：三分离（数据 / 口径 / 运算）

一个实验一个目录，四个文件各司其职：

```
scripts/quant/<experiment>/
├── data.json        数据真源（实验问题、测量日期、raw 指针）
├── config.json      口径与判据（单位、剔除规则、阈值）
├── standardize.py   标准化：raw → out/tidy.csv（只换算与标记，不做统计）
├── calc.R           运算：只读 tidy + config，输出 result.txt（PASS/FAIL）
└── 说明.md          怎么跑、阈值变更史、已知坑
```

**为什么要分**：口径一旦混进数据，换个人就不知道 `mbps=481.36` 是 MB/s 还是 MiB/s；
混进运算，就不知道那条 `FAIL` 是数据的问题还是阈值改了。
分开之后，报告里"并发 16 以上不再涨"这句话才有东西可指 —— 数据在 tidy、阈值在 config、判定在 calc。

**三条硬约定**：

1. **raw 不改**（它是仪器产物）；换算与派生列一律由 `standardize.py` 生成到 `out/`。
2. **阈值是配置，不是真理**；改了必须在 `说明.md` 的「阈值变更」里写为什么。
3. **每条判据自己跑出 PASS/FAIL**，不许写"结果与预期基本符合"。

样例：[`e1-throughput/`](e1-throughput/)（含一次真实的单位自检纠错记录）。

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
