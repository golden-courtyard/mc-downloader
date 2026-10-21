//! Linux 文件系统语义集成测试（spec §5.3 / design §7 的五条判据）。
//!
//! **为什么整文件 `#![cfg(target_os = "linux")]`**
//! 这五条判据的主体都是「Linux 的文件系统 / 进程语义」。用 crate 级 `#![cfg]`
//! 而不是给每个测试、每个 `use`、每个 helper 单独挂 `#[cfg]`：
//! 非 Linux 上整个 target 编译成**空 crate**，`cargo test --test linux_semantics`
//! 就是 `running 0 tests`（不是失败、也不是 skip），而且不会为「没用到的 import」
//! 报 `unused_imports` —— 仓里 `clippy --all-targets -D warnings` 会拦这一类。
//!
//! **刻意不打桩**（design §6）：这里唯一被测的东西就是「真实文件系统怎么做」。
//! 换成内存 fs 或 mock `std::fs` 等于把被测物本身替换掉，测出来的结论没有意义。
//!
//! **隔离与清理**：每个测试自建 `temp_dir()/mcd-linux-sem-<pid>-<tag>`，
//! 由 [`TempDir`] 的 `Drop` 递归删除 —— 断言失败 panic 时同样会清，不留垃圾。

#![cfg(target_os = "linux")]

use std::fs::{self, File};
use std::io::Read;
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

// ─────────────────────────── 公共脚手架 ───────────────────────────

/// 测试自建的临时目录，`Drop` 时递归删除。
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!("mcd-linux-sem-{}-{tag}", std::process::id()));
        // 同一 pid 的旧残留（上次被信号杀掉、Drop 没跑到）先清掉，保证起点干净
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("创建临时目录失败");
        println!("[tmp] {}", path.display());
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn join(&self, name: &str) -> PathBuf {
        self.path.join(name)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        // 清理失败不算测试失败（权限/占用的偶发问题不该掩盖判据本身），但必须出声
        if let Err(e) = fs::remove_dir_all(&self.path) {
            eprintln!("[!] 清理临时目录失败 {}: {e}", self.path.display());
        }
    }
}

/// 把一个被测命令的完整现场拼进断言消息：只看退出码是查不出原因的。
fn describe(what: &str, out: &Output) -> String {
    format!(
        "{what} 失败：exit={:?}\n--- stdout ---\n{}\n--- stderr ---\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    )
}

/// 判据 5 的核心断言：进程**不因 fd 耗尽**失败。
///
/// 只查字符串不是因为够用，而是因为这是「失败时能不能看出病因」的唯一外部证据：
/// `EMFILE` 经 anyhow 包装后最后只剩一句 `Too many open files (os error 24)`。
fn assert_no_fd_exhaustion(what: &str, out: &Output) {
    for (stream, text) in [
        ("stdout", String::from_utf8_lossy(&out.stdout)),
        ("stderr", String::from_utf8_lossy(&out.stderr)),
    ] {
        for needle in ["Too many open files", "EMFILE", "os error 24"] {
            assert!(
                !text.contains(needle),
                "{what}: {stream} 里出现 fd 耗尽的痕迹 `{needle}` —— 并发下句柄用光了\n{}",
                describe(what, out),
            );
        }
    }
}

// ─────────────────────── 本地假源（判据 5 用） ───────────────────────

/// stdlib `http.server` 起的本地源（见下方 [`SERVER_SNIPPET`]）。
///
/// **只用标准库，不依赖仓里其它脚本**（`scripts/net_faults/fakeorigin.py` 是另一批
/// 任务的产物，测试不该被它的存在与否绑架）。代价是 `http.server` **不支持 Range**，
/// 所以判据 5 的载荷刻意小于 8 MB 分片阈值，让它走单流下载路径。
///
/// **一处必须自己动手改的地方**：不用 `-m http.server` 的 CLI，而是 `-c` 内联起来，
/// 只为把 listen backlog 调大。`socketserver.TCPServer.request_queue_size` 的 stdlib
/// 默认值是 **5**，64 条连接同时到达时 accept 队列会溢出、内核丢 SYN、客户端按 RTO
/// 退避 —— 团队实测耗时差 **7–8 倍**（`d6dffbe`，同一处问题在假源里已修）。
/// 判据 5 要压的正是「64 条同时在飞」，源头被排队截断就等于没测。
struct HttpServer {
    child: Child,
}

/// 内联静态源：**唯一非默认设置是 backlog**，其余与 `-m http.server` 等价。
const SERVER_SNIPPET: &str = r#"
import functools, http.server, socketserver, sys
port, directory = int(sys.argv[1]), sys.argv[2]
socketserver.TCPServer.request_queue_size = 128
http.server.ThreadingHTTPServer.request_queue_size = 128
http.server.test(
    HandlerClass=functools.partial(http.server.SimpleHTTPRequestHandler, directory=directory),
    ServerClass=http.server.ThreadingHTTPServer,
    port=port, bind="127.0.0.1",
)
"#;

impl HttpServer {
    fn start(root: &Path, port: u16, log: &Path) -> Self {
        let out = File::create(log).expect("创建假源日志文件失败");
        let err = out.try_clone().expect("复制假源日志句柄失败");
        let child = Command::new("python3")
            .arg("-c")
            .arg(SERVER_SNIPPET)
            .arg(port.to_string())
            .arg(root)
            .stdin(Stdio::null())
            .stdout(Stdio::from(out))
            .stderr(Stdio::from(err))
            .spawn()
            .unwrap_or_else(|e| panic!("起本地假源失败（Linux CI 必须有 python3）: {e}"));
        Self { child }
    }

    /// 等端口真的可连。**不用 `sleep 1` 猜**：猜短了 flaky、猜长了拖慢整轮测试。
    fn wait_ready(&mut self, port: u16, log: &Path) {
        let addr = (Ipv4Addr::LOCALHOST, port).into();
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if let Ok(Some(status)) = self.child.try_wait() {
                panic!(
                    "假源进程起来了却立刻退出（{status}），日志:\n{}",
                    fs::read_to_string(log).unwrap_or_default()
                );
            }
            if TcpStream::connect_timeout(&addr, Duration::from_millis(200)).is_ok() {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!(
            "假源 10s 内没在 127.0.0.1:{port} 上起来，日志:\n{}",
            fs::read_to_string(log).unwrap_or_default()
        );
    }
}

impl Drop for HttpServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// 让内核挑一个空闲端口再放掉。理论上有 TOCTOU 窗口，实际概率可忽略；
/// 真撞上时 [`HttpServer::wait_ready`] 会带着 python 的日志响亮失败。
fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("挑空闲端口失败");
    let port = listener.local_addr().expect("读本地地址失败").port();
    drop(listener);
    port
}

// ─────────────────────────── 五条判据 ───────────────────────────

/// 判据 1：路径**大小写敏感** —— `A` 与 `a` 是两个文件，不能被合并。
///
/// 为什么只在 Linux 上测：本机 Windows/NTFS 实测（2026-10-21，探针落在系统临时目录）——
/// 建 `A` `a` 后目录里只有 **1 个**条目，两个路径读到同一份内容 `"lower-a"`。
/// 大小写不敏感的文件系统上，这条判据的**前提**（两个独立文件）根本不成立。
#[test]
fn case_sensitive_paths() {
    let dir = TempDir::new("case");
    let upper = dir.join("A");
    let lower = dir.join("a");

    fs::write(&upper, b"upper-A").expect("写 A 失败（大小写敏感的 fs 上不该冲突）");
    fs::write(&lower, b"lower-a").expect("写 a 失败（若被合并到 A，这里就覆盖了 A 的内容）");

    // 「不得合并」最直接的证据：目录里是 2 个条目，不是 1 个
    let entries = fs::read_dir(dir.path()).expect("读目录失败").count();
    assert_eq!(
        entries, 2,
        "目录里只有 {entries} 个条目 —— 文件系统把 A 和 a 合并了，不是大小写敏感语义"
    );

    assert!(upper.exists(), "A 应当存在");
    assert!(lower.exists(), "a 应当存在");
    assert_eq!(
        fs::read(&upper).expect("读 A 失败"),
        b"upper-A",
        "A 的内容被 a 覆盖了"
    );
    assert_eq!(
        fs::read(&lower).expect("读 a 失败"),
        b"lower-a",
        "a 的内容被 A 覆盖了"
    );
}

/// 判据 2：`set_len` 造出**稀疏文件** —— 逻辑长度 1 GiB，实际块占用远小于此。
///
/// 为什么只在 Linux 上测：判据靠 POSIX 的 `st_blocks`（`MetadataExt::blocks()`，
/// unix-only）。Windows 侧没有同一个探针（要 GetFileInformationByHandle 看
/// 稀疏/压缩标志），没法用同一条判据判定。
///
/// **SKIP 语义（CL-03）**：文件系统不支持稀疏时打印 `SKIP:` 并正常返回 ——
/// 但 `cargo test` 没有 skip 状态，结果栏照样显示 passed。所以 CI 设
/// `REQUIRE_SPARSE=1`：**此时 SKIP 直接判失败**，不许把「没测到」记成「通过」。
#[test]
fn sparse_set_len() {
    /// 1 GiB：足够大到「有没有真分配」一目了然，又不用真写盘。
    const LEN: u64 = 1 << 30;
    /// 「远小于」的量化：块占用 < 逻辑长度的 1/8。
    /// ext4 上纯 `set_len` 不写数据时 `st_blocks` 应为 0；留 8 倍余量是因为
    /// 某些 fs 会预分配元数据/少量区段，而这里要抓的是「整份真分配」那种失败。
    const BUDGET: u64 = LEN / 8;

    let dir = TempDir::new("sparse");
    let path = dir.join("hole.bin");

    let file = File::create(&path).expect("创建稀疏测试文件失败");
    file.set_len(LEN).expect("set_len(1GiB) 失败");
    let meta = file.metadata().expect("读 metadata 失败");

    assert_eq!(meta.len(), LEN, "逻辑长度应当就是 set_len 给的值");

    // POSIX 规定 st_blocks 的单位是 512 字节（不是 st_blksize）
    let allocated = meta.blocks() * 512;

    if allocated >= BUDGET {
        if std::env::var("REQUIRE_SPARSE").as_deref() == Ok("1") {
            panic!(
                "SKIP 即失败（REQUIRE_SPARSE=1）：当前文件系统不支持稀疏文件，\
                 set_len({LEN}) 实占了 {allocated} 字节"
            );
        }
        println!(
            "SKIP: 当前文件系统不支持稀疏文件 —— set_len({LEN}) 实占 {allocated} 字节，\
             无法验证以下判据；设置 REQUIRE_SPARSE=1 可让这种情形直接失败"
        );
        return;
    }

    println!("[sparse] len={LEN} 实占={allocated} 字节");
}

/// 判据 3：`rename` **覆盖**已存在的目标（POSIX 语义），且内容换成源文件那一份。
///
/// 为什么只在 Linux 上测：这条判据描述的是 POSIX `rename(2)` 的语义。
/// 需要如实说明的是 —— 本机 Windows/NTFS 实测（2026-10-21）`std::fs::rename`
/// 覆盖已存在文件**同样成功**（Rust std 已抹平了这一层差异）。所以 `cfg(linux)`
/// 在这里的作用是「**守住 Linux 侧的语义判据**」（CI 平台就是 Linux），
/// 而不是「Windows 上这条会失败」。
#[test]
fn rename_overwrites() {
    let dir = TempDir::new("rename");
    let a = dir.join("A");
    let b = dir.join("B");

    fs::write(&a, b"AAA").expect("写 A 失败");
    fs::write(&b, b"BBBB").expect("写 B 失败");

    fs::rename(&b, &a).expect("B 存在时 rename(B, A) 失败 —— POSIX 语义下应当覆盖成功");

    assert_eq!(
        fs::read(&a).expect("读 A 失败"),
        b"BBBB",
        "A 的内容应当换成 B 的"
    );
    assert!(!b.exists(), "rename 之后源路径 B 不应再存在");
    assert_eq!(
        fs::read_dir(dir.path()).expect("读目录失败").count(),
        1,
        "覆盖后目录里应当只剩 A 一个条目"
    );
}

/// 判据 4：**打开中的文件可以 unlink**，且删除后原句柄仍能读到完整数据。
///
/// 这是「inode 引用计数」语义：名字从目录里消失，但数据活到最后一个句柄关闭为止。
/// 它正是断点续传敢在文件被打开时做清理的前提。
///
/// 为什么只在 Linux 上测：同判据 3，本机 Windows/NTFS 实测（2026-10-21）
/// `remove_file` 在句柄打开时也成功、之后原句柄也读得出原内容 —— Rust std 已经
/// 抹平了差异。`cfg(linux)` 是为了**守住 Linux 侧判据**（CI 平台），
/// 不是「Windows 上会失败」。
#[test]
fn unlink_open_file() {
    const PAYLOAD: &[u8] = b"payload-survives-unlink";

    let dir = TempDir::new("unlink");
    let path = dir.join("open_then_unlink.bin");

    fs::write(&path, PAYLOAD).expect("写测试文件失败");
    let mut handle = File::open(&path).expect("打开测试文件失败");

    fs::remove_file(&path)
        .expect("打开中的文件 remove_file 失败 —— Linux 允许，说明没有句柄泄漏问题");

    // 名字立刻消失
    assert!(!path.exists(), "unlink 之后路径不应再 exists");
    assert_eq!(
        fs::read_dir(dir.path()).expect("读目录失败").count(),
        0,
        "unlink 之后目录里不该还留着条目"
    );

    // 但数据要活着 —— 句柄不能因为 unlink 而失效
    let mut got = Vec::new();
    handle
        .read_to_end(&mut got)
        .expect("unlink 之后原句柄读失败 —— 句柄被 unlink 弄坏了");
    assert_eq!(
        got, PAYLOAD,
        "unlink 之后原句柄读到的内容不对（数据被提前释放）"
    );
    assert_eq!(
        handle.metadata().expect("读句柄 metadata 失败").len(),
        PAYLOAD.len() as u64,
        "unlink 之后句柄上的元数据不应被销毁"
    );
}

/// 判据 5：`--concurrency 64` 下**真实下载**不因 `EMFILE` 失败。
///
/// 为什么只在 Linux 上测：`EMFILE` / `ulimit` 这套是 POSIX 的 fd 预算模型；
/// 而且假源用的是 stdlib `http.server`，Windows 上跑测试还得额外依赖 python3。
///
/// ## 路线选择（为什么不照抄 spec 里那一行 `fetch` 命令，也不走 `ulimit` 路线）
///
/// 1. **`fetch` 路径下 `--concurrency 64` 是空转。** `fetch` 只构造**一个** job，
///    `download::run` 里是 `buffer_unordered(opts.concurrency)` 作用在 1 个 job 上
///    ⇒ 并发度恒为 1。用它断言「不因 EMFILE 失败」必然永远绿，测不到任何东西。
///    所以拆成两段：**阶段 A** 跑 spec 原文那条命令（当 CLI 形态的 smoke），
///    **阶段 B** 用 `run --parallel` + 64 个 task，**真的 64 个文件同时下** ——
///    触发条件（并发 64）与判据（不因 fd 耗尽失败）才对得上因果关系。
/// 2. **`ulimit -n 256` 路线被否。** 压低上限测的是「环境分给多少 fd」，
///    不是「工具在并发下会不会耗尽并优雅处理」；而且仓里 `unsafe_code = "forbid"`，
///    测试内没法 `setrlimit`，只能套 `sh -c 'ulimit -n N; exec …'`，
///    等于把被测进程塞进 shell 里、退出码语义多绕一层。
///    触发条件应当是「并发够高」，不是「上限够低」。
#[test]
fn fd_under_concurrency() {
    const N: usize = 64;
    /// 1 MiB < 8 MB 分片阈值 ⇒ 走单流下载，**不需要 Range**（`http.server` 不支持 Range）
    const SIZE: usize = 1 << 20;

    let tmp = TempDir::new("fd");
    let www = tmp.join("www");
    fs::create_dir_all(&www).expect("创建假源目录失败");

    let payload: Vec<u8> = (0..SIZE).map(|i| (i % 251) as u8).collect();
    fs::write(www.join("blob.bin"), &payload).expect("写假源载荷失败");

    let port = free_port();
    let log = tmp.join("python.log");
    let mut server = HttpServer::start(&www, port, &log);
    server.wait_ready(port, &log);
    let url = format!("http://127.0.0.1:{port}/blob.bin");

    // 被测二进制是 cargo 编出来的那个（CARGO_BIN_EXE_* 由 cargo 注入，不靠 PATH）
    let bin = env!("CARGO_BIN_EXE_mc-downloader");

    // ── 阶段 A：spec 原文的形态（单文件 fetch，64 并发）──
    let out_a = Command::new(bin)
        .args(["--concurrency", "64", "fetch"])
        .arg(&url)
        .arg("--dir")
        .arg(tmp.join("one"))
        .args(["--name", "t.bin"])
        .output()
        .expect("执行被测二进制失败");
    assert!(
        out_a.status.success(),
        "{}",
        describe("阶段 A（fetch --concurrency 64）", &out_a)
    );
    assert_no_fd_exhaustion("阶段 A（fetch --concurrency 64）", &out_a);

    let single = fs::read(tmp.join("one/t.bin")).expect("阶段 A 退出 0 但没有产物");
    assert_eq!(single.len(), SIZE, "阶段 A 落盘大小不对");
    assert!(single == payload, "阶段 A 落盘内容与源不一致");

    // ── 阶段 B：真的 64 个文件同时下（这才是「并发 64」的因果触发点）──
    let many = tmp.join("many");
    let mut script = String::from("# 64 个 generic task，一次 run --parallel\n");
    for i in 0..N {
        script.push_str(&format!(
            "[[task]]\nkind = \"generic\"\nurl = \"{url}\"\ndir = \"{}\"\nname = \"p{i:02}.bin\"\nsize = {SIZE}\n\n",
            many.display()
        ));
    }
    let script_path = tmp.join("dl.toml");
    fs::write(&script_path, script).expect("写下载脚本失败");

    let out_b = Command::new(bin)
        .args(["--concurrency", "64", "run"])
        .arg(&script_path)
        .arg("--parallel")
        .output()
        .expect("执行被测二进制失败");
    assert!(
        out_b.status.success(),
        "{}",
        describe("阶段 B（run --parallel，64 任务）", &out_b)
    );
    assert_no_fd_exhaustion("阶段 B（run --parallel，64 任务）", &out_b);

    // 退出码 0 不等于下对了：逐个打开产物看（§四.13 判据落在产物上）
    for i in 0..N {
        let p = tmp.join(&format!("many/p{i:02}.bin"));
        let data = fs::read(&p).unwrap_or_else(|e| {
            panic!(
                "第 {i} 个产物缺失 {}: {e}\n{}",
                p.display(),
                describe("阶段 B（run --parallel，64 任务）", &out_b)
            )
        });
        assert_eq!(data.len(), SIZE, "第 {i} 个产物大小不对: {}", p.display());
        assert!(
            data == payload,
            "第 {i} 个产物内容与源不一致: {}",
            p.display()
        );
    }
}
