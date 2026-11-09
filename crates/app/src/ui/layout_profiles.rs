//! 命名设备布局的管理界面，复用已有持久化与安全停止/启动事务。

use super::*;

impl App {
    pub(super) fn draw_layout_profiles(
        &mut self,
        ui: &mut egui::Ui,
        cfg: &mut Config,
        dirty: &mut bool,
    ) {
        let busy = transfers::center().has_active()
            || self.pending_forget_device.is_some()
            || self.forget_all_pending
            || self.quitting;
        card(
            ui,
            "布局方案",
            "为办公室、书房等场景保存设备方位、偏移和连接目标。",
            |ui| {
                ui.label(
                    RichText::new(
                        "切换方案会安全结束当前连接；原先已启用时，再连接方案中的目标设备。",
                    )
                    .small()
                    .color(MUTED),
                );
                let draft_id = ui.id().with("new_layout_profile_name");
                let mut name = ui
                    .ctx()
                    .data_mut(|data| data.get_temp::<String>(draft_id).unwrap_or_default());
                ui.horizontal_wrapped(|ui| {
                    if ui
                        .add_sized(
                            [ui.available_width().min(230.0), 34.0],
                            egui::TextEdit::singleline(&mut name)
                                .char_limit(40)
                                .hint_text("方案名称，例如：办公室"),
                        )
                        .changed()
                    {
                        ui.ctx()
                            .data_mut(|data| data.insert_temp(draft_id, name.clone()));
                    }
                    if ui
                        .add_enabled_ui(!busy && !cfg.devices.records.is_empty(), |ui| {
                            secondary_button(ui, "保存当前布局")
                        })
                        .inner
                        .clicked()
                        && self.save_layout_profile(cfg, dirty, &name, false)
                    {
                        ui.ctx().data_mut(|data| data.remove::<String>(draft_id));
                    }
                });
                if cfg.layout_profiles.is_empty() {
                    ui.add_space(5.0);
                    ui.label(
                        RichText::new("尚无布局方案。先在“我的设备”中配对和排列电脑，再保存。")
                            .small()
                            .color(MUTED),
                    );
                }
                for (index, profile) in cfg.layout_profiles.clone().iter().enumerate() {
                    // 列表删除后索引会移动，名称也入ID以免把旧删除确认传给另一方案。
                    ui.push_id((&profile.name, index), |ui| {
                        ui.add_space(6.0);
                        ui.separator();
                        ui.horizontal_wrapped(|ui| {
                            ui.label(RichText::new(&profile.name).strong());
                            ui.label(
                                RichText::new(format!("{} 台设备", profile.devices.len()))
                                    .small()
                                    .color(MUTED),
                            );
                        });
                        ui.horizontal_wrapped(|ui| {
                            if ui
                                .add_enabled_ui(!busy, |ui| secondary_button(ui, "应用布局"))
                                .inner
                                .clicked()
                            {
                                self.apply_layout_profile(cfg, dirty, profile);
                            }
                            if ui
                                .add_enabled(!busy, egui::Button::new("更新为当前布局"))
                                .clicked()
                            {
                                self.save_layout_profile(cfg, dirty, &profile.name, true);
                            }
                            let delete_id = ui.id().with("confirm_delete_profile");
                            let confirming = ui
                                .ctx()
                                .data_mut(|data| data.get_temp::<bool>(delete_id).unwrap_or(false));
                            if confirming {
                                if ui
                                    .add_enabled(!busy, egui::Button::new("确认删除方案"))
                                    .clicked()
                                {
                                    let mut next = cfg.clone();
                                    // 本帧其他按钮可能已修改列表，按快照匹配，不能误删后移的方案。
                                    if let Some(position) = next
                                        .layout_profiles
                                        .iter()
                                        .position(|saved| saved == profile)
                                    {
                                        next.layout_profiles.remove(position);
                                        if self.store_config(&next) {
                                            *cfg = next;
                                            *dirty = false;
                                            self.form_error = None;
                                            self.connection_notice =
                                                Some("布局方案已删除，设备和配对仍保留。".into());
                                            ui.ctx()
                                                .data_mut(|data| data.remove::<bool>(delete_id));
                                        }
                                    }
                                }
                                if ui.small_button("取消").clicked() {
                                    ui.ctx().data_mut(|data| data.remove::<bool>(delete_id));
                                }
                            } else if ui.add_enabled(!busy, egui::Button::new("删除…")).clicked()
                            {
                                ui.ctx().data_mut(|data| data.insert_temp(delete_id, true));
                            }
                        });
                    });
                }
                ui.add_space(5.0);
                ui.label(RichText::new(if busy {
                "请先完成文件传输或设备操作，再管理布局方案。"
            } else {
                "方案不会恢复已移除的设备，也不会覆盖设备名称、最新地址或配对权限。当前每次连接一台目标设备。"
            }).small().color(if busy { AMBER } else { MUTED }));
            },
        );
    }

    fn save_layout_profile(
        &mut self,
        cfg: &mut Config,
        dirty: &mut bool,
        name: &str,
        replace: bool,
    ) -> bool {
        let mut next = cfg.clone();
        if let Err(error) = crate::layout_profiles::save(&mut next, name, replace) {
            self.form_error = Some(error);
            return false;
        }
        if !self.store_config(&next) {
            return false;
        }
        *cfg = next;
        *dirty = false;
        self.form_error = None;
        self.connection_notice = Some(
            if replace {
                "布局方案已更新。"
            } else {
                "布局方案已保存。"
            }
            .into(),
        );
        true
    }

    fn apply_layout_profile(
        &mut self,
        cfg: &mut Config,
        dirty: &mut bool,
        profile: &crate::layout_profiles::LayoutProfile,
    ) {
        if transfers::center().has_active()
            || self.pending_forget_device.is_some()
            || self.forget_all_pending
            || self.quitting
        {
            self.form_error = Some("请先完成文件传输或设备操作，再应用布局方案。".into());
            return;
        }
        let (next, summary) = match profile.apply(cfg) {
            Ok(next) => next,
            Err(error) => {
                self.form_error = Some(error);
                return;
            }
        };
        let was_running = self.handle.is_running();
        // 请求停止旧会话后才发布新布局；start会在线程内join，保证旧输入完整清理后再重连。
        self.handle.stop();
        self.handle.status.allow_add_device(false);
        if !self.store_config(&next) {
            return;
        }
        *cfg = next;
        *dirty = false;
        self.peer_ip = cfg.connect.clone().unwrap_or_default();
        self.manual_connection = cfg.connect.is_some();
        self.handle.status.clear_error();
        if was_running && cfg.devices.active().is_some() {
            self.handle.start();
        }
        self.form_error = None;
        let missing = if summary.missing > 0 {
            format!("，跳过 {} 台已移除设备", summary.missing)
        } else {
            String::new()
        };
        self.connection_notice = Some(format!(
            "已应用“{}”：恢复 {} 台设备布局{}。",
            profile.name, summary.restored, missing
        ));
    }
}
