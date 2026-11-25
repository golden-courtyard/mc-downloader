set_option maxRecDepth 10000

/- 片切分逻辑的模型（对应 `src/piece.rs` 的 `PieceTable::with_conns` / `range`）。

常量与实现一致：`NO_SPLIT_BELOW = 8 MiB`、`MIN_PIECE = 1 MiB`、`MAX_PIECE = 16 MiB`、`PAGE = 4096`。

未证明：真实实现的位图、续传、IO —— 这里只建模**纯算术**的切分。 -/

namespace Pieces

def PAGE : Nat := 4096
def MIN_PIECE : Nat := 1024 * 1024
def MAX_PIECE : Nat := 16 * 1024 * 1024
def NO_SPLIT_BELOW : Nat := 8 * 1024 * 1024

/-- 向下取整到页（`PAGE` 是 2 的幂，等价于 `x & !(PAGE-1)`）。 -/
def floorToPage (x : Nat) : Nat := x / PAGE * PAGE

/-- 片的字节数。小文件不分片（一片覆盖整个文件）。 -/
def pieceSize (fileSize conns : Nat) : Nat :=
  if fileSize < NO_SPLIT_BELOW then max fileSize 1
  else floorToPage (min MAX_PIECE (max MIN_PIECE (fileSize / max conns 1)))

/-- 片数 = ⌈fileSize / pieceSize⌉（至少 1）。 -/
def total (fileSize conns : Nat) : Nat :=
  max ((fileSize + pieceSize fileSize conns - 1) / pieceSize fileSize conns) 1

/-- 第 i 片的起始偏移。 -/
def start (fileSize conns i : Nat) : Nat := i * pieceSize fileSize conns

/-- 第 i 片的结束偏移（不含）。 -/
def stop (fileSize conns i : Nat) : Nat :=
  min ((i + 1) * pieceSize fileSize conns) fileSize

theorem pieceSize_pos (fileSize conns : Nat) : 0 < pieceSize fileSize conns := by
  unfold pieceSize
  split
  · exact Nat.lt_of_lt_of_le (by decide : 0 < 1) (Nat.le_max_right _ _)
  · unfold floorToPage
    have hpage : 0 < PAGE := by decide
    have hge : PAGE ≤ min MAX_PIECE (max MIN_PIECE (fileSize / max conns 1)) := by
      refine Nat.le_min.mpr ⟨by decide, ?_⟩
      exact Nat.le_trans (by decide : PAGE ≤ MIN_PIECE) (Nat.le_max_left _ _)
    have : 0 < min MAX_PIECE (max MIN_PIECE (fileSize / max conns 1)) / PAGE :=
      Nat.div_pos hge hpage
    exact Nat.mul_pos this hpage

/-- 每片长度不超过 `pieceSize`：`stop ≤ start + pieceSize`。 -/
theorem len_le_piece (fileSize conns i : Nat) :
    stop fileSize conns i ≤ start fileSize conns i + pieceSize fileSize conns := by
  unfold stop start
  refine Nat.le_trans (Nat.min_le_left _ _) ?_
  have : (i + 1) * pieceSize fileSize conns = i * pieceSize fileSize conns + pieceSize fileSize conns := by
    rw [Nat.succ_mul]
  omega

/-- 片不越界：`stop ≤ fileSize`。 -/
theorem stop_le_fileSize (fileSize conns i : Nat) : stop fileSize conns i ≤ fileSize := by
  unfold stop
  exact Nat.min_le_right _ _

/-- 相邻片首尾相接：前一片的停点要么正好是后一片的起点，要么已经到文件末尾。 -/
theorem contiguous (fileSize conns i : Nat) :
    stop fileSize conns i = start fileSize conns (i + 1) ∨ stop fileSize conns i = fileSize := by
  unfold stop start
  rcases Nat.le_total ((i + 1) * pieceSize fileSize conns) fileSize with h | h
  · left
    rw [Nat.min_eq_left h, Nat.succ_mul]
  · right
    exact Nat.min_eq_right h

/-- 契约（**带条件**）：文件足够大时，片数才保证不少于连接数。

`fileSize ≥ conns × MIN_PIECE` 与 `fileSize ≥ NO_SPLIT_BELOW` 同时成立时，`conns ≤ total`。
没有第一个条件时契约不成立 —— 例如 20 MiB 文件配 32 连接只切成 20 片（每片夹到 1 MiB 下界）。 -/
theorem total_ge_conns (fileSize conns : Nat)
    (hbig : NO_SPLIT_BELOW ≤ fileSize) (hmin : conns * MIN_PIECE ≤ fileSize) (hc : 0 < conns) :
    conns ≤ total fileSize conns := by
  unfold total
  have hps_pos : 0 < pieceSize fileSize conns := pieceSize_pos fileSize conns
  have hdiv : MIN_PIECE ≤ fileSize / conns := by
    rw [Nat.le_div_iff_mul_le hc, Nat.mul_comm]
    exact hmin
  have hmaxc : max conns 1 = conns := Nat.max_eq_left (Nat.succ_le_of_lt hc)
  have hps_le : pieceSize fileSize conns ≤ fileSize / conns := by
    unfold pieceSize
    simp only [Nat.not_lt.mpr hbig, ↓reduceIte, hmaxc]
    unfold floorToPage
    have hle1 : min MAX_PIECE (max MIN_PIECE (fileSize / conns)) ≤ fileSize / conns := by
      rcases Nat.le_total (fileSize / conns) MIN_PIECE with h | h
      · have hx : fileSize / conns = MIN_PIECE := Nat.le_antisymm h hdiv
        rw [hx]
        exact Nat.min_le_right _ _
      · have hmax : max MIN_PIECE (fileSize / conns) = fileSize / conns := Nat.max_eq_right h
        rw [hmax]
        exact Nat.min_le_right _ _
    calc min MAX_PIECE (max MIN_PIECE (fileSize / conns)) / PAGE * PAGE
        ≤ min MAX_PIECE (max MIN_PIECE (fileSize / conns)) := Nat.div_mul_le_self _ _
      _ ≤ fileSize / conns := hle1
  have hmul : pieceSize fileSize conns * conns ≤ fileSize := by
    rw [Nat.le_div_iff_mul_le hc] at hps_le
    exact hps_le
  have hc_le : conns ≤ fileSize / pieceSize fileSize conns := by
    rw [Nat.le_div_iff_mul_le hps_pos, Nat.mul_comm]
    exact hmul
  -- ⌈fs/ps⌉ ≥ ⌊fs/ps⌋：加 ps-1 再整除只会更大
  have hmono :
      fileSize / pieceSize fileSize conns ≤
        (fileSize + pieceSize fileSize conns - 1) / pieceSize fileSize conns := by
    apply Nat.div_le_div_right
    rw [Nat.add_sub_assoc hps_pos]
    exact Nat.le_add_right _ _
  exact Nat.le_trans hc_le (Nat.le_trans hmono (Nat.le_max_left _ _))

end Pieces
