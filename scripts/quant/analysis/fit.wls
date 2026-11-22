(* ==========================================================================
   scripts/quant/analysis/fit.wls —— 双路径之一（Wolfram 15.0）

   模型（派发单钉死，R 路径读同一份输入、同一模型）：
       T = a·B + b·H          a = 1/BW, b = t_hop
   无截距的二元最小二乘 = "过原点线性回归"的二维版本。
   p_min = 10 · t_hop · BW —— 让一片的传输时间是"跳变代价"10 倍所需的最小片大小。

   为什么不留截距：在 6 个 conns 点上 H 与常数项高度共线，留截距会让 b 与截距互相
   吸收，t_hop 不再可识别（这正是本实验要报的量）。

   输入：scripts/quant/raw/fit-input.csv（build_inputs.py 生成）
   输出：analysis/out-wolfram.csv、figures/fig{1,2,3}-*.png
   跑法：wolframscript -file scripts/quant/analysis/fit.wls
   ========================================================================== *)

root = DirectoryName[DirectoryName[DirectoryName[DirectoryName[$InputFileName]]]];
raw = FileNameJoin[{root, "scripts", "quant", "raw", "fit-input.csv"}];
fig = FileNameJoin[{root, "scripts", "quant", "figures"}];
out = FileNameJoin[{root, "scripts", "quant", "analysis", "out-wolfram.csv"}];

If[!FileExistsQ[raw], Print["缺输入: " <> raw]; Quit[1]];

num[x_] := If[NumericQ[x], x, ToExpression[ToString[x]]];
data = Rest[Import[raw, "CSV"]];
rows = <|"group" -> #[[1]], "conns" -> num[#[[2]]], "bytes" -> num[#[[4]]],
         "secs" -> num[#[[5]]], "mbps" -> num[#[[6]]], "hops" -> num[#[[7]]]|> & /@ data;

byGroup[g_] := Select[rows, #["group"] === g &];
groups = {"e1-unlimited", "e1-netem400"};

fitOne[grp_] := Module[{rs, dm, y, x, a, b, bw, thop, pmin, sse, sst, r2},
  rs = byGroup[grp];
  dm = N[({#["bytes"], #["hops"]} & /@ rs)];
  y = N[(#["secs"] & /@ rs)];
  x = LeastSquares[dm, y];
  {a, b} = x;
  bw = 1/a; thop = b; pmin = 10 thop bw;
  sse = Total[(y - dm . x)^2];
  sst = Total[y^2];
  r2 = 1 - sse/sst;
  <|"group" -> grp, "n" -> Length[rs], "a" -> a, "b" -> b,
    "BW_MBps" -> bw/1048576, "t_hop_ms" -> thop*1000,
    "p_min_MB" -> pmin/1048576, "R2" -> r2|>
  ];
fits = fitOne /@ groups;

(* 各 conns 的中位吞吐 / 中位 H *)
medBy[grp_, key_] := Module[{rs = byGroup[grp], cs},
  cs = Union[#["conns"] & /@ rs];
  Table[{c, Median[N[Select[rs, #["conns"] === c &][[All, key]]]]}, {c, cs}]];
kneeOf[grp_] := Module[{t, mx, thr, pick},
  t = medBy[grp, "mbps"]; mx = Max[t[[All, 2]]]; thr = 0.95 mx;
  pick = Select[t, #[[2]] >= thr &, 1];
  If[pick === {}, Missing["None"], pick[[1, 1]]]];

p1 = ListLinePlot[{medBy["e1-unlimited", "mbps"], medBy["e1-netem400", "mbps"]},
   PlotMarkers -> Automatic, Frame -> True, ImageSize -> 700, PlotRange -> All,
   FrameLabel -> {"--piece-conns", "throughput (MB/s)"},
   PlotLegends -> {"loopback unlimited", "netem 400 mbit"},
   PlotLabel -> "E1: throughput vs piece-conns (256 MiB, median of 3)"];
p2 = Module[{f = fits[[1]], rs = byGroup["e1-unlimited"], hmax, bb = 256.*1048576},
   hmax = Max[#["hops"] & /@ rs];
   Show[ListPlot[({#["hops"], #["secs"]} & /@ rs), Frame -> True, ImageSize -> 700,
     PlotRange -> All, FrameLabel -> {"H (measured write-order hops)", "T (s)"},
     PlotLabel -> "E1 loopback: T vs H (dashed = fit at fixed B=256 MiB)"],
    Plot[f["a"] bb + f["b"] h, {h, 0, hmax}, PlotStyle -> Dashed]]];
p3 = BarChart[medBy["e1-unlimited", "hops"][[All, 2]],
   ChartLabels -> (ToString /@ (medBy["e1-unlimited", "hops"][[All, 1]])),
   Frame -> True, ImageSize -> 700,
   FrameLabel -> {"--piece-conns", "H (median hops)"},
   PlotLabel -> "E1: write-order hops grow with concurrency"];

CreateDirectory[fig];
Export[FileNameJoin[{fig, "fig1-throughput-vs-conns.png"}], p1, "PNG"];
Export[FileNameJoin[{fig, "fig2-T-vs-H.png"}], p2, "PNG"];
Export[FileNameJoin[{fig, "fig3-hops-vs-conns.png"}], p3, "PNG"];

hdr = {"group", "n", "a_s_per_byte", "b_s_per_hop", "BW_MBps", "t_hop_ms", "p_min_MB", "R2", "knee_conns"};
rec[f_] := {f["group"], f["n"], f["a"], f["b"], f["BW_MBps"], f["t_hop_ms"],
   f["p_min_MB"], f["R2"], kneeOf[f["group"]]};
Export[out, Prepend[rec /@ fits, hdr], "CSV"];

Print["=== Wolfram path (fit.wls) ==="];
Print[Grid[Prepend[rec /@ fits, hdr], Frame -> All]];
Print["wrote: " <> out];
