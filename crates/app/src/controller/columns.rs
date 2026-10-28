//! 进程列表的“详细信息”列：列选择、列宽，以及静态信息（用户名、路径等）的按需读取。

use std::rc::Rc;

use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};
use tml_core::{DetailColumn, SortKey};

use super::{Controller, State};
use crate::details::{self, InfoWorker, Shown};
use crate::{AppWindow, Columns, DetailChoice, DetailHeader};

/// 详细信息列在排序序号中的起点：序号 = 100 + 列在 `DetailColumn::ALL` 中的位置。
pub const SORT_BASE: i32 = 100;

pub fn sort_index(c: DetailColumn) -> i32 {
    SORT_BASE + DetailColumn::ALL.iter().position(|&x| x == c).unwrap_or(0) as i32
}

pub fn sort_column(i: i32) -> Option<DetailColumn> {
    let i = usize::try_from(i.checked_sub(SORT_BASE)?).ok()?;
    DetailColumn::ALL.get(i).copied()
}

/// 显示或隐藏一列：新显示的列追加在最右侧，使用默认宽度。
pub fn toggle(shown: &mut Vec<Shown>, column: DetailColumn) {
    if let Some(i) = shown.iter().position(|s| s.column == column) {
        shown.remove(i);
    } else {
        shown.push(Shown {
            column,
            width: details::default_width(column),
        });
    }
}

/// 隐藏了正在排序的列时，回到默认的按 CPU 降序。返回是否改动了排序。
pub fn fix_sort(state_sort: &mut SortKey, descending: &mut bool, shown: &[Shown]) -> bool {
    match *state_sort {
        SortKey::Detail(c) if !shown.iter().any(|s| s.column == c) => {
            *state_sort = SortKey::Cpu;
            *descending = true;
            true
        }
        _ => false,
    }
}

/// 原地更新模型：行数不变时只改有变化的行。整体换模型会让界面重建表头，正在拖动的分隔线随之失效。
fn assign<T: Clone + PartialEq + 'static>(m: &VecModel<T>, v: Vec<T>) {
    if m.row_count() != v.len() {
        m.set_vec(v);
        return;
    }
    for (i, x) in v.into_iter().enumerate() {
        if m.row_data(i).as_ref() != Some(&x) {
            m.set_row_data(i, x);
        }
    }
}

/// 是否显示了需要读取静态信息的列。
fn needs_static(shown: &[Shown]) -> bool {
    shown.iter().any(|s| s.column.needs_static())
}

impl Controller {
    pub(super) fn bind_columns(self: &Rc<Self>, ui: &AppWindow) {
        let cols = ui.global::<Columns>();
        let c = Rc::downgrade(self);
        cols.set_extra(ModelRc::from(self.detail_headers.clone()));
        cols.set_choices(ModelRc::from(self.detail_choices.clone()));
        cols.on_toggle_detail(move |i| {
            let Some(c) = c.upgrade() else { return };
            let Some(&column) = usize::try_from(i)
                .ok()
                .and_then(|i| DetailColumn::ALL.get(i))
            else {
                return;
            };
            c.edit_columns(|s| toggle(&mut s.details, column));
        });
        let c = Rc::downgrade(self);
        cols.on_reset_details(move || {
            if let Some(c) = c.upgrade() {
                c.edit_columns(|s| s.details.clear());
            }
        });
        let c = Rc::downgrade(self);
        cols.on_resize_detail(move |i, w| {
            if let Some(c) = c.upgrade() {
                c.set_detail_width(i, Some(w));
            }
        });
        let c = Rc::downgrade(self);
        cols.on_reset_detail_width(move |i| {
            if let Some(c) = c.upgrade() {
                c.set_detail_width(i, None);
            }
        });
        self.sync_columns();
    }

    /// 改动显示的列：同步表头，按需开始或停止读取静态信息，并重建列表（排序可能因此改变）。
    fn edit_columns(&self, f: impl FnOnce(&mut State)) {
        {
            let mut guard = self.state.borrow_mut();
            let s = &mut *guard;
            f(s);
            fix_sort(&mut s.opts.sort_key, &mut s.opts.descending, &s.details);
        }
        self.sync_columns();
        self.track_info();
        self.refresh_rows();
    }

    /// 第 `i` 个显示中的列设为宽度 `w`（限制在允许范围内）；`None` 表示恢复默认宽度。
    fn set_detail_width(&self, i: i32, w: Option<f32>) {
        {
            let mut s = self.state.borrow_mut();
            let Some(shown) = usize::try_from(i).ok().and_then(|i| s.details.get_mut(i)) else {
                return;
            };
            shown.width = w
                .unwrap_or_else(|| details::default_width(shown.column))
                .clamp(details::MIN_WIDTH, details::MAX_WIDTH);
        }
        self.sync_columns();
    }

    /// 把显示中的列与列选择菜单的选项写入界面。
    pub(super) fn sync_columns(&self) {
        let Some(ui) = self.ui.upgrade() else { return };
        let s = self.state.borrow();
        let cols = ui.global::<Columns>();
        let headers: Vec<DetailHeader> = s
            .details
            .iter()
            .map(|d| DetailHeader {
                title: details::title(d.column).into(),
                width: d.width,
                right: details::right_aligned(d.column),
                sort: sort_index(d.column),
            })
            .collect();
        cols.set_extra_total(s.details.iter().map(|d| d.width).sum());
        assign(&self.detail_headers, headers);
        let choices: Vec<DetailChoice> = DetailColumn::ALL
            .iter()
            .map(|&c| DetailChoice {
                title: details::title(c).into(),
                shown: s.details.iter().any(|d| d.column == c),
            })
            .collect();
        assign(&self.detail_choices, choices);
    }

    /// 显示了静态信息列时，把新出现的进程交给后台读取；不再显示时释放缓存。
    pub(super) fn track_info(&self) {
        let fresh = {
            let mut guard = self.state.borrow_mut();
            let s = &mut *guard;
            if !needs_static(&s.details) {
                s.info.clear();
                return;
            }
            s.info.track(s.snapshot.processes.iter().map(|p| p.key))
        };
        if fresh.is_empty() {
            return;
        }
        let worker = self.info_worker.get_or_init(|| {
            InfoWorker::spawn(|batch| {
                let _ = slint::invoke_from_event_loop(move || {
                    crate::CONTROLLER.with(|c| {
                        if let Some(c) = c.borrow().as_ref() {
                            c.on_info(batch);
                        }
                    })
                });
            })
        });
        worker.read(fresh);
    }

    /// 后台读到的一批静态信息。
    fn on_info(&self, batch: Vec<(tml_core::ProcessKey, Option<tml_core::StaticInfo>)>) {
        let changed = {
            let mut s = self.state.borrow_mut();
            let mut changed = false;
            for (key, info) in batch {
                match info {
                    Some(info) => changed |= s.info.loaded(key, info),
                    None => s.info.forget(key),
                }
            }
            changed
        };
        if changed {
            self.refresh_rows();
        }
    }

    /// 当前进程行的详细信息单元格文字。
    pub(super) fn detail_cells(s: &State, p: &tml_core::Process) -> ModelRc<SharedString> {
        if s.details.is_empty() {
            return ModelRc::default();
        }
        ModelRc::new(VecModel::from(
            s.details
                .iter()
                .map(|d| SharedString::from(details::cell(p, d.column)))
                .collect::<Vec<_>>(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sort_index_round_trips() {
        for c in DetailColumn::ALL {
            assert_eq!(sort_column(sort_index(c)), Some(c));
        }
        assert_eq!(sort_column(4), None);
        assert_eq!(
            sort_column(SORT_BASE + DetailColumn::ALL.len() as i32),
            None
        );
    }

    #[test]
    fn toggling_appends_and_removes() {
        let mut v = Vec::new();
        toggle(&mut v, DetailColumn::User);
        toggle(&mut v, DetailColumn::Threads);
        assert_eq!(
            v.iter().map(|s| s.column).collect::<Vec<_>>(),
            [DetailColumn::User, DetailColumn::Threads]
        );
        assert_eq!(v[0].width, details::default_width(DetailColumn::User));
        toggle(&mut v, DetailColumn::User);
        assert_eq!(
            v.iter().map(|s| s.column).collect::<Vec<_>>(),
            [DetailColumn::Threads]
        );
    }

    #[test]
    fn hiding_sorted_column_falls_back_to_cpu() {
        let shown = vec![Shown {
            column: DetailColumn::Threads,
            width: 72.0,
        }];
        let (mut k, mut d) = (SortKey::Detail(DetailColumn::Threads), false);
        assert!(!fix_sort(&mut k, &mut d, &shown));
        assert!(fix_sort(&mut k, &mut d, &[]));
        assert_eq!((k, d), (SortKey::Cpu, true));
        let (mut k, mut d) = (SortKey::Name, false);
        assert!(!fix_sort(&mut k, &mut d, &[]));
        assert_eq!((k, d), (SortKey::Name, false));
    }
}
