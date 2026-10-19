//! 已保存设备的本地管理；清单记录不代表设备在线。

use super::*;
use crate::devices::DevicePlacement;
use ms_proto::Edge;

fn side_label(side: Option<Edge>) -> &'static str {
    match side {
        Some(Edge::Left) => "左侧",
        Some(Edge::Right) => "右侧",
        Some(Edge::Top) => "上方",
        Some(Edge::Bottom) => "下方",
        None => "暂不放置",
    }
}

fn side_key(side: Edge) -> &'static str {
    match side {
        Edge::Left => "left",
        Edge::Right => "right",
        Edge::Top => "top",
        Edge::Bottom => "bottom",
    }
}

impl App {
    pub(super) fn draw_device_catalog(
        &mut self,
        ui: &mut egui::Ui,
        cfg: &mut Config,
        dirty: &mut bool,
    ) {
        let transferring = transfers::center().has_active();
        card(
            ui,
            "我的设备",
            "保存多台电脑，选择一台作为当前连接目标。清单会保留离线设备。",
            |ui| {
                if cfg.devices.records.is_empty() {
                    ui.label(
                        RichText::new("完成一次配对后，电脑名称、地址与屏幕方位会保存在这里。")
                            .small()
                            .color(MUTED),
                    );
                }
                let records = cfg.devices.records.clone();
                for record in records {
                    ui.push_id(&record.fingerprint, |ui| {
                        let selected = cfg
                            .devices
                            .active()
                            .is_some_and(|active| active.fingerprint == record.fingerprint);
                        ui.add_space(5.0);
                        ui.horizontal_wrapped(|ui| {
                            ui.label(RichText::new(&record.name).strong());
                            ui.label(
                                RichText::new(if selected { "已选择" } else { "已保存" })
                                    .small()
                                    .color(if selected { PURPLE } else { MUTED }),
                            );
                            ui.label(
                                RichText::new(side_label(record.placement.map(|place| place.side)))
                                    .small()
                                    .color(MUTED),
                            );
                        });
                        if let Some(address) = &record.address {
                            ui.label(RichText::new(address).small().monospace().color(MUTED));
                        } else {
                            ui.label(RichText::new("自动发现 · 按设备身份连接").small().color(MUTED));
                        }
                        if ui
                                .add_enabled_ui(
                                    !transferring && record.placement.is_some(),
                                    |ui| {
                                        secondary_button(
                                            ui,
                                            if selected { "重新连接" } else { "连接这台电脑" },
                                        )
                                    },
                                )
                                .inner
                                .on_disabled_hover_text(if transferring {
                                    "请先完成或取消文件传输。"
                                } else {
                                    "先在设备设置中选择屏幕方位。"
                                })
                                .clicked()
                        {
                            self.connect_saved_device(cfg, dirty, &record.fingerprint);
                        }
                            egui::CollapsingHeader::new("设备设置")
                                .id_salt("saved_device_settings")
                                .show(ui, |ui| {
                                    let name_id = ui.id().with("name_draft");
                                    let mut name = ui.ctx().data_mut(|data| {
                                        data.get_temp::<String>(name_id)
                                            .unwrap_or_else(|| record.name.clone())
                                    });
                                    ui.label(RichText::new("设备名称").small().color(MUTED));
                                    let changed = ui
                                        .add_sized(
                                            [ui.available_width().min(280.0), 34.0],
                                            egui::TextEdit::singleline(&mut name)
                                                .char_limit(80)
                                                .hint_text("例如：书房 Mac"),
                                        )
                                        .changed();
                                    if changed {
                                        ui.ctx().data_mut(|data| data.insert_temp(name_id, name.clone()));
                                    }
                                    if name.trim() != record.name && ui.small_button("保存名称").clicked() {
                                        if name.trim().is_empty() {
                                            self.form_error = Some("设备名称不能为空。".into());
                                        } else if let Some(saved) = cfg.devices.records.iter_mut().find(|saved| saved.fingerprint == record.fingerprint) {
                                            saved.name = name.trim().to_string();
                                            ui.ctx().data_mut(|data| data.remove::<String>(name_id));
                                            self.form_error = None;
                                            *dirty = true;
                                        }
                                    }
                                    ui.add_space(5.0);
                                    ui.label(RichText::new("相对本机的方位").small().color(MUTED));
                                    let mut side = record.placement.map(|place| place.side);
                                    let previous_side = side;
                                    egui::ComboBox::from_id_salt("saved_device_side")
                                        .selected_text(side_label(side))
                                        .show_ui(ui, |ui| {
                                            for option in [None, Some(Edge::Left), Some(Edge::Right), Some(Edge::Top), Some(Edge::Bottom)] {
                                                ui.selectable_value(&mut side, option, side_label(option));
                                            }
                                        });
                                    if side != previous_side {
                                        let placement = side.map(|side| DevicePlacement {
                                            side,
                                            offset_px: record.placement.map_or(0.0, |place| place.offset_px),
                                        });
                                        if transferring && selected {
                                            self.form_error = Some("请先完成或取消文件传输，再更改当前设备的方位。".into());
                                        } else {
                                            self.place_saved_device(cfg, dirty, &record.fingerprint, placement);
                                        }
                                    }
                                    if let Some(placement) = record.placement {
                                        ui.add_space(4.0);
                                        ui.label(RichText::new("沿边偏移").small().color(MUTED));
                                        let mut offset = placement.offset_px;
                                        if ui.add_enabled(!transferring || !selected,
                                            egui::DragValue::new(&mut offset)
                                                .speed(5.0)
                                                .range(-100_000.0..=100_000.0)
                                                .suffix(" px"),
                                        ).changed() {
                                            self.place_saved_device(cfg, dirty, &record.fingerprint, Some(DevicePlacement { offset_px: offset, ..placement }));
                                        }
                                        ui.label(RichText::new("0：按比例对齐；非零：按实际像素对齐，向下或向右为正。")
                                            .small().color(MUTED));
                                    }
                                    ui.add_space(6.0);
                                    egui::CollapsingHeader::new("设备身份")
                                        .id_salt("saved_device_identity")
                                        .show(ui, |ui| {
                                            ui.label(RichText::new("证书指纹 · SHA-256").small().color(MUTED));
                                            let displayed = record.fingerprint.chars().collect::<Vec<_>>()
                                                .chunks(16)
                                                .map(|chunk| chunk.iter().collect::<String>())
                                                .collect::<Vec<_>>()
                                                .join("\n");
                                            ui.label(RichText::new(displayed).monospace().small());
                                            if ui.small_button("复制完整指纹").clicked() {
                                                ui.ctx().copy_text(record.fingerprint.clone());
                                            }
                                        });
                                    let forget_id = ui.id().with("forget_saved_device");
                                    let confirming = ui.ctx().data_mut(|data| data.get_temp::<bool>(forget_id).unwrap_or(false));
                                    if confirming {
                                        ui.label(RichText::new("当前连接会暂停，并撤销这台设备在本机的配对。另一端需分别忘记。")
                                            .small().color(AMBER));
                                        ui.horizontal_wrapped(|ui| {
                                            if ui.add_enabled(!transferring, egui::Button::new("确认移除本机配对")).clicked() {
                                                self.forget_saved_device(cfg, dirty, &record.fingerprint);
                                                ui.ctx().data_mut(|data| data.remove::<bool>(forget_id));
                                            }
                                            if ui.small_button("取消").clicked() {
                                                ui.ctx().data_mut(|data| data.remove::<bool>(forget_id));
                                            }
                                        });
                                    } else if ui.add_enabled(!transferring, egui::Button::new("移除本机配对…")).clicked() {
                                        ui.ctx().data_mut(|data| data.insert_temp(forget_id, true));
                                    }
                                });
                        ui.add_space(7.0);
                        ui.separator();
                    });
                }
                ui.add_space(7.0);
                if ui
                    .add_enabled_ui(!transferring, |ui| secondary_button(ui, "添加新设备"))
                    .inner
                    .clicked()
                {
                    self.add_saved_device(cfg, dirty);
                }
                ui.label(
                    RichText::new(if transferring {
                        "文件传输期间暂不能切换、添加或移除设备。"
                    } else {
                        "添加时暂停当前连接；两端如已有其他配对，请分别点击添加新设备，再核对配对码。"
                    })
                    .small()
                    .color(if transferring { AMBER } else { MUTED }),
                );
            },
        );
    }

    fn connect_saved_device(&mut self, cfg: &mut Config, dirty: &mut bool, fingerprint: &str) {
        if transfers::center().has_active() {
            self.form_error = Some("请先完成或取消文件传输，再切换设备。".into());
            return;
        }
        let mut next = cfg.clone();
        if let Err(error) = next.devices.select(fingerprint) {
            self.form_error = Some(error);
            return;
        }
        let Some(record) = next.devices.active().cloned() else {
            return;
        };
        let Some(placement) = record.placement else {
            return;
        };
        next.connect = record.address;
        next.side = side_key(placement.side).into();
        next.display_offset = placement.offset_px;
        if !self.store_config(&next) {
            return;
        }
        *cfg = next;
        *dirty = false;
        self.peer_ip = cfg.connect.clone().unwrap_or_default();
        self.manual_connection = cfg.connect.is_some();
        self.handle.stop();
        self.handle.status.allow_add_device(false);
        self.handle.status.clear_error();
        self.handle.start();
        self.form_error = None;
        self.connection_notice = Some(format!("正在连接 {}。", record.name));
    }

    fn place_saved_device(
        &mut self,
        cfg: &mut Config,
        dirty: &mut bool,
        fingerprint: &str,
        placement: Option<DevicePlacement>,
    ) {
        let selected = cfg
            .devices
            .active()
            .is_some_and(|record| record.fingerprint == fingerprint);
        let mut next = cfg.clone();
        if let Err(error) = next.devices.set_placement(fingerprint, placement) {
            self.form_error = Some(error);
            return;
        }
        if selected {
            if let Some(placement) = placement {
                next.side = side_key(placement.side).into();
                next.display_offset = placement.offset_px;
            } else {
                next.connect = None;
                if !self.store_config(&next) {
                    return;
                }
                self.handle.stop();
                self.handle.status.allow_add_device(false);
                self.manual_connection = false;
                self.peer_ip.clear();
            }
        }
        *cfg = next;
        *dirty = !selected || placement.is_some();
        self.form_error = None;
    }

    fn add_saved_device(&mut self, cfg: &mut Config, dirty: &mut bool) {
        if transfers::center().has_active() {
            self.form_error = Some("请先完成或取消文件传输，再添加设备。".into());
            return;
        }
        let mut next = cfg.clone();
        next.devices.select_local();
        next.connect = None;
        self.handle.stop();
        if !self.store_config(&next) {
            return;
        }
        *cfg = next;
        *dirty = false;
        self.manual_connection = false;
        self.peer_ip.clear();
        self.handle.status.allow_add_device(true);
        self.handle.status.clear_error();
        self.handle.start();
        self.form_error = None;
        self.connection_notice = Some("正在查找新设备，请在另一台电脑启用互控。".into());
    }

    fn forget_saved_device(&mut self, cfg: &mut Config, _dirty: &mut bool, fingerprint: &str) {
        if transfers::center().has_active() || self.pending_forget_device.is_some() {
            self.form_error = Some("请先完成文件传输或当前设备操作。".into());
            return;
        }
        let Some(record) = cfg.devices.record(fingerprint) else {
            return;
        };
        let id = match record.id() {
            Ok(id) => id,
            Err(error) => {
                self.form_error = Some(error);
                return;
            }
        };
        if self.ui_preview {
            self.connection_notice = Some("预览模式：未撤销实际设备信任。".into());
            return;
        }
        // 撤销成功后才在主线程删除清单；写信任失败仍保留记录供重试。
        self.pending_forget_device = Some(fingerprint.to_owned());
        self.handle.status.allow_add_device(false);
        self.handle.forget_device(id.fingerprint());
        self.form_error = None;
        self.connection_notice = Some("正在移除本机的设备配对。".into());
    }
}
