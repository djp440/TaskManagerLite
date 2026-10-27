//! 逻辑处理器网格的布局计算（纯函数，可单测）：给定小图数量与网格区尺寸，得出列数、行数与小图尺寸。
//!
//! 目标：小图接近 1.6:1，不窄于 `MIN_CELL_W`、不矮于 `MIN_CELL_H`。核数多到放不下时不再压扁小图，
//! 而是保持最小高度、让网格区自己滚动（`scroll`），并为滚动条让出宽度。

/// 小图之间的间距。
pub const GAP: f32 = 6.0;
/// 小图的最小宽度。
pub const MIN_CELL_W: f32 = 64.0;
/// 小图的最小高度（每行至少约 24px）。
pub const MIN_CELL_H: f32 = 24.0;
/// 小图理想的宽高比。
const ASPECT: f32 = 1.6;
/// 出现竖向滚动条时为它预留的宽度。
pub const SCROLLBAR_W: f32 = 14.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GridPlan {
    pub cols: usize,
    pub rows: usize,
    pub cell_w: f32,
    pub cell_h: f32,
    /// 全部行的总高度；`scroll` 为 false 时等于网格区高度。
    pub content_h: f32,
    /// 网格区放不下全部行（按最小高度），需要滚动。
    pub scroll: bool,
}

/// 宽度 `w` 内最多放几列（每列不窄于 `MIN_CELL_W`），且不超过小图数量。
fn max_cols(count: usize, w: f32) -> usize {
    (((w + GAP) / (MIN_CELL_W + GAP)).floor() as usize).clamp(1, count)
}

fn plan_with(count: usize, cols: usize, w: f32, h: f32) -> GridPlan {
    let rows = count.div_ceil(cols);
    let gaps_w = GAP * (cols - 1) as f32;
    let gaps_h = GAP * (rows - 1) as f32;
    GridPlan {
        cols,
        rows,
        cell_w: (w - gaps_w) / cols as f32,
        cell_h: (h - gaps_h) / rows as f32,
        content_h: h,
        scroll: false,
    }
}

/// 计算网格布局。`count` 为小图数量，`w`、`h` 为网格区的可用尺寸（逻辑像素）。
pub fn grid_plan(count: usize, w: f32, h: f32) -> GridPlan {
    let count = count.max(1);
    let (w, h) = (w.max(1.0), h.max(1.0));

    // 先在不滚动的前提下找列数：从理想列数起逐步加列（减少行数），直到小图不矮于最小高度。
    let widest = max_cols(count, w);
    let ideal = ((count as f32 * w / (h * ASPECT)).sqrt().round() as usize).clamp(1, widest);
    if let Some(plan) = (ideal..=widest).map(|cols| plan_with(count, cols, w, h)).find(|p| p.cell_h >= MIN_CELL_H) {
        return plan;
    }

    // 放不下：用尽可能多的列、固定最小高度，网格区滚动；宽度要扣掉滚动条。
    let w = (w - SCROLLBAR_W).max(1.0);
    let cols = max_cols(count, w);
    let rows = count.div_ceil(cols);
    GridPlan {
        cols,
        rows,
        cell_w: (w - GAP * (cols - 1) as f32) / cols as f32,
        cell_h: MIN_CELL_H,
        content_h: rows as f32 * MIN_CELL_H + GAP * (rows - 1) as f32,
        scroll: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 小图总数不超过 cols*rows，且最后一行不会整行为空。
    fn assert_covers(p: &GridPlan, count: usize) {
        assert!(p.cols * p.rows >= count, "{p:?}");
        assert!(p.cols * (p.rows - 1) < count, "{p:?}");
    }

    #[test]
    fn few_cores_fit_without_scrolling() {
        // 12 个逻辑处理器、760x480 窗口里的网格区（约 700x80）：2 行，不滚动，小图不矮于 24。
        let p = grid_plan(12, 700.0, 80.0);
        assert_eq!((p.cols, p.rows), (8, 2));
        assert!(!p.scroll && p.cell_h >= MIN_CELL_H && p.cell_w >= MIN_CELL_W, "{p:?}");
        assert!((p.cell_h - 37.0).abs() < 0.01, "{p:?}");
        assert_covers(&p, 12);
        // 大窗口：小图舒展，仍不滚动。
        let p = grid_plan(12, 1100.0, 400.0);
        assert!(!p.scroll && p.cell_h > 60.0, "{p:?}");
    }

    #[test]
    fn sixty_four_cores_in_a_short_area_keep_min_height_and_scroll() {
        // 64 核、网格区只有 80px 高：不能把小图压到 6px，而是 24px 一行并滚动。
        let p = grid_plan(64, 700.0, 80.0);
        assert!(p.scroll);
        // 扣掉滚动条后宽 686px，最多 9 列（每列至少 64px），64/9 向上取整 8 行。
        assert_eq!((p.cols, p.rows), (9, 8));
        assert_eq!(p.cell_h, MIN_CELL_H);
        assert!(p.cell_w >= MIN_CELL_W, "{p:?}");
        assert_eq!(p.content_h, 8.0 * 24.0 + 7.0 * GAP);
        assert!(p.content_h > 80.0);
        assert_covers(&p, 64);
    }

    #[test]
    fn one_hundred_twenty_eight_cores_grow_the_content_height() {
        let p = grid_plan(128, 700.0, 80.0);
        assert!(p.scroll);
        assert_eq!((p.cols, p.rows), (9, 15));
        assert_eq!(p.cell_h, MIN_CELL_H);
        assert_eq!(p.content_h, 15.0 * 24.0 + 14.0 * GAP);
        assert_covers(&p, 128);
        // 行数随核数增长，内容高度也随之增长。
        assert!(p.content_h > grid_plan(64, 700.0, 80.0).content_h);
    }

    #[test]
    fn big_area_fits_many_cores_without_scrolling() {
        // 1100x500 的网格区放得下 64 核和 128 核，小图都不矮于 24。
        for count in [64, 128] {
            let p = grid_plan(count, 1100.0, 500.0);
            assert!(!p.scroll && p.cell_h >= MIN_CELL_H && p.cell_w >= MIN_CELL_W, "{count}: {p:?}");
            assert_eq!(p.content_h, 500.0);
            assert_covers(&p, count);
        }
        let p = grid_plan(64, 1100.0, 500.0);
        assert_eq!((p.cols, p.rows), (9, 8));
        let p = grid_plan(128, 1100.0, 500.0);
        assert_eq!((p.cols, p.rows), (13, 10));
    }

    #[test]
    fn degenerate_sizes_do_not_panic() {
        for (count, w, h) in [(0, 0.0, 0.0), (1, 10.0, 10.0), (5, 30.0, 1000.0), (200, 5000.0, 5.0)] {
            let p = grid_plan(count, w, h);
            assert!(p.cols >= 1 && p.rows >= 1 && p.cell_w.is_finite() && p.cell_h.is_finite(), "{p:?}");
        }
    }
}
