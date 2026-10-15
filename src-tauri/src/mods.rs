use std::fs;
use std::io::Read as _;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::config;
use crate::paths::Root;

/// 网关日志只扫到这个字节数：mod 的装载与自检都发生在 boot 附近，
/// 后面滚起来的战斗日志跟这一页无关。
const GATEWAY_SCAN_LIMIT: u64 = 2_000_000;

/// 动过这些文件的 mod 不能单独回滚：半截恢复会把包变成开不了的档
/// （`Script.pvf` 与 `sk.dat` 是一对，`dinput8.dll` 与 `plugins.json` 是一对）。
const BRICK_FILES: [&str; 6] = [
    "Script.pvf",
    "sk.dat",
    "DFO.exe",
    "dinput8.dll",
    "ChineseLocalization.dll",
    "plugins.json",
];

#[derive(Serialize)]
pub struct ModsState {
    pub server: ServerMods,
    pub scripts: ScriptMods,
    pub client: ClientMods,
    pub modkit: ModkitMods,
}

// ------------------------------------------------------------------ 服务端 mod

#[derive(Serialize)]
pub struct ServerMods {
    pub mods_dir: String,
    pub enabled_present: bool,
    /// 网关自己报的那句启用清单结论，原样搬过来，不在界面里重述一遍。
    pub gateway_note: String,
    pub broken_note: String,
    /// 名单写在盘上、但上一场网关报「没找到」：这个位置它不读，开关是空转的。
    pub ignored_note: String,
    /// 装载证据来自哪一场的日志；空字符串表示还没有跑过。
    pub evidence_run: String,
    pub mods: Vec<ServerMod>,
}

#[derive(Serialize)]
pub struct ServerMod {
    pub id: String,
    pub purpose: String,
    pub enabled: bool,
    /// 这一场的日志里真的装载了它（被禁用的 mod 只会出现在清单里，不会出现在日志里）。
    pub seen: bool,
}

#[derive(Serialize)]
pub struct ScriptMods {
    pub dir: String,
    pub exists: bool,
    pub scripts: Vec<ScriptFile>,
    /// 网关报「已接收」的、由 mod 自带的脚本（`<mod id>:<文件名>`）。
    /// 它们不在盘上，只列目录会漏掉这一份 —— 玩家看到的「脚本 1 份」就是这么来的。
    pub provided: Vec<String>,
}

#[derive(Serialize)]
pub struct ScriptFile {
    pub name: String,
    pub size: u64,
}

// ---------------------------------------------------------------- 客户端 mod 插件

#[derive(Serialize)]
pub struct ClientMods {
    pub dir: String,
    pub present: bool,
    pub auto_discover: bool,
    pub plugins: Vec<ClientPlugin>,
    pub host_log: String,
    pub host_tail: Vec<String>,
}

#[derive(Serialize)]
pub struct ClientPlugin {
    pub id: String,
    pub dll: String,
    /// 清单里有没有这一条：没有但文件在场的，是靠自动发现被加载的。
    pub in_manifest: bool,
    pub enabled: bool,
    pub deps: Vec<String>,
    /// 插件自己写的状态文件里的 `ready`；没写过就是 `None`。
    pub ready: Option<bool>,
    pub reject_reason: String,
}

// -------------------------------------------------------------------- modkit 安装层

#[derive(Serialize)]
pub struct ModkitMods {
    pub present: bool,
    pub registry: String,
    pub total: usize,
    pub entries: usize,
    pub risky: Vec<ModkitRisk>,
}

#[derive(Serialize)]
pub struct ModkitRisk {
    pub id: String,
    pub target: String,
    pub kind: String,
}

#[derive(Deserialize)]
struct EnabledFile {
    #[serde(default)]
    disabled: Vec<String>,
}

#[derive(Deserialize)]
struct PluginFile {
    #[serde(rename = "autoDiscover", default = "is_true")]
    auto_discover: bool,
    #[serde(default)]
    plugins: Vec<PluginEntry>,
}

#[derive(Deserialize)]
struct PluginEntry {
    #[serde(default)]
    name: String,
    #[serde(default)]
    path: String,
    #[serde(default = "is_true")]
    enabled: bool,
    #[serde(default)]
    deps: Vec<String>,
}

#[derive(Deserialize)]
struct PluginStatus {
    #[serde(default)]
    ready: Option<bool>,
    #[serde(rename = "rejectReason", default)]
    reject_reason: String,
}

#[derive(Deserialize)]
struct Registry {
    #[serde(default)]
    mods: Vec<RegistryMod>,
}

#[derive(Deserialize)]
struct RegistryMod {
    #[serde(default)]
    id: String,
    #[serde(default)]
    entries: Option<Vec<RegistryEntry>>,
}

#[derive(Deserialize)]
struct RegistryEntry {
    #[serde(default)]
    kind: String,
    #[serde(default)]
    target: String,
}

fn is_true() -> bool {
    true
}

pub fn state(root: &Root) -> ModsState {
    let scan = scan_gateway_log(root);
    ModsState {
        server: server_mods(root, &scan),
        scripts: script_mods(root, &scan),
        client: client_mods(root),
        modkit: modkit_mods(root),
    }
}

fn read_enabled_file(path: &Path) -> Result<EnabledFile, String> {
    let text = fs::read_to_string(path).map_err(|err| format!("enabled.json 读不动：{err}"))?;
    serde_json::from_str(&text).map_err(|err| format!("enabled.json 解析不了：{err}"))
}

fn purpose_of(purposes: &[(String, String)], id: &str) -> String {
    purposes
        .iter()
        .find(|(seen, _)| seen == id)
        .map(|(_, purpose)| purpose.clone())
        .unwrap_or_default()
}

/// 最近一场网关 stderr 里跟 mod 有关的全部事实。
///
/// 为什么从日志取而不是从目录扫：服务端 mod 是编译进网关二进制的，
/// 盘上有没有 `mods/` 都跟它装没装载无关；只有日志说得出「这一场到底装载了哪几个」。
struct GatewayScan {
    /// 这一场真的装载了的 mod id（`已装载 …` 那一句报的名单）。
    loaded: Vec<String>,
    /// 每个 mod 自己写的用途文案（`[mod <id>] 已启用：<用途>`）。
    purposes: Vec<(String, String)>,
    /// `servermod: 启用清单：…` 后面网关自己的结论，原样搬过来。
    note: String,
    /// 装载证据来自哪一场；空字符串 = 还没跑过。
    run: String,
    /// 网关接收下来的规则脚本（`奖励规则脚本 N 份：<mod>:<文件>`）。
    scripts: Vec<String>,
}

/// 网关会去哪一个 `mods/` 找 `enabled.json`。
///
/// 它开的是**相对路径** `mods/enabled.json`，基准是网关自己的工作目录；编排程序在
/// `internal/launcher/serverrun.go` 里把 cwd 设成 `gatewayWorkingDirectory(module, 内层PVF)`：
/// 内层 PVF 是显式指定的才用模块根，否则用模块根往上两级（= `<包根>/server`）。
/// 而 `serverModuleDir()` 又要求那一层带 `module dfolan` 的 `go.mod` —— 纯二进制包没有，
/// 于是退回 `"."`，相对路径就落在 cwd 上。本包是拆壳包、没有 go.mod ⇒ `<包根>/server/mods`。
fn runtime_mods_dir(root: &Root) -> std::path::PathBuf {
    let module = root.work_dir();
    let declares_module = |dir: &Path| match fs::read_to_string(dir.join("go.mod")) {
        Ok(text) => text
            .lines()
            .map(str::trim)
            .any(|line| line == "module dfolan" || line.starts_with("module dfolan ")),
        Err(_) => false,
    };
    // 模块根带 `module dfolan` 时它自己就是答案；否则服务端退回相对路径，落在 cwd 上。
    if declares_module(&module) {
        module.join("mods")
    } else {
        root.path.join("server").join("mods")
    }
}

fn server_mods(root: &Root, scan: &GatewayScan) -> ServerMods {
    let mods_dir = runtime_mods_dir(root);
    let enabled_path = mods_dir.join("enabled.json");
    let mut disabled: Vec<String> = Vec::new();
    let mut broken_note = String::new();
    let enabled_present = enabled_path.is_file();
    if enabled_present {
        match read_enabled_file(&enabled_path) {
            Ok(file) => disabled = file.disabled,
            Err(err) => broken_note = err,
        }
    }

    let mut mods: Vec<ServerMod> = scan
        .loaded
        .iter()
        .map(|id| ServerMod {
            purpose: purpose_of(&scan.purposes, id),
            id: id.clone(),
            enabled: !disabled.iter().any(|seen| seen == id),
            seen: true,
        })
        .collect();
    for id in disabled.iter().filter(|id| !scan.loaded.contains(id)) {
        mods.push(ServerMod {
            purpose: purpose_of(&scan.purposes, id),
            id: id.clone(),
            enabled: false,
            seen: false,
        });
    }
    mods.sort_by(|a, b| a.id.cmp(&b.id));

    // 名单在场、可上一场网关报的是「没找到」，而且文件比那场日志还老 ⇒ 它读的不是这个位置，
    // 开关按下去只是写了一份没人看的文件。这条只能由实机日志说话，界面上必须显出来。
    let mut ignored_note = String::new();
    if enabled_present && scan.note.contains("没有 enabled.json") {
        let written = fs::metadata(&enabled_path).and_then(|m| m.modified()).ok();
        let logged = root
            .newest_run_dir()
            .map(|dir| fs::metadata(dir.join("gateway.err")).and_then(|m| m.modified()).ok())
            .flatten();
        if let (Some(written), Some(logged)) = (written, logged) {
            if written <= logged {
                ignored_note = format!(
                    "名单写在 {}，可上一场网关报「{}」—— 它不读这个位置，这里的开关按了不生效",
                    enabled_path.display(),
                    scan.note
                );
            }
        }
    }

    ServerMods {
        mods_dir: mods_dir.display().to_string(),
        enabled_present,
        gateway_note: scan.note.clone(),
        broken_note,
        ignored_note,
        evidence_run: scan.run.clone(),
        mods,
    }
}

fn scan_gateway_log(root: &Root) -> GatewayScan {
    let empty = GatewayScan {
        loaded: Vec::new(),
        purposes: Vec::new(),
        note: String::new(),
        run: String::new(),
        scripts: Vec::new(),
    };
    let Some(dir) = root.newest_run_dir() else {
        return empty;
    };
    let path = dir.join("gateway.err");
    let Ok(mut file) = fs::File::open(&path) else {
        return empty;
    };
    let mut buf = Vec::new();
    if file
        .by_ref()
        .take(GATEWAY_SCAN_LIMIT)
        .read_to_end(&mut buf)
        .is_err()
    {
        return empty;
    }
    let text = String::from_utf8_lossy(&buf);

    let mut out = GatewayScan {
        loaded: Vec::new(),
        purposes: Vec::new(),
        note: String::new(),
        run: dir
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default(),
        scripts: Vec::new(),
    };
    for line in text.lines() {
        if let Some((id, purpose)) = enabled_line(line) {
            if !out.purposes.iter().any(|(seen, _)| seen == &id) {
                out.purposes.push((id, purpose));
            }
        }
        // 被禁用的 mod 走另一条文案，它才是「这一场它真的没装载」的第一证据。
        if let Some((id, words)) = disabled_line(line) {
            if !out.purposes.iter().any(|(seen, _)| seen == &id) {
                out.purposes.push((id, words));
            }
        }
        if let Some(ids) = summary_line(line) {
            for id in ids {
                if !out.loaded.contains(&id) {
                    out.loaded.push(id);
                }
            }
        }
        if let Some(pos) = line.find("启用清单：") {
            out.note = line[pos + "启用清单：".len()..].trim().to_string();
        }
        // 清单读成功时网关不打「启用清单：」那一句，只打这一句。
        if let Some(pos) = line.find("已禁用 ") {
            out.note = line[pos..].trim().to_string();
        }
        if let Some(pos) = line.find("规则脚本 ") {
            if let Some(rest) = line[pos..].split_once('：').map(|(_, tail)| tail) {
                for item in rest.split(',') {
                    let item = item.trim();
                    if !item.is_empty() && !out.scripts.contains(&item.to_string()) {
                        out.scripts.push(item.to_string());
                    }
                }
            }
        }
    }
    out
}

/// `[mod <id>] 已启用：<用途>` —— 服务端把这一条当「mod 真的注册上了」的第一证据，
/// 用途文案也是它自己写的，比启动器编一份名字表可信。
fn enabled_line(line: &str) -> Option<(String, String)> {
    let rest = line.split_once("[mod ")?.1;
    let (id, tail) = rest.split_once("] 已启用：")?;
    let id = id.trim();
    if id.is_empty() {
        return None;
    }
    Some((id.to_string(), tail.trim().to_string()))
}

/// `[mod <id>] 已被禁用（mods/enabled.json），本次不加载任何规则` —— 同一宿主函数
/// 在 `Enabled()` 为假时打的另一条；用途文案此时拿不到，只能引用网关自己的这句
/// （括号里是它给的落点，界面不该重复一遍）。
fn disabled_line(line: &str) -> Option<(String, String)> {
    let rest = line.split_once("[mod ")?.1;
    let (id, tail) = rest.split_once("] 已被禁用")?;
    let id = id.trim();
    if id.is_empty() {
        return None;
    }
    let words = match tail.rsplit_once('）') {
        Some((_, after)) => after.trim_start_matches(['，', ' ']).to_string(),
        None => tail.trim().to_string(),
    };
    Some((id.to_string(), words))
}

/// `servermod: 已装载 N 个服务端 mod：<id>、<id>；boot 钩子 …` —— 取 mod 名单那一段。
///
/// 这一场只有 1 个 mod，日志里看不出分隔符是 `,` 还是 `、`，所以按「mod id 的合法字符
/// 之外都算分隔」来切（`validModID` 只认小写字母、数字、点、短横线），两种写法都能解。
fn summary_line(line: &str) -> Option<Vec<String>> {
    if !line.contains("已装载") {
        return None;
    }
    let rest = line.split_once("个服务端 mod：")?.1;
    let head = rest.split('；').next().unwrap_or(rest);
    Some(
        head.split(|ch: char| ch == ',' || ch == '，' || ch == '、' || ch.is_whitespace())
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .map(str::to_string)
            .collect(),
    )
}

fn script_mods(root: &Root, scan: &GatewayScan) -> ScriptMods {
    let dir = root.mods_dir();
    let exists = dir.is_dir();
    let mut scripts = Vec::new();
    if exists {
        if let Ok(entries) = fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .map(|ext| !ext.eq_ignore_ascii_case("lua"))
                    .unwrap_or(true)
                {
                    continue;
                }
                let Some(name) = path.file_name().map(|n| n.to_string_lossy().into_owned()) else {
                    continue;
                };
                let size = entry.metadata().map(|meta| meta.len()).unwrap_or_default();
                scripts.push(ScriptFile { name, size });
            }
        }
    }
    scripts.sort_by(|a, b| a.name.cmp(&b.name));
    ScriptMods {
        dir: dir.display().to_string(),
        exists,
        scripts,
        provided: scan.scripts.clone(),
    }
}

fn client_mods(root: &Root) -> ClientMods {
    let dir = config::client_dir(root).join(".115us-mods");
    let manifest = dir.join("plugins.json");
    let present = dir.is_dir();
    let parsed: Option<PluginFile> = fs::read_to_string(&manifest)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok());

    let mut plugins: Vec<ClientPlugin> = Vec::new();
    let mut auto_discover = false;
    if let Some(file) = parsed {
        auto_discover = file.auto_discover;
        for entry in file.plugins {
            let status = read_status(&dir, &entry.name);
            plugins.push(ClientPlugin {
                id: entry.name,
                dll: base_name(&entry.path),
                in_manifest: true,
                enabled: entry.enabled,
                deps: entry.deps,
                ready: status.0,
                reject_reason: status.1,
            });
        }
        // 文件在场但清单里没列的 DLL 走自动发现：它照样会被加载，漏报等于骗人。
        for dll in dlls_in(&dir) {
            if plugins.iter().any(|p| p.dll == dll) {
                continue;
            }
            plugins.push(ClientPlugin {
                id: format!("（清单外）{dll}"),
                dll,
                in_manifest: false,
                enabled: auto_discover,
                deps: Vec::new(),
                ready: None,
                reject_reason: String::new(),
            });
        }
    }
    plugins.sort_by(|a, b| a.dll.cmp(&b.dll));

    let host_log = config::client_dir(root).join("client-host.log");
    ClientMods {
        dir: dir.display().to_string(),
        present,
        auto_discover,
        plugins,
        host_tail: tail_lines(&host_log, 6),
        host_log: host_log.display().to_string(),
    }
}

fn base_name(path: &str) -> String {
    Path::new(&path.replace('/', "\\"))
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string())
}

fn dlls_in(dir: &Path) -> Vec<String> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            path.extension()
                .and_then(|ext| ext.to_str())
                .map(|ext| ext.eq_ignore_ascii_case("dll"))
                .unwrap_or(false)
                .then(|| path.file_name().map(|n| n.to_string_lossy().into_owned()))?
        })
        .collect()
}

fn read_status(dir: &Path, id: &str) -> (Option<bool>, String) {
    let path = dir.join(format!("{id}.status.json"));
    let Ok(text) = fs::read_to_string(&path) else {
        return (None, String::new());
    };
    match serde_json::from_str::<PluginStatus>(&text) {
        Ok(status) => (status.ready, status.reject_reason),
        Err(_) => (None, String::new()),
    }
}

fn tail_lines(path: &Path, count: usize) -> Vec<String> {
    let Ok(text) = fs::read_to_string(path) else {
        return Vec::new();
    };
    let mut lines: Vec<String> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect();
    let keep = lines.len().saturating_sub(count);
    lines.drain(..keep);
    lines
}

fn modkit_mods(root: &Root) -> ModkitMods {
    let registry = config::client_dir(root)
        .join(".launcher-mods")
        .join("modkit")
        .join("registry.json");
    let present = registry.is_file();
    let parsed: Option<Registry> = fs::read_to_string(&registry)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok());
    let Some(registry_file) = parsed else {
        return ModkitMods {
            present,
            registry: registry.display().to_string(),
            total: 0,
            entries: 0,
            risky: Vec::new(),
        };
    };

    let mut entries = 0usize;
    let mut risky = Vec::new();
    for module in &registry_file.mods {
        let list = module.entries.as_deref().unwrap_or_default();
        entries += list.len();
        for entry in list {
            let target = base_name(&entry.target);
            if BRICK_FILES.iter().any(|brick| *brick == target) {
                risky.push(ModkitRisk {
                    id: module.id.clone(),
                    target,
                    kind: entry.kind.clone(),
                });
            }
        }
    }
    ModkitMods {
        present,
        registry: registry.display().to_string(),
        total: registry_file.mods.len(),
        entries,
        risky,
    }
}

// ------------------------------------------------------------------------ 写回

/// 勾选/取消一个服务端 mod：写 `<整合包根>/mods/enabled.json` 的禁用名单。
///
/// 口径跟服务端 `servermod.SetModEnabled` 一致（同一份文件、同样是禁用名单、
/// 同样拒绝覆盖解析不了的旧文件），因为源码里明写了「唯一出路是把该 mod 写进
/// mods/enabled.json（启动器「MOD 工具」页可做）」——这一页就是那个页。
/// 落点由 `runtime_mods_dir` 按网关自己的解析算，不按「模块根」想当然。
pub fn set_mod_enabled(root: &Root, id: &str, enabled: bool) -> Result<String, String> {
    if !valid_mod_id(id) {
        return Err(format!(
            "mod id 不合法：{id}（只允许小写字母、数字、点与短横线，长度 1~64）"
        ));
    }
    let mods_dir = runtime_mods_dir(root);
    let path = mods_dir.join("enabled.json");

    let mut disabled: Vec<String> = Vec::new();
    if path.is_file() {
        let file = read_enabled_file(&path).map_err(|err| {
            format!("{err} —— 启动器拒绝覆盖，请先人工修好：{}", path.display())
        })?;
        disabled = file.disabled;
    }
    disabled.retain(|seen| seen.trim() != id);
    if !enabled {
        disabled.push(id.to_string());
    }
    disabled.sort();

    fs::create_dir_all(&mods_dir).map_err(|err| format!("建 mods 目录失败：{err}"))?;
    let body = serde_json::json!({
        "schema": 1,
        "disabled": disabled,
        "updatedAt": chrono::Local::now().to_rfc3339(),
        "updatedBy": "DFO-115US 启动器",
    });
    let tmp = mods_dir.join("enabled.json.tmp");
    let text = serde_json::to_string_pretty(&body).map_err(|err| err.to_string())?;
    fs::write(&tmp, format!("{text}\n")).map_err(|err| format!("写临时文件失败：{err}"))?;
    fs::rename(&tmp, &path).map_err(|err| format!("替换 enabled.json 失败：{err}"))?;

    Ok(format!(
        "已{}服务端 mod「{id}」（{}，下次启动服务端生效）",
        if enabled { "启用" } else { "停用" },
        path.display()
    ))
}

/// 与服务端 `validModID` 同一口径，免得写出一个服务端根本不认的名单。
fn valid_mod_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value.chars().all(|ch| {
            ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '.' || ch == '-'
        })
}

