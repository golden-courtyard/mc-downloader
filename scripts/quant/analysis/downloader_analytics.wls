(* Analytics for the downloader (Wolfram) *)
(* R gives data and intervals; this file gives the analytic objects: transfer function,
   noise gain, AIMD sawtooth, false-positive probability of the stall guard. *)
(* Constants come from the source: ALPHA = 0.3 in src/source.rs; the AIMD rule and the
   4096-byte progress floor in src/download.rs. *)
(* ASCII-only output: the Windows console mangles UTF-8, and numbers are what matter. *)
(* Usage: wolframscript -file scripts/quant/analysis/downloader_analytics.wls *)

Print["=== 1. SourcePool EWMA: noise gain and half-life ==="];
(* readable number: plain when >= 1e-3, manual "m.mmme-9" form otherwise
   (ScientificForm wraps across lines in a script console, which ruins the table) *)
(* go through Log[10, v] so values below the machine minimum still format correctly *)
fmt[v_] := Module[{l, e, m},
   If[v >= 1*^-3, ToString[N[v, 6]],
    l = N[Log[10, v]];
    e = Floor[l];
    m = 10^(l - e);
    ToString[NumberForm[m, {3, 2}]] <> "e" <> ToString[e]]];

gain[a_] := a/(2 - a);
halfLife[a_] := Log[2]/Log[1/(1 - a)];
alpha = 3/10;
Print["alpha            = ", alpha];
Print["noise gain       = ", N[gain[alpha], 6], "   (variance gain; std keeps ",
  N[100 gain[alpha]^0.5, 4], "% of the input's)"];
Print["half-life        = ", N[halfLife[alpha], 6], " samples"];
Print["pole z           = ", 1 - alpha, "  (inside the unit circle => stable; nearer 1 => slower)"];
Print[""];
Print["  alpha    noise gain   half-life"];
Do[Print[StringPadRight[ToString[a], 8], " ", StringPadRight[ToString[N[gain[a], 4]], 12],
   ToString[N[halfLife[a], 4]]], {a, {0.05, 0.1, 0.3, 0.5, 0.8}}];

Print[""];
Print["=== 2. AIMD sawtooth: mean concurrency at peak B ==="];
avgBudget[B_] := (B + B/2)/2;
Print["B=16  -> mean ", avgBudget[16], "  (", N[100 avgBudget[16]/16, 4], "% of peak)"];
Print["B=64  -> mean ", avgBudget[64], "  (", N[100 avgBudget[64]/64, 4], "% of peak)"];
Print["Ideal AIMD (add +1 per tick, halve on a >25% drop, cut at most once per 2 ticks)"];
Print["keeps a mean of 75% of the peak, independent of B. If throughput is proportional"];
Print["to concurrency, the sawtooth itself costs about 25%."];

Print[""];
Print["=== 3. Stall guard: false-positive probability under a Poisson progress process ==="];
(* one progress event per 4096 bytes; lambda = bps/4096 events per second *)
pFalse[bps_, w_] := Exp[-w bps/4096];   (* exact, so tiny values do not underflow *)
Print["  KB/s     P(gap>10s)      P(gap>20s)"];
Do[
  Print[StringPadRight[ToString[bps/1024.], 9], " ",
    StringPadRight[fmt[pFalse[bps, 10]], 16],
    fmt[pFalse[bps, 20]]],
  {bps, {1024, 4096, 16384, 65536, 262144}}];
Print["At 1 KB/s the 10 s window misfires about 8% of the time; at 64 KB/s it is ~1e-70."];
Print["That is why the window is max(10s, 2*timeout) rather than a fixed 5 s."];
