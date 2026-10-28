# 0001 直接调用 Windows API 采集，而不是使用 sysinfo

状态：已接受（2026-10-27）

## 背景

本项目的首要目标是冷启动快、常驻开销低。进程快照每秒采集一次，采集路径的开销会直接体现在本程序自身的 CPU 占用上。

## 决定

`core` crate 通过 `windows` crate 直接调用系统接口：

- 进程快照：`NtQuerySystemInformation(SystemProcessInformation)`，一次系统调用拿到全部进程的 CPU 时间、专用工作集、I/O 字节数、父 PID 和创建时间。
- 系统级指标：`GetSystemTimes`、`GlobalMemoryStatusEx`、PDH 英文计数器（磁盘）、`GetIfTable2`（网络）。
- 关键进程判定：`NtQueryInformationProcess(ProcessBreakOnTermination)`。

UI 只依赖 `core` 暴露的快照类型，不直接接触 Win32。

## 取舍

- 放弃了 sysinfo 的跨平台能力和现成封装；换来单次系统调用的快照、可控的字段集合（专用工作集、父进程创建时间校验）和更低的采集开销。
- 未公开的 `NtQuery*` 结构需要自己声明，未来 Windows 版本若改变布局需要跟进（这些结构多年来保持兼容）。

## 已知近似

进程列表的“磁盘速率”取自 `SYSTEM_PROCESS_INFORMATION` 的 `ReadTransferCount + WriteTransferCount`。这是进程全部 I/O 字节数，也包括命名管道、部分网络等非磁盘 I/O，因此会比原生任务管理器（基于 ETW 磁盘事件）偏高。要精确统计需要 ETW 内核会话（需管理员权限）。提权运行时已改用 ETW 磁盘事件，见 [ADR-0003](0003-etw-disk-io.md)；未提权时仍是这个近似值，列头显示“磁盘 ≈”。“单进程网络”仍推迟到后续版本。
