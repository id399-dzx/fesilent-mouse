//! Native egui views for Fesilent Mouse. All network input is a draft until applied.

use super::*;
use egui::{Align, Color32, FontId, Layout, RichText, Stroke, Vec2};

const INK: Color32 = Color32::from_rgb(28, 37, 51);
const MUTED: Color32 = Color32::from_rgb(107, 119, 136);
const BLUE: Color32 = Color32::from_rgb(40, 100, 220);
const BLUE_WASH: Color32 = Color32::from_rgb(236, 243, 255);
const GREEN: Color32 = Color32::from_rgb(22, 128, 105);
const BORDER: Color32 = Color32::from_rgb(226, 232, 240);
const BACKGROUND: Color32 = Color32::from_rgb(243, 246, 250);
const ERROR: Color32 = Color32::from_rgb(176, 48, 54);

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Section {
    Connection,
    Controls,
    Sharing,
    About,
}

pub(super) fn install_theme(ctx: &egui::Context) {
    let mut style = (*ctx.style()).clone();
    style.visuals = egui::Visuals::light();
    style.visuals.override_text_color = Some(INK);
    style.visuals.panel_fill = BACKGROUND;
    style.visuals.window_fill = Color32::WHITE;
    style.visuals.faint_bg_color = BACKGROUND;
    style.visuals.extreme_bg_color = Color32::WHITE;
    style.visuals.selection.bg_fill = BLUE_WASH;
    style.visuals.selection.stroke = Stroke::new(1.0, BLUE);
    style.visuals.hyperlink_color = BLUE;
    style.visuals.widgets.inactive.bg_fill = Color32::WHITE;
    style.visuals.widgets.inactive.weak_bg_fill = Color32::WHITE;
    style.visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, BORDER);
    style.visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, INK);
    style.visuals.widgets.hovered.bg_fill = BLUE_WASH;
    style.visuals.widgets.hovered.weak_bg_fill = BLUE_WASH;
    style.visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, BLUE);
    style.visuals.widgets.active.bg_fill = BLUE_WASH;
    style.visuals.widgets.active.bg_stroke = Stroke::new(1.0, BLUE);
    style.visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
    style.spacing.item_spacing = Vec2::new(10.0, 10.0);
    style.spacing.button_padding = Vec2::new(14.0, 9.0);
    style.spacing.interact_size.y = 34.0;
    style.spacing.slider_width = 240.0;
    style.text_styles.insert(egui::TextStyle::Heading, FontId::proportional(23.0));
    style.text_styles.insert(egui::TextStyle::Body, FontId::proportional(14.0));
    style.text_styles.insert(egui::TextStyle::Button, FontId::proportional(14.0));
    style.text_styles.insert(egui::TextStyle::Small, FontId::proportional(12.0));
    ctx.set_style(style);
}

fn card(ui: &mut egui::Ui, title: &str, subtitle: &str, content: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::new()
        .fill(Color32::WHITE)
        .stroke(Stroke::new(1.0, BORDER))
        .corner_radius(14)
        .inner_margin(20)
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            if !title.is_empty() {
                ui.label(RichText::new(title).size(17.0).strong());
            }
            if !subtitle.is_empty() {
                ui.label(RichText::new(subtitle).small().color(MUTED));
            }
            ui.add_space(5.0);
            content(ui);
        });
}

fn notice(ui: &mut egui::Ui, text: &str, color: Color32, fill: Color32) {
    egui::Frame::new().fill(fill).corner_radius(9).inner_margin(12).show(ui, |ui| {
        ui.set_min_width(ui.available_width());
        ui.label(RichText::new(text).color(color));
    });
}

fn primary_button(ui: &mut egui::Ui, label: &str) -> egui::Response {
    ui.add(egui::Button::new(RichText::new(label).color(Color32::WHITE))
        .fill(BLUE).stroke(Stroke::NONE).corner_radius(8).min_size(Vec2::new(118.0, 39.0)))
}

fn section_intro(ui: &mut egui::Ui, title: &str, subtitle: &str) {
    ui.label(RichText::new(title).size(21.0).strong());
    ui.label(RichText::new(subtitle).color(MUTED));
    ui.add_space(7.0);
}

fn brand_mark(ui: &mut egui::Ui) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(43.0), egui::Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, 12, BLUE);
    let center = rect.center();
    painter.line_segment([center + Vec2::new(-9.0, -6.0), center + Vec2::new(4.0, -6.0)], Stroke::new(2.5, Color32::WHITE));
    painter.line_segment([center + Vec2::new(-9.0, -6.0), center + Vec2::new(-9.0, 9.0)], Stroke::new(2.5, Color32::WHITE));
    painter.line_segment([center + Vec2::new(-9.0, 1.0), center + Vec2::new(-1.0, 1.0)], Stroke::new(2.5, Color32::WHITE));
    painter.line_segment([center + Vec2::new(1.0, 6.0), center + Vec2::new(10.0, 6.0)], Stroke::new(2.5, Color32::from_rgb(160, 235, 230)));
    painter.line_segment([center + Vec2::new(6.0, 2.0), center + Vec2::new(10.0, 6.0)], Stroke::new(2.5, Color32::from_rgb(160, 235, 230)));
}

impl App {
    pub(super) fn draw(&mut self, ctx: &egui::Context) {
        let running = self.handle.is_running();
        self.set_toggle_text(running);
        if ctx.input_mut(|input| input.consume_key(egui::Modifiers::COMMAND, egui::Key::W)) {
            self.hide_requested = true;
        }
        // Clone before drawing: never keep the shared configuration lock across UI callbacks.
        let mut cfg = self.config.lock().map(|cfg| cfg.clone()).unwrap_or_default();
        let previous_clipboard = cfg.clipboard;
        let mut dirty = false;

        egui::TopBottomPanel::top("brand_header")
            .frame(egui::Frame::new().fill(Color32::WHITE).inner_margin(egui::Margin::symmetric(26, 19)))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    brand_mark(ui);
                    ui.vertical(|ui| {
                        ui.label(RichText::new("Fesilent Mouse").size(25.0).strong());
                        ui.label(RichText::new("一套键鼠，连接你的工作空间").small().color(MUTED));
                    });
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let label = if running { "暂停互控" } else { "启用互控" };
                        if primary_button(ui, label).clicked() {
                            if running {
                                self.handle.stop();
                            } else if self.apply_connection(&mut cfg, false) {
                                self.handle.start();
                            }
                        }
                        let (text, color, fill) = if self.handle.status.connected() && running {
                            ("● 已连接", GREEN, Color32::from_rgb(233, 247, 242))
                        } else if running {
                            ("● 正在连接", Color32::from_rgb(157, 103, 24), Color32::from_rgb(255, 247, 229))
                        } else {
                            ("● 已暂停", MUTED, BACKGROUND)
                        };
                        egui::Frame::new().fill(fill).corner_radius(20).inner_margin(egui::Margin::symmetric(12, 7)).show(ui, |ui| {
                            ui.label(RichText::new(text).size(12.0).color(color));
                        });
                    });
                });
                ui.add_space(15.0);
                ui.horizontal(|ui| {
                    for (section, label) in [
                        (Section::Connection, "01  连接与设备"),
                        (Section::Controls, "02  键鼠设置"),
                        (Section::Sharing, "03  传输与剪贴板"),
                        (Section::About, "04  关于与诊断"),
                    ] {
                        let selected = self.section == section;
                        let button = egui::Button::new(RichText::new(label).color(if selected { BLUE } else { MUTED }).strong())
                            .fill(if selected { BLUE_WASH } else { Color32::TRANSPARENT })
                            .stroke(Stroke::NONE).corner_radius(8);
                        if ui.add(button).clicked() { self.section = section; }
                    }
                });
            });

        egui::TopBottomPanel::bottom("app_footer")
            .frame(egui::Frame::new().fill(BACKGROUND).inner_margin(egui::Margin::symmetric(26, 11)))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("局域网直连  /  键鼠共享  /  剪贴板同步").size(11.0).color(MUTED));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(RichText::new("关闭窗口后继续在托盘运行").size(11.0).color(MUTED));
                    });
                });
            });

        egui::CentralPanel::default().frame(egui::Frame::new().fill(BACKGROUND).inner_margin(egui::Margin::symmetric(26, 22)))
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().id_salt("main_settings_scroll").auto_shrink([false, false]).show(ui, |ui| {
                    if !daemon::input_permission_granted() {
                        notice(ui, "需要辅助功能权限：请在系统设置中允许 Fesilent Mouse 控制键盘与鼠标。", Color32::from_rgb(139, 88, 20), Color32::from_rgb(255, 247, 229));
                        if ui.button("打开系统设置并授权").clicked() { daemon::open_input_permission_settings(); }
                        ui.add_space(8.0);
                    }
                    if let Some(error) = self.form_error.clone().or_else(|| self.handle.status.last_error()) {
                        notice(ui, &format!("连接提示：{error}"), ERROR, Color32::from_rgb(255, 239, 239));
                        ui.add_space(8.0);
                    }
                    // Confirm/cancel must remain reachable on every page when a transfer opens the window.
                    if self.section != Section::Sharing && transfers::center().has_active() {
                        self.draw_transfers(ui, false);
                        ui.add_space(12.0);
                    }
                    match self.section {
                        Section::Connection => self.draw_connection(ui, &mut cfg, &mut dirty),
                        Section::Controls => self.draw_controls(ui, &mut cfg, &mut dirty),
                        Section::Sharing => self.draw_sharing(ui, &mut cfg, &mut dirty),
                        Section::About => self.draw_about(ui, &mut cfg, &mut dirty),
                    }
                    ui.add_space(8.0);
                });
            });
        if dirty && self.store_config(&cfg) && previous_clipboard != cfg.clipboard && self.handle.is_running() {
            // Clipboard workers are created per connection, so changing their switch requires a restart.
            self.handle.stop();
            self.handle.start();
        }
        ctx.request_repaint_after(Duration::from_millis(500));
    }

    fn store_config(&mut self, cfg: &Config) -> bool {
        if let Err(error) = config::save(cfg) {
            self.form_error = Some(format!("设置保存失败：{error}"));
            return false;
        }
        if let Ok(mut shared) = self.config.lock() {
            *shared = cfg.clone();
            true
        } else {
            self.form_error = Some("暂时无法更新设置，请重试。".to_owned());
            false
        }
    }

    fn apply_connection(&mut self, cfg: &mut Config, restart: bool) -> bool {
        if restart && transfers::center().has_active() {
            self.form_error = Some("请先完成或取消文件传输，再应用连接设置。".to_owned());
            return false;
        }
        let address = if self.manual_connection {
            match config::normalize_peer_address(&self.peer_ip) {
                Ok(Some(address)) => Some(address),
                Ok(None) => {
                    self.form_error = Some("请输入对端 IP，或切换到自动发现。".to_owned());
                    return false;
                }
                Err(error) => {
                    self.form_error = Some(error);
                    return false;
                }
            }
        } else { None };
        cfg.connect = address;
        if !self.store_config(cfg) { return false; }
        if let Some(address) = &cfg.connect { self.peer_ip = address.clone(); }
        self.form_error = None;
        self.handle.status.clear_error();
        if restart && self.handle.is_running() {
            self.handle.stop();
            self.handle.start();
            self.connection_notice = Some("连接设置已保存，正在使用新地址重新连接。".to_owned());
        } else {
            self.connection_notice = Some("连接设置已保存。启用互控后开始连接。".to_owned());
        }
        true
    }

    fn draw_connection(&mut self, ui: &mut egui::Ui, cfg: &mut Config, dirty: &mut bool) {
        section_intro(ui, "让两台电脑，像一台一样顺手", "在同一局域网中共享键盘、鼠标与剪贴板。先连接，再把光标推向屏幕边缘。");
        card(ui, "", "", |ui| self.draw_device_pair(ui));
        ui.add_space(13.0);
        card(ui, "连接方式", "建议只在 Windows 端填写 Mac 的 IP；Mac 端使用自动发现，留空等待连接。", |ui| {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.manual_connection, false, "自动发现");
                ui.selectable_value(&mut self.manual_connection, true, "手动连接");
            });
            ui.add_space(5.0);
            if self.manual_connection {
                ui.label(RichText::new("对端 IP 地址").strong());
                ui.horizontal(|ui| {
                    let width = (ui.available_width() - 140.0).max(180.0);
                    ui.add_sized([width, 38.0], egui::TextEdit::singleline(&mut self.peer_ip)
                        .hint_text("例如 192.168.1.20（端口可省略）").margin(egui::Margin::symmetric(11, 9)));
                    if ui.add_enabled_ui(!transfers::center().has_active(), |ui| primary_button(ui, "应用连接")).inner.clicked() {
                        self.apply_connection(cfg, true);
                    }
                });
                ui.label(RichText::new("只填 IP 会自动补全端口 53728。输入过程中不会重新连接，点击应用后生效。").small().color(MUTED));
            } else {
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.label(if cfg!(target_os = "macos") { "Mac 端保持留空，等待 Windows 主动连接。" } else { "自动查找同一网络中的 Mac。" });
                        ui.label(RichText::new("若未发现设备，可在 Windows 端切换手动连接，填写 Mac 的 IP。").small().color(MUTED));
                    });
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.add_enabled(!transfers::center().has_active(), egui::Button::new("应用连接")).clicked() {
                            self.apply_connection(cfg, true);
                        }
                    });
                });
            }
            if transfers::center().has_active() {
                ui.label(RichText::new("请先完成或取消文件传输，再应用连接设置。").small().color(Color32::from_rgb(157, 103, 24)));
            }
            let applied = if self.manual_connection { cfg.connect.as_deref() == Some(self.peer_ip.trim()) } else { cfg.connect.is_none() };
            if !applied {
                ui.label(RichText::new("有待应用的连接更改").small().color(Color32::from_rgb(157, 103, 24)));
            } else if let Some(message) = &self.connection_notice {
                ui.label(RichText::new(message).small().color(GREEN));
            }
            ui.add_space(5.0);
            ui.separator();
            ui.add_space(3.0);
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("本机 IPv4").small().color(MUTED));
                if self.local_ips.is_empty() { ui.label("暂未识别到网络地址"); }
                for address in &self.local_ips {
                    if ui.button(RichText::new(format!("{address}  复制")).monospace().size(12.0)).clicked() {
                        ui.ctx().copy_text(address.clone());
                        self.connection_notice = Some(format!("已复制本机地址 {address}"));
                    }
                }
                if ui.small_button("刷新").clicked() { self.local_ips = local_ipv4_addresses(); }
            });
            if let Some(address) = cfg.connect.as_deref().and_then(route_local_address) {
                ui.label(RichText::new(format!("连接到此对端时使用的本机地址：{address}")).small().color(MUTED));
            }
            ui.label(RichText::new("存在多个地址时，选择 Wi-Fi / 以太网地址；VPN 或虚拟网卡地址可能无法互通。").small().color(MUTED));
        });
        ui.add_space(13.0);
        card(ui, "连接前，确认这三步", "", |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("1  同一局域网").strong());
                ui.label(RichText::new("两台电脑连接同一 Wi-Fi 或路由器").small().color(MUTED));
                ui.separator();
                ui.label(RichText::new("2  两端启用互控").strong());
                ui.label(RichText::new("两台电脑都启动软件并启用互控").small().color(MUTED));
                ui.separator();
                ui.label(RichText::new("3  推向屏幕边缘").strong());
            });
            ui.label(RichText::new("无法连接时：检查对端正在运行、防火墙允许局域网访问；只让一端手动填写 IP，另一端留空。").small().color(MUTED));
            if ui.checkbox(&mut cfg.auto_start, "下次打开软件时自动启用互控").changed() { *dirty = true; }
        });
    }

    fn draw_device_pair(&self, ui: &mut egui::Ui) {
        let width = ui.available_width();
        let (rect, _) = ui.allocate_exact_size(Vec2::new(width, 112.0), egui::Sense::hover());
        let painter = ui.painter();
        let center = rect.center();
        let distance = (width * 0.265).min(225.0);
        let is_mac = cfg!(target_os = "macos");
        let connected = self.handle.status.connected() && self.handle.is_running();
        let accent = if connected { GREEN } else { BLUE };
        for (x, title, subtitle, local) in [
            (center.x - distance, if is_mac { "Mac" } else { "Windows" }, "本机 · 键鼠与剪贴板", true),
            (center.x + distance, if is_mac { "Windows" } else { "Mac" }, if connected { "对端 · 已建立连接" } else { "对端 · 等待连接" }, false),
        ] {
            let screen = egui::Rect::from_center_size(egui::pos2(x, rect.top() + 30.0), Vec2::new(64.0, 42.0));
            painter.rect_filled(screen.expand(5.0), 10, if local { BLUE_WASH } else { BACKGROUND });
            painter.rect_stroke(screen, 5, Stroke::new(1.8, if local { BLUE } else { MUTED }), egui::StrokeKind::Inside);
            painter.line_segment([egui::pos2(x, screen.bottom()), egui::pos2(x, screen.bottom() + 7.0)], Stroke::new(1.8, if local { BLUE } else { MUTED }));
            painter.line_segment([egui::pos2(x - 12.0, screen.bottom() + 7.0), egui::pos2(x + 12.0, screen.bottom() + 7.0)], Stroke::new(1.8, if local { BLUE } else { MUTED }));
            painter.text(egui::pos2(x, rect.top() + 76.0), egui::Align2::CENTER_CENTER, title, FontId::proportional(16.0), INK);
            painter.text(egui::pos2(x, rect.top() + 100.0), egui::Align2::CENTER_CENTER, subtitle, FontId::proportional(12.0), MUTED);
        }
        let y = rect.top() + 31.0;
        let left = center.x - distance + 55.0;
        let right = center.x + distance - 55.0;
        painter.line_segment([egui::pos2(left, y), egui::pos2(center.x - 34.0, y)], Stroke::new(1.5, BORDER));
        painter.line_segment([egui::pos2(center.x + 34.0, y), egui::pos2(right, y)], Stroke::new(1.5, BORDER));
        painter.circle_filled(egui::pos2(center.x, y), 22.0, if connected { Color32::from_rgb(233, 247, 242) } else { BLUE_WASH });
        painter.text(egui::pos2(center.x, y), egui::Align2::CENTER_CENTER, "↔", FontId::proportional(24.0), accent);
        painter.text(egui::pos2(center.x, rect.top() + 78.0), egui::Align2::CENTER_CENTER,
            if connected { "工作空间已连接" } else { "共享，始于连接" }, FontId::proportional(12.0), MUTED);
    }

    fn draw_controls(&mut self, ui: &mut egui::Ui, cfg: &mut Config, dirty: &mut bool) {
        section_intro(ui, "顺着你的习惯，安排键鼠", "设置在对应平台生效，调整后会即时保存。");
        if cfg!(target_os = "macos") {
            card(ui, "Windows 键盘，在 Mac 上自然使用", "本机作为被控端，接收 Windows 的键鼠输入。", |ui| {
                if ui.checkbox(&mut cfg.swap_modifiers, "交换 Ctrl 与 Command").changed() { *dirty = true; }
                ui.label(RichText::new("开启后，Windows 键盘的 Ctrl+C / Ctrl+V 会作为 Mac 的 ⌘C / ⌘V 使用。").small().color(MUTED));
                ui.add_space(10.0);
                notice(ui, "设备方位、滚轮速度和滚动方向，请在 Windows 端设置。Mac 端不会主动夺取控制权。", BLUE, BLUE_WASH);
            });
        } else {
            card(ui, "屏幕布局", "选择 Mac 相对这台 Windows 电脑的位置。", |ui| {
                ui.horizontal(|ui| {
                    egui::Grid::new("side_grid").spacing([5.0, 5.0]).show(ui, |ui| {
                        blank_cell(ui); side_cell(ui, &mut cfg.side, dirty, "top", "上方"); blank_cell(ui); ui.end_row();
                        side_cell(ui, &mut cfg.side, dirty, "left", "左侧");
                        ui.add_sized(SIDE_CELL, egui::Label::new(RichText::new("本机").color(MUTED)));
                        side_cell(ui, &mut cfg.side, dirty, "right", "右侧"); ui.end_row();
                        blank_cell(ui); side_cell(ui, &mut cfg.side, dirty, "bottom", "下方"); blank_cell(ui); ui.end_row();
                    });
                    ui.add_space(22.0);
                    ui.vertical(|ui| {
                        let side = match cfg.side.as_str() { "left" => "左", "top" => "上", "bottom" => "下", _ => "右" };
                        ui.label(RichText::new(format!("Mac 在{side}边")).size(17.0).strong());
                        ui.label(format!("光标抵达本机{side}边缘后，继续推出即可控制 Mac。"));
                        ui.label(RichText::new("返回同一边缘并继续推，即可回到 Windows。").small().color(MUTED));
                    });
                });
            });
            ui.add_space(13.0);
            card(ui, "滚动与切换手感", "仅在本机控制 Mac 时生效。", |ui| {
                ui.label(RichText::new("纵向滚轮速度").strong());
                cfg.scroll_speed = config::normalize_scroll_speed(cfg.scroll_speed);
                if ui.add(egui::Slider::new(&mut cfg.scroll_speed, 1.0..=3.0).step_by(0.1).fixed_decimals(1).suffix(" ×")).changed() {
                    cfg.scroll_speed = config::normalize_scroll_speed(cfg.scroll_speed); *dirty = true;
                }
                ui.label(RichText::new("1.0 × 为原始速度，仅调整上下滚动。").small().color(MUTED));
                if ui.checkbox(&mut cfg.invert_scroll, "反转滚轮方向").changed() { *dirty = true; }
                egui::CollapsingHeader::new("高级：屏幕边缘触发距离").show(ui, |ui| {
                    ui.label(RichText::new("越大越不容易误切换；设为 0 时，触碰边缘即切换。").small().color(MUTED));
                    if ui.add(egui::Slider::new(&mut cfg.edge_push, 0.0..=300.0).suffix(" px").text("进入 Mac")).changed() { *dirty = true; }
                    if ui.add(egui::Slider::new(&mut cfg.return_push, 0.0..=300.0).suffix(" px").text("返回本机")).changed() { *dirty = true; }
                });
                ui.label(RichText::new("Ctrl / Command 按键交换请在 Mac 端设置。").small().color(MUTED));
            });
        }
    }

    fn draw_transfers(&mut self, ui: &mut egui::Ui, show_empty: bool) {
        let transfers = transfers::center().snapshot();
        if transfers.is_empty() && !show_empty { return; }
        card(ui, "文件传输", "在一台电脑复制文件，到另一台粘贴，即可通过局域网传输。", |ui| {
            if transfers.is_empty() {
                ui.add_space(8.0);
                ui.label(RichText::new("暂无正在传输的文件").color(MUTED));
                ui.label(RichText::new("连接后，你的传输进度与确认请求会出现在这里。").small().color(MUTED));
                ui.add_space(8.0);
            }
            for transfer in &transfers {
                let direction = match transfer.dir { Dir::Send => "发送", Dir::Recv => "接收" };
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new(&transfer.name).strong());
                    ui.label(RichText::new(format!("{direction} · {}", human_size(transfer.total))).small().color(MUTED));
                });
                match transfer.phase {
                    Phase::Confirm => {
                        ui.label(RichText::new("大文件传输需要你的确认。").color(Color32::from_rgb(157, 103, 24)));
                        ui.horizontal(|ui| {
                            if primary_button(ui, "确认发送").clicked() { transfers::center().decide(transfer.id, true); }
                            if ui.button("取消").clicked() { transfers::center().decide(transfer.id, false); }
                        });
                    }
                    Phase::Active => {
                        let fraction = if transfer.total == 0 { 0.0 } else { (transfer.transferred as f32 / transfer.total as f32).clamp(0.0, 1.0) };
                        let mut text = format!("{} / {}", human_size(transfer.transferred), human_size(transfer.total));
                        if transfer.rate_bps > 0.0 {
                            text.push_str(&format!(" · {}/s", human_size(transfer.rate_bps as u64)));
                            if let Some(eta) = transfer.eta_secs { text.push_str(&format!(" · 剩余{}", human_secs(eta))); }
                        }
                        ui.add(egui::ProgressBar::new(fraction).fill(BLUE).text(text));
                        if ui.small_button("取消传输").clicked() { transfers::center().cancel(transfer.id); }
                    }
                    Phase::Done => { ui.colored_label(GREEN, "已完成"); }
                    Phase::Failed => { ui.colored_label(ERROR, "传输失败，请检查连接后重试。"); }
                    Phase::Cancelled => { ui.label(RichText::new("已取消").color(MUTED)); }
                }
                ui.add_space(5.0);
            }
            let history = transfers::center().history();
            if !history.is_empty() {
                egui::CollapsingHeader::new(format!("最近传输 · {} 条", history.len())).show(ui, |ui| {
                    for item in &history {
                        let direction = match item.dir { Dir::Send => "发送", Dir::Recv => "接收" };
                        let outcome = match item.outcome { Phase::Done => "完成", Phase::Failed => "失败", _ => "取消" };
                        ui.label(RichText::new(format!("{direction}  {}  · {} · {} · {outcome}", item.name, human_size(item.bytes), human_secs(item.secs))).small());
                    }
                });
            }
        });
    }

    fn draw_sharing(&mut self, ui: &mut egui::Ui, cfg: &mut Config, dirty: &mut bool) {
        section_intro(ui, "复制在这里，粘贴在另一边", "文字、图片与文件，在你的两台电脑间流动。");
        self.draw_transfers(ui, true);
        ui.add_space(13.0);
        card(ui, "剪贴板同步", "历史仅保留在内存中，退出软件后清空。", |ui| {
            let active_transfer = transfers::center().has_active();
            if ui.add_enabled(!active_transfer, egui::Checkbox::new(&mut cfg.clipboard, "同步两台电脑的剪贴板")).changed() { *dirty = true; }
            ui.label(RichText::new("关闭后也会关闭复制粘贴式文件传输；更改开关后会重新连接。").small().color(MUTED));
            if active_transfer {
                ui.label(RichText::new("请先完成或取消文件传输，再更改剪贴板同步。").small().color(Color32::from_rgb(157, 103, 24)));
            }
            let clips = cliphist::snapshot();
            ui.add_space(5.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("最近内容 · {} 条", clips.len())).strong());
                if !clips.is_empty() && ui.small_button("清空历史").clicked() {
                    cliphist::clear(); self.clip_result = None;
                }
            });
            if clips.is_empty() {
                ui.label(RichText::new("暂无剪贴板历史。连接后复制的内容会出现在这里。").small().color(MUTED));
            }
            for entry in &clips {
                ui.separator();
                ui.horizontal(|ui| {
                    if let Some(content) = &entry.refill {
                        if ui.small_button("回填").clicked() {
                            self.clip_result = Some(match daemon::write_clipboard(content) {
                                Ok(()) => "已回填到本机剪贴板。".to_owned(),
                                Err(error) => format!("回填失败：{error}"),
                            });
                        }
                    } else { ui.label(RichText::new("过大").small().color(MUTED)); }
                    ui.vertical(|ui| {
                        ui.label(RichText::new(if entry.from_peer { "来自对端" } else { "本机发出" }).small().color(MUTED));
                        ui.label(&entry.preview);
                    });
                });
            }
            if let Some(result) = &self.clip_result { ui.label(RichText::new(result).small().color(BLUE)); }
        });
    }

    fn draw_about(&mut self, ui: &mut egui::Ui, cfg: &mut Config, dirty: &mut bool) {
        section_intro(ui, "让工具安静地，融入工作", "Fesilent Mouse · 连接 Windows 与 Mac 的轻量桌面工具。");
        card(ui, "启动与后台运行", "关闭设置窗口会隐藏到系统托盘，互控与传输仍会继续。", |ui| {
            if ui.checkbox(&mut cfg.auto_start, "打开软件时自动启用互控").changed() { *dirty = true; }
            if ui.checkbox(&mut self.autostart_on, "登录电脑后自动启动 Fesilent Mouse").changed() {
                if let Some(mut autostart) = platform_autostart() {
                    if let Err(error) = autostart.set_enabled(self.autostart_on) {
                        self.autostart_on = !self.autostart_on;
                        self.form_error = Some(format!("无法更改开机启动：{error}"));
                    }
                } else {
                    self.autostart_on = false;
                    self.form_error = Some("当前系统未提供开机启动设置。".to_owned());
                }
            }
            ui.label(RichText::new("可通过托盘菜单打开设置、暂停互控或退出软件。").small().color(MUTED));
        });
        ui.add_space(13.0);
        card(ui, "连接诊断", "遇到问题时，导出诊断包，便于检查日志与连接设置。", |ui| {
            ui.label(self.handle.status_summary());
            ui.horizontal(|ui| {
                if ui.button("导出诊断包").clicked() {
                    self.diag_result = Some(match diag::export(self.args.log_dir.as_deref()) {
                        Ok(path) => format!("已导出到：{}", path.display()),
                        Err(error) => format!("导出失败：{error}"),
                    });
                }
                ui.label(RichText::new("最近 7 天日志 + 配置 + 版本 → 桌面").small().color(MUTED));
            });
            if let Some(result) = &self.diag_result { ui.label(RichText::new(result).small().color(BLUE)); }
        });
        ui.add_space(13.0);
        card(ui, "Fesilent Mouse", &format!("版本 {} · Rust 原生桌面应用", env!("CARGO_PKG_VERSION")), |ui| {
            ui.label("基于 Yujay-Z / MouseShare 开源项目衍生，保留原项目的许可证与作者署名。");
            ui.horizontal_wrapped(|ui| {
                ui.hyperlink_to("Fesilent Mouse 项目", "https://github.com/id399-dzx/fesilent-mouse");
                ui.hyperlink_to("原作者与上游项目", "https://github.com/Yujay-Z/MouseShare");
            });
        });
    }
}

/// Ask the operating system for the route's local address; UDP connect sends no packet.
fn route_local_address(peer: &str) -> Option<String> {
    let peer: std::net::SocketAddr = peer.parse().ok()?;
    let socket = std::net::UdpSocket::bind(if peer.is_ipv4() { "0.0.0.0:0" } else { "[::]:0" }).ok()?;
    socket.connect(peer).ok()?;
    Some(socket.local_addr().ok()?.ip().to_string())
}

/// Keep multiple IPv4 addresses visible: guessing a single adapter often selects a VPN.
pub(super) fn local_ipv4_addresses() -> Vec<String> {
    let mut addresses = Vec::new();
    #[cfg(target_os = "macos")]
    {
        if let Ok(output) = std::process::Command::new("/sbin/ifconfig").output() {
            for line in String::from_utf8_lossy(&output.stdout).lines() {
                let mut fields = line.split_whitespace();
                if fields.next() != Some("inet") { continue; }
                if let Some(address) = fields.next()
                    && let Ok(ip) = address.parse::<std::net::Ipv4Addr>()
                    && !ip.is_loopback() && !ip.is_unspecified() && !ip.is_link_local()
                {
                    addresses.push(ip.to_string());
                }
            }
        }
    }
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        if let Ok(output) = std::process::Command::new("ipconfig").creation_flags(0x0800_0000).output() {
            for line in String::from_utf8_lossy(&output.stdout).lines() {
                if !line.contains("IPv4") { continue; }
                for part in line.split(|character: char| !(character.is_ascii_digit() || character == '.')) {
                    if let Ok(ip) = part.parse::<std::net::Ipv4Addr>() {
                        if !ip.is_loopback() && !ip.is_unspecified() && !ip.is_link_local() {
                            addresses.push(ip.to_string());
                        }
                    }
                }
            }
        }
    }
    if addresses.is_empty() {
        for peer in ["192.168.1.1:53728", "192.168.3.1:53728", "10.0.0.1:53728"] {
            if let Some(address) = route_local_address(peer) { addresses.push(address); }
        }
    }
    addresses.sort();
    addresses.dedup();
    addresses
}
