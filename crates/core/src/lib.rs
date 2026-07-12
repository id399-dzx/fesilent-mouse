//! ms-core —— 平台无关核心层：控制权状态机、屏幕布局/坐标映射、会话管理、
//! 心跳/重连状态机、剪贴板与文件传输逻辑（DEVELOPMENT.md §2.2）。

pub mod clip_sync;
pub mod control;
pub mod engine;
pub mod input_map;
pub mod kb_layout;
pub mod layout;
pub mod pipeline;
pub mod safety;
pub mod shadow;
