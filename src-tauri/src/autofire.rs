//! 按键连发 / 一键连招引擎。
//!
//! 三条从旧版实机验证继承下来的硬规矩，违反任何一条就直接不可用：
//!
//! - DFO.exe 用 DirectInput 读键，`SendInput` 必须发**扫描码**（`KEYEVENTF_SCANCODE`），
//!   发虚拟键码在游戏里完全无效。
//! - 合成按键同样会写进 DirectInput 的键位数组，所以监听真实按键必须过滤注入事件
//!   （`LLKHF_INJECTED`），否则自己按的键把自己再次触发，松开后停不下来。
//! - 只有目标进程在前台时才发送，免得把键打进聊天框或别的窗口。
//!
//! 本模块不注入客户端、不装驱动，只做系统级模拟输入。

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, SyncSender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::config;
use crate::paths::Root;

pub const DEFAULT_HOTKEY: &str = "F10";
pub const DEFAULT_TARGET: &str = "dfo.exe";

/// 松开后还要继续连发的宽限（毫秒）：手指抖动 150ms 内算「还按着」。
const HOLD_GRACE: Duration = Duration::from_millis(150);
/// 发送节拍。间隔本身由每条配置的 intervalMs 决定，这里只是轮询粒度。
const TICK: Duration = Duration::from_millis(2);
/// 前台进程判定的刷新周期。
const SCAN_INTERVAL: Duration = Duration::from_millis(500);
/// 同一类诊断话的最短间隔：旧版引擎也是 3 秒节流，刷屏比没反馈更糟。
const DIAG_GAP: Duration = Duration::from_secs(3);
const HOTKEY_ID: i32 = 1;
/// 配置改了就请监听线程重挂总开关热键（RegisterHotKey 必须在建它的那个线程上做）。
const WM_REREG_HOTKEY: u32 = 0x8001;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Step {
    pub key: String,
    pub hold_ms: u32,
    pub gap_ms: u32,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Entry {
    /// `rapid` 按住连发 / `combo` 一键连招。
    #[serde(rename = "type")]
    pub kind: String,
    pub key: String,
    pub interval_ms: u32,
    pub hold_ms: u32,
    pub trigger: String,
    pub steps: Vec<Step>,
    pub enabled: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Config {
    pub enabled: bool,
    pub mute: bool,
    pub hotkey: String,
    pub targets: Vec<String>,
    pub entries: Vec<Entry>,
}

#[derive(Clone, Debug, Serialize)]
pub struct KeyOption {
    pub label: String,
    pub value: String,
}

// ---------------------------------------------------------------- 键名

const KEY_NAMES: [(&str, u16); 42] = [
    ("BACK", 0x08),
    ("TAB", 0x09),
    ("RETURN", 0x0D),
    ("ENTER", 0x0D),
    ("SHIFT", 0x10),
    ("CTRL", 0x11),
    ("CONTROL", 0x11),
    ("ALT", 0x12),
    ("PAUSE", 0x13),
    ("CAPITAL", 0x14),
    ("ESCAPE", 0x1B),
    ("ESC", 0x1B),
    ("SPACE", 0x20),
    ("PGUP", 0x21),
    ("PAGEUP", 0x21),
    ("PGDN", 0x22),
    ("PAGEDOWN", 0x22),
    ("END", 0x23),
    ("HOME", 0x24),
    ("LEFT", 0x25),
    ("UP", 0x26),
    ("RIGHT", 0x27),
    ("DOWN", 0x28),
    ("SNAPSHOT", 0x2C),
    ("PRINTSCREEN", 0x2C),
    ("INSERT", 0x2D),
    ("DELETE", 0x2E),
    ("SCROLL", 0x91),
    ("SCROLLLOCK", 0x91),
    ("OEM1", 0xBA),
    ("OEM2", 0xBF),
    ("OEM3", 0xC0),
    ("OEM4", 0xDB),
    ("OEM5", 0xDC),
    ("OEM6", 0xDD),
    ("OEM7", 0xDE),
    ("OEM8", 0xDF),
    ("MULTIPLY", 0x6A),
    ("ADD", 0x6B),
    ("SUBTRACT", 0x6D),
    ("DECIMAL", 0x6E),
    ("DIVIDE", 0x6F),
];

/// 认 A-Z、0-9、F1-F24、NUM0-9、0xNN、VK_ 前缀和上面这张表。
/// 刻意不含 Win 键：它不该被连发，也给不出名字。
pub fn parse_key(raw: &str) -> Result<u16, String> {
    let text = raw.trim().to_uppercase();
    let text = text.strip_prefix("VK_").unwrap_or(&text);
    if text.is_empty() {
        return Err("按键名不能是空的".to_string());
    }
    let bytes: Vec<u8> = text.as_bytes().to_vec();
    if bytes.len() == 1 {
        let c = bytes[0];
        if (b'A'..=b'Z').contains(&c) || (b'0'..=b'9').contains(&c) {
            return Ok(u16::from(c));
        }
    }
    if let Some(hex) = text.strip_prefix("0X") {
        return u16::from_str_radix(hex, 16)
            .map_err(|_| format!("十六进制按键名「{raw}」读不出来"));
    }
    if let Some(rest) = text.strip_prefix('F') {
        if let Ok(n) = rest.parse::<u32>() {
            if (1..=24).contains(&n) {
                return Ok(0x70 + (n - 1) as u16);
            }
        }
    }
    if let Some(rest) = text.strip_prefix("NUM") {
        if let Ok(n) = rest.parse::<u32>() {
            if (0..=9).contains(&n) {
                return Ok(0x60 + n as u16);
            }
        }
    }
    KEY_NAMES
        .iter()
        .find(|(name, _)| *name == text)
        .map(|(_, vk)| *vk)
        .ok_or_else(|| {
            format!("不认识的按键「{raw}」（可用 A-Z、0-9、F1-F24、SPACE、NUM0 或 0x58）")
        })
}

pub fn key_name(vk: u16) -> String {
    if (0x70..=0x87).contains(&vk) {
        return format!("F{}", 1 + vk - 0x70);
    }
    if (0x60..=0x69).contains(&vk) {
        return format!("NUM{}", vk - 0x60);
    }
    if (('A' as u16)..=('Z' as u16)).contains(&vk) || (('0' as u16)..=('9' as u16)).contains(&vk) {
        return char::from(vk as u8).to_string();
    }
    KEY_NAMES
        .iter()
        .find(|(_, got)| *got == vk)
        .map(|(name, _)| (*name).to_string())
        .unwrap_or_else(|| format!("0x{vk:02X}"))
}

/// 界面下拉框的候选，顺序固定。
pub fn key_options() -> Vec<KeyOption> {
    let mut out: Vec<KeyOption> = Vec::new();
    for c in b'A'..=b'Z' {
        let name = char::from(c).to_string();
        out.push(KeyOption { label: name.clone(), value: name });
    }
    for c in b'0'..=b'9' {
        let name = char::from(c).to_string();
        out.push(KeyOption { label: name.clone(), value: name });
    }
    for i in 1..=12 {
        out.push(KeyOption { label: format!("F{i}"), value: format!("F{i}") });
    }
    for i in 0..=9 {
        out.push(KeyOption { label: format!("小键盘{i}"), value: format!("NUM{i}") });
    }
    for (value, label) in [("LEFT", "←"), ("UP", "↑"), ("RIGHT", "→"), ("DOWN", "↓")] {
        out.push(KeyOption { label: label.to_string(), value: value.to_string() });
    }
    for name in ["SPACE", "TAB", "PAUSE", "SCROLL", "SNAPSHOT", "OEM3"] {
        out.push(KeyOption { label: name.to_string(), value: name.to_string() });
    }
    out
}

// ---------------------------------------------------------------- 归一化与校验

fn clamp(value: u32, low: u32, high: u32, fallback: u32) -> u32 {
    if value < low {
        fallback
    } else {
        value.min(high)
    }
}

/// 越界值修正是静默的（旧版一致）：间隔小到会淹没游戏输入，所以低于 10ms 一律回默认值。
pub fn normalize(cfg: &mut Config) {
    if cfg.hotkey.trim().is_empty() {
        cfg.hotkey = DEFAULT_HOTKEY.to_string();
    } else {
        cfg.hotkey = cfg.hotkey.trim().to_uppercase();
    }
    if cfg.targets.is_empty() {
        cfg.targets = vec![DEFAULT_TARGET.to_string()];
    }
    cfg.targets.iter_mut().for_each(|t| {
        *t = t.trim().to_lowercase();
    });
    cfg.targets.retain(|t| !t.is_empty());
    if cfg.targets.is_empty() {
        cfg.targets = vec![DEFAULT_TARGET.to_string()];
    }
    for entry in cfg.entries.iter_mut() {
        if entry.kind != "combo" {
            entry.kind = "rapid".to_string();
        }
        entry.interval_ms = clamp(entry.interval_ms, 10, 1000, 30);
        entry.hold_ms = clamp(entry.hold_ms, 1, entry.interval_ms, 15);
        for step in entry.steps.iter_mut() {
            step.hold_ms = clamp(step.hold_ms, 1, 500, 30);
            step.gap_ms = step.gap_ms.min(1000);
        }
    }
}

/// 只校验启用了的条目（旧版口径：停用那条不拦保存）。
pub fn validate(cfg: &Config) -> Result<(), String> {
    let hotkey = parse_key(&cfg.hotkey).map_err(|err| format!("总开关热键：{err}"))?;
    let mut rapid_keys: Vec<(u16, usize)> = Vec::new();
    for (index, entry) in cfg.entries.iter().enumerate() {
        if !entry.enabled {
            continue;
        }
        let label = format!("第 {} 条", index + 1);
        if entry.kind == "combo" {
            let trigger =
                parse_key(&entry.trigger).map_err(|err| format!("{label} 触发键：{err}"))?;
            if entry.steps.is_empty() {
                return Err(format!("{label} 一键连招里一个动作都没有"));
            }
            for (step_index, step) in entry.steps.iter().enumerate() {
                parse_key(&step.key)
                    .map_err(|err| format!("{label} 第 {} 步：{err}", step_index + 1))?;
            }
            if trigger == hotkey {
                return Err(format!("{label} 的触发键和总开关热键撞了，按下去只会开关整个功能"));
            }
            for (seen, seen_index) in &rapid_keys {
                if *seen == trigger {
                    return Err(format!(
                        "{label} 的触发键与第 {} 条的连发键相同，按住连发会顺手触发这套连招",
                        seen_index + 1
                    ));
                }
            }
        } else {
            let key = parse_key(&entry.key).map_err(|err| format!("{label}：{err}"))?;
            if key == hotkey {
                return Err(format!("{label} 的连发键就是总开关热键，两个动作没法分开"));
            }
            rapid_keys.push((key, index));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------- 盘上配置

pub fn load(root: &Root) -> Config {
    let value = config::read_json(&root.settings_json());
    let mut cfg: Config = value
        .get("autofire")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();
    if cfg.hotkey.trim().is_empty() {
        cfg.hotkey = DEFAULT_HOTKEY.to_string();
    }
    if cfg.targets.is_empty() {
        cfg.targets = vec![DEFAULT_TARGET.to_string()];
    }
    normalize(&mut cfg);
    cfg
}

/// 写 `autofire` 小节：已知的键覆盖，别人写在这节里的其它键原样留着。
/// `autoPickup` 是旧启动器读的那一枚，这里跟着 `game.autoPickup` 同步一份，
/// 免得两个启动器在同一台机器上各拿一份答案。
pub fn save(root: &Root, cfg: &Config) -> Result<(), String> {
    let pickup = config::settings(root).auto_pickup;
    let built = serde_json::to_value(cfg).map_err(|err| err.to_string())?;
    config::set_section(root, "autofire", |section| {
        if let serde_json::Value::Object(inner) = built {
            for (key, value) in inner {
                section.insert(key, value);
            }
        }
        section.insert(
            "autoPickup".to_string(),
            serde_json::Value::Bool(pickup),
        );
    })
}

// ---------------------------------------------------------------- 引擎

#[derive(Clone, Copy, Debug)]
enum Phase {
    Idle,
    Down(Instant),
}

#[derive(Clone, Debug)]
struct Rapid {
    label: usize,
    vk: u16,
    interval: Duration,
    hold: Duration,
    held: bool,
    release_at: Option<Instant>,
    phase: Phase,
    cycle_at: Option<Instant>,
    taps: u64,
}

#[derive(Clone, Debug)]
struct Combo {
    label: usize,
    trigger: u16,
    steps: Vec<(u16, Duration, Duration)>,
    busy: bool,
    taps: u64,
}

#[derive(Debug)]
struct Live {
    cfg: Config,
    rapids: Vec<Rapid>,
    combos: Vec<Combo>,
    master_vk: u16,
    master: bool,
    running: bool,
    mute: bool,
    focused: bool,
    pids: HashSet<u32>,
    scanned: Instant,
    listener_tid: u32,
    hotkey_error: String,
    combo_tx: Option<SyncSender<usize>>,
    // 下面这几组是给「为什么没反应」用的：闸门有六道，光看界面分不出卡在哪一道。
    hook_events: u64,
    sends: u64,
    send_fails: u64,
    last_fail: String,
    skip_focus: u64,
    skip_master: u64,
    diag: Vec<String>,
    diag_at: Option<Instant>,
}

impl Default for Live {
    fn default() -> Self {
        Live {
            cfg: Config::default(),
            rapids: Vec::new(),
            combos: Vec::new(),
            master_vk: 0,
            master: false,
            running: false,
            mute: false,
            focused: false,
            pids: HashSet::new(),
            scanned: Instant::now(),
            listener_tid: 0,
            hotkey_error: String::new(),
            combo_tx: None,
            hook_events: 0,
            sends: 0,
            send_fails: 0,
            last_fail: String::new(),
            skip_focus: 0,
            skip_master: 0,
            diag: Vec::new(),
            diag_at: None,
        }
    }
}

fn lock(live: &Arc<Mutex<Live>>) -> MutexGuard<'_, Live> {
    // 只有中毒（别的线程 panic 时正持锁）才会走到这里；引擎里没有任何 panic 路径。
    live.lock().unwrap_or_else(|err| err.into_inner())
}

/// 记一句诊断。同一类话 3 秒内只留一句，最多攒 8 句：连发本身是按秒计的事，
/// 全部转成日志会把运行日志冲烂，反倒看不出第一条卡在哪。
fn note_diag(guard: &mut Live, text: String) {
    let now = Instant::now();
    if let Some(at) = guard.diag_at {
        if now.duration_since(at) < DIAG_GAP {
            return;
        }
    }
    guard.diag_at = Some(now);
    if guard.diag.len() >= 8 {
        guard.diag.remove(0);
    }
    guard.diag.push(text);
}

#[derive(Clone, Debug, Serialize)]
pub struct EntryLive {
    pub label: usize,
    pub kind: String,
    pub held: bool,
    pub busy: bool,
    pub taps: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct State {
    pub running: bool,
    pub master: bool,
    pub focused: bool,
    pub hotkey: String,
    pub hotkey_error: String,
    pub targets: Vec<String>,
    pub pids: Vec<u32>,
    pub entries: EntryLiveList,
    pub hook_events: u64,
    pub sends: u64,
    pub send_fails: u64,
    pub last_fail: String,
    pub skip_focus: u64,
    pub skip_master: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct EntryLiveList(pub Vec<EntryLive>);

#[derive(Default)]
pub struct Engine {
    live: Arc<Mutex<Live>>,
    keep_up: Arc<AtomicBool>,
    handles: Mutex<Vec<JoinHandle<()>>>,
}

impl Engine {
    pub fn new() -> Self {
        Engine::default()
    }

    /// 与旧版一致的语义：`enabled=true` 才真的挂监听；已经开着再改配置，只换内容不动热键开关。
    pub fn apply(&self, root: &Root, mut cfg: Config) -> Result<String, String> {
        validate(&cfg).map_err(|err| err.to_string())?;
        normalize(&mut cfg);
        save(root, &cfg)?;
        if cfg.enabled {
            self.start(cfg)
        } else {
            self.stop();
            Ok("连发已停用，监听已摘掉".to_string())
        }
    }

    pub fn start(&self, mut cfg: Config) -> Result<String, String> {
        normalize(&mut cfg);
        validate(&cfg)?;
        let master_vk = parse_key(&cfg.hotkey)?;

        {
            let mut guard = lock(&self.live);
            if guard.running {
                // 已经在跑：换上内容，热键开关保持玩家当下按出来的状态（旧版同形）。
                // 这里绝不能换新通道——播放器线程手上握着旧的接收端，换了等于把连招的线拔了。
                fill(&mut guard, &cfg, master_vk, None);
                drop(guard);
                self.reregister_hotkey();
                return Ok("连发配置已更新，正在运行的那条热键开关没动".to_string());
            }
        }

        let (tx, rx) = mpsc::sync_channel::<usize>(16);
        {
            let mut guard = lock(&self.live);
            fill(&mut guard, &cfg, master_vk, Some(tx));
            guard.running = true;
            guard.master = true;
            guard.hotkey_error = String::new();
            // 计数从这一场重新起算：界面上的数字要能对上「这一次到底收没收到键」。
            guard.hook_events = 0;
            guard.sends = 0;
            guard.send_fails = 0;
            guard.last_fail = String::new();
            guard.skip_focus = 0;
            guard.skip_master = 0;
            guard.diag.clear();
            guard.diag_at = None;
        }
        self.keep_up.store(true, Ordering::SeqCst);

        let live = Arc::clone(&self.live);
        let keep_up = Arc::clone(&self.keep_up);
        let mut handles = lock_handles(&self.handles);
        handles.push(spawn("af-listener", live.clone(), keep_up.clone(), move |live, keep_up| {
            listener_loop(live, keep_up)
        }));
        handles.push(spawn("af-sender", live.clone(), keep_up.clone(), move |live, keep_up| {
            sender_loop(live, keep_up)
        }));
        handles.push(spawn("af-player", live, keep_up, move |live, keep_up| {
            player_loop(live, keep_up, rx)
        }));
        drop(handles);

        // 等监听线程把自己的线程号报上来：stop() 要靠它投 WM_QUIT 才收得干净。
        for _ in 0..100 {
            if lock(&self.live).listener_tid != 0 {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let guard = lock(&self.live);
        if guard.listener_tid == 0 {
            return Err("连发没能起监听线程，请重启启动器再试".to_string());
        }
        let note = if guard.hotkey_error.is_empty() {
            String::new()
        } else {
            format!("；{}", guard.hotkey_error)
        };
        Ok(format!(
            "连发已启用：热键 {} 开关，共 {} 条{}",
            guard.cfg.hotkey,
            guard.cfg.entries.len(),
            note
        ))
    }

    pub fn stop(&self) {
        self.keep_up.store(false, Ordering::SeqCst);
        let tid = {
            let mut guard = lock(&self.live);
            guard.running = false;
            guard.master = false;
            guard.combo_tx = None;
            guard.pids.clear();
            guard.focused = false;
            // 清零：下一次 start() 靠「tid 又从 0 变成非 0」判断新监听线程已就位。
            let tid = guard.listener_tid;
            guard.listener_tid = 0;
            tid
        };
        #[cfg(windows)]
        if tid != 0 {
            use windows::Win32::Foundation::{LPARAM, WPARAM};
            use windows::Win32::UI::WindowsAndMessaging::{PostThreadMessageW, WM_QUIT};
            unsafe {
                let _ = PostThreadMessageW(tid, WM_QUIT, WPARAM(0), LPARAM(0));
            }
        }
        // 收线程：监听线程退出前会自己摘钩子与热键。
        let mut handles = lock_handles(&self.handles);
        let taken = std::mem::take(&mut *handles);
        drop(handles);
        for handle in taken {
            for _ in 0..50 {
                if handle.is_finished() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }

    pub fn state(&self) -> State {
        let guard = lock(&self.live);
        let mut entries: Vec<EntryLive> = Vec::new();
        for rapid in &guard.rapids {
            entries.push(EntryLive {
                label: rapid.label,
                kind: "rapid".to_string(),
                held: rapid.held,
                busy: false,
                taps: rapid.taps,
            });
        }
        for combo in &guard.combos {
            entries.push(EntryLive {
                label: combo.label,
                kind: "combo".to_string(),
                held: false,
                busy: combo.busy,
                taps: combo.taps,
            });
        }
        entries.sort_by_key(|item| item.label);
        State {
            running: guard.running,
            master: guard.master,
            focused: guard.focused,
            hotkey: guard.cfg.hotkey.clone(),
            hotkey_error: guard.hotkey_error.clone(),
            targets: guard.cfg.targets.clone(),
            pids: guard.pids.iter().copied().collect(),
            entries: EntryLiveList(entries),
            hook_events: guard.hook_events,
            sends: guard.sends,
            send_fails: guard.send_fails,
            last_fail: guard.last_fail.clone(),
            skip_focus: guard.skip_focus,
            skip_master: guard.skip_master,
        }
    }

    /// 取走攒下的诊断话，交给运行日志。每 3 秒最多一条，界面刷屏比没反馈更糟。
    pub fn take_diag(&self) -> Vec<String> {
        std::mem::take(&mut lock(&self.live).diag)
    }

    fn reregister_hotkey(&self) {
        let tid = lock(&self.live).listener_tid;
        #[cfg(windows)]
        if tid != 0 {
            use windows::Win32::Foundation::{LPARAM, WPARAM};
            use windows::Win32::UI::WindowsAndMessaging::PostThreadMessageW;
            unsafe {
                let _ = PostThreadMessageW(tid, WM_REREG_HOTKEY, WPARAM(0), LPARAM(0));
            }
        }
    }
}

fn lock_handles(mutex: &Mutex<Vec<JoinHandle<()>>>) -> MutexGuard<'_, Vec<JoinHandle<()>>> {
    mutex.lock().unwrap_or_else(|err| err.into_inner())
}

/// 把配置摊成两张发送计划表。解析不出来的键直接跳过那条，不让一个坏键拖垮整份配置。
/// `combo_tx` 只在给定时才替换：引擎已经在跑时传 `None`，保留播放器线程正在听的那一端。
fn fill(guard: &mut Live, cfg: &Config, master_vk: u16, combo_tx: Option<SyncSender<usize>>) {
    guard.cfg = cfg.clone();
    guard.master_vk = master_vk;
    guard.mute = cfg.mute;
    if let Some(tx) = combo_tx {
        guard.combo_tx = Some(tx);
    }
    guard.rapids.clear();
    guard.combos.clear();
    for (index, entry) in cfg.entries.iter().enumerate() {
        if !entry.enabled {
            continue;
        }
        if entry.kind == "combo" {
            let Ok(trigger) = parse_key(&entry.trigger) else { continue };
            let mut steps = Vec::new();
            for step in &entry.steps {
                let Ok(vk) = parse_key(&step.key) else { continue };
                steps.push((
                    vk,
                    Duration::from_millis(u64::from(step.hold_ms)),
                    Duration::from_millis(u64::from(step.gap_ms)),
                ));
            }
            if steps.is_empty() {
                continue;
            }
            guard.combos.push(Combo {
                label: index,
                trigger,
                steps,
                busy: false,
                taps: 0,
            });
        } else {
            let Ok(vk) = parse_key(&entry.key) else { continue };
            guard.rapids.push(Rapid {
                label: index,
                vk,
                interval: Duration::from_millis(u64::from(entry.interval_ms)),
                hold: Duration::from_millis(u64::from(entry.hold_ms)),
                held: false,
                release_at: None,
                phase: Phase::Idle,
                cycle_at: None,
                taps: 0,
            });
        }
    }
}

#[cfg(windows)]
thread_local! {
    /// 低级钩子的回调一定跑在装它的那个线程上，所以回调靠线程局部量找到自己的状态。
    static HOOK_TARGET: std::cell::RefCell<Option<Arc<Mutex<Live>>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(windows)]
fn spawn(
    name: &str,
    live: Arc<Mutex<Live>>,
    keep_up: Arc<AtomicBool>,
    body: impl FnOnce(Arc<Mutex<Live>>, Arc<AtomicBool>) + Send + 'static,
) -> JoinHandle<()> {
    std::thread::Builder::new()
        .name(name.to_string())
        .spawn(move || body(live, keep_up))
        // 建不起来只有系统线程耗尽这一种可能，此时没有可恢复的余地。
        .expect("连发线程创建失败")
}

#[cfg(not(windows))]
fn spawn(
    _name: &str,
    live: Arc<Mutex<Live>>,
    keep_up: Arc<AtomicBool>,
    _body: impl FnOnce(Arc<Mutex<Live>>, Arc<AtomicBool>) + Send + 'static,
) -> JoinHandle<()> {
    // 非 Windows：没有钩子可挂，起一个立刻退出的占位线程让状态自洽。
    let _ = (live.clone(), keep_up.clone());
    std::thread::spawn(|| {
        let _ = (live, keep_up);
    })
}

#[cfg(windows)]
fn listener_loop(live: Arc<Mutex<Live>>, _keep_up: Arc<AtomicBool>) {
    use windows::Win32::System::Threading::GetCurrentThreadId;
    use windows::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, GetMessageW, SetWindowsHookExW, TranslateMessage, UnhookWindowsHookEx,
        MSG, WH_KEYBOARD_LL,
    };

    HOOK_TARGET.with(|cell| *cell.borrow_mut() = Some(Arc::clone(&live)));
    let tid = unsafe { GetCurrentThreadId() };
    lock(&live).listener_tid = tid;

    let hook = unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook_proc), None, 0) };
    let hook = match hook {
        Ok(hook) => hook,
        Err(err) => {
            let mut guard = lock(&live);
            guard.running = false;
            guard.hotkey_error = format!("挂键盘监听失败：{err}");
            HOOK_TARGET.with(|cell| *cell.borrow_mut() = None);
            return;
        }
    };
    register_master(&live);

    let mut msg = MSG::default();
    loop {
        let got = unsafe { GetMessageW(&mut msg, None, 0, 0) };
        if got.0 <= 0 {
            break;
        }
        match msg.message {
            windows::Win32::UI::WindowsAndMessaging::WM_HOTKEY => {
                if msg.wParam.0 as i32 == HOTKEY_ID {
                    toggle_master(&live);
                }
            }
            WM_REREG_HOTKEY => register_master(&live),
            _ => {
                unsafe {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
        }
    }

    unsafe {
        let _ = UnhookWindowsHookEx(hook);
        windows::Win32::UI::Input::KeyboardAndMouse::UnregisterHotKey(None, HOTKEY_ID).ok();
    }
    HOOK_TARGET.with(|cell| *cell.borrow_mut() = None);
}

#[cfg(windows)]
fn register_master(live: &Arc<Mutex<Live>>) {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS,
    };

    let vk = lock(live).master_vk;
    if vk == 0 {
        return;
    }
    unsafe {
        // 先摘再挂：同一个 id 重复注册会失败，改完热键必须换绑。
        let _ = UnregisterHotKey(None, HOTKEY_ID);
        let done = RegisterHotKey(None, HOTKEY_ID, HOT_KEY_MODIFIERS(0), u32::from(vk));
        let mut guard = lock(live);
        guard.hotkey_error = match done {
            Ok(()) => String::new(),
            Err(err) => format!(
                "总开关热键 {} 被别人占了（多半是游戏自己绑了它），换个键试试：{err}",
                key_name(vk)
            ),
        };
    }
}

/// 总开关热键走 RegisterHotKey 而不是钩子：这个键要被启动器吃掉，不该再传到游戏里。
#[cfg(windows)]
fn toggle_master(live: &Arc<Mutex<Live>>) {
    let (next, mute) = {
        let mut guard = lock(live);
        guard.master = !guard.master;
        // 这条不经节流：玩家刚按的那一下必须有个准信，好把「热键没绑上」和
        // 「热键正好把它关了」这两种情况分清楚。
        let line = format!(
            "连发总开关{}（再按一下 {} 切回来）",
            if guard.master { "已恢复" } else { "已暂停" },
            guard.cfg.hotkey
        );
        guard.diag.push(line);
        if guard.diag.len() > 8 {
            guard.diag.remove(0);
        }
        (guard.master, guard.mute)
    };
    if !mute {
        // Beep 会占住调用线程 120ms，绝不能压在监听线程上（钩子回调也在它上面跑）。
        std::thread::spawn(move || {
            use windows::Win32::System::Diagnostics::Debug::Beep;
            unsafe {
                let _ = Beep(if next { 880 } else { 587 }, 120);
            }
        });
    }
}

#[cfg(windows)]
unsafe extern "system" fn hook_proc(
    code: i32,
    wparam: windows::Win32::Foundation::WPARAM,
    lparam: windows::Win32::Foundation::LPARAM,
) -> windows::Win32::Foundation::LRESULT {
    use windows::Win32::UI::WindowsAndMessaging::{
        CallNextHookEx, KBDLLHOOKSTRUCT, LLKHF_INJECTED, WM_KEYDOWN, WM_KEYUP, WM_SYSKEYDOWN,
        WM_SYSKEYUP,
    };

    if code >= 0 {
        let target = HOOK_TARGET.with(|cell| cell.borrow().clone());
        if let Some(live) = target {
            let info = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
            let vk = info.vkCode as u16;
            let msg = wparam.0 as u32;
            // 注入事件一律不看：否则自己发的键会把自己再次触发，松开也停不下来。
            if (info.flags & LLKHF_INJECTED).0 == 0 {
                let down = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
                let up = msg == WM_KEYUP || msg == WM_SYSKEYUP;
                if down || up {
                    note_key(&live, vk, down);
                }
            }
        }
    }
    // 永远放行：这是按住连发，不是屏蔽按键，真实那一下照样要进游戏。
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

fn note_key(live: &Arc<Mutex<Live>>, vk: u16, down: bool) {
    let mut guard = lock(live);
    if !guard.running {
        return;
    }
    // 物理键事件先计数：这一条是 0，就说明监听压根没收到按键，后面三道闸门都无从谈起。
    guard.hook_events += 1;
    let now = Instant::now();
    if down {
        // 总开关关着也要记住「键正按着」：否则玩家先按住键再按热键打开，
        // 打开后等不到按下边沿，这一整轮就是死的。
        for index in 0..guard.rapids.len() {
            if guard.rapids[index].vk != vk {
                continue;
            }
            let was = guard.rapids[index].held;
            guard.rapids[index].held = true;
            guard.rapids[index].release_at = None;
            if !was && guard.master {
                let name = key_name(vk);
                note_diag(&mut guard, format!("连发开始：{name}"));
            }
        }
        if guard.master {
            for index in 0..guard.combos.len() {
                if guard.combos[index].trigger != vk || guard.combos[index].busy {
                    continue;
                }
                if let Some(tx) = guard.combo_tx.clone() {
                    if tx.try_send(index).is_ok() {
                        guard.combos[index].busy = true;
                    }
                }
            }
        }
    } else {
        for rapid in guard.rapids.iter_mut() {
            if rapid.vk == vk && rapid.held {
                rapid.release_at = Some(now + HOLD_GRACE);
            }
        }
    }
}

#[cfg(windows)]
fn sender_loop(live: Arc<Mutex<Live>>, keep_up: Arc<AtomicBool>) {
    use windows::Win32::Media::{timeBeginPeriod, timeEndPeriod};

    unsafe {
        timeBeginPeriod(1);
    }
    while keep_up.load(Ordering::SeqCst) {
        tick(&live);
        std::thread::sleep(TICK);
    }
    unsafe {
        timeEndPeriod(1);
    }
}

#[cfg(not(windows))]
fn sender_loop(live: Arc<Mutex<Live>>, keep_up: Arc<AtomicBool>) {
    while keep_up.load(Ordering::SeqCst) {
        tick(&live);
        std::thread::sleep(TICK);
    }
}

/// 2ms 一次的节拍：松手判定 → 抬起收尾 → 该按的下一下。全程不睡、不等待，锁只在毫秒级持有。
fn tick(live: &Arc<Mutex<Live>>) {
    let now = Instant::now();
    let mut guard = lock(live);
    if !guard.running {
        return;
    }
    if now.duration_since(guard.scanned) >= SCAN_INTERVAL {
        refresh_focus(&mut guard);
    }
    let active = guard.master && guard.focused;

    // 按着键却一次都没出手：把到底是哪一道闸挡住的写进诊断，否则玩家只能瞎猜。
    if !active && guard.rapids.iter().any(|rapid| rapid.held) {
        if !guard.master {
            guard.skip_master += 1;
            let hotkey = guard.cfg.hotkey.clone();
            note_diag(&mut guard, format!("总开关关着，连发没发送：按一下 {hotkey} 恢复"));
        } else {
            guard.skip_focus += 1;
            let targets = guard.cfg.targets.join("、");
            note_diag(&mut guard, format!("游戏不在前台，连发没发送：找的是 {targets}"));
        }
    }

    let mut sent = 0_u64;
    let mut fail: Option<String> = None;
    for index in 0..guard.rapids.len() {
        if let Some(at) = guard.rapids[index].release_at {
            if now >= at {
                guard.rapids[index].held = false;
                guard.rapids[index].release_at = None;
            }
        }
        let vk = guard.rapids[index].vk;
        match guard.rapids[index].phase {
            Phase::Down(since) => {
                // 一下「点」的时长就是 holdMs：到点必须抬起，否则游戏收到的是一直按住，
                // 连发变成压键，间隔配得再小也没用。
                let tapped = now.duration_since(since) >= guard.rapids[index].hold;
                if !guard.rapids[index].held || !active || tapped {
                    match release_key(vk) {
                        Ok(()) => {
                            guard.rapids[index].phase = Phase::Idle;
                            sent += 1;
                        }
                        // 抬起都没送出去，说明这一下根本没进系统：收摊等下一次按下边沿。
                        Err(msg) => {
                            guard.rapids[index].held = false;
                            guard.rapids[index].phase = Phase::Idle;
                            fail = Some(msg);
                        }
                    }
                }
            }
            Phase::Idle if !guard.rapids[index].held || !active => {
                guard.rapids[index].cycle_at = None;
            }
            Phase::Idle => {
                let due = guard.rapids[index]
                    .cycle_at
                    .map(|at| now.duration_since(at) >= guard.rapids[index].interval)
                    .unwrap_or(true);
                if due {
                    match press_key(vk) {
                        Ok(()) => {
                            guard.rapids[index].phase = Phase::Down(now);
                            guard.rapids[index].cycle_at = Some(now);
                            guard.rapids[index].taps += 1;
                            sent += 1;
                        }
                        // 发不出去（通常是权限）就别再原地反复试，等这一轮松手。
                        Err(msg) => {
                            guard.rapids[index].held = false;
                            fail = Some(msg);
                        }
                    }
                }
            }
        }
    }
    guard.sends += sent;
    if let Some(msg) = fail {
        guard.send_fails += 1;
        guard.last_fail = msg.clone();
        note_diag(&mut guard, msg);
    }
}

#[cfg(windows)]
fn refresh_focus(guard: &mut Live) {
    use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};

    guard.scanned = Instant::now();
    let pids: HashSet<u32> = crate::procutil::snapshot()
        .into_iter()
        .filter(|entry| guard.cfg.targets.iter().any(|t| t == &entry.name))
        .map(|entry| entry.pid)
        .collect();
    guard.pids = pids;
    let mut foreground = 0_u32;
    let hwnd = unsafe { GetForegroundWindow() };
    unsafe {
        GetWindowThreadProcessId(hwnd, Some(&mut foreground));
    }
    guard.focused = foreground != 0 && guard.pids.contains(&foreground);
}

#[cfg(not(windows))]
fn refresh_focus(guard: &mut Live) {
    guard.scanned = Instant::now();
    guard.pids.clear();
    guard.focused = false;
}

#[cfg(windows)]
fn send_scan(vk: u16, up: bool) -> Result<(), String> {
    use windows::Win32::Foundation::GetLastError;
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT_0, INPUT, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY,
        KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE, MAPVK_VK_TO_VSC, MapVirtualKeyW, VIRTUAL_KEY,
    };

    let mapped = unsafe { MapVirtualKeyW(u32::from(vk), MAPVK_VK_TO_VSC) } as u16 & 0xFF;
    if mapped == 0 {
        return Err(format!("{} 映射不出扫描码，换一个键试试", key_name(vk)));
    }
    let mut flags = KEYEVENTF_SCANCODE;
    // 方向键 / 编辑键 / 小键盘除号这些是扩展键：不带 E0 前缀，游戏收到的是别的字符。
    if is_extended(vk) {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }
    if up {
        flags |= KEYEVENTF_KEYUP;
    }
    let input = INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                // 刻意留 0：游戏按扫描码认键，带上虚拟键码反而会让某些键走两条路。
                wVk: VIRTUAL_KEY(0),
                wScan: mapped,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    let sent = unsafe { SendInput(&[input], std::mem::size_of::<INPUT>() as i32) };
    if sent == 1 {
        return Ok(());
    }
    // 错误码要在这里马上取：中间再调任何 API 都会把它冲掉。
    let code = unsafe { GetLastError() }.0;
    Err(format!(
        "SendInput 只送达 {sent} 条（Win32 错误 {code}）：通常是对局以更高权限运行，请以管理员身份重启启动器"
    ))
}

#[cfg(not(windows))]
fn send_scan(_vk: u16, _up: bool) -> Result<(), String> {
    Err("这套平台没有键盘注入可用".to_string())
}

/// 哪些虚拟键码带 E0 前缀。不带这个前缀，方向键注入进去会变成主键盘的其它字符
/// （VK_RIGHT → 扫描码 0x4D → 游戏里是「M」），所以一键搓招里用到方向键时必须带。
/// 旧版 Go 引擎从来没设过这一位，这里是刻意补的正确性；SCROLLLOCK 不在扩展集内。
fn is_extended(vk: u16) -> bool {
    matches!(
        vk,
        0x21..=0x28 | 0x2C | 0x2D | 0x2E | 0x6F | 0x90 | 0xA3 | 0xA5
    )
}

fn press_key(vk: u16) -> Result<(), String> {
    send_scan(vk, false)
}

fn release_key(vk: u16) -> Result<(), String> {
    send_scan(vk, true)
}

#[cfg(windows)]
fn player_loop(
    live: Arc<Mutex<Live>>,
    keep_up: Arc<AtomicBool>,
    rx: mpsc::Receiver<usize>,
) {
    while keep_up.load(Ordering::SeqCst) {
        let Ok(index) = rx.recv() else { break };
        play_combo(&live, index);
    }
}

#[cfg(not(windows))]
fn player_loop(live: Arc<Mutex<Live>>, keep_up: Arc<AtomicBool>, rx: mpsc::Receiver<usize>) {
    while keep_up.load(Ordering::SeqCst) {
        if let Ok(index) = rx.recv() {
            play_combo(&live, index);
        }
    }
}

/// 记一句诊断（自己取锁）：钩子、节拍、连招三条线都要能说话。
fn note_line(live: &Arc<Mutex<Live>>, msg: String) {
    note_diag(&mut lock(live), msg);
}

/// 注入失败必须落进诊断：这种话只有玩家看得见，不写就是无声失败。
fn note_fail(live: &Arc<Mutex<Live>>, msg: String) {
    let mut guard = lock(live);
    guard.send_fails += 1;
    guard.last_fail = msg.clone();
    note_diag(&mut guard, msg);
}

/// 一套连招单独跑一个线程：它要按节奏睡，绝不能压在 2ms 的节拍线程上。
fn play_combo(live: &Arc<Mutex<Live>>, index: usize) {
    let steps = {
        let guard = lock(live);
        match guard.combos.get(index) {
            Some(combo) => combo.steps.clone(),
            None => return,
        }
    };
    let mut played = 0_u64;
    for (vk, hold, gap) in steps {
        let gate = {
            let guard = lock(live);
            if !guard.running {
                Some("连发已经停了")
            } else if !guard.master {
                Some("总开关关着")
            } else if !guard.focused {
                Some("游戏不在前台")
            } else {
                None
            }
        };
        // 每一动之前重看闸门：中途关了或切了窗口就停在这儿，打一半比整套没打更容易送人头。
        if let Some(reason) = gate {
            note_line(live, format!("一键连招中断：{reason}"));
            break;
        }
        if let Err(msg) = press_key(vk) {
            note_fail(live, msg);
            break;
        }
        lock(live).sends += 1;
        std::thread::sleep(hold);
        if let Err(msg) = release_key(vk) {
            note_fail(live, msg);
            break;
        }
        lock(live).sends += 1;
        played += 1;
        if !gap.is_zero() {
            std::thread::sleep(gap);
        }
    }
    let mut guard = lock(live);
    if let Some(combo) = guard.combos.get_mut(index) {
        combo.busy = false;
        if played > 0 {
            combo.taps += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_same_names_as_the_old_launcher() {
        assert_eq!(parse_key("x").unwrap(), 0x58);
        assert_eq!(parse_key("5").unwrap(), 0x35);
        assert_eq!(parse_key("vk_f10").unwrap(), 0x79);
        assert_eq!(parse_key("NUM3").unwrap(), 0x63);
        assert_eq!(parse_key("0x58").unwrap(), 0x58);
        assert_eq!(parse_key("SPACE").unwrap(), 0x20);
        assert_eq!(parse_key("RIGHT").unwrap(), 0x27);
        assert!(parse_key("LWIN").is_err());
        assert!(parse_key("").is_err());
        assert_eq!(key_name(0x27), "RIGHT");
        assert_eq!(key_name(0x79), "F10");
        assert_eq!(key_name(0x63), "NUM3");
    }

    #[test]
    fn clamps_out_of_range_rhythm() {
        let mut cfg = Config {
            hotkey: "f10".to_string(),
            targets: vec![],
            entries: vec![Entry {
                kind: "rapid".to_string(),
                key: "X".to_string(),
                interval_ms: 1,
                hold_ms: 9999,
                enabled: true,
                ..Default::default()
            }],
            ..Default::default()
        };
        normalize(&mut cfg);
        assert_eq!(cfg.entries[0].interval_ms, 30);
        assert_eq!(cfg.entries[0].hold_ms, 30);
        assert_eq!(cfg.targets, vec![DEFAULT_TARGET.to_string()]);
        assert_eq!(cfg.hotkey, "F10");
    }

    #[test]
    fn rejects_colliding_hotkeys() {
        let mut cfg = Config {
            hotkey: "F10".to_string(),
            entries: vec![Entry {
                kind: "rapid".to_string(),
                key: "F10".to_string(),
                enabled: true,
                ..Default::default()
            }],
            ..Default::default()
        };
        assert!(validate(&cfg).is_err());
        // 停用那条不拦保存：玩家可能就是先存着再说。
        cfg.entries[0].enabled = false;
        assert!(validate(&cfg).is_ok());
    }

    #[test]
    fn rejects_combo_trigger_shared_with_rapid() {
        let cfg = Config {
            hotkey: "F10".to_string(),
            entries: vec![
                Entry {
                    kind: "rapid".to_string(),
                    key: "Z".to_string(),
                    enabled: true,
                    ..Default::default()
                },
                Entry {
                    kind: "combo".to_string(),
                    trigger: "Z".to_string(),
                    steps: vec![Step { key: "X".to_string(), hold_ms: 30, gap_ms: 40 }],
                    enabled: true,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        assert!(validate(&cfg).is_err());
    }

    #[test]
    fn arrows_are_extended_but_letters_are_not() {
        assert!(is_extended(0x27));
        assert!(is_extended(0x2E));
        assert!(!is_extended(0x58));
        assert!(!is_extended(0x63));
        // 大写锁定键不带 E0，误标会让它变成另一个键。
        assert!(!is_extended(0x91));
    }

    #[test]
    fn held_key_when_a_gate_is_closed_says_which_one() {
        let cases = [(false, true, "总开关"), (true, false, "游戏不在前台")];
        for (master, focused, want) in cases {
            let live = Arc::new(Mutex::new(Live::default()));
            {
                let mut guard = lock(&live);
                guard.running = true;
                guard.master = master;
                guard.focused = focused;
                guard.rapids.push(Rapid {
                    label: 1,
                    vk: 0x53,
                    interval: Duration::from_millis(60),
                    hold: Duration::from_millis(15),
                    held: true,
                    release_at: None,
                    phase: Phase::Idle,
                    cycle_at: None,
                    taps: 0,
                });
            }
            tick(&live);
            let guard = lock(&live);
            assert_eq!(guard.sends, 0, "闸门关着的时候不该冒充发过一下");
            assert!(
                guard.diag.iter().any(|line| line.contains(want)),
                "没把卡住的那一道闸说出来：{:?}",
                guard.diag
            );
        }
    }
}
