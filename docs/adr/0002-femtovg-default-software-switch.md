# 0002 默认 femtovg 渲染，保留 software 渲染开关

状态：已接受（2026-10-27）

## 背景

Slint 在 Windows 上可选 femtovg（OpenGL）、software、skia 三种渲染器。skia 的体积与“轻量”目标冲突；femtovg 与 software 的冷启动差异需要实测。

## 决定

- 默认 cargo feature `renderer-femtovg`。
- 提供 `renderer-software` feature，构建时可切换：`cargo build -p tml-app --release --no-default-features --features renderer-software`。
- 启动耗时由程序内埋点写入 `logs/`，两种渲染器用同一测量脚本 `scripts/measure-startup.ps1` 对比，以数据决定是否改默认值。

## 取舍

femtovg 需要初始化 OpenGL 上下文，冷启动比 software 多一段 GPU 驱动加载时间，但抗锯齿曲线和大面积重绘的 CPU 占用更好。暂不设冷启动硬指标，只持续测量。
