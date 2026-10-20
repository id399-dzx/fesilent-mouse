//! 设备工作台：布局与连接围绕两台电脑组织，设置草稿只在明确操作后应用。

use super::*;
use crate::status::ConnectionPhase;
use egui::{Align, Color32, FontId, Layout, RichText, Stroke, Vec2};
use ms_core::control::ControlState;

#[path = "device_catalog.rs"]
mod device_catalog;
#[path = "local_network.rs"]
mod local_network;
#[path = "theme.rs"]
mod theme;
#[path = "workspace.rs"]
mod workspace;
use theme::*;
use workspace::Side;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Section {
    Connection,
    Controls,
    Sharing,
    About,
}

pub(super) fn install_theme(ctx: &egui::Context) {
    theme::install(ctx);
}

fn nav_button(ui: &mut egui::Ui, selected: bool, label: &str, wide: bool) -> bool {
    let button = egui::Button::new(RichText::new(label).color(if selected {
        Color32::WHITE
    } else {
        MUTED
    }))
    .fill(if selected {
        PRIMARY
    } else {
        Color32::TRANSPARENT
    })
    .stroke(Stroke::NONE)
    .corner_radius(30);
    if wide {
        ui.add_sized([ui.available_width(), 40.0], button).clicked()
    } else {
        ui.add(button).clicked()
    }
}

fn brand(ui: &mut egui::Ui) {
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(Vec2::splat(26.0), egui::Sense::hover());
        let painter = ui.painter();
        let c = rect.center();
        let points = [
            c + Vec2::new(0.0, -10.0),
            c + Vec2::new(9.0, -5.0),
            c + Vec2::new(9.0, 5.0),
            c + Vec2::new(0.0, 10.0),
            c + Vec2::new(-9.0, 5.0),
            c + Vec2::new(-9.0, -5.0),
        ];
        for i in 0..6 {
            painter.line_segment(
                [points[i], points[(i + 1) % 6]],
                Stroke::new(1.6_f32, PURPLE),
            );
        }
        for i in [1, 3, 5] {
            painter.line_segment([c, points[i]], Stroke::new(1.5_f32, PURPLE));
        }
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            ui.label(RichText::new("Fesilent").size(19.0).strong());
            ui.label(
                RichText::new("MOUSE WORKSPACE")
                    .size(9.0)
                    .color(MUTED)
                    .extra_letter_spacing(0.8),
            );
        });
    });
}

impl App {
    pub(super) fn draw(&mut self, ctx: &egui::Context) {
        if self.quitting || self.pending_forget_device.is_some() || self.forget_all_pending {
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.spinner();
                ui.label(if self.quitting {
                    "正在释放输入并退出…"
                } else {
                    "正在撤销本机配对，请稍候…"
                });
            });
            ctx.request_repaint_after(Duration::from_millis(100));
            return;
        }
        self.set_toggle_text(self.handle.is_running());
        if ctx.input_mut(|input| input.consume_key(egui::Modifiers::COMMAND, egui::Key::W)) {
            self.hide_requested = true;
        }
        // 绘制期间只用配置快照，不持有跨线程锁。
        let mut cfg = self.config.lock().map(|c| c.clone()).unwrap_or_default();
        let previous_clipboard = cfg.clipboard;
        // 只有真实完成TLS/SAS的连接才能进入已保存清单；发现广播不授予身份。
        let mut registered = false;
        if self.handle.status.connected()
            && let Some(fp) = self.handle.status.peer_fingerprint()
        {
            let hex = ms_core::topology::DeviceId::from_fingerprint(fp).to_string();
            if cfg.devices.record(&hex).is_none() {
                let name = format!("设备 {}", cfg.devices.records.len() + 1);
                let legacy_side = match cfg.side.as_str() {
                    "left" => ms_proto::Edge::Left,
                    "top" => ms_proto::Edge::Top,
                    "bottom" => ms_proto::Edge::Bottom,
                    _ => ms_proto::Edge::Right,
                };
                match cfg.devices.upsert_authenticated(
                    fp,
                    &name,
                    cfg.connect.as_deref(),
                    legacy_side,
                ) {
                    Ok(()) => {
                        if cfg.devices.active_target.is_none() {
                            let _ = cfg.devices.select(&hex);
                        }
                        if cfg.devices.active_target.is_none() {
                            self.connection_notice = Some(
                                "新设备已保存，请在我的设备中设置空闲方位并选择连接目标。".into(),
                            );
                        }
                        registered = true;
                    }
                    Err(error) => self.form_error = Some(error),
                }
            }
        }
        let mut dirty = registered;
        let wide = ctx.screen_rect().width() >= 850.0;

        self.draw_footer(ctx, &cfg);
        if wide {
            egui::SidePanel::left("workspace_navigation")
                .exact_width(182.0)
                .resizable(false)
                .frame(
                    egui::Frame::new()
                        .fill(Color32::from_rgb(252, 251, 253))
                        .inner_margin(18),
                )
                .show(ctx, |ui| {
                    brand(ui);
                    ui.add_space(32.0);
                    eyebrow(ui, "WORKSPACE");
                    ui.add_space(4.0);
                    self.draw_navigation(ui, true);
                    ui.with_layout(Layout::bottom_up(Align::LEFT), |ui| {
                        ui.label(
                            RichText::new("关闭窗口后继续在托盘运行")
                                .size(11.0)
                                .color(MUTED),
                        );
                        ui.label(RichText::new("局域网直连").small().color(PURPLE));
                        ui.add_space(12.0);
                    });
                });
        } else {
            egui::TopBottomPanel::top("workspace_navigation_compact")
                .frame(
                    egui::Frame::new()
                        .fill(Color32::from_rgb(252, 251, 253))
                        .inner_margin(egui::Margin::symmetric(22, 13)),
                )
                .show(ctx, |ui| {
                    brand(ui);
                    ui.add_space(9.0);
                    ui.horizontal_wrapped(|ui| self.draw_navigation(ui, false));
                });
        }

        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(BACKGROUND)
                    .inner_margin(egui::Margin::symmetric(26, 23)),
            )
            .show(ctx, |ui| {
                page_background(ui);
                egui::ScrollArea::vertical()
                    .id_salt(("workspace_scroll", self.section as u8))
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        let (eyebrow_text, title, subtitle) = match self.section {
                            Section::Connection => (
                                "DEVICE STUDIO / 设备互联",
                                "你的工作空间",
                                "设备间共享键鼠、剪贴板与文件。",
                            ),
                            Section::Sharing => (
                                "SHARED SPACE / 内容流转",
                                "共享记录",
                                "查看文件进度，找回刚刚复制的内容。",
                            ),
                            Section::Controls => (
                                "CUSTOMIZE / 使用偏好",
                                "顺手的每一次切换",
                                "调整键鼠手感，设置会自动保存。",
                            ),
                            Section::About => (
                                "FESILENT / 关于",
                                "关于与诊断",
                                "连接详情、运行日志与应用信息。",
                            ),
                        };
                        eyebrow(ui, eyebrow_text);
                        ui.add_space(4.0);
                        ui.label(RichText::new(title).size(27.0).strong());
                        ui.label(RichText::new(subtitle).color(MUTED));
                        ui.add_space(17.0);
                        if let Some(error) = self.form_error.clone() {
                            notice(ui, &error, ERROR);
                            ui.add_space(12.0);
                        }
                        if self.section != Section::Connection
                            && self.handle.status.last_error().is_some()
                            && let Some(error) = self.handle.status.last_error()
                        {
                            notice(ui, &error, ERROR);
                            if ui.link("查看连接与设备").clicked() {
                                self.section = Section::Connection;
                            }
                            ui.add_space(12.0);
                        }
                        match self.section {
                            Section::Connection => self.draw_connection(ui, &mut cfg, &mut dirty),
                            Section::Controls => self.draw_controls(ui, &mut cfg, &mut dirty),
                            Section::Sharing => self.draw_sharing(ui, &mut cfg, &mut dirty),
                            Section::About => self.draw_about(ui, &mut cfg, &mut dirty),
                        }
                        ui.add_space(12.0);
                    });
            });

        if dirty
            && self.store_config(&cfg)
            && previous_clipboard != cfg.clipboard
            && self.handle.is_running()
        {
            // 剪贴板工作任务按会话建立，开关变化需要安全重连。
            self.handle.stop();
            self.handle.start();
            self.settings_notice = Some((
                "共享设置已保存，正在重新连接".into(),
                std::time::Instant::now(),
            ));
        }
        self.draw_pairing_dialog(ctx);
        self.draw_forget_confirmation(ctx);
        ctx.request_repaint_after(Duration::from_millis(250));
    }

    fn draw_navigation(&mut self, ui: &mut egui::Ui, wide: bool) {
        for (section, label) in [
            (Section::Connection, "工作空间"),
            (Section::Sharing, "共享记录"),
            (Section::Controls, "使用偏好"),
            (Section::About, "关于与诊断"),
        ] {
            if nav_button(ui, self.section == section, label, wide) {
                self.section = section;
            }
            if wide {
                ui.add_space(4.0);
            }
        }
    }

    fn draw_footer(&mut self, ctx: &egui::Context, cfg: &Config) {
        egui::TopBottomPanel::bottom("workspace_footer")
            .frame(
                egui::Frame::new()
                    .fill(Color32::from_rgb(253, 252, 254))
                    .inner_margin(egui::Margin::symmetric(23, 10)),
            )
            .show(ctx, |ui| {
                let items = transfers::center().snapshot();
                let active: Vec<_> = items
                    .iter()
                    .filter(|item| matches!(item.phase, Phase::Active | Phase::Confirm))
                    .collect();
                let pending = active
                    .iter()
                    .filter(|item| item.phase == Phase::Confirm)
                    .count();
                ui.horizontal_wrapped(|ui| {
                    if pending > 0 {
                        ui.label(
                            RichText::new(format!("{pending} 项文件等待发送确认"))
                                .small()
                                .color(AMBER),
                        );
                    } else if !active.is_empty() {
                        let total = active
                            .iter()
                            .fold(0_u64, |sum, item| sum.saturating_add(item.total));
                        let done = active
                            .iter()
                            .fold(0_u64, |sum, item| sum.saturating_add(item.transferred));
                        ui.label(
                            RichText::new(format!("{} 个文件传输中", active.len()))
                                .small()
                                .color(PURPLE),
                        );
                        if total > 0 {
                            ui.add(
                                egui::ProgressBar::new(
                                    (done as f32 / total as f32).clamp(0.0, 1.0),
                                )
                                .desired_width(115.0)
                                .fill(PURPLE),
                            );
                        }
                    } else {
                        let sharing = if cfg.clipboard {
                            "剪贴板同步已开启"
                        } else {
                            "剪贴板同步已关闭"
                        };
                        ui.label(RichText::new(sharing).size(11.0).color(MUTED));
                        ui.label(RichText::new("· 暂无文件传输").size(11.0).color(MUTED));
                    }
                    if ui
                        .small_button(if pending > 0 {
                            "查看并确认"
                        } else {
                            "共享记录"
                        })
                        .clicked()
                    {
                        self.section = Section::Sharing;
                    }
                    if let Some((message, when)) = &self.settings_notice
                        && when.elapsed() < Duration::from_secs(4)
                    {
                        ui.label(RichText::new(message).size(11.0).color(GREEN));
                    }
                });
            });
    }

    pub(super) fn store_config(&mut self, cfg: &Config) -> bool {
        let mut normalized = cfg.clone();
        normalized.normalize();
        if let Some(active) = normalized.devices.active_target.clone() {
            let side = match normalized.side.as_str() {
                "left" => ms_proto::Edge::Left,
                "top" => ms_proto::Edge::Top,
                "bottom" => ms_proto::Edge::Bottom,
                _ => ms_proto::Edge::Right,
            };
            if let Err(error) = normalized.devices.set_placement(
                &active,
                Some(crate::devices::DevicePlacement {
                    side,
                    offset_px: normalized.display_offset,
                }),
            ) {
                self.form_error = Some(error);
                return false;
            }
        }
        if let Err(error) = normalized.devices.validate() {
            self.form_error = Some(error);
            return false;
        }
        let cfg = &normalized;
        if !self.ui_preview
            && let Err(error) = config::save(cfg)
        {
            self.form_error = Some(format!("设置保存失败：{error}"));
            return false;
        }
        match self.config.lock() {
            Ok(mut shared) => {
                *shared = cfg.clone();
                self.settings_notice = Some(("设置已保存".into(), std::time::Instant::now()));
                true
            }
            Err(_) => {
                self.form_error = Some("暂时无法更新设置，请重试。".to_owned());
                false
            }
        }
    }

    fn apply_connection(&mut self, cfg: &mut Config, restart: bool) -> bool {
        if transfers::center().has_active() {
            self.form_error = Some("请先完成或取消文件传输，再更改连接。".into());
            return false;
        }
        let address = if self.is_mac_ui() && !cfg.mac_control {
            // Mac 固定等待接入，不再暴露容易造成两端主动拨号的地址表单。
            None
        } else if self.manual_connection {
            match config::normalize_peer_address(&self.peer_ip) {
                Ok(Some(address)) => Some(address),
                Ok(None) => {
                    self.form_error = Some("请输入对端的 IP，或选择自动发现。".into());
                    return false;
                }
                Err(error) => {
                    self.form_error = Some(error);
                    return false;
                }
            }
        } else {
            None
        };
        cfg.connect = address;
        if let Some(target) = &cfg.devices.active_target
            && let Some(record) = cfg
                .devices
                .records
                .iter_mut()
                .find(|record| &record.fingerprint == target)
        {
            record.address = cfg.connect.clone();
        }
        if !self.store_config(cfg) {
            return false;
        }
        if let Some(address) = &cfg.connect {
            self.peer_ip = address.clone();
        }
        self.form_error = None;
        self.handle.status.clear_error();
        if restart && self.handle.is_running() {
            self.handle.stop();
        }
        self.handle.start();
        self.connection_notice = Some(
            if self.is_mac_ui() && !cfg.mac_control {
                "正在等待已配对的电脑连接。"
            } else {
                "连接设置已应用。"
            }
            .into(),
        );
        true
    }

    fn draw_connection_status(&mut self, ui: &mut egui::Ui, cfg: &mut Config) {
        let running = self.handle.is_running();
        let phase = self.handle.status.phase();
        let permission = daemon::input_permission_granted();
        let needs_permission = self.is_mac_ui() && !permission && !self.ui_preview;
        let (title, detail, color) = if needs_permission {
            (
                "需要辅助功能权限".to_owned(),
                "在这台 Mac 上允许 Fesilent Mouse 控制键盘与鼠标。".to_owned(),
                AMBER,
            )
        } else if self.handle.status.connected() && running {
            let control = if self.handle.status.local_locked() {
                "已临时锁在本机"
            } else {
                match self.handle.status.control_state() {
                    ControlState::Remote => "当前控制对端设备",
                    ControlState::Controlled => "正在接收对端的键鼠输入",
                    ControlState::Local => "当前使用本机键鼠",
                }
            };
            let quality = self
                .handle
                .status
                .rtt_ms()
                .map(|rtt| format!(" · 延迟 {rtt} ms"))
                .unwrap_or_default();
            (
                "设备已连接".to_owned(),
                format!(
                    "{}{control}{quality}",
                    cfg.devices
                        .active()
                        .map(|d| format!("{} · ", d.name))
                        .unwrap_or_default()
                ),
                GREEN,
            )
        } else {
            let (title, detail, color) = match phase {
                ConnectionPhase::Discovering => (
                    "正在搜索设备",
                    "请确认两台电脑连接同一局域网，Mac 已开始等待接入。",
                    PURPLE,
                ),
                ConnectionPhase::Listening => (
                    "等待对端接入",
                    "在 Windows 上打开 Fesilent Mouse，选择连接这台 Mac。",
                    PURPLE,
                ),
                ConnectionPhase::Connecting => ("正在连接设备", "正在与对端建立加密连接。", PURPLE),
                ConnectionPhase::Pairing => (
                    "请确认配对码",
                    "核对两台电脑上的六位配对码，确认是你要连接的设备。",
                    AMBER,
                ),
                ConnectionPhase::Reconnecting => (
                    "连接中断，正在重连",
                    "确认对端仍在运行，或展开连接管理检查地址。",
                    AMBER,
                ),
                ConnectionPhase::PermissionRequired => (
                    "需要辅助功能权限",
                    "请在 Mac 的系统设置中完成授权后重试。",
                    AMBER,
                ),
                ConnectionPhase::Failed => (
                    "暂时无法连接",
                    "检查下面的提示，处理后点击重新连接。",
                    ERROR,
                ),
                _ if running => ("正在准备连接", "正在准备设备与网络服务。", PURPLE),
                _ => (
                    "尚未连接设备",
                    "准备好后连接设备，即可开始共享键鼠与剪贴板。",
                    MUTED,
                ),
            };
            (title.to_owned(), detail.to_owned(), color)
        };
        let detail = if phase == ConnectionPhase::Reconnecting {
            self.handle
                .status
                .retry_seconds()
                .map(|seconds| format!("{detail} · {seconds} 秒后重试"))
                .unwrap_or(detail)
        } else {
            detail
        };
        card(ui, "", "", |ui| {
            ui.horizontal_wrapped(|ui| {
                let (dot, _) = ui.allocate_exact_size(Vec2::splat(12.0), egui::Sense::hover());
                ui.painter().circle_filled(dot.center(), 4.0, color);
                ui.vertical(|ui| {
                    ui.label(RichText::new(&title).size(17.0).strong());
                    ui.label(RichText::new(&detail).small().color(MUTED));
                });
            });
            ui.add_space(9.0);
            ui.horizontal_wrapped(|ui| {
                if needs_permission {
                    if primary_button(ui, "打开系统设置并授权").clicked() {
                        daemon::open_input_permission_settings();
                    }
                } else {
                    let label = if running {
                        if self.handle.status.connected() {
                            "暂停连接"
                        } else {
                            "取消连接"
                        }
                    } else if self.handle.status.last_error().is_some() {
                        "重新连接"
                    } else if self.is_mac_ui() {
                        "等待对端接入"
                    } else {
                        "连接设备"
                    };
                    let enabled = running || !transfers::center().has_active();
                    if ui
                        .add_enabled_ui(enabled, |ui| primary_button(ui, label))
                        .inner
                        .clicked()
                    {
                        if running {
                            self.handle.stop();
                        } else {
                            self.apply_connection(cfg, false);
                        }
                    }
                }
                if self.handle.status.connected()
                    && secondary_button(
                        ui,
                        if self.handle.status.local_locked() {
                            "解除临时锁定"
                        } else {
                            "暂时锁在本机"
                        },
                    )
                    .clicked()
                {
                    self.handle
                        .status
                        .request_local_lock(!self.handle.status.local_locked());
                    if self.ui_preview {
                        self.handle
                            .status
                            .set_local_locked(!self.handle.status.local_locked());
                    }
                }
                if let Some(address) = self.handle.status.peer_address() {
                    ui.label(RichText::new(address).small().monospace().color(MUTED));
                } else if running
                    && !self.handle.status.connected()
                    && !self.is_mac_ui()
                    && ui.link("找不到设备？检查连接方式").clicked()
                {
                    ui.ctx().data_mut(|data| {
                        data.insert_temp(egui::Id::new("show_connection_details"), true)
                    });
                }
            });
            if let Some(error) = self.handle.status.last_error() {
                ui.add_space(7.0);
                ui.label(RichText::new(error).small().color(ERROR));
            }
        });
    }

    fn draw_connection(&mut self, ui: &mut egui::Ui, cfg: &mut Config, dirty: &mut bool) {
        self.draw_connection_status(ui, cfg);
        ui.add_space(15.0);
        self.draw_local_connection_addresses(ui, cfg);
        if self.handle.status.connected() && !cfg.onboarding_done {
            ui.add_space(15.0);
            card(ui, "连接成功，试试这三步", "", |ui| {
                ui.label("1. 朝对端所在边缘继续推鼠标，即可跨屏。");
                ui.label(format!(
                    "2. {} 切换/返回；{} 临时锁在本机。",
                    cfg.switch_shortcut.label(),
                    cfg.lock_shortcut.label()
                ));
                ui.label(
                    "3. 复制文件后跨屏，在对端粘贴；关闭窗口仍在托盘运行，托盘退出才结束共享。",
                );
                if secondary_button(ui, "知道了").clicked() {
                    cfg.onboarding_done = true;
                    *dirty = true;
                }
            });
        }
        ui.add_space(15.0);
        card(ui, "", "", |ui| {
            eyebrow(ui, "DEVICE CANVAS");
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("设备排列").size(17.0).strong());
                ui.label(
                    RichText::new(if self.is_mac_ui() && !cfg.mac_control {
                        "接收模式 · 连接关系示意"
                    } else {
                        "拖动对端设备，吸附到任意一侧"
                    })
                    .small()
                    .color(MUTED),
                );
            });
            self.draw_device_workspace(ui, cfg, dirty);
            ui.separator();
            ui.add_space(5.0);
            if self.is_mac_ui() && !cfg.mac_control {
                ui.label(
                    RichText::new("这台 Mac 默认接收控制；可在使用偏好中开启 Mac 主控。")
                        .small()
                        .color(MUTED),
                );
            } else {
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new("对端的位置").small().color(MUTED));
                    for side in Side::ALL {
                        let selected = cfg.side == side.key();
                        if ui
                            .add(
                                egui::Button::new(RichText::new(side.label()).color(if selected {
                                    Color32::WHITE
                                } else {
                                    MUTED
                                }))
                                .fill(if selected { PRIMARY } else { BACKGROUND })
                                .stroke(Stroke::NONE)
                                .corner_radius(30)
                                .min_size(Vec2::new(37.0, 30.0)),
                            )
                            .clicked()
                        {
                            cfg.side = side.key().to_owned();
                            *dirty = true;
                        }
                    }
                });
                let side = Side::from_config(&cfg.side);
                ui.label(
                    RichText::new(format!(
                        "向{}边缘继续推鼠标进入对端；向{}侧推回本机。",
                        side.label(),
                        side.opposite().label()
                    ))
                    .small()
                    .color(MUTED),
                );
            }
        });
        ui.add_space(15.0);
        self.draw_connection_details(ui, cfg);
        ui.add_space(15.0);
        self.draw_device_catalog(ui, cfg, dirty);
    }

    fn draw_local_connection_addresses(&mut self, ui: &mut egui::Ui, cfg: &Config) {
        let port = cfg.port.unwrap_or(config::DEFAULT_PEER_PORT);
        let addresses = local_network::connection_addresses(&self.local_ips, port);
        card(
            ui,
            "本机连接地址",
            "自动识别已连接网络，地址变化后自动更新。可复制到另一台电脑手动连接。",
            |ui| {
                if addresses.is_empty() {
                    ui.label(
                        RichText::new("暂未识别到局域网 IPv4。连接 Wi-Fi 或网线后会自动更新。")
                            .small()
                            .color(MUTED),
                    );
                }
                for address in &addresses {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new(address).monospace().size(17.0).strong());
                        if secondary_button(ui, "复制地址")
                            .on_hover_text("复制本机 IPv4 和监听端口")
                            .clicked()
                        {
                            ui.ctx().copy_text(address.clone());
                            self.connection_notice = Some(format!("已复制 {address}"));
                        }
                    });
                }
                if !self.handle.is_running() {
                    ui.label(
                        RichText::new("启用连接后，其他电脑才能通过此地址接入。")
                            .small()
                            .color(MUTED),
                    );
                }
                if let Some(message) = &self.connection_notice {
                    ui.label(RichText::new(message).small().color(GREEN));
                }
            },
        );
    }

    fn draw_connection_details(&mut self, ui: &mut egui::Ui, cfg: &mut Config) {
        card(ui, "", "", |ui| {
            let force_open = ui.ctx().data_mut(|data| {
                let id = egui::Id::new("show_connection_details");
                let open = data.get_temp::<bool>(id).unwrap_or(false);
                data.remove::<bool>(id);
                open
            });
            egui::CollapsingHeader::new(if self.is_mac_ui() {
                "本机地址与设备管理"
            } else {
                "连接管理"
            })
            .id_salt("connection_details")
            .default_open(self.manual_connection || self.is_mac_ui())
            .open(if force_open { Some(true) } else { None })
            .show(ui, |ui| {
                if !self.is_mac_ui() || cfg.mac_control {
                    ui.horizontal_wrapped(|ui| {
                        ui.selectable_value(&mut self.manual_connection, false, "自动发现");
                        ui.selectable_value(&mut self.manual_connection, true, "手动 IP");
                    });
                    ui.add_space(5.0);
                    if self.manual_connection {
                        ui.label(RichText::new("对端的局域网 IPv4").small().color(MUTED));
                        let response = ui.add_sized(
                            [ui.available_width().min(370.0), 35.0],
                            egui::TextEdit::singleline(&mut self.peer_ip)
                                .hint_text("例如 192.168.1.20，端口可省略")
                                .margin(egui::Margin::symmetric(11, 8)),
                        );
                        if response.lost_focus()
                            && let Ok(Some(address)) = config::normalize_peer_address(&self.peer_ip)
                        {
                            self.peer_ip = address;
                        }
                        ui.label(
                            RichText::new(
                                "端口省略或留空时自动补为 53728，已填写的有效端口会保留。",
                            )
                            .small()
                            .color(MUTED),
                        );
                    } else {
                        ui.label(
                            RichText::new("搜索同一网络中的设备；接收端使用自动发现并启用连接。")
                                .small()
                                .color(MUTED),
                        );
                    }
                    let draft = if self.manual_connection {
                        match config::normalize_peer_address(&self.peer_ip) {
                            Ok(Some(address)) => cfg.connect.as_deref() != Some(address.as_str()),
                            _ => true,
                        }
                    } else {
                        cfg.connect.is_some()
                    };
                    ui.horizontal_wrapped(|ui| {
                        let label = if self.handle.is_running() {
                            "应用并重新连接"
                        } else {
                            "保存并连接设备"
                        };
                        if ui
                            .add_enabled_ui(!transfers::center().has_active(), |ui| {
                                secondary_button(ui, label)
                            })
                            .inner
                            .clicked()
                        {
                            self.apply_connection(cfg, true);
                        }
                        if draft {
                            ui.label(RichText::new("连接更改尚未应用").small().color(AMBER));
                        }
                    });
                    if transfers::center().has_active() {
                        ui.label(
                            RichText::new("传输期间暂不能更改连接。")
                                .small()
                                .color(AMBER),
                        );
                    }
                    ui.add_space(8.0);
                    ui.separator();
                }
                if let Some(message) = &self.connection_notice {
                    ui.label(RichText::new(message).small().color(GREEN));
                }
                ui.add_space(8.0);
                ui.separator();
                ui.add_space(7.0);
                ui.horizontal_wrapped(|ui| {
                    ui.label(
                        RichText::new("更换电脑或需要重新配对？")
                            .small()
                            .color(MUTED),
                    );
                    if ui
                        .add_enabled(
                            !transfers::center().has_active(),
                            egui::Button::new("忘记已配对设备"),
                        )
                        .clicked()
                    {
                        self.confirm_forget_devices = true;
                    }
                });
            });
        });
    }

    fn draw_device_workspace(&mut self, ui: &mut egui::Ui, cfg: &mut Config, dirty: &mut bool) {
        let editable = !self.is_mac_ui() || cfg.mac_control;
        // Mac 的 cfg.side 不是 Windows 传来的布局，不能把它当成已同步的位置。
        let mut side = if editable {
            Side::from_config(&cfg.side)
        } else {
            Side::Right
        };
        let height = if side.vertical() { 360.0 } else { 244.0 };
        let (canvas, _) = ui.allocate_exact_size(
            Vec2::new(ui.available_width(), height),
            egui::Sense::hover(),
        );
        let painter = ui.painter().with_clip_rect(canvas);
        painter.rect_filled(canvas, 18, Color32::from_rgb(253, 252, 254));
        for x in (0..(canvas.width() as i32)).step_by(22) {
            for y in (0..(canvas.height() as i32)).step_by(22) {
                painter.circle_filled(
                    canvas.left_top() + Vec2::new(x as f32 + 10.0, y as f32 + 10.0),
                    0.65,
                    Color32::from_rgb(229, 224, 236),
                );
            }
        }
        glow(
            &painter,
            canvas.center() + Vec2::new(75.0, -15.0),
            140.0,
            Color32::from_rgba_unmultiplied(190, 184, 255, 55),
        );
        let rects = workspace::device_rects(canvas, side);
        let drag_id = ui.id().with("mac_device_drag_origin");
        let response = ui.interact(
            rects.mac.expand2(Vec2::new(7.0, 22.0)),
            ui.id().with("mac_device"),
            if editable {
                egui::Sense::drag()
            } else {
                egui::Sense::hover()
            },
        );
        if editable && response.drag_started() {
            ui.ctx()
                .data_mut(|data| data.insert_temp(drag_id, rects.mac.center()));
        }
        let mut mac_rect = rects.mac;
        if editable
            && response.dragged()
            && let Some(pointer) = response.interact_pointer_pos()
        {
            mac_rect = egui::Rect::from_center_size(pointer, rects.mac.size());
            side = workspace::snapped_side(rects.windows.center(), pointer, side);
            painter.rect_stroke(
                workspace::device_rects(canvas, side).mac,
                13,
                Stroke::new(1.0_f32, PURPLE),
                egui::StrokeKind::Inside,
            );
        }
        if editable && response.drag_stopped() {
            if let Some(pointer) = response.interact_pointer_pos() {
                let origin = ui.ctx().data(|data| data.get_temp::<egui::Pos2>(drag_id));
                if origin.is_some_and(|origin| origin.distance(pointer) > 12.0) {
                    cfg.side = workspace::snapped_side(
                        rects.windows.center(),
                        pointer,
                        Side::from_config(&cfg.side),
                    )
                    .key()
                    .to_owned();
                    *dirty = true;
                }
            }
            ui.ctx().data_mut(|data| data.remove::<egui::Pos2>(drag_id));
        }
        if editable {
            response.clone().on_hover_cursor(egui::CursorIcon::Grab);
        }
        let connected = self.handle.status.connected();
        let state = self.handle.status.control_state();
        let local_active = connected && state != ControlState::Remote;
        let peer_active = connected && state == ControlState::Remote;
        draw_screen(
            &painter,
            rects.windows,
            if self.is_mac_ui() { "Mac" } else { "Windows" },
            "本机",
            local_active,
            self.is_mac_ui(),
        );
        let peer_name = cfg
            .devices
            .active()
            .map(|d| d.name.as_str())
            .unwrap_or("对端设备");
        draw_screen(
            &painter,
            mac_rect,
            peer_name,
            "当前连接目标",
            peer_active,
            !self.is_mac_ui(),
        );
        if editable {
            painter.line_segment(
                workspace::crossing_segment(rects.windows, Side::from_config(&cfg.side)),
                Stroke::new(4.0_f32, PURPLE),
            );
        }
        if !side.vertical() && !response.dragged() {
            let start = if side == Side::Left {
                rects.windows.left_center() - Vec2::new(13.0, 0.0)
            } else {
                rects.windows.right_center() + Vec2::new(13.0, 0.0)
            };
            let end = if side == Side::Left {
                mac_rect.right_center() + Vec2::new(13.0, 0.0)
            } else {
                mac_rect.left_center() - Vec2::new(13.0, 0.0)
            };
            painter.arrow(start, end - start, Stroke::new(1.5_f32, PURPLE));
        }
    }

    fn draw_controls(&mut self, ui: &mut egui::Ui, cfg: &mut Config, dirty: &mut bool) {
        card(
            ui,
            "控制方式",
            "Mac 主控需手动开启；仅接收模式不会触发本机边缘跨屏。",
            |ui| {
                if self.is_mac_ui()
                    && ui
                        .checkbox(&mut cfg.mac_control, "允许这台 Mac 主动控制对端")
                        .changed()
                {
                    cfg.no_takeover = false;
                    *dirty = true;
                }
                if !self.is_mac_ui() || cfg.mac_control {
                    if ui
                        .checkbox(&mut cfg.no_takeover, "这台电脑只接收控制")
                        .changed()
                    {
                        *dirty = true;
                    }
                } else {
                    ui.label(
                        RichText::new("当前仅接收控制，屏幕边缘不会主动跨屏。")
                            .small()
                            .color(MUTED),
                    );
                }
            },
        );
        ui.add_space(15.0);
        self.draw_shortcuts(ui, cfg, dirty);
        ui.add_space(15.0);
        if !self.is_mac_ui() || cfg.mac_control {
            card(
                ui,
                "屏幕切换手感",
                "先选择习惯的手感，也可展开高级设置微调。",
                |ui| {
                    let presets = [
                        ("轻触", 30.0, 20.0),
                        ("标准", 100.0, 60.0),
                        ("防误触", 180.0, 120.0),
                    ];
                    ui.horizontal_wrapped(|ui| {
                        for (label, enter, leave) in presets {
                            let selected = cfg.edge_push == enter && cfg.return_push == leave;
                            if ui
                                .add(
                                    egui::Button::new(RichText::new(label).color(if selected {
                                        Color32::WHITE
                                    } else {
                                        INK
                                    }))
                                    .fill(if selected { PRIMARY } else { BACKGROUND })
                                    .stroke(Stroke::NONE)
                                    .corner_radius(30),
                                )
                                .clicked()
                            {
                                cfg.edge_push = enter;
                                cfg.return_push = leave;
                                *dirty = true;
                            }
                        }
                        if !presets.iter().any(|(_, enter, leave)| {
                            cfg.edge_push == *enter && cfg.return_push == *leave
                        }) {
                            ui.label(RichText::new("自定义").small().color(PURPLE));
                        }
                    });
                    ui.add_space(3.0);
                    ui.label(
                        RichText::new(
                            "轻触：更快跨屏   标准：平衡手感   防误触：更容易操作边缘菜单",
                        )
                        .small()
                        .color(MUTED),
                    );
                    egui::CollapsingHeader::new("高级 · 边缘推挤距离").show(ui, |ui| {
                        ui.label(
                            RichText::new("贴住边缘后继续推够此距离才切换；0 表示触边即切换。")
                                .small()
                                .color(MUTED),
                        );
                        if ui
                            .add(
                                egui::Slider::new(&mut cfg.edge_push, 0.0..=300.0)
                                    .suffix(" px")
                                    .text("进入对端"),
                            )
                            .changed()
                        {
                            *dirty = true;
                        }
                        if ui
                            .add(
                                egui::Slider::new(&mut cfg.return_push, 0.0..=300.0)
                                    .suffix(" px")
                                    .text("返回本机"),
                            )
                            .changed()
                        {
                            *dirty = true;
                        }
                    });
                    let side = Side::from_config(&cfg.side);
                    ui.add_space(5.0);
                    notice(
                        ui,
                        &format!(
                            "试一试：向{}边缘继续推鼠标，进入对端；再向{}侧推回。",
                            side.label(),
                            side.opposite().label()
                        ),
                        PURPLE,
                    );
                },
            );
            ui.add_space(15.0);
            card(
                ui,
                "对端滚轮速度",
                "只调整上下滚动，与滚动方向独立。",
                |ui| {
                    cfg.scroll_speed = config::normalize_scroll_speed(cfg.scroll_speed);
                    if ui
                        .add(
                            egui::Slider::new(&mut cfg.scroll_speed, 1.0..=3.0)
                                .step_by(0.1)
                                .fixed_decimals(1)
                                .suffix(" ×"),
                        )
                        .changed()
                    {
                        cfg.scroll_speed = config::normalize_scroll_speed(cfg.scroll_speed);
                        *dirty = true;
                    }
                    if ui
                        .checkbox(&mut cfg.invert_scroll, "反转滚轮方向")
                        .changed()
                    {
                        *dirty = true;
                    }
                    if secondary_button(ui, "恢复默认手感").clicked() {
                        cfg.scroll_speed = 1.0;
                        cfg.invert_scroll = false;
                        cfg.edge_push = 100.0;
                        cfg.return_push = 60.0;
                        *dirty = true;
                    }
                    ui.label(
                        RichText::new("Ctrl / Command 按键交换请在 Mac 端设置。")
                            .small()
                            .color(MUTED),
                    );
                },
            );
        }
        ui.add_space(15.0);
        self.draw_startup(ui, cfg, dirty);
    }

    fn draw_shortcuts(&mut self, ui: &mut egui::Ui, cfg: &mut Config, dirty: &mut bool) {
        card(
            ui,
            "切换与临时锁定",
            "快捷键在连接期间生效，可在本机或对端随时返回。",
            |ui| {
                if ui
                    .checkbox(&mut cfg.shortcuts_enabled, "启用切换快捷键")
                    .changed()
                {
                    *dirty = true;
                }
                ui.add_enabled_ui(cfg.shortcuts_enabled, |ui| {
                    *dirty |= shortcut_editor(ui, "切换 / 返回本机", &mut cfg.switch_shortcut);
                    *dirty |= shortcut_editor(ui, "锁在本机 / 解锁", &mut cfg.lock_shortcut);
                });
                if cfg.switch_shortcut == cfg.lock_shortcut {
                    ui.label(
                        RichText::new("两个动作需要使用不同的组合键，将恢复默认锁定键。 ")
                            .small()
                            .color(AMBER),
                    );
                }
                ui.label(RichText::new("临时锁定会释放对端按键并阻止自动跨屏，适合全屏操作。只接收模式仍可使用返回和锁定。").small().color(MUTED));
                if self.handle.status.connected()
                    && secondary_button(
                        ui,
                        if self.handle.status.local_locked() {
                            "解除临时锁定"
                        } else {
                            "暂时锁在本机"
                        },
                    )
                    .clicked()
                {
                    self.handle
                        .status
                        .request_local_lock(!self.handle.status.local_locked());
                    if self.ui_preview {
                        self.handle
                            .status
                            .set_local_locked(!self.handle.status.local_locked());
                    }
                }
                ui.add_space(6.0);
                if ui
                    .checkbox(&mut cfg.swap_modifiers, "按功能交换 Ctrl / Command")
                    .changed()
                {
                    *dirty = true;
                }
                ui.label(
                    RichText::new(
                        "在接收端生效：Windows Ctrl+C 与 Mac ⌘C 按功能对应。两端键盘布局仍需一致。",
                    )
                    .small()
                    .color(MUTED),
                );
            },
        );
    }

    fn draw_startup(&mut self, ui: &mut egui::Ui, cfg: &mut Config, dirty: &mut bool) {
        card(
            ui,
            "启动与后台运行",
            "关闭窗口后继续运行，可在托盘中暂停连接或退出。",
            |ui| {
                if ui
                    .checkbox(&mut cfg.auto_start, "打开软件时自动连接设备")
                    .changed()
                {
                    *dirty = true;
                }
                if ui
                    .checkbox(&mut self.autostart_on, "登录电脑后自动启动 Fesilent Mouse")
                    .changed()
                {
                    if self.ui_preview {
                        self.settings_notice = Some((
                            "预览模式：未更改系统启动项".into(),
                            std::time::Instant::now(),
                        ));
                    } else if let Some(mut autostart) = platform_autostart() {
                        if let Err(error) = autostart.set_enabled(self.autostart_on) {
                            self.autostart_on = !self.autostart_on;
                            self.form_error = Some(format!("无法更改开机启动：{error}"));
                        }
                    } else {
                        self.autostart_on = false;
                        self.form_error = Some("当前系统未提供开机启动设置。".into());
                    }
                }
            },
        );
    }

    fn draw_transfers(&mut self, ui: &mut egui::Ui) {
        let items = transfers::center().snapshot();
        card(
            ui,
            "文件传输",
            "复制文件后切换到另一台电脑粘贴。",
            |ui| {
                if items.is_empty() {
                    ui.add_space(17.0);
                    ui.label(RichText::new("暂无文件传输").size(16.0).color(MUTED));
                    ui.label(
                        RichText::new("传输开始后，进度与确认请求会显示在这里。")
                            .small()
                            .color(MUTED),
                    );
                    ui.add_space(17.0);
                }
                for item in &items {
                    let direction = if item.dir == Dir::Send {
                        "发送"
                    } else {
                        "接收"
                    };
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new(&item.name).strong());
                        ui.label(
                            RichText::new(format!("{direction} · {}", human_size(item.total)))
                                .small()
                                .color(MUTED),
                        );
                    });
                    match item.phase {
                        Phase::Confirm => {
                            ui.label(
                                RichText::new("大于 1 GB，确认后开始发送。")
                                    .small()
                                    .color(AMBER),
                            );
                            ui.horizontal(|ui| {
                                if primary_button(ui, "确认发送").clicked() {
                                    transfers::center().decide(item.id, true);
                                }
                                if secondary_button(ui, "取消").clicked() {
                                    transfers::center().decide(item.id, false);
                                }
                            });
                        }
                        Phase::Active => {
                            let fraction = if item.total == 0 {
                                0.0
                            } else {
                                (item.transferred as f32 / item.total as f32).clamp(0.0, 1.0)
                            };
                            let mut text = format!(
                                "{} / {}",
                                human_size(item.transferred),
                                human_size(item.total)
                            );
                            if item.rate_bps > 0.0 {
                                text.push_str(&format!(
                                    " · {}/s",
                                    human_size(item.rate_bps as u64)
                                ));
                            }
                            if let Some(eta) = item.eta_secs {
                                text.push_str(&format!(" · 剩余{}", human_secs(eta)));
                            }
                            ui.add(egui::ProgressBar::new(fraction).fill(PURPLE).text(text));
                            if ui.small_button("取消传输").clicked() {
                                transfers::center().cancel(item.id);
                            }
                        }
                        Phase::Done => {
                            ui.horizontal_wrapped(|ui| {
                                ui.label(RichText::new("已完成").small().color(GREEN));
                                if item.dir == Dir::Recv
                                    && ui.small_button("打开所在位置").clicked()
                                {
                                    self.open_receive_directory();
                                }
                            });
                        }
                        Phase::Failed => {
                            if let Some(error) = &item.error {
                                ui.label(RichText::new(error).small().color(ERROR));
                            }
                            ui.label(
                                RichText::new("检查连接后，重新复制文件并粘贴以重试。")
                                    .small()
                                    .color(MUTED),
                            );
                        }
                        Phase::Cancelled => {
                            ui.label(RichText::new("已取消").small().color(MUTED));
                        }
                    }
                    ui.add_space(8.0);
                }
                let history = transfers::center().history();
                if !history.is_empty() {
                    ui.separator();
                    egui::CollapsingHeader::new(format!("最近传输 · {} 条", history.len())).show(
                        ui,
                        |ui| {
                            for item in history {
                                let outcome = match item.outcome {
                                    Phase::Done => "完成",
                                    Phase::Failed => "失败",
                                    _ => "取消",
                                };
                                let direction = if item.dir == Dir::Send {
                                    "发送"
                                } else {
                                    "接收"
                                };
                                ui.horizontal_wrapped(|ui| {
                                    ui.label(
                                        RichText::new(format!(
                                            "{} · {direction} · {} · {} · {outcome}",
                                            item.name,
                                            human_size(item.bytes),
                                            human_secs(item.secs)
                                        ))
                                        .small(),
                                    );
                                    if item.dir == Dir::Recv
                                        && item.outcome == Phase::Done
                                        && ui.small_button("所在位置").clicked()
                                    {
                                        self.open_receive_directory();
                                    }
                                });
                                if let Some(error) = &item.error {
                                    ui.label(RichText::new(error).small().color(ERROR));
                                }
                            }
                        },
                    );
                }
            },
        );
    }

    fn open_receive_directory(&mut self) {
        if self.ui_preview {
            self.form_error = Some("预览模式：未打开系统文件夹。".into());
            return;
        }
        match ms_net::transfer::default_receive_dir() {
            Some(path) => {
                if let Err(error) = open_path(&path) {
                    self.form_error = Some(error);
                }
            }
            None => self.form_error = Some("暂时无法确定文件接收目录。".into()),
        }
    }

    fn draw_sharing(&mut self, ui: &mut egui::Ui, cfg: &mut Config, dirty: &mut bool) {
        self.draw_transfers(ui);
        ui.add_space(15.0);
        card(
            ui,
            "剪贴板历史",
            "仅保留跨机同步过的内容；退出软件后清空。",
            |ui| {
                let active_transfer = transfers::center().has_active();
                if ui
                    .add_enabled(
                        !active_transfer,
                        egui::Checkbox::new(&mut cfg.clipboard, "同步两台电脑的剪贴板"),
                    )
                    .changed()
                {
                    *dirty = true;
                }
                ui.label(
                    RichText::new("关闭后也会关闭复制粘贴式文件传输；更改后会重新连接。")
                        .small()
                        .color(MUTED),
                );
                if active_transfer {
                    ui.label(
                        RichText::new("文件传输期间暂不能更改同步开关。")
                            .small()
                            .color(AMBER),
                    );
                }
                let clips = cliphist::snapshot();
                ui.add_space(9.0);
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(format!("最近内容 · {} 条", clips.len()))
                            .small()
                            .color(MUTED),
                    );
                    if !clips.is_empty() && ui.small_button("清空历史").clicked() {
                        cliphist::clear();
                        self.clip_result = None;
                    }
                });
                if clips.is_empty() {
                    ui.label(
                        RichText::new("暂无共享内容。连接设备后，跨机复制的内容会出现在这里。")
                            .small()
                            .color(MUTED),
                    );
                }
                for entry in clips {
                    ui.separator();
                    ui.label(
                        RichText::new(if entry.from_peer {
                            "来自对端"
                        } else {
                            "本机发出"
                        })
                        .small()
                        .color(PURPLE),
                    );
                    ui.label(&entry.preview);
                    if let Some(content) = &entry.refill {
                        if ui.small_button("复制到本机").clicked() {
                            self.clip_result = Some(if self.ui_preview {
                                "预览模式：未写入系统剪贴板。".into()
                            } else {
                                match daemon::write_clipboard(content) {
                                    Ok(()) => "已复制到本机剪贴板，可以粘贴。".into(),
                                    Err(error) => format!("复制失败：{error}"),
                                }
                            });
                        }
                    } else {
                        ui.label(RichText::new("内容较大，仅显示预览").small().color(MUTED));
                    }
                }
                if let Some(result) = &self.clip_result {
                    ui.label(RichText::new(result).small().color(PURPLE));
                }
            },
        );
    }

    fn draw_about(&mut self, ui: &mut egui::Ui, _cfg: &mut Config, _dirty: &mut bool) {
        card(
            ui,
            "连接诊断",
            "遇到连接问题时，导出近期日志与设备配置。",
            |ui| {
                ui.label(self.handle.status_summary());
                ui.add_space(5.0);
                if secondary_button(ui, "导出诊断包").clicked() {
                    self.diag_result = Some(if self.ui_preview {
                        "预览模式：未导出用户日志。".into()
                    } else {
                        match diag::export(self.args.log_dir.as_deref()) {
                            Ok(path) => format!("已导出到：{}", path.display()),
                            Err(error) => format!("导出失败：{error}"),
                        }
                    });
                }
                ui.label(
                    RichText::new("最近 7 天日志、配置和版本信息，导出至桌面。")
                        .small()
                        .color(MUTED),
                );
                if let Some(result) = &self.diag_result {
                    ui.label(RichText::new(result).small().color(PURPLE));
                }
            },
        );
        ui.add_space(15.0);
        card(
            ui,
            "Fesilent Mouse",
            &format!("版本 {} · 原生 Rust / egui", env!("CARGO_PKG_VERSION")),
            |ui| {
                ui.label(
                    RichText::new("基于 Yujay-Z / MouseShare 的 Fork，保留原作者署名与项目来源。")
                        .color(MUTED),
                );
                ui.label(
                    RichText::new("上游尚未附加开源许可证。来源与权利边界见 NOTICE.md。")
                        .small()
                        .color(MUTED),
                );
                ui.add_space(5.0);
                ui.horizontal_wrapped(|ui| {
                    ui.hyperlink_to("Fesilent 官网", "https://fesilent.com");
                    ui.hyperlink_to("项目源码", "https://github.com/id399-dzx/fesilent-mouse");
                    ui.hyperlink_to("原作者与上游", "https://github.com/Yujay-Z/MouseShare");
                });
            },
        );
    }

    fn draw_pairing_dialog(&mut self, ctx: &egui::Context) {
        if let Some(request) = self.handle.status.pending_pairing() {
            egui::Window::new("确认新设备")
                .id(egui::Id::new("confirm_peer_pairing"))
                .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
                .collapsible(false)
                .resizable(false)
                .default_width(350.0)
                .show(ctx, |ui| {
                    eyebrow(ui, "PAIR YOUR DEVICES");
                    ui.label("请核对两台电脑上的配对码是否一致。");
                    ui.label(
                        RichText::new(&request.address)
                            .small()
                            .monospace()
                            .color(MUTED),
                    );
                    ui.add_space(12.0);
                    ui.label(
                        RichText::new(&request.sas)
                            .monospace()
                            .size(34.0)
                            .color(PURPLE),
                    );
                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        if primary_button(ui, "一致，确认配对").clicked() {
                            self.handle.status.decide_pairing(true);
                        }
                        if secondary_button(ui, "拒绝").clicked() {
                            self.handle.status.decide_pairing(false);
                        }
                    });
                });
        }
    }

    fn draw_forget_confirmation(&mut self, ctx: &egui::Context) {
        if !self.confirm_forget_devices {
            return;
        }
        egui::Window::new("忘记已配对设备？")
            .id(egui::Id::new("forget_paired_devices"))
            .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
            .collapsible(false)
            .resizable(false)
            .default_width(350.0)
            .show(ctx, |ui| {
                ui.label(
                    "当前连接会停止。请在两台电脑上都先忘记已配对设备，再重新连接并核对配对码。",
                );
                ui.label(
                    RichText::new("本机身份与其他设置会保留。")
                        .small()
                        .color(MUTED),
                );
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if primary_button(ui, "忘记并断开").clicked() {
                        self.confirm_forget_devices = false;
                        if self.ui_preview {
                            self.connection_notice = Some("预览模式：未修改已配对设备。".into());
                        } else {
                            self.pending_forget_device = None;
                            self.forget_all_pending = true;
                            self.handle.forget_devices();
                            self.connection_notice = Some(
                                "正在清除本机配对记录；请在另一台电脑上也先忘记设备，再重新连接。"
                                    .into(),
                            );
                        }
                    }
                    if secondary_button(ui, "取消").clicked() {
                        self.confirm_forget_devices = false;
                    }
                });
            });
    }
}

fn shortcut_editor(
    ui: &mut egui::Ui,
    label: &str,
    shortcut: &mut crate::config::ShortcutConfig,
) -> bool {
    let before = *shortcut;
    ui.horizontal_wrapped(|ui| {
        ui.label(label);
        let mut modifiers = match (shortcut.ctrl, shortcut.alt, shortcut.shift, shortcut.meta) {
            (true, true, false, false) => 0,
            (true, false, true, false) => 1,
            (false, true, true, false) => 2,
            (true, false, false, true) => 3,
            _ => 0,
        };
        let labels = ["Ctrl + Alt", "Ctrl + Shift", "Alt + Shift", "Ctrl + Win/⌘"];
        egui::ComboBox::from_id_salt((label, "modifiers"))
            .selected_text(labels[modifiers])
            .show_ui(ui, |ui| {
                for (index, text) in labels.iter().enumerate() {
                    ui.selectable_value(&mut modifiers, index, *text);
                }
            });
        (shortcut.ctrl, shortcut.alt, shortcut.shift, shortcut.meta) = match modifiers {
            1 => (true, false, true, false),
            2 => (false, true, true, false),
            3 => (true, false, false, true),
            _ => (true, true, false, false),
        };
        egui::ComboBox::from_id_salt((label, "key"))
            .selected_text(shortcut.label().rsplit(" + ").next().unwrap_or("空格"))
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut shortcut.key, 0x2C, "空格");
                for index in 0..26 {
                    ui.selectable_value(
                        &mut shortcut.key,
                        4 + index,
                        char::from(b'A' + index as u8).to_string(),
                    );
                }
                for index in 1..=12 {
                    ui.selectable_value(&mut shortcut.key, 0x39 + index, format!("F{index}"));
                }
            });
    });
    *shortcut != before
}

fn draw_screen(
    painter: &egui::Painter,
    rect: egui::Rect,
    name: &str,
    role: &str,
    active: bool,
    mac: bool,
) {
    painter.rect_filled(
        rect.expand(5.0),
        17,
        Color32::from_rgba_unmultiplied(34, 27, 60, 5),
    );
    painter.rect_filled(
        rect,
        13,
        if mac {
            Color32::from_rgb(246, 241, 254)
        } else {
            Color32::from_rgb(248, 248, 251)
        },
    );
    glow(
        &painter.with_clip_rect(rect.shrink(1.0)),
        rect.right_bottom() - Vec2::new(25.0, 20.0),
        105.0,
        if mac {
            Color32::from_rgba_unmultiplied(190, 184, 255, 135)
        } else {
            Color32::from_rgba_unmultiplied(146, 213, 255, 85)
        },
    );
    glow(
        &painter.with_clip_rect(rect.shrink(1.0)),
        rect.right_bottom() - Vec2::new(55.0, 3.0),
        75.0,
        Color32::from_rgba_unmultiplied(255, 205, 216, 110),
    );
    painter.rect_stroke(
        rect,
        13,
        Stroke::new(
            if active { 2.0_f32 } else { 1.0_f32 },
            if active {
                PURPLE
            } else {
                Color32::from_rgb(216, 208, 231)
            },
        ),
        egui::StrokeKind::Inside,
    );
    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        if active { "正在使用" } else { name },
        FontId::proportional(if active { 13.0 } else { 17.0 }),
        if active { PURPLE } else { INK },
    );
    painter.line_segment(
        [
            rect.center_bottom() + Vec2::new(0.0, 1.0),
            rect.center_bottom() + Vec2::new(0.0, 8.0),
        ],
        Stroke::new(2.0_f32, Color32::from_rgb(211, 203, 224)),
    );
    painter.line_segment(
        [
            rect.center_bottom() + Vec2::new(-17.0, 8.0),
            rect.center_bottom() + Vec2::new(17.0, 8.0),
        ],
        Stroke::new(2.0_f32, Color32::from_rgb(211, 203, 224)),
    );
    painter.text(
        rect.center_bottom() + Vec2::new(0.0, 27.0),
        egui::Align2::CENTER_CENTER,
        name,
        FontId::proportional(14.0),
        INK,
    );
    painter.text(
        rect.center_bottom() + Vec2::new(0.0, 47.0),
        egui::Align2::CENTER_CENTER,
        role,
        FontId::proportional(11.0),
        MUTED,
    );
}

pub(super) fn spawn_local_ip_monitor() -> std::sync::mpsc::Receiver<Vec<String>> {
    local_network::monitor()
}
