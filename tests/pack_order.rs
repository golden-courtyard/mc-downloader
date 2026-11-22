//! 回归锁：`pack` 的**支持性检查必须在任何下载之前**。
//!
//! `src/pack/mod.rs` 与 `src/pack/spec.rs` 各写了一遍这条硬约束（「否则下完几个 GB 才说不支持」），
//! 但此前**没有任何测试执行过 `pack::install`** —— 把 `check_supported()` 挪到 `vanilla::install()`
//! 之后一行，全仓测试照旧全绿。
//!
//! 这条**零网络可测**：`parse()` 只读 zip，Forge 包在 `check_supported()` 就 bail，根本走不到下载。
//! 为了让"没联网"这件事有判据，这里给一个**必然连不上**的代理：如果顺序被改回"先下载"，
//! 报错会变成连接失败，断言 `暂不支持 Forge` 就会失败。

use std::io::Write;

use mc_downloader::download::DownloadOpts;
use mc_downloader::pack;

const INDEX: &str = "modrinth.index.json";

fn make_pack(path: &std::path::Path) {
    let f = std::fs::File::create(path).unwrap();
    let mut z = zip::ZipWriter::new(f);
    z.start_file(INDEX, zip::write::SimpleFileOptions::default())
        .unwrap();
    // 只要 dependencies 里出现 forge 就够了：formatVersion 走必需字段，files 可以空
    let json = r#"{
      "formatVersion": 1,
      "game": "minecraft",
      "versionId": "1.0.0",
      "name": "order-check",
      "files": [],
      "dependencies": { "minecraft": "1.20.1", "forge": "47.4.10" }
    }"#;
    z.write_all(json.as_bytes()).unwrap();
    z.finish().unwrap();
}

#[tokio::test]
async fn forge_pack_fails_before_any_download_or_mkdir() {
    let root = std::env::temp_dir().join(format!("mcd-packorder-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();

    let pack = root.join("forge.mrpack");
    make_pack(&pack);
    let inst = root.join("inst"); // 故意不预先创建：install 若先建目录，这条会被抓到
    let opts = DownloadOpts {
        // 任何网络尝试都会以"连接被拒"失败，从而与"暂不支持 Forge"区分开
        proxy: Some("http://127.0.0.1:9".into()),
        ..DownloadOpts::default()
    };

    // 这个测试不依赖真实的源：Forge 包在支持性检查就 bail，走不到下载
    let src = mc_downloader::settings::Sources {
        metadata: "http://127.0.0.1:9/m".into(),
        assets: "http://127.0.0.1:9/a".into(),
        fabric: "http://127.0.0.1:9/f".into(),
        forge_meta: "http://127.0.0.1:9/fm".into(),
        forge_maven: "http://127.0.0.1:9/fv".into(),
        neoforge: "http://127.0.0.1:9/n".into(),
    };
    let err = pack::install(&pack, &inst, &opts, &src)
        .await
        .expect_err("Forge 包必须被拒绝");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("暂不支持 Forge"),
        "应命中支持性检查（而不是先下载再失败）: {msg}"
    );
    assert!(
        !inst.join("versions").exists(),
        "拒绝之前不许建 versions/（说明已经走过了 vanilla 下载那一步）"
    );
    assert!(
        !inst.join("libraries").exists(),
        "拒绝之前不许建 libraries/"
    );

    let _ = std::fs::remove_dir_all(&root);
}
