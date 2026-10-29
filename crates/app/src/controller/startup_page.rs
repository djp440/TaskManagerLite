use super::*;
use crate::{StartupData, StartupRow};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

impl Controller {
    pub(super) fn bind_startup(self: &Rc<Self>, ui: &AppWindow) {
        let weak = Rc::downgrade(self);
        ui.global::<StartupData>().on_sort(move |key| {
            if let Some(c) = weak.upgrade() {
                if let Some(ui) = c.ui.upgrade() {
                    let d = ui.global::<StartupData>();
                    d.set_descending(d.get_sort_key() == key && !d.get_descending());
                    d.set_sort_key(key);
                    c.startup_work(false);
                }
            }
        });
        let weak = Rc::downgrade(self);
        ui.global::<StartupData>().on_refresh(move || {
            if let Some(c) = weak.upgrade() {
                c.startup_work(false);
            }
        });
        let weak = Rc::downgrade(self);
        ui.global::<StartupData>().on_toggle(move || {
            if let Some(c) = weak.upgrade() {
                c.startup_work(true);
            }
        });
        // 恢复到启动项页时，组件的 init 可能早于回调绑定。
        if ui.get_page() == 3 {
            self.startup_work(false);
        }
    }

    fn startup_work(&self, toggle: bool) {
        let Some(ui) = self.ui.upgrade() else { return };
        let data = ui.global::<StartupData>();
        if data.get_busy() {
            return;
        }
        let entry = usize::try_from(data.get_selected())
            .ok()
            .and_then(|i| self.startup_entries.borrow().get(i).cloned());
        if toggle && entry.is_none() {
            return;
        }
        data.set_busy(true);
        let px = crate::icons::physical_size(ui.window().scale_factor());
        let work = std::thread::Builder::new()
            .name("startup-items".into())
            .spawn(move || {
                let result = if toggle {
                    let e = entry.as_ref().unwrap();
                    tml_core::startup::set(e, !e.enabled)
                        .map(|()| "状态已更新".to_owned())
                        .unwrap_or_else(|e| format!("无法修改启动项：{e}"))
                } else {
                    String::new()
                };
                let (entries, errors) = tml_core::startup::list();
                let icons: Vec<_> = entries.iter().map(|e| icon(e.target.as_deref(), px)).collect();
                let note = std::iter::once(result)
                    .chain(errors)
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
                    .join("；");
                let _ = slint::invoke_from_event_loop(move || {
                    crate::CONTROLLER.with(|cell| {
                        let Some(c) = cell.borrow().clone() else {
                            return;
                        };
                        let Some(ui) = c.ui.upgrade() else { return };
                        let data = ui.global::<StartupData>();
                        let mut entries: Vec<_> = entries.into_iter().zip(icons).collect();
                        entries.sort_by(|(a, _), (b, _)| {
                            compare(a, b, data.get_sort_key(), data.get_descending())
                        });
                        let (entries, icons): (Vec<_>, Vec<_>) = entries.into_iter().unzip();
                        let current = usize::try_from(data.get_selected())
                            .ok()
                            .and_then(|i| c.startup_entries.borrow().get(i).cloned());
                        let selected = current.and_then(|old| {
                            entries
                                .iter()
                                .position(|e| e.source == old.source && e.name == old.name)
                        });
                        let rows = entries
                            .iter()
                            .zip(icons)
                            .map(|(e, icon)| StartupRow {
                                icon: icon.map_or_else(
                                    || c.blank_icon.clone(),
                                    |b| {
                                        Image::from_rgba8(SharedPixelBuffer::clone_from_slice(
                                            &b.rgba, b.width, b.height,
                                        ))
                                    },
                                ),
                                name: e.name.as_str().into(),
                                publisher: e.publisher.as_str().into(),
                                source: e.source_label().into(),
                                command: e.command.as_str().into(),
                                enabled: e.enabled,
                                admin: e.needs_admin(),
                            })
                            .collect::<Vec<_>>();
                        *c.startup_entries.borrow_mut() = entries;
                        let data = ui.global::<StartupData>();
                        data.set_rows(ModelRc::new(VecModel::from(rows)));
                        data.set_selected(selected.map_or(-1, |i| i as i32));
                        data.set_note(note.into());
                        data.set_busy(false);
                    });
                });
            });
        if let Err(e) = work {
            data.set_busy(false);
            data.set_note(format!("无法创建后台线程：{e}").into());
        }
    }
}

/// 启动项的图标，按路径和尺寸缓存：每次点击列表都会重新枚举，不必反复读文件资源。
/// 路径解析不出或文件不存在时用通用程序图标。
fn icon(target: Option<&std::path::Path>, px: u32) -> Option<tml_core::IconBitmap> {
    static CACHE: Mutex<Option<HashMap<(PathBuf, u32), Option<tml_core::IconBitmap>>>> =
        Mutex::new(None);
    let Some(path) = target else {
        return tml_core::default_icon(px);
    };
    let mut guard = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    guard
        .get_or_insert_with(HashMap::new)
        .entry((path.to_path_buf(), px))
        .or_insert_with(|| {
            tml_core::file_icon(path, px).or_else(|| tml_core::default_icon(px))
        })
        .clone()
}

fn compare(
    a: &tml_core::startup::Entry,
    b: &tml_core::startup::Entry,
    key: i32,
    descending: bool,
) -> std::cmp::Ordering {
    {
        let cmp = match key {
            1 => a.publisher.to_lowercase().cmp(&b.publisher.to_lowercase()),
            2 => a.enabled.cmp(&b.enabled),
            3 => a.source_label().cmp(b.source_label()),
            _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
        }
        .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        .then(a.source.cmp(&b.source));
        if descending { cmp.reverse() } else { cmp }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

fn sort_entries(entries: &mut [tml_core::startup::Entry], key: i32, descending: bool) {
    entries.sort_by(|a, b| compare(a, b, key, descending));
}

    #[test]
    fn startup_columns_sort_both_directions() {
        let entry = |name: &str, publisher: &str, source, enabled| tml_core::startup::Entry {
            name: name.into(),
            publisher: publisher.into(),
            source,
            enabled,
            command: String::new(),
            target: None,
        };
        for (key, first) in [(0, "Alpha"), (1, "Beta"), (2, "Beta"), (3, "Alpha")] {
            let mut rows = vec![entry("Beta", "A", 1, false), entry("Alpha", "Z", 0, true)];
            sort_entries(&mut rows, key, false);
            assert_eq!(rows[0].name, first);
            sort_entries(&mut rows, key, true);
            assert_ne!(rows[0].name, first);
        }
    }
}
