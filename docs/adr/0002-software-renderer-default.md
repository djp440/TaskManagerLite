# 0002 默认 software 渲染，保留 femtovg 开关

状态：已接受（2026-10-27）。最初决定默认 femtovg、以实测定夺；同日实测后改为默认 software。

## 背景

Slint 在 Windows 上可选 femtovg（OpenGL）、software、skia 三种渲染器。skia 的体积与“轻量”目标冲突，不考虑。femtovg 与 software 的取舍按实测数据决定。

## 实测（2026-10-27，Ryzen 5 9600X，Win10 19045，3840×2160 @200%，release 构建）

| 指标 | femtovg | software |
|---|---|---|
| 窗口组件创建完成（自进程创建起，精确） | 166 ms | 46 ms |
| 首个快照帧（10 次中位数） | 286 ms | 91 ms（近似，见下） |
| 常驻 CPU，1 秒采样间隔，20 秒 | 0.08% | 0.08% |
| 工作集 / 私有提交 | 92.5 / 131.6 MB | 35.8 / 21.0 MB |

software 渲染器不支持 Slint 的渲染通知，首帧时间是“数据提交后事件循环空闲”的近似打点，会略早于真正上屏；但仅窗口组件创建一项就快了 120 ms，结论不受影响。

测量脚本：`scripts/measure-startup.ps1`（启动耗时）、`scripts/measure-idle.ps1`（常驻开销）。

## 决定

- 默认 cargo feature `renderer-software`。
- 保留 `renderer-femtovg`：`cargo build -p tml-app --release --no-default-features --features renderer-femtovg`。

## 取舍

femtovg 的额外开销几乎全部来自 OpenGL 上下文与显卡驱动的加载，冷启动慢约 120–200 ms、内存多约 100 MB。software 的代价是：重绘全部在 CPU 上完成，窗口很大或动画很多时 CPU 占用会上升。当前界面每秒更新一次，实测常驻占用与 femtovg 持平。将来如果加入持续动画，需要用 `measure-idle.ps1` 重新评估。
