use tauri::{Manager, RunEvent, WindowEvent};

use crate::logbus::Bus;
use crate::session::Session;

mod autofire;
mod commands;
mod config;
mod health;
mod logbus;
mod mods;
mod panels;
mod paths;
mod portguard;
mod ports;
mod procutil;
mod report;
mod saves;
mod session;
mod site;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // 状态必须在建窗之前就挂上：tauri 是「按配置创建主窗 → 再跑 setup」，而命令走 async 时
    // `State` 是在后台运行时里解引用的，界面首帧那次 `config_get` 可能落在 `app.manage` 之前，
    // 玩家会看到「state not managed for field `state` on command `config_get`」。
    // 所以能纯算的部分全提到这里，`AppHandle` 相关的接线留在 setup 里补。
    let exe = std::env::current_exe().ok();
    let root = paths::resolve_root(exe.as_deref());
    let app_dir = paths::app_dir();
    let bus = Bus::new(&app_dir.join("logs"));
    let reporter = report::Reporter::new(&app_dir, root.clone(), bus.clone());
    let session = Session::new(root.clone(), bus.clone());
    session.attach_reporter(reporter.clone());
    let autofire = std::sync::Arc::new(autofire::Engine::new());

    let builder = tauri::Builder::default()
        .manage(commands::AppState {
            session: session.clone(),
            reporter: reporter.clone(),
            autofire: autofire.clone(),
            app_dir: app_dir.clone(),
            root: root.clone(),
        })
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init());

    #[cfg(windows)]
    let builder = builder.plugin(tauri_plugin_single_instance::init(|_app, _args, _cwd| {}));

    builder
        .setup(move |app| {
            bus.attach(app.handle().clone());
            bus.info(root.log_line());
            bus.info(format!(
                "{} {} 启动",
                commands::APP_NAME,
                commands::APP_VERSION
            ));
            bus.info(format!("数据目录：{}", app_dir.display()));
            // 出包漏了 `--features prod` 时，窗口会去载 devUrl（localhost:1420）而不是内嵌资源，
            // 玩家只看到一张「localhost 拒绝连接」。把这行地址留在日志里，一眼就能判。
            if let Some(win) = app.get_webview_window("main") {
                if let Ok(url) = win.url() {
                    bus.info(format!("界面地址：{url}"));
                }
            }

            if let Err(err) = paths::check_writable(&root.path) {
                bus.error(&err);
            }

            install_panic_hook(reporter.clone(), bus.clone());
            reporter.start();

            session.attach(app.handle().clone());
            // 连发的诊断与实况由引擎自己的线程写日志，不经过界面：工具页没开着也要留下一场能对账的。
            let log_bus = bus.clone();
            autofire.attach_log(std::sync::Arc::new(move |line: String| log_bus.info(line)));

            // 首次使用的后台准备：只补「缺的那份配置」，绝不碰玩家的存档库文件。
            std::thread::spawn(move || prepare_environment(session, autofire));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::app_info,
            commands::log_snapshot,
            commands::readiness_state,
            commands::env_check,
            commands::game_status,
            commands::game_start,
            commands::game_stop,
            commands::ports_report,
            commands::force_clean,
            commands::config_get,
            commands::set_account,
            commands::set_game_mode,
            commands::set_auto_stop,
            commands::set_auto_pickup,
            commands::mods_state,
            commands::set_server_mod,
            commands::set_client_mod,
            commands::import_client_mod,
            commands::autofire_get,
            commands::autofire_live,
            commands::autofire_set_master,
            commands::autofire_save,
            commands::autofire_stop,
            commands::saves_state,
            commands::saves_backup,
            commands::saves_restore,
            commands::set_auto_report,
            commands::reports_retry,
            commands::feedback_meta,
            commands::feedback_board,
            commands::feedback_issue,
            commands::feedback_mine,
            commands::feedback_submit,
            commands::pick_client_dir,
            commands::discover_client,
            commands::quit_with_choice,
            panels::panel_open,
            panels::panel_dock,
            panels::panel_closed
        ])
        .build(tauri::generate_context!())
        .expect("启动器初始化失败")
        .run(|app, event| {
            if let RunEvent::Exit = event {
                // 摘钩子：低级键盘钩子是进程级的，不摘就会跟着进程一起没，
                // 但热键与线程消息还得自己收干净，否则下一次启动可能撞到自己留下的绑定。
                if let Some(state) = app.try_state::<commands::AppState>() {
                    state.autofire.stop();
                }
                return;
            }
            if let RunEvent::WindowEvent { label, event, .. } = event {
                match event {
                    WindowEvent::Moved(pos) if label.starts_with(panels::PREFIX) => {
                        // 面板窗口被拖回主窗范围就归位；判定时机等 panels 里的防抖。
                        panels::note_moved(app, &label, pos);
                    }
                    // 关闭拦截交给界面：主窗弹三选一，面板窗只是收回到停靠区。
                    _ => {}
                }
            }
        });
}

/// 启动器自己崩了也要留一条报告。release 走 `panic = "abort"`，钩子里是同步入队，
/// 进程随后才终止 —— 报告落在磁盘上，下一次启动由上报线程补发。
fn install_panic_hook(reporter: std::sync::Arc<report::Reporter>, bus: std::sync::Arc<Bus>) {
    std::panic::set_hook(Box::new(move |info| {
        // 上报这条路径自己 panic 时不能再递归进钩子。
        thread_local! {
            static INSIDE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
        }
        if INSIDE.with(|flag| flag.replace(true)) {
            return;
        }
        let message = info
            .payload()
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| info.payload().downcast_ref::<&str>().map(|s| s.to_string()))
            .unwrap_or_else(|| "未知 panic".to_string());
        let at = info
            .location()
            .map(|l| format!("{}:{}", l.file(), l.line()))
            .unwrap_or_else(|| "位置未知".to_string());
        bus.error(format!("启动器内部异常：{message}（{at}）"));
        let mut ev = report::Event::new(
            "launcher_exception",
            "启动器内部异常退出",
            &format!("{message}\n位置：{at}"),
        );
        ev.severity = "无法继续游戏";
        ev.steps = "正常使用启动器过程中出现，未做其它操作。".to_string();
        reporter.report(ev);
        INSIDE.with(|flag| flag.set(false));
    }));
}

/// 与旧版 `autoPrepare` 同一件事的最小版：认客户端、落存档配置、按配置把连发挂上。
fn prepare_environment(session: Session, autofire: std::sync::Arc<autofire::Engine>) {
    let root = session.root();
    let bus = session.bus();
    let client_dir = config::client_dir(&root);
    if !paths::has_client_files(&client_dir) {
        bus.warn("未找到游戏客户端，请在首页手动选择 DFO.exe 所在目录");
    } else if config::stored_client_dir(&root).as_deref() == Some(client_dir.as_path()) {
        bus.info(format!("游戏客户端：{}", client_dir.display()));
    } else {
        // 走到这一支说明用的是兜底探到的目录：落一次配置，让玩家在设置页看得见这条事实。
        match config::set_client_dir(&root, &client_dir) {
            Ok(saved) => bus.ok(format!("已识别游戏客户端：{}", saved.display())),
            Err(err) => bus.error(format!("写入客户端目录失败：{err}")),
        }
    }

    match config::ensure_storage_route(&root) {
        Ok(route) if route.created => bus.ok(format!("本地存储已就绪：{}（库文件由服务端首次连接时创建）", route.describe)),
        Ok(route) => bus.info(format!("本地存储无需初始化：{}", route.describe)),
        Err(err) => bus.error(err),
    }

    // 连发：配置里开着就自己把监听挂上，和旧版一样不用玩家再点一次。
    // 放后台线程是有意的——挂钩子 + RegisterHotKey + 等线程报到，最坏一秒，不能拖主窗。
    let cfg = autofire::load(&root);
    if cfg.enabled {
        match autofire.start(cfg) {
            Ok(message) => bus.info(message),
            Err(err) => bus.error(format!("连发没能启用：{err}")),
        }
    }

    let readiness = session.readiness();
    if readiness.can_start {
        bus.ok("环境已准备完成");
    }
    // 触发一次 game-phase/readiness 事件，界面首屏不用等轮询。
    session.touch_readiness();
}
