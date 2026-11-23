# 排查

## 「能读不能写，但 icacls 显示 (F)」

**Windows 的完整性级别（IL）是叠加在 ACL 之上的第二层强制机制。** Low IL 进程对 Medium
对象的写请求一律拒绝，**不管 ACL 是否给了完全控制**。`%TEMP%`、`%APPDATA%` 默认都是 Medium。

对下载器来说这意味着：`-d` 指向的目录若是 Low IL，落盘会失败，而错误看起来像"磁盘写不进去"——
磁盘明明可写、`icacls` 又显示 `(F)`。

### 判据

**三条同时成立，就该查 IL，别再查 ACL：**

1. 程序**能读不能写**（能列目录、能读已有文件，写新文件报错）
2. `icacls <目录>` 显示账户有 `(F)` 完全控制
3. 报错是 `Access denied` / `WinError 5` / 某个库自己的 IO 错误码

### 怎么查

```powershell
icacls <目录> | findstr Mandatory      # 目录是不是 Low
whoami /groups | findstr Label         # 进程是不是 Low
```

mc-downloader 落盘失败时，错误链里通常会带 `os error 5`（拒绝访问）或 `os error 32`（占用）；
配合上面两条命令，能分清是 ACL、IL、还是文件被别的程序占着。

### 怎么修

**需要管理员**，因为 `setintegritylevel` 要求 `WRITE_OWNER`：

```powershell
icacls <目录> /setintegritylevel Medium /T /C
```

**`/T` 必须加** —— IL 是静态继承（创建时复制），改父目录不会自动改已有子对象。

### 一个实测案例

某台机器上 `D:\...\tools` 被设成 Low IL ⇒ 从那里启动的进程也在 Low ⇒ 往 `%TEMP%` 写被拒 ⇒
上层的办公软件报 `0x507 (Io Class: Access Code: 7)`。

**报错完全不指向 IL**：它看起来像"磁盘写不进去"，而磁盘明明可写；`icacls` 又显示 `(F)`。
排查绕了五个假设才到 IL。

### 与我们自己踩过的坑区分

| 现象 | 真正原因 | 看哪 |
|---|---|---|
| `os error 5` + `icacls` 显示 `(F)` | **完整性级别**（本文） | `icacls … \| findstr Mandatory` |
| `os error 5` + `icacls` 显示无写权限 | ACL | `icacls <目录>` |
| `os error 32` | 文件被别的进程占用 | 关掉占用者 |
| 写到一半失败、`.part` 残留 | 磁盘满 / 配额 | 先看剩余空间，再看 `--max-speed` 是否把超时压爆 |
