#!/usr/bin/env python3
# ============================================================================
# scripts/quant/win_extents.py —— Windows 上的 "filefrag"：数一个文件有多少个 extent
#
# 为什么需要它：E6 要比「稀疏 set_len」vs「先顺序写零」的碎片化程度，而
#   `defrag D: /A /V` 需要管理员权限（实测返回 0x89000024 ERROR_NOT_ENOUGH_PRIVILEGES，
#   本会话不允许提权）。`FSCTL_GET_RETRIEVAL_POINTERS` 只要对文件有读权限就能调，
#   返回的是文件自己的 VCN→LCN 映射 —— 与 Linux `filefrag` 的 extent 数是同一个量，
#   可以直接横向对照。
#
# 输出（一行 JSON）：{"path":…,"bytes":…,"extents":N,"allocated":N,"holes":N,
#                     "clusters":N,"runs":[[vcn_start,lcns,count],…]}
#   extents    实际返回的映射条目数（含空洞条目）—— 报告的"extent 数"用这个
#   allocated  其中 LCN 有值的条目数（真正占盘的 run）
#   holes      LCN = -1 的条目数（NTFS 的 sparse run）
#
# 用法：python win_extents.py <file>    （x64 Windows；只用标准库）
# ============================================================================

import ctypes
import json
import os
import sys
from ctypes import Structure, c_longlong, c_void_p, wintypes

GENERIC_READ = 0x80000000
FILE_SHARE_ALL = 0x1 | 0x2 | 0x4
OPEN_EXISTING = 3
FILE_FLAG_BACKUP_SEMANTICS = 0x02000000
FSCTL_GET_RETRIEVAL_POINTERS = 0x00090073
ERROR_MORE_DATA = 234


class STARTING_VCN_INPUT_BUFFER(Structure):
    _fields_ = [("StartingVcn", c_longlong)]


class RETRIEVAL_POINTERS_BUFFER(Structure):
    _fields_ = [
        ("ExtentCount", wintypes.DWORD),
        ("StartingVcn", c_longlong),
        # Extents[] 紧随其后；用偏移自己解析，免得为变长数组造 65536 个元素
    ]


k32 = ctypes.WinDLL("kernel32", use_last_error=True)
k32.CreateFileW.restype = wintypes.HANDLE
k32.CreateFileW.argtypes = [wintypes.LPCWSTR, wintypes.DWORD, wintypes.DWORD,
                            c_void_p, wintypes.DWORD, wintypes.DWORD, wintypes.HANDLE]
k32.DeviceIoControl.restype = wintypes.BOOL
k32.DeviceIoControl.argtypes = [wintypes.HANDLE, wintypes.DWORD, c_void_p, wintypes.DWORD,
                                c_void_p, wintypes.DWORD, ctypes.POINTER(wintypes.DWORD),
                                c_void_p]
k32.CloseHandle.argtypes = [wintypes.HANDLE]
k32.GetDiskFreeSpaceW.argtypes = [wintypes.LPCWSTR, ctypes.POINTER(wintypes.DWORD),
                                  ctypes.POINTER(wintypes.DWORD), ctypes.POINTER(wintypes.DWORD),
                                  ctypes.POINTER(wintypes.DWORD)]


def extents(path):
    h = k32.CreateFileW(path, GENERIC_READ, FILE_SHARE_ALL, None, OPEN_EXISTING,
                        FILE_FLAG_BACKUP_SEMANTICS, None)
    if h == wintypes.HANDLE(-1).value or h is None:
        raise ctypes.WinError(ctypes.get_last_error())
    try:
        buf = ctypes.create_string_buffer(65536)
        entries = []
        vcn = 0
        while True:
            inp = STARTING_VCN_INPUT_BUFFER(vcn)
            got = wintypes.DWORD(0)
            ok = k32.DeviceIoControl(h, FSCTL_GET_RETRIEVAL_POINTERS,
                                     ctypes.byref(inp), ctypes.sizeof(inp),
                                     buf, ctypes.sizeof(buf),
                                     ctypes.byref(got), None)
            err = ctypes.get_last_error()
            if not ok and err != ERROR_MORE_DATA:
                raise ctypes.WinError(err)
            rp = ctypes.cast(buf, ctypes.POINTER(RETRIEVAL_POINTERS_BUFFER)).contents
            n = rp.ExtentCount
            # 数组起点：DWORD ExtentCount(0..4) + 对齐 4 + LARGE_INTEGER StartingVcn(8..16) ⇒ 16
            base = 16
            raw = buf.raw
            vcn = rp.StartingVcn
            for i in range(n):
                off = base + i * 16
                next_vcn = c_longlong.from_buffer_copy(raw[off:off + 8]).value
                lcn = c_longlong.from_buffer_copy(raw[off + 8:off + 16]).value
                entries.append((vcn, lcn, next_vcn - vcn))
                vcn = next_vcn
            if ok:
                break
        return entries
    finally:
        k32.CloseHandle(h)


def main():
    path = os.path.abspath(sys.argv[1])
    spc = wintypes.DWORD(0)
    bps = wintypes.DWORD(0)
    freec = wintypes.DWORD(0)
    totalc = wintypes.DWORD(0)
    root = os.path.splitdrive(path)[0] + "\\"
    if not k32.GetDiskFreeSpaceW(root, ctypes.byref(spc), ctypes.byref(bps),
                                 ctypes.byref(freec), ctypes.byref(totalc)):
        raise ctypes.WinError(ctypes.get_last_error())
    entries = extents(path)
    holes = sum(1 for (_v, l, _c) in entries if l == -1)
    out = {
        "path": path,
        "bytes": os.path.getsize(path),
        "cluster_bytes": spc.value * bps.value,
        "extents": len(entries),
        "allocated": len(entries) - holes,
        "holes": holes,
        "clusters": sum(c for (_v, l, c) in entries if l != -1),
        "runs": [[v, l, c] for (v, l, c) in entries][:64],
    }
    print(json.dumps(out, ensure_ascii=False))


if __name__ == "__main__":
    main()
