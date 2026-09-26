//! 构建脚本：把程序图标与清单嵌入可执行文件。
//!
//! 直接调用 Windows SDK 自带的 `rc.exe` 编译 `assets/app.rc`，不引入 winres / embed-resource
//! 之类的额外依赖，然后让链接器把生成的 `.res` 一起链进去。
//! 若本机没有 `rc.exe`（未安装 Windows SDK），则跳过嵌入并给出提示——程序依然可以正常构建，
//! 只是没有自定义图标，托盘图标会退回系统默认图标。

use std::{
    env,
    path::{Path, PathBuf},
    process::Command,
};

fn main() {
    println!("cargo:rerun-if-changed=assets/app.rc");
    println!("cargo:rerun-if-changed=assets/app.ico");
    println!("cargo:rerun-if-changed=assets/app.manifest");
    println!("cargo:rerun-if-env-changed=RC");

    // 只对 Windows 目标生效
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let Some(rc) = find_resource_compiler() else {
        println!("cargo:warning=未找到 rc.exe（Windows SDK），跳过图标与清单嵌入");
        return;
    };

    let manifest_dir =
        PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("缺少 CARGO_MANIFEST_DIR"));
    let assets = manifest_dir.join("assets");
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("缺少 OUT_DIR"));
    let resource = out_dir.join("app.res");

    // rc.exe 的输出路径与引用文件都相对于工作目录解析，因此显式指定工作目录为 assets
    let status = Command::new(&rc)
        .current_dir(&assets)
        .arg("/nologo")
        .arg("/fo")
        .arg(&resource)
        .arg("app.rc")
        .status();

    match status {
        Ok(status) if status.success() => {
            // 链接器可以直接接受 .res 文件作为输入
            println!("cargo:rustc-link-arg-bins={}", resource.display());
        }
        Ok(status) => {
            println!("cargo:warning=rc.exe 编译资源失败（退出码 {status}），跳过图标与清单嵌入")
        }
        Err(error) => println!("cargo:warning=无法运行 rc.exe：{error}，跳过图标与清单嵌入"),
    }
}

/// 查找 `rc.exe`：优先使用 `RC` 环境变量，其次在 Windows SDK 的安装目录里取版本号最高的一份。
fn find_resource_compiler() -> Option<PathBuf> {
    if let Some(path) = env::var_os("RC") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Some(path);
        }
    }

    // 同时覆盖 64 位与 32 位宿主：进程是 64 位的，优先 x64。
    // 环境变量缺失时回退到标准安装路径——有些精简环境（如内置 Administrator 会话）
    // 会把 ProgramFiles(x86) 清空，但 SDK 文件其实就在默认位置。
    let mut roots = Vec::new();
    for variable in ["ProgramFiles(x86)", "ProgramFiles"] {
        if let Some(root) = env::var_os(variable) {
            if !root.is_empty() {
                roots.push(
                    PathBuf::from(root)
                        .join("Windows Kits")
                        .join("10")
                        .join("bin"),
                );
            }
        }
    }
    if roots.is_empty() {
        roots.push(PathBuf::from(r"C:\Program Files (x86)\Windows Kits\10\bin"));
        roots.push(PathBuf::from(r"C:\Program Files\Windows Kits\10\bin"));
    }

    let mut candidates = Vec::new();
    for root in roots {
        let Ok(versions) = std::fs::read_dir(&root) else {
            continue;
        };
        for version in versions.flatten() {
            for host in ["x64", "x86"] {
                let candidate = version.path().join(host).join("rc.exe");
                if candidate.is_file() {
                    candidates.push(candidate);
                }
            }
        }
    }

    // 目录名是形如 10.0.26100.0 的版本号，按数字段排序后取最新
    candidates.sort_by_key(|path| version_key(path));
    candidates.pop()
}

/// 从 `.../10.0.26100.0/x64/rc.exe` 中取出 `[10, 0, 26100, 0]` 用于比较。
fn version_key(path: &Path) -> Vec<u32> {
    path.ancestors()
        .nth(2)
        .and_then(|dir| dir.file_name())
        .map(|name| {
            name.to_string_lossy()
                .split('.')
                .map(|part| part.parse().unwrap_or(0))
                .collect()
        })
        .unwrap_or_default()
}
