#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod controller;
mod details;
mod format;
mod icons;
mod logging;
mod placement;
mod sampling;
mod screenshot;
mod settings;
mod texts;
mod tray;
#[cfg(all(test, feature = "renderer-software"))]
mod toolbar_fit;

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use slint::{ComponentHandle, RenderingState, Timer, TimerMode};

use controller::Controller;

slint::include_modules!();

thread_local! {
    static CONTROLLER: RefCell<Option<Rc<Controller>>> = const { RefCell::new(None) };
}

const RENDERER: &str = if cfg!(feature = "renderer-software") { "software" } else { "femtovg" };

/// 开发用参数：测量、截图与 `--allow-multiple` 不受单实例限制，也不占用单实例锁。
const UNRESTRICTED_FLAGS: [&str; 6] = [
    "--measure-startup",
    "--screenshot",
    "--menu-shot",
    "--menu-action",
    "--service-action",
    "--allow-multiple",
];

fn main() -> Result<(), slint::PlatformError> {
    if let Some(code) = tml_core::startup::run_cli(&std::env::args().collect::<Vec<_>>()) {
        std::process::exit(code);
    }
    // 服务页的提权子进程：启动、停止或重启一个服务后退出。
    if let Some(code) = tml_core::services::run_cli(&std::env::args().collect::<Vec<_>>()) {
        std::process::exit(code);
    }
    // 接管 Ctrl+Shift+Esc 的命令行开关（含提权子进程）：执行完直接退出，不创建窗口、不占单实例锁。
    if let Some(code) = tml_core::run_cli(&std::env::args().collect::<Vec<_>>()) {
        std::process::exit(code);
    }
    // 函数以任何方式返回时停止本进程的 ETW 磁盘会话（崩溃与 panic 另有钩子兜底）。
    let _trace_guard = tml_core::TraceGuard;
    // 单实例：已有实例则通知它把窗口带到前台，本进程直接退出。须在打开日志等之前完成。
    let mut instance = if std::env::args().any(|a| UNRESTRICTED_FLAGS.contains(&a.as_str())) {
        None
    } else {
        match tml_core::acquire(tml_core::is_relaunch(std::env::args())) {
            tml_core::Acquired::Primary(i) => Some(i),
            tml_core::Acquired::Existing => return Ok(()),
        }
    };
    // `--measure-startup`：首个带进程快照的帧渲染完成后记录耗时并退出，供测量脚本使用。
    let measure = std::env::args().any(|a| a == "--measure-startup");
    let entered = tml_core::ms_since_process_start();
    let mut log = logging::Log::open();
    log.line(format!("启动 渲染器={RENDERER} 提权={}", tml_core::is_elevated()));
    log.line(format!("进入 main {entered:.1} ms"));

    // 设置在创建窗口前读取（一个很小的文件）；开发模式不读写用户设置，保证截图与测量可复现。
    let dev_mode = measure || std::env::args().any(|a| a == "--screenshot" || a == "--menu-shot" || a == "--menu-action" || a == "--service-action");
    let store = if dev_mode { None } else { settings::Store::from_env() };
    let (loaded, outcome) = store.as_ref().map_or((settings::Settings::default(), None), |s| {
        let (settings, outcome) = s.load();
        (settings, Some(outcome))
    });
    log.line(format!("设置 {outcome:?} 窗口={:?}", loaded.window));
    // 被 IFEO 重定向启动时，第一个参数是 taskmgr.exe 的路径（其余页签参数同理），本程序不使用它们。
    if tml_core::launched_as_taskmgr_debugger(&std::env::args().collect::<Vec<_>>()) {
        log.line("由 Taskmgr.exe 的 IFEO 重定向启动");
    }
    // 意愿为开启时核对登记是否仍指向本 exe（只读一个注册表值）；失效的处理入口在设置页。
    if loaded.replace_taskmgr
        && let Ok(exe) = std::env::current_exe()
    {
        log.line(format!("接管任务管理器 意愿=开 登记={:?}", tml_core::Ifeo::system().status(&exe)));
    }

    let ui = AppWindow::new()?;
    if store.is_some() {
        placement::apply(&ui, &loaded);
    }
    log.line(format!("窗口组件创建 {:.1} ms", tml_core::ms_since_process_start()));
    let sampler = sampling::spawn(loaded.view == tml_core::ViewMode::Group, loaded.accurate_disk_io, loaded.background_sampling, |sample| {
        let _ = slint::invoke_from_event_loop(move || {
            CONTROLLER.with(|c| {
                if let Some(c) = c.borrow().as_ref() {
                    c.on_sample(sample);
                }
            })
        });
    });
    let controller = Controller::new(&ui, sampler, log, loaded, store);
    CONTROLLER.with(|c| *c.borrow_mut() = Some(controller.clone()));
    let saver = controller.clone();
    ui.window().on_close_requested(move || saver.close_requested());

    // 首帧与首个快照帧的耗时（自进程创建起算，含加载器时间）。
    let first_frame_logged = Cell::new(false);
    let data_frame_logged = Cell::new(false);
    let notifier = ui.window().set_rendering_notifier(move |state, _| {
        if !matches!(state, RenderingState::AfterRendering) {
            return;
        }
        CONTROLLER.with(|c| {
            let Some(c) = c.borrow().clone() else { return };
            if !first_frame_logged.replace(true) {
                c.log.borrow_mut().line(format!("首帧 {:.1} ms", tml_core::ms_since_process_start()));
            }
            if c.has_first_data() && !data_frame_logged.replace(true) {
                c.log.borrow_mut().line(format!("首个快照帧 {:.1} ms", tml_core::ms_since_process_start()));
                if measure {
                    let _ = slint::quit_event_loop();
                }
            }
        });
    });
    let poll = Rc::new(Timer::default());
    if notifier.is_err() {
        // software 渲染器没有渲染通知：改为在事件循环空闲时打点，作为近似值。
        controller.log.borrow_mut().line("渲染器不支持渲染通知，首帧耗时为近似值");
        Timer::single_shot(Duration::ZERO, || {
            CONTROLLER.with(|c| {
                if let Some(c) = c.borrow().as_ref() {
                    c.log.borrow_mut().line(format!("首帧 {:.1} ms", tml_core::ms_since_process_start()));
                }
            })
        });
        let weak_poll = Rc::downgrade(&poll);
        poll.start(TimerMode::Repeated, Duration::from_millis(1), move || {
            CONTROLLER.with(|c| {
                if let Some(c) = c.borrow().as_ref()
                    && c.has_first_data()
                {
                    if let Some(t) = weak_poll.upgrade() {
                        t.stop();
                    }
                    c.log.borrow_mut().line(format!("首个快照帧 {:.1} ms", tml_core::ms_since_process_start()));
                    if measure {
                        let _ = slint::quit_event_loop();
                    }
                }
            })
        });
    }

    if let Some(dir) = std::env::args().skip_while(|a| a != "--screenshot").nth(1) {
        screenshot::run(&ui, dir.into(), Duration::from_secs(8));
    }

    if let Some(spec) = std::env::args().skip_while(|a| a != "--menu-shot").nth(1) {
        screenshot::run_menu(&ui, &spec, Duration::from_secs(5));
    }

    if let Some(spec) = std::env::args().skip_while(|a| a != "--menu-action").nth(1) {
        screenshot::run_menu_action(&ui, &spec, Duration::from_secs(4));
    }

    // `--service-action <start|stop|restart|goto>:<服务名>`：自动化验收服务页（不弹 UAC，结果写日志）。
    if let Some(spec) = std::env::args().skip_while(|a| a != "--service-action").nth(1) {
        controller.queue_service_action(&spec);
    }

    // 其他实例请求唤起时，在界面线程还原并前置窗口（窗口隐藏在托盘时先重新显示）。
    if let Some(i) = instance.as_mut() {
        i.listen(|| {
            let _ = slint::invoke_from_event_loop(|| {
                let controller = CONTROLLER.with(|c| c.borrow().clone());
                match controller {
                    Some(c) => c.show_window(),
                    None => tml_core::activate_own_window(),
                }
            });
        });
    }

    // 窗口最小化或隐藏时停止采样（开启“后台继续采样”则只采系统指标）。
    let visibility = Timer::default();
    let tracker = controller.clone();
    visibility.start(TimerMode::Repeated, Duration::from_millis(300), move || {
        tracker.poll_visible();
        tracker.track_window();
    });

    // 窗口隐藏到托盘后，“最后一个窗口关闭”不再结束事件循环，退出统一走 `quit_event_loop`。
    controller.start(std::env::args().any(|a| a == tml_core::AUTOSTART_ARG))?;
    slint::run_event_loop_until_quit()?;
    ui.hide()?;
    controller.remove_tray();
    drop(poll);
    CONTROLLER.with(|c| c.borrow_mut().take());
    Ok(())
}
