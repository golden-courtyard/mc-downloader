(* Cross-check of the versioning-vs-notes numbers (Wolfram) *)
(* Reads the same CSV as scripts/quant/analysis/versioning_vs_notes.R and recomputes the key numbers. *)
(* Usage: wolframscript -file scripts/quant/analysis/versioning_vs_notes.wls *)
(* ASCII-only output: the Windows console mangles UTF-8 here, and the numbers are what matter. *)

csv = FileNameJoin[{Directory[], "docs", "reports", "raw", "versioning-vs-notes.csv"}];
If[! FileExistsQ[csv], Print["CSV not found: ", csv]; Exit[1]];

data = Normal[Import[csv, "Dataset", Header -> True]];
(* same convention as Python/R: the first row is the repo-root bucket, not a release *)
body = Rest[data];
Print["n (intervals) = ", Length[body]];

num[v_] := If[v === "" || v === Missing["NotAvailable"], Missing[], ToExpression[v]];
field[name_] := num /@ (body[[All, name]]);

impl = field["add_implemented"];
commits = field["commits"];
feature = field["impl_feature"];

Print["median implemented = ", N[Median[DeleteMissing[impl]], 6]];
Print["mean   implemented = ", N[Mean[DeleteMissing[impl]], 6]];
Print["median commits     = ", N[Median[DeleteMissing[commits]], 6]];

segOf[tag_] := Which[
  StringContainsQ[tag, "-alpha."], "alpha",
  StringContainsQ[tag, "-rc."], "rc",
  True, "stable"];
segs = segOf /@ body[[All, "tag"]];

(* share of feature notes among implemented, per segment; keep rows where both are present *)
share[seg_] := Module[{idx, i, f, ok},
  idx = Flatten@Position[segs, seg];
  i = impl[[idx]];
  f = feature[[idx]];
  ok = MapThread[And, {Not@*MissingQ /@ i, Not@*MissingQ /@ f}];
  i = Pick[i, ok];
  f = Pick[f, ok];
  If[Total[i] == 0, Missing[], N[100 Total[f]/Total[i], 6]]
];

Print["alpha: feature share (%) = ", share["alpha"]];
Print["rc   : feature share (%) = ", share["rc"]];
