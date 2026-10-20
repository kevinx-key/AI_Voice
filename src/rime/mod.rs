//! Rime（倾听输入法）交付层：检测 / 静默安装 / 铺方案包 / 写配置（备份+合并）/ 部署 / 卸载
//!
//! 设计红线（见实施计划 B3.2）：
//! 1. 不把 Rime 塞进本进程——只做交付层/界面层/配置层
//! 2. 方案包只铺到用户目录的独立子目录，卸载才干净
//! 3. `*.custom.yaml` 只 patch 我们管理的键：标记块 + 合并写 + 写入前 .bak 备份
//! 4. 用户自己的 `*.custom.yaml` / `*.userdb\` / `build\` 一律不动

pub mod schemes;

use anyhow::{bail, Context, Result};
use schemes::{Lang, Plan};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// 我们管理的标记块（卸载/重写时按标记摘除）
pub const MARK_BEGIN: &str = "# >>> listen managed >>>";
pub const MARK_END: &str = "# <<< listen managed <<<";

/// 旧名（AI Voice 时期）写进用户配置的标记：清理时一并认，别把旧块留在用户配置里
pub const LEGACY_MARK_BEGIN: &str = "# >>> ai-voice managed >>>";
pub const LEGACY_MARK_END: &str = "# <<< ai-voice managed <<<";

/// 我们铺到用户目录的方案包目录（决策：三套全装，只启用主力语言）
pub const ALL_SCHEME_DIRS: [&str; 3] = ["rime-ice", "easy_en", "jaroomaji"];

/// 铺文件时跳过的目录（纯文档/构建工具/备用方案，不参与输入）
pub const SKIP_DIRS: [&str; 4] = [".github", "img4md", "dict_tools", "others"];

/// 铺文件时跳过的根级文件（plum 官方 recipe 也没装这些）
pub const SKIP_FILES: [&str; 8] = [
    "README.md",
    "LICENSE",
    "AGENTS.md",
    "CHANGELOG.md",
    ".gitignore",
    ".gitmodules",
    "recipe.yaml",
    "customize.recipe.yaml",
];

/// 我们铺的文件清单（卸载时按清单删，绝不碰用户文件）
pub const MANIFEST: &str = ".listen-files.txt";

/// 每种语言只保留一种最佳输入法：同语言的其它方案**不铺**，避免安装后出现一堆可选方案。
/// （中文=雾凇全拼 rime_ice；英文=melt_eng/easy_en；日文=jaroomaji；双拼/九键等一律不装）
pub const SKIP_SCHEMA_FILES: [&str; 10] = [
    "t9.schema.yaml",
    "radical_pinyin.schema.yaml",
    "double_pinyin.schema.yaml",
    "double_pinyin_abc.schema.yaml",
    "double_pinyin_mspy.schema.yaml",
    "double_pinyin_sogou.schema.yaml",
    "double_pinyin_flypy.schema.yaml",
    "double_pinyin_ziguang.schema.yaml",
    "double_pinyin_jiajia.schema.yaml",
    // 方案包自带的 default.yaml（含 9 方案 schema_list）；不铺，改用我们写的 default.custom.yaml
    "default.yaml",
];

/// 该相对路径是否属于"同语言多余方案"，铺文件时跳过
fn skip_scheme_file(rel: &Path) -> bool {
    rel.file_name()
        .and_then(|s| s.to_str())
        .map(|n| SKIP_SCHEMA_FILES.contains(&n))
        .unwrap_or(false)
}

/// 旧名（AI Voice 时期）的清单文件名：读不到新清单时回退读它，卸载时一并清掉
pub const LEGACY_MANIFEST: &str = ".ai-voice-files.txt";

/// 我们管理的方案包附带的方案 id（卸载时要摘掉对应的 .custom.yaml / build 产物）
pub const ALL_SCHEMA_IDS: [&str; 3] = ["rime_ice", "easy_en", "jaroomaji"];

/// 我们铺的方案包里的全部方案 id（含 rime-ice 家族），卸载时清 build 产物用
pub const OUR_SCHEMA_IDS: [&str; 13] = [
    "rime_ice",
    "melt_eng",
    "radical_pinyin",
    "t9",
    "double_pinyin",
    "double_pinyin_abc",
    "double_pinyin_mspy",
    "double_pinyin_sogou",
    "double_pinyin_flypy",
    "double_pinyin_ziguang",
    "double_pinyin_jiajia",
    "easy_en",
    "jaroomaji",
];

pub const GRAMMAR_FILE: &str = "wanxiang-lts-zh-hans.gram";

/// 打字侧热词表（Rime custom_phrase 纯文本表）：由 `src/hotwords.rs` 从热词包生成
pub const TERMS_FILE: &str = "listen_hotwords.txt";

/// 用户词学习钩子（lua 脚本）在用户目录里的相对路径
pub const LEARN_LUA: &str = crate::learn::LUA_REL;

#[derive(Debug, Clone, Default)]
pub struct WeaselInfo {
    pub installed: bool,
    pub version: String,
    pub install_dir: Option<PathBuf>,
    pub deployer: Option<PathBuf>,
    pub user_dir: PathBuf,
    /// 用户目录是否来自配置/注册表（false = 默认 %APPDATA%\Rime）
    pub user_dir_custom: bool,
}

// ---------------------------------------------------------------- 检测

fn env_dir(key: &str) -> Option<PathBuf> {
    std::env::var_os(key).map(PathBuf::from)
}

#[cfg(windows)]
fn reg_read_string(root: windows::Win32::System::Registry::HKEY, sub: &str, value: &str) -> Option<String> {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::ERROR_SUCCESS;
    use windows::Win32::System::Registry::*;
    let sub_w: Vec<u16> = sub.encode_utf16().chain(Some(0)).collect();
    let val_w: Vec<u16> = value.encode_utf16().chain(Some(0)).collect();
    let mut hkey = HKEY::default();
    let rc = unsafe {
        RegOpenKeyExW(
            root,
            PCWSTR(sub_w.as_ptr()),
            Some(0),
            KEY_READ,
            &mut hkey,
        )
    };
    if rc != ERROR_SUCCESS {
        return None;
    }
    let mut ty = REG_VALUE_TYPE(0);
    let mut buf = vec![0u8; 4096];
    let mut len = buf.len() as u32;
    let rc = unsafe {
        RegQueryValueExW(
            hkey,
            PCWSTR(val_w.as_ptr()),
            None,
            Some(&mut ty),
            Some(buf.as_mut_ptr()),
            Some(&mut len),
        )
    };
    unsafe {
        let _ = RegCloseKey(hkey);
    }
    if rc != ERROR_SUCCESS || len == 0 {
        return None;
    }
    let n = (len as usize / 2).saturating_sub(1);
    let wide = unsafe { std::slice::from_raw_parts(buf.as_ptr() as *const u16, n) };
    let s = String::from_utf16_lossy(wide).trim().to_string();
    if s.is_empty() { None } else { Some(s) }
}

#[cfg(not(windows))]
fn reg_read_string(_r: (), _s: &str, _v: &str) -> Option<String> {
    None
}

/// 从卸载字符串反推安装目录：`"C:\...\uninstall.exe" /S` → `C:\...`
fn install_dir_from_uninstall(s: &str) -> Option<String> {
    let s = s.trim();
    let path = if let Some(rest) = s.strip_prefix('"') {
        rest.split('"').next().unwrap_or(rest)
    } else {
        s.split_whitespace().next().unwrap_or(s)
    };
    let dir = Path::new(path).parent()?;
    if dir.as_os_str().is_empty() {
        None
    } else {
        Some(dir.to_string_lossy().into_owned())
    }
}

/// 倾听输入法的卸载信息（版本 + 安装目录）
#[cfg(windows)]
fn weasel_registry() -> Option<(String, PathBuf)> {
    use windows::Win32::System::Registry::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
    const SUBS: [&str; 4] = [
        r"SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall\Weasel",
        r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\Weasel",
        r"SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall\weasel",
        r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\weasel",
    ];
    for root in [HKEY_LOCAL_MACHINE, HKEY_CURRENT_USER] {
        for sub in SUBS {
            let ver = reg_read_string(root, sub, "DisplayVersion").unwrap_or_default();
            let mut loc = reg_read_string(root, sub, "InstallLocation").unwrap_or_default();
            // 多数安装器把 InstallLocation 留空；从 UninstallString 反推安装目录
            // （"<dir>\uninstall.exe" → <dir>）
            if loc.trim().is_empty() {
                if let Some(us) = reg_read_string(root, sub, "UninstallString") {
                    loc = install_dir_from_uninstall(&us).unwrap_or_default();
                }
            }
            if !ver.is_empty() || !loc.is_empty() {
                return Some((ver, PathBuf::from(loc)));
            }
        }
    }
    None
}

#[cfg(not(windows))]
fn weasel_registry() -> Option<(String, PathBuf)> {
    None
}

/// 在候选目录里找 WeaselDeployer.exe（含一层子目录，兼容 weasel-<ver>\ 布局）
fn find_deployer(root: &Path) -> Option<PathBuf> {
    let direct = root.join("WeaselDeployer.exe");
    if direct.is_file() {
        return Some(direct);
    }
    let subs = std::fs::read_dir(root).ok()?;
    for e in subs.flatten() {
        let p = e.path();
        if p.is_dir() {
            let c = p.join("WeaselDeployer.exe");
            if c.is_file() {
                return Some(c);
            }
        }
    }
    None
}

/// 检测倾听输入法：注册表 + 常见安装目录 + 用户目录
pub fn detect(cfg_user_dir: &str) -> WeaselInfo {
    let mut info = WeaselInfo::default();

    // 用户目录：配置 > 注册表 > 默认
    if !cfg_user_dir.trim().is_empty() {
        info.user_dir = PathBuf::from(cfg_user_dir.trim());
        info.user_dir_custom = true;
    } else {
        #[cfg(windows)]
        {
            use windows::Win32::System::Registry::HKEY_CURRENT_USER;
            if let Some(d) = reg_read_string(HKEY_CURRENT_USER, r"Software\Rime\Weasel", "RimeUserDir") {
                if !d.trim().is_empty() {
                    info.user_dir = PathBuf::from(d.trim());
                    info.user_dir_custom = true;
                }
            }
        }
        if info.user_dir.as_os_str().is_empty() {
            info.user_dir = env_dir("APPDATA")
                .map(|d| d.join("Rime"))
                .unwrap_or_else(|| PathBuf::from(r"Rime"));
        }
    }

    // 安装信息
    if let Some((ver, loc)) = weasel_registry() {
        info.version = ver;
        if !loc.as_os_str().is_empty() {
            info.install_dir = Some(loc.clone());
            info.deployer = find_deployer(&loc);
        }
    }
    if info.deployer.is_none() {
        let mut cands: Vec<PathBuf> = Vec::new();
        for key in ["ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA"] {
            if let Some(base) = env_dir(key) {
                cands.push(base.join("Rime"));
                cands.push(base.join("Programs").join("Rime"));
            }
        }
        for c in cands {
            if let Some(d) = find_deployer(&c) {
                info.install_dir = c.parent().map(|p| p.to_path_buf());
                info.deployer = Some(d);
                break;
            }
        }
    }
    info.installed = info.deployer.is_some();
    if info.installed && info.version.is_empty() {
        info.version = "已安装（版本未知）".into();
    }
    info
}

/// 静默安装倾听输入法（NSIS `/S`）；装完返回重新检测的结果
pub fn install_silent(installer: &Path) -> Result<WeaselInfo> {
    if !installer.is_file() {
        bail!("倾听输入法安装器不存在：{}", installer.display());
    }
    tracing::info!("静默安装倾听输入法：{}", installer.display());
    let mut cmd = std::process::Command::new(installer);
    cmd.arg("/S");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = cmd.spawn().context("启动倾听输入法安装器失败")?;
    let deadline = Instant::now() + Duration::from_secs(300);
    loop {
        if let Some(st) = child.try_wait().context("等待安装器失败")? {
            tracing::info!("倾听输入法安装器退出：{st}");
            break;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            bail!("倾听输入法安装超时（300s）");
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    std::thread::sleep(Duration::from_secs(2));
    let info = detect("");
    if !info.installed {
        bail!("倾听输入法安装后仍未检测到 WeaselDeployer.exe（可能被 UAC 拦下，请手动安装一次）");
    }
    Ok(info)
}

// ---------------------------------------------------------------- 铺方案包

fn fmt_mb(b: u64) -> String {
    format!("{:.1} MB", b as f64 / 1048576.0)
}

fn read_manifest(user_dir: &Path) -> std::collections::HashSet<String> {
    std::fs::read_to_string(user_dir.join(MANIFEST))
        .or_else(|_| std::fs::read_to_string(user_dir.join(LEGACY_MANIFEST)))
        .map(|s| {
            s.lines()
                .map(|l| l.trim().replace('\\', "/"))
                .filter(|l| !l.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// 把我们生成的文件登记进卸载清单（卸载时按清单删，绝不碰用户文件）
///
/// 注意：必须在 `copy_schemes()` **之后**调用 —— copy_schemes 会把「清单里有、本轮源里没有」
/// 的条目当陈旧文件删掉。
pub fn register_file(user_dir: &Path, rel: &str) -> Result<()> {
    let mut owned = read_manifest(user_dir);
    if !owned.insert(rel.to_string()) {
        return Ok(());
    }
    let mut list: Vec<String> = owned.into_iter().collect();
    list.sort();
    std::fs::write(user_dir.join(MANIFEST), list.join("\n"))
        .with_context(|| format!("写清单失败: {}", user_dir.join(MANIFEST).display()))?;
    Ok(())
}

/// 从卸载清单里摘掉（文件已删时用）
pub fn unregister_file(user_dir: &Path, rel: &str) {
    let mut owned = read_manifest(user_dir);
    if !owned.remove(rel) {
        return;
    }
    let mut list: Vec<String> = owned.into_iter().collect();
    list.sort();
    let _ = std::fs::write(user_dir.join(MANIFEST), list.join("\n"));
}

/// 递归收集源目录里的相对文件路径（跳过纯文档目录）
fn walk_files(root: &Path, rel: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for e in std::fs::read_dir(root.join(rel))
        .with_context(|| format!("读取失败: {}", root.join(rel).display()))?
    {
        let e = e?;
        let name = e.file_name().to_string_lossy().to_string();
        let r = rel.join(&name);
        if e.file_type()?.is_dir() {
            if rel.as_os_str().is_empty() && SKIP_DIRS.contains(&name.as_str()) {
                continue;
            }
            walk_files(root, &r, out)?;
        } else {
            if rel.as_os_str().is_empty() && SKIP_FILES.contains(&name.as_str()) {
                continue;
            }
            out.push(r);
        }
    }
    Ok(())
}

/// 源/目标是否「同大小且同 mtime」（同长度但内容不同的更新靠 mtime 区分）
fn same_file(from: &Path, to: &Path) -> bool {
    let (Ok(a), Ok(b)) = (from.metadata(), to.metadata()) else {
        return false;
    };
    if a.len() != b.len() {
        return false;
    }
    match (a.modified(), b.modified()) {
        (Ok(x), Ok(y)) => x == y,
        _ => false,
    }
}

/// 把方案包**平铺**铺进 Rime 用户目录（librime 只在用户目录根找 <schema>.schema.yaml，
/// 不搜子目录；词典/ lua / opencc 的相对引用也都以用户目录为根）。
///
/// 保护策略：目标文件已存在且不在我们清单里 → 视为用户文件，跳过不覆盖。
pub fn copy_schemes(res_dir: &Path, user_dir: &Path) -> Result<Vec<String>> {
    let src_root = res_dir.join("rime").join("schemes");
    let mut owned = read_manifest(user_dir);
    let mut log = Vec::new();
    let mut added = 0u64;
    let mut skipped = 0usize;
    let mut pruned = 0usize;

    // 先收集本次源里的全部相对路径，用于清理「上次铺过、这次源里已没有」的陈旧文件
    let mut wanted: std::collections::HashSet<String> = std::collections::HashSet::new();
    for name in ALL_SCHEME_DIRS {
        let src = src_root.join(name);
        if !src.is_dir() {
            continue;
        }
        let mut files = Vec::new();
        walk_files(&src, Path::new(""), &mut files)?;
        for rel in files {
            if skip_scheme_file(&rel) {
                continue;
            }
            wanted.insert(rel.to_string_lossy().replace('\\', "/"));
        }
    }
    let stale: Vec<String> = owned
        .iter()
        .filter(|k| !wanted.contains(*k))
        .cloned()
        .collect();
    for key in stale {
        let p = user_dir.join(key.replace('/', std::path::MAIN_SEPARATOR_STR));
        if p.is_file() && std::fs::remove_file(&p).is_ok() {
            owned.remove(&key);
            pruned += 1;
        }
    }

    for name in ALL_SCHEME_DIRS {
        let src = src_root.join(name);
        if !src.is_dir() {
            log.push(format!("  !! 缺少方案包：{}", src.display()));
            continue;
        }
        let mut files = Vec::new();
        walk_files(&src, Path::new(""), &mut files)?;
        let mut n = 0usize;
        let mut bytes = 0u64;
        for rel in files {
            if skip_scheme_file(&rel) {
                continue;
            }
            let key = rel.to_string_lossy().replace('\\', "/");
            let from = src.join(&rel);
            let to = user_dir.join(&rel);
            let sz = from.metadata().map(|m| m.len()).unwrap_or(0);
            let same = to.is_file() && same_file(&from, &to);
            if same && owned.contains(&key) {
                n += 1;
                continue;
            }
            if to.is_file() && !owned.contains(&key) {
                skipped += 1;
                continue; // 用户的文件，不动
            }
            if let Some(dir) = to.parent() {
                std::fs::create_dir_all(dir).ok();
            }
            std::fs::copy(&from, &to).with_context(|| format!("复制失败: {}", from.display()))?;
            owned.insert(key);
            added += sz;
            bytes += sz;
            n += 1;
        }
        log.push(format!("  {name}: {n} 个文件 / {}", fmt_mb(bytes)));
    }
    let mut list: Vec<String> = owned.into_iter().collect();
    list.sort();
    std::fs::write(user_dir.join(MANIFEST), list.join("\n"))
        .with_context(|| format!("写清单失败: {}", user_dir.join(MANIFEST).display()))?;
    // 迁移：旧名清单已被新清单取代，删掉免留垃圾
    let _ = std::fs::remove_file(user_dir.join(LEGACY_MANIFEST));
    // 清掉因删文件而变空的目录（用户有文件就保留）
    for name in ALL_SCHEME_DIRS {
        let _ = prune_empty_dirs(&user_dir.join(name), user_dir);
    }
    log.push(format!(
        "方案包合计新增/更新 {}（跳过用户同名文件 {skipped} 个{}）",
        fmt_mb(added),
        if pruned > 0 {
            format!("，清理陈旧文件 {pruned} 个")
        } else {
            String::new()
        }
    ));
    Ok(log)
}

/// 自底向上删空目录（只删空的）
fn prune_empty_dirs(dir: &Path, stop: &Path) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let children: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
    for c in children {
        if c.is_dir() {
            prune_empty_dirs(&c, stop);
        }
    }
    if dir != stop {
        let empty = std::fs::read_dir(dir).map(|mut i| i.next().is_none()).unwrap_or(false);
        if empty {
            let _ = std::fs::remove_dir(dir);
        }
    }
}

/// 铺语法模型到用户目录根（rime-ice 的 grammar.language 约定：文件名去扩展名 = language）
pub fn copy_grammar(res_dir: &Path, user_dir: &Path) -> Result<Option<String>> {
    let src = res_dir.join("rime").join("lm").join(GRAMMAR_FILE);
    if !src.is_file() {
        return Ok(None);
    }
    let dst = user_dir.join(GRAMMAR_FILE);
    let same = dst.is_file() && same_file(&src, &dst);
    if same {
        return Ok(Some(format!("语法模型已就位（{}）", fmt_mb(src.metadata()?.len()))));
    }
    std::fs::copy(&src, &dst).with_context(|| format!("复制语法模型失败: {}", src.display()))?;
    Ok(Some(format!(
        "语法模型 → {}（{}）",
        dst.display(),
        fmt_mb(src.metadata()?.len())
    )))
}

// ---------------------------------------------------------------- patch 文件

fn backup(path: &Path) -> Result<()> {
    if !path.is_file() {
        return Ok(());
    }
    let bak = path.with_extension("yaml.bak");
    std::fs::copy(path, &bak).with_context(|| format!("备份失败: {}", path.display()))?;
    tracing::info!("已备份 {}", bak.display());
    Ok(())
}

/// 摘掉标记块
fn strip_managed(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut inside = false;
    for line in text.lines() {
        if line.contains(MARK_BEGIN) || line.contains(LEGACY_MARK_BEGIN) {
            inside = true;
            continue;
        }
        if line.contains(MARK_END) || line.contains(LEGACY_MARK_END) {
            inside = false;
            continue;
        }
        if !inside {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// 删除 `patch:` 之下某个顶层键块（含更深缩进的子键）。用于清理用户手工加的
/// `ascii_composer`（会与我们写入的 switch_key 冲突）。
fn remove_key_block(text: &str, key: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut skipping = false;
    for line in text.lines() {
        if skipping {
            // 子键（缩进 >= 4 空格）继续跳过；遇到下一个顶层键(2 空格)结束
            if line.starts_with("    ") {
                continue;
            }
            skipping = false;
        }
        let is_target = line.strip_prefix("  ").is_some_and(|r| {
            !r.starts_with(' ') && r.split(':').next().unwrap_or("").trim() == key
        });
        if is_target {
            skipping = true;
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// 从文件里删掉某个「非我们管理」的顶层键块（保留我们的标记块内容不动，因会重建）
fn remove_key_from_file(path: &Path, key: &str) -> Result<()> {
    if !path.is_file() {
        return Ok(());
    }
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("读取失败: {}", path.display()))?;
    let text = strip_managed(&raw);
    let cleaned = remove_key_block(&text, key);
    if cleaned != text {
        backup(path)?;
        std::fs::write(path, &cleaned)
            .with_context(|| format!("写回失败: {}", path.display()))?;
        tracing::info!("已清理 {} 里手工的 `{key}` 配置", path.display());
    }
    Ok(())
}

fn is_patch_line(l: &str) -> bool {
    let t = l.trim_end();
    t == "patch:" || t == "patch :"
}

/// 文件里 patch: 之下已有的顶层键（缩进 2），用于冲突检测
fn patch_keys(text: &str) -> Vec<String> {
    let mut keys = Vec::new();
    let mut in_patch = false;
    for line in text.lines() {
        if line.trim_start().starts_with('#') || line.trim().is_empty() {
            continue;
        }
        if !in_patch {
            if is_patch_line(line) {
                in_patch = true;
            }
            continue;
        }
        if !line.starts_with(' ') {
            break; // 回到顶层
        }
        if let Some(rest) = line.strip_prefix("  ") {
            if rest.starts_with(' ') {
                continue; // 缩进更深，属于子键
            }
            let key = rest.split(':').next().unwrap_or("").trim();
            if !key.is_empty() {
                keys.push(key.to_string());
            }
        }
    }
    keys
}

/// 我们这段内容里的顶层键（缩进 2）
fn block_keys(block: &str) -> Vec<String> {
    block
        .lines()
        .filter_map(|l| l.strip_prefix("  "))
        .filter(|r| !r.starts_with(' ') && !r.trim_start().starts_with('#'))
        .filter_map(|r| r.split(':').next().map(|s| s.trim().to_string()))
        .filter(|s| !s.is_empty())
        .collect()
}

/// 把我们的标记块合并写进 `patch:` 之下（不整文件覆盖；已有同名键则跳过并报错）
pub fn patch_managed(path: &Path, block: &str) -> Result<()> {
    let existed = path.is_file();
    let raw = if existed {
        std::fs::read_to_string(path).with_context(|| format!("读取失败: {}", path.display()))?
    } else {
        String::new()
    };
    let text = strip_managed(&raw);

    let ours = block_keys(block);
    let theirs = patch_keys(&text);
    let conflicts: Vec<&String> = ours.iter().filter(|k| theirs.contains(k)).collect();
    if !conflicts.is_empty() {
        let names: Vec<String> = conflicts.iter().map(|s| s.to_string()).collect();
        bail!(
            "{} 里已有同名配置 [{}]，为避免破坏你的设置，已跳过写入（请手工合并）",
            path.file_name().unwrap_or_default().to_string_lossy(),
            names.join(", ")
        );
    }

    let mut body = text;
    if !body.lines().any(is_patch_line) {
        if !body.is_empty() && !body.ends_with('\n') {
            body.push('\n');
        }
        body.push_str("patch:\n");
    }

    let mut out = String::with_capacity(body.len() + block.len() + 128);
    let mut inserted = false;
    for line in body.lines() {
        out.push_str(line);
        out.push('\n');
        if !inserted && is_patch_line(line) {
            out.push_str(MARK_BEGIN);
            out.push('\n');
            out.push_str(block);
            // block 无尾换行时先补一个，避免 MARK_END 粘在上一行（卸载会多删一行用户配置）
            if !out.ends_with('\n') {
                out.push('\n');
            }
            out.push_str(MARK_END);
            out.push('\n');
            inserted = true;
        }
    }
    if !inserted {
        bail!("未找到 patch: 行，放弃写入 {}", path.display());
    }
    if existed {
        backup(path)?;
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).ok();
    }
    std::fs::write(path, out).with_context(|| format!("写入失败: {}", path.display()))?;
    tracing::info!("已写入 {}", path.display());
    Ok(())
}

/// 摘掉标记块；文件里只剩空行/注释/`patch:` 时删除该文件。返回 true = 文件已删除
pub fn unpatch_managed(path: &Path) -> Result<bool> {
    if !path.is_file() {
        return Ok(false);
    }
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("读取失败: {}", path.display()))?;
    if !raw.contains(MARK_BEGIN) && !raw.contains(LEGACY_MARK_BEGIN) {
        return Ok(false); // 不是我们写的，不动
    }
    let text = strip_managed(&raw);
    let meaningful = text
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.trim_start().starts_with('#'))
        .any(|l| !is_patch_line(l));
    if !meaningful {
        backup(path)?;
        std::fs::remove_file(path).with_context(|| format!("删除失败: {}", path.display()))?;
        tracing::info!("已删除（内容全是我们写的）{}", path.display());
        return Ok(true);
    }
    backup(path)?;
    std::fs::write(path, text).with_context(|| format!("写回失败: {}", path.display()))?;
    tracing::info!("已摘除管理块 {}", path.display());
    Ok(false)
}

// ---------------------------------------------------------------- 写配置

/// 写 default.custom.yaml（方案列表）+ 各方案 .custom.yaml（语法模型 / 简繁默认态 / 热词表 / 学习钩子）
///
/// `hotwords` = 是否已铺出打字热词表（决定要不要给 rime_ice patch `custom_phrase/user_dict`）
/// `learn_path` = 用户词学习日志路径（Some = 挂 lua 钩子）
pub fn write_configs(
    user_dir: &Path,
    plan: &Plan,
    cfg: &crate::config::Rime,
    hotwords: bool,
    // 打字联想（`custom_phrase/enable_completion`）——只在 `hotwords=true` 时有意义
    prefix_completion: bool,
    learn_path: Option<&str>,
) -> Result<Vec<String>> {
    let mut log = Vec::new();
    std::fs::create_dir_all(user_dir)
        .with_context(|| format!("创建用户目录失败: {}", user_dir.display()))?;

    let block = schemes::default_block(plan, cfg.zh_trad);
    let p = user_dir.join("default.custom.yaml");
    // 清理用户手工加的 ascii_composer（否则与我们写入的 switch_key 同名冲突，patch_managed 会跳过）
    let _ = remove_key_from_file(&p, "ascii_composer");
    match patch_managed(&p, &block) {
        Ok(()) => log.push(format!("  方案列表 → {}（{:?}）", p.display(), plan.schema_list)),
        Err(e) => log.push(format!("  !! {e}")),
    }

    for schema in &plan.patch_schemas {
        let yaml = user_dir.join(format!("{schema}.schema.yaml"));
        let sw_idx = if *schema == "rime_ice" {
            std::fs::read_to_string(&yaml)
                .ok()
                .and_then(|s| schemes::switch_index(&s, "traditionalization"))
        } else {
            None
        };
        let use_grammar = cfg.grammar && plan.grammar_schema == Some(*schema);
        // 热词表：只有 rime_ice 带 custom_phrase 翻译器，且方案文件里也定义了才 patch
        let use_terms = hotwords && *schema == "rime_ice" && schema_has(&yaml, "custom_phrase:");
        // 学习钩子：往 processors 里追加一个旁路 lua（返回 2 = 不处理按键）
        let learn = if *schema == "rime_ice" && schema_has(&yaml, "processors:") {
            learn_path
        } else {
            None
        };
        let sb = schemes::schema_block(
            sw_idx,
            cfg.zh_trad,
            use_grammar,
            use_terms,
            prefix_completion,
            learn,
        );
        if sb.trim().is_empty() {
            // 没内容可写：把旧的摘掉
            let f = user_dir.join(format!("{schema}.custom.yaml"));
            let _ = unpatch_managed(&f);
            continue;
        }
        let f = user_dir.join(format!("{schema}.custom.yaml"));
        match patch_managed(&f, &sb) {
            Ok(()) => log.push(format!(
                "  {} → {}{}{}{}",
                schema,
                f.display(),
                if use_grammar { "（含语法模型）" } else { "" },
                if use_terms {
                    if prefix_completion {
                        "（含打字热词表 + 前缀联想）"
                    } else {
                        "（含打字热词表）"
                    }
                } else {
                    ""
                },
                if learn.is_some() { "（含用户词学习钩子）" } else { "" }
            )),
            Err(e) => log.push(format!("  !! {e}")),
        }
    }
    Ok(log)
}

/// 方案文件里是否定义了某个键（决定能不能 patch 它）
fn schema_has(schema_yaml: &Path, key: &str) -> bool {
    std::fs::read_to_string(schema_yaml)
        .map(|s| s.lines().any(|l| l.trim_start().starts_with(key)))
        .unwrap_or(false)
}

/// 摘掉我们写的所有配置（保留用户自己的内容）
pub fn unwrite_configs(user_dir: &Path) -> Vec<String> {
    let mut log = Vec::new();
    let mut files: Vec<PathBuf> = vec![user_dir.join("default.custom.yaml")];
    for id in ALL_SCHEMA_IDS {
        files.push(user_dir.join(format!("{id}.custom.yaml")));
    }
    for f in files {
        match unpatch_managed(&f) {
            Ok(true) => log.push(format!("  删除 {}", f.display())),
            Ok(false) => {}
            Err(e) => log.push(format!("  !! {e}")),
        }
    }
    log
}

// ---------------------------------------------------------------- 提权启动

/// 提权启动的进程（ShellExecuteEx + runas，UAC 弹窗由用户确认）
#[cfg(windows)]
struct ElevatedProc(windows::Win32::Foundation::HANDLE);

#[cfg(windows)]
fn spawn_elevated(exe: &Path, params: &str) -> Result<ElevatedProc> {
    use windows::core::PCWSTR;
    use windows::Win32::UI::Shell::{SHELLEXECUTEINFOW, ShellExecuteExW, SEE_MASK_NOCLOSEPROCESS};
    let verb: Vec<u16> = "runas\0".encode_utf16().collect();
    let file: Vec<u16> = exe
        .as_os_str()
        .to_string_lossy()
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let params: Vec<u16> = params.encode_utf16().chain(Some(0)).collect();
    let mut sei = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS,
        lpVerb: PCWSTR(verb.as_ptr()),
        lpFile: PCWSTR(file.as_ptr()),
        lpParameters: PCWSTR(params.as_ptr()),
        nShow: 0, // SW_HIDE，别闪窗
        ..Default::default()
    };
    unsafe { ShellExecuteExW(&mut sei) }
        .map_err(|e| anyhow::anyhow!("提权启动失败（UAC 被取消或没有管理员权限）: {e}"))?;
    if sei.hProcess.is_invalid() {
        bail!("提权启动未返回进程句柄");
    }
    Ok(ElevatedProc(sei.hProcess))
}

#[cfg(windows)]
impl ElevatedProc {
    /// 等待结束；返回 Some(exit_code) 或 None（超时）
    fn wait(&self, timeout: Duration) -> Option<u32> {
        use windows::Win32::Foundation::{WAIT_OBJECT_0, WAIT_TIMEOUT};
        use windows::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject};
        let ms = timeout.as_millis().min(u32::MAX as u128) as u32;
        let r = unsafe { WaitForSingleObject(self.0, ms) };
        if r == WAIT_TIMEOUT {
            return None;
        }
        if r != WAIT_OBJECT_0 {
            return None;
        }
        let mut code = 0u32;
        unsafe { GetExitCodeProcess(self.0, &mut code) }.ok()?;
        Some(code)
    }

    fn kill(&self) {
        use windows::Win32::System::Threading::TerminateProcess;
        unsafe {
            let _ = TerminateProcess(self.0, 1);
        }
    }
}

#[cfg(windows)]
impl Drop for ElevatedProc {
    fn drop(&mut self) {
        use windows::Win32::Foundation::CloseHandle;
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

// ---------------------------------------------------------------- 部署

/// 最新的 weasel 日志（只认本次部署之后写过的）
fn weasel_log_since(since: std::time::SystemTime) -> Option<PathBuf> {
    let tmp = env_dir("TEMP").or_else(|| env_dir("TMP"))?;
    let dir = tmp.join("rime.weasel");
    let mut newest: Option<(std::time::SystemTime, PathBuf)> = None;
    for e in std::fs::read_dir(&dir).ok()?.flatten() {
        let p = e.path();
        if p.extension().map(|x| x == "log").unwrap_or(false) {
            let Ok(t) = e.metadata().and_then(|m| m.modified()) else {
                continue;
            };
            if t < since {
                continue;
            }
            if newest.as_ref().map(|(nt, _)| t > *nt).unwrap_or(true) {
                newest = Some((t, p));
            }
        }
    }
    newest.map(|(_, p)| p)
}

fn tail_lines(path: &Path, n: usize) -> String {
    let Ok(text) = std::fs::read_to_string(path) else {
        return String::new();
    };
    let lines: Vec<&str> = text.lines().collect();
    lines
        .iter()
        .rev()
        .take(n)
        .rev()
        .cloned()
        .collect::<Vec<_>>()
        .join("\n")
}

/// 调 WeaselDeployer 重新部署（**提权执行**）并等它真正跑完
///
/// 为什么必须提权：
/// - 倾听输入法安装器是 `RequestExecutionLevel admin`，装完 WeaselServer 也是提权进程；
/// - 部署时部署器要先通过 IPC 让 WeaselServer 进维护模式（释放 build\*.bin 的文件锁），
///   非提权进程给提权进程发消息会被 UIPI 拦掉 → 部署器永久卡住（实测复现 3 次）；
/// - 反过来提权进程给非提权进程发消息是允许的，所以「一律提权」两种情况都能跑通。
///
/// 另外：`/deploy` 必须是**精确的整串参数**（weasel 用 wcscmp 比较），
/// 多带任何参数都会掉进 GUI 分支弹设置窗并占住部署互斥体。
pub fn deploy(info: &WeaselInfo, schemas: &[&str]) -> Result<String> {
    let deployer = info
        .deployer
        .clone()
        .context("未找到 WeaselDeployer.exe（倾听输入法未安装？）")?;
    let started = Instant::now();
    let started_wall = std::time::SystemTime::now();
    // 完成判定：build\<schema>.schema.yaml 全部被重写（librime 每个方案都会写这个）
    let marks: Vec<(PathBuf, Option<std::time::SystemTime>)> = schemas
        .iter()
        .map(|s| {
            let p = info.user_dir.join("build").join(format!("{s}.schema.yaml"));
            let t = p.metadata().ok().and_then(|m| m.modified().ok());
            (p, t)
        })
        .collect();

    tracing::info!("提权部署：{} /deploy（可能弹 UAC）", deployer.display());
    #[cfg(windows)]
    let proc = spawn_elevated(&deployer, "/deploy")?;
    #[cfg(not(windows))]
    let proc = {
        bail!("Rime 部署仅支持 Windows")
    };

    let deadline = Instant::now() + Duration::from_secs(900);
    let mut last_err = String::new();
    let mut exited: Option<u32> = None;

    // 完成判定以「部署器退出」为准（它把任务跑完才退）；
    // 产物时间戳只做退出后的校验——librime 是先写 <schema>.schema.yaml 再建词库表，
    // 只盯产物会在词库表还没建完时就误判完成。
    loop {
        if let Some(code) = proc.wait(Duration::from_secs(2)) {
            exited = Some(code);
            break;
        }
        if let Some(lp) = weasel_log_since(started_wall) {
            let tail = tail_lines(&lp, 60);
            if tail.contains("missing input schema") {
                last_err = format!("部署报错：\n{}", tail_lines(&lp, 12));
                break;
            }
        }
        if Instant::now() > deadline {
            last_err = "部署超时（900s）".into();
            break;
        }
    }

    if !last_err.is_empty() {
        proc.kill();
        bail!("{last_err}");
    }
    match exited {
        Some(0) => {}
        Some(code) => {
            let tail = weasel_log_since(started_wall)
                .map(|p| tail_lines(&p, 10))
                .unwrap_or_default();
            bail!(
                "部署器退出码 {code}（1 = 有另一个部署进程在跑，先关掉倾听输入法设置窗/等它结束）\n{tail}"
            );
        }
        None => {
            proc.kill();
            bail!(
                "部署超时（900s，日志见 {}）",
                info.user_dir.join("build").display()
            );
        }
    }

    // 产物校验
    let stale: Vec<String> = marks
        .iter()
        .filter(|(p, old)| {
            !p.metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .map(|t| Some(t) != *old)
                .unwrap_or(false)
        })
        .map(|(p, _)| {
            p.file_name()
                .map(|x| x.to_string_lossy().to_string())
                .unwrap_or_default()
        })
        .collect();

    let log_path = weasel_log_since(started_wall);
    let tail = log_path
        .as_ref()
        .map(|p| format!("[{}]\n{}", p.display(), tail_lines(p, 12)))
        .unwrap_or_else(|| "（未找到 weasel 日志）".into());
    Ok(format!(
        "部署完成，耗时 {:.1}s（方案 {:?}）{}{}",
        started.elapsed().as_secs_f32(),
        schemas,
        if stale.is_empty() {
            String::new()
        } else {
            format!("  ⚠ 未更新的产物: {}", stale.join(", "))
        },
        format!("\n{tail}")
    ))
}

// ---------------------------------------------------------------- 卸载

/// 保守卸载：按清单删我们铺的文件（连带空目录），保留用户词库与配置
pub fn uninstall_our_files(user_dir: &Path, res_dir: &Path) -> Vec<String> {
    let mut log = Vec::new();
    let owned = read_manifest(user_dir);
    let mut dirs: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    let mut n = 0usize;
    for key in &owned {
        let p = user_dir.join(key.replace('/', std::path::MAIN_SEPARATOR_STR));
        if p.is_file() {
            match std::fs::remove_file(&p) {
                Ok(()) => {
                    n += 1;
                    let mut d = p.parent().map(|x| x.to_path_buf());
                    while let Some(cur) = d {
                        if cur == *user_dir {
                            break;
                        }
                        dirs.insert(cur.clone());
                        d = cur.parent().map(|x| x.to_path_buf());
                    }
                }
                Err(e) => log.push(format!("  !! 删除 {} 失败: {e}", p.display())),
            }
        }
    }
    // 自底向上删空目录（只删空的，用户有文件就保留）
    let mut sorted: Vec<PathBuf> = dirs.into_iter().collect();
    sorted.sort_by_key(|p| std::cmp::Reverse(p.components().count()));
    for d in sorted {
        let empty = std::fs::read_dir(&d).map(|mut i| i.next().is_none()).unwrap_or(false);
        if empty {
            let _ = std::fs::remove_dir(&d);
        }
    }
    log.push(format!("  已删除 {n} 个我们铺的文件"));

    // 旧的「子目录布局」残留（早期版本铺过）也一并清掉
    for d in ALL_SCHEME_DIRS {
        let p = user_dir.join(d);
        if p.is_dir() {
            match std::fs::remove_dir_all(&p) {
                Ok(()) => log.push(format!("  删除目录 {}", p.display())),
                Err(e) => log.push(format!("  !! 删除 {} 失败: {e}", p.display())),
            }
        }
    }
    let g = user_dir.join(GRAMMAR_FILE);
    if g.is_file() {
        match std::fs::remove_file(&g) {
            Ok(()) => log.push(format!("  删除语法模型 {}", g.display())),
            Err(e) => log.push(format!("  !! 删除 {} 失败: {e}", g.display())),
        }
    }
    // 打字热词表（清单丢过也能清干净）
    let t = user_dir.join(TERMS_FILE);
    if t.is_file() {
        match std::fs::remove_file(&t) {
            Ok(()) => log.push(format!("  删除热词表 {}", t.display())),
            Err(e) => log.push(format!("  !! 删除 {} 失败: {e}", t.display())),
        }
    }
    // 用户词学习钩子
    let l = user_dir.join(LEARN_LUA);
    if l.is_file() {
        match std::fs::remove_file(&l) {
            Ok(()) => log.push(format!("  删除学习钩子 {}", l.display())),
            Err(e) => log.push(format!("  !! 删除 {} 失败: {e}", l.display())),
        }
    }
    for name in [MANIFEST, LEGACY_MANIFEST] {
        let m = user_dir.join(name);
        if m.is_file() {
            let _ = std::fs::remove_file(&m);
        }
    }
    // build 里的编译产物（可再生成，不删会白占 ~170MB）
    let build = user_dir.join("build");
    let mut n_build = 0usize;
    for id in OUR_SCHEMA_IDS {
        for ext in ["schema.yaml", "prism.bin", "table.bin", "reverse.bin"] {
            let p = build.join(format!("{id}.{ext}"));
            if p.is_file() && std::fs::remove_file(&p).is_ok() {
                n_build += 1;
            }
        }
    }
    if n_build > 0 {
        log.push(format!("  已删除 {n_build} 个编译产物（build\\）"));
    }
    log.extend(unwrite_configs(user_dir));
    let _ = res_dir;
    log
}

/// 铺用户词学习钩子（lua 脚本）到用户目录，并登记进卸载清单
///
/// ⚠️ 必须在 `copy_schemes()` **之后**调用：copy_schemes 会把清单里「本轮源里没有」的条目当陈旧文件删掉。
pub fn write_learn_lua(user_dir: &Path) -> Result<String> {
    let p = user_dir.join(LEARN_LUA);
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d)
            .with_context(|| format!("创建目录失败: {}", d.display()))?;
    }
    std::fs::write(&p, crate::learn::LUA_BODY)
        .with_context(|| format!("写学习钩子失败: {}", p.display()))?;
    register_file(user_dir, LEARN_LUA)?;
    Ok(format!("  用户词学习钩子 → {}", p.display()))
}

/// 生成一份部署计划（供 GUI/CLI 共用）
pub fn make_plan(cfg: &crate::config::Rime) -> Result<Plan> {
    let lang = cfg.lang();
    if lang == Lang::Fr {
        bail!("法文方案包未内置（二期预留）");
    }
    schemes::plan(lang, cfg.english_mixed, cfg.zh_trad)
        .with_context(|| format!("{} 暂无可用方案包", lang.label()))
}
