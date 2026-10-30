//! 运行新任务（#69）：Ctrl+N 或工具栏按钮打开对话框，输入程序、文件夹、文档或网址，可选以管理员身份运行。
//!
//! 启动（含等待 UAC）与“浏览…”的文件选择都在后台线程里完成，界面线程只负责显示状态。
//! 日志只记录开始、成功、失败与错误码，不记录输入的命令行。

use super::*;
use crate::{RunTaskData, file_dialog, texts};
use std::path::PathBuf;
use tml_core::RunError;

impl Controller {
    pub(super) fn bind_run_task(self: &Rc<Self>, ui: &AppWindow) {
        let data = ui.global::<RunTaskData>();
        data.set_history(self.run_history_model());
        let weak = Rc::downgrade(self);
        data.on_opened(move || {
            if let Some(c) = weak.upgrade() {
                let n = c.settings.borrow().run_history.len();
                c.log.borrow_mut().line(format!("运行新任务 打开对话框 历史={n}"));
            }
        });
        let weak = Rc::downgrade(self);
        data.on_submit(move || {
            if let Some(c) = weak.upgrade() {
                c.submit_run_task();
            }
        });
        let weak = Rc::downgrade(self);
        data.on_browse(move || {
            if let Some(c) = weak.upgrade() {
                c.browse_run_task();
            }
        });
    }

    /// 对话框里“最近使用”列表的数据（最新在前）。
    fn run_history_model(&self) -> ModelRc<SharedString> {
        let items: Vec<SharedString> = self.settings.borrow().run_history.iter().map(|h| h.as_str().into()).collect();
        ModelRc::new(VecModel::from(items))
    }

    /// 确定：读取输入与勾选，在后台线程里交给外壳启动。
    fn submit_run_task(&self) {
        let Some(ui) = self.ui.upgrade() else { return };
        let data = ui.global::<RunTaskData>();
        if data.get_busy() {
            return;
        }
        let input = data.get_input().to_string();
        let admin = data.get_as_admin();
        data.set_error("".into());
        data.set_busy(true);
        self.log.borrow_mut().line(format!("运行新任务 开始 管理员={admin}"));
        let spawned = std::thread::Builder::new().name("run-task".into()).spawn({
            let input = input.clone();
            move || {
                let result = tml_core::run_task(&input, admin);
                let _ = slint::invoke_from_event_loop(move || {
                    crate::CONTROLLER.with(|c| {
                        if let Some(c) = c.borrow().as_ref() {
                            c.finish_run_task(&input, admin, result);
                        }
                    })
                });
            }
        });
        if let Err(e) = spawned {
            self.finish_run_task(&input, admin, Err(RunError::System(tml_core::SystemError::from(e))));
        }
    }

    /// 启动结果回到界面线程：成功则记入历史并关闭对话框；失败则在对话框里显示原因，对话框保持打开。
    fn finish_run_task(&self, input: &str, admin: bool, result: Result<(), RunError>) {
        let Some(ui) = self.ui.upgrade() else { return };
        let data = ui.global::<RunTaskData>();
        data.set_busy(false);
        match result {
            Ok(()) => {
                settings::push_run_history(&mut self.settings.borrow_mut().run_history, input);
                data.set_history(self.run_history_model());
                // 立即写入设置文件：历史不应只在正常退出时才保存。
                self.save_settings();
                self.log.borrow_mut().line(format!("运行新任务 成功 管理员={admin}"));
                ui.invoke_close_run_task();
            }
            Err(e) => {
                self.log.borrow_mut().line(format!("运行新任务 失败 管理员={admin} 错误码={}", e.code()));
                // 用户在等待期间已经取消了对话框：没有地方显示，只留日志。
                if data.get_open() {
                    data.set_error(texts::run_error(&e).into());
                }
            }
        }
    }

    /// 浏览…：在后台线程里弹出系统文件选择对话框（以主窗口为所有者，模态）。
    fn browse_run_task(&self) {
        let Some(ui) = self.ui.upgrade() else { return };
        let data = ui.global::<RunTaskData>();
        if data.get_busy() {
            return;
        }
        data.set_busy(true);
        let owner = self.with_active(mini::hwnd_of).flatten().map_or(0, |h| h.0 as isize);
        self.log.borrow_mut().line("运行新任务 浏览 打开文件选择对话框");
        let spawned = std::thread::Builder::new().name("run-task-browse".into()).spawn(move || {
            let picked = file_dialog::pick_file(owner, texts::browse_title(), &texts::browse_filters());
            let _ = slint::invoke_from_event_loop(move || {
                crate::CONTROLLER.with(|c| {
                    if let Some(c) = c.borrow().as_ref() {
                        c.finish_browse(picked);
                    }
                })
            });
        });
        if spawned.is_err() {
            self.finish_browse(None);
        }
    }

    fn finish_browse(&self, picked: Option<PathBuf>) {
        let Some(ui) = self.ui.upgrade() else { return };
        let data = ui.global::<RunTaskData>();
        data.set_busy(false);
        let mut log = self.log.borrow_mut();
        match picked {
            Some(path) => {
                data.set_input(file_dialog::input_text(&path).into());
                data.set_error("".into());
                log.line("运行新任务 浏览 已选择文件");
            }
            None => log.line("运行新任务 浏览 已取消"),
        }
    }
}
