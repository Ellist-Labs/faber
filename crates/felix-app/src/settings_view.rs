use felix_settings::{AutoSave, Settings};
use gpui::{
    AnyElement, App, Context, Div, FocusHandle, Focusable, Global, IntoElement, MouseButton,
    Render, Window, div, prelude::*, px,
};

use crate::theme::{ActiveTheme, RuntimeTheme, apply_settings};
use crate::ui::{Divider, h_flex, v_flex};

/// App-wide settings global. The Settings tab is the sole writer; every
/// change persists to disk immediately.
pub struct SettingsStore(pub Settings);

impl Global for SettingsStore {}

// ── setting registry ───────────────────────────────────────────────────────────
// Adding a setting = one field on `Settings` + one entry below.

enum SettingControl {
    Select {
        options: &'static [(&'static str, &'static str)], // (value, label)
        get: fn(&Settings) -> &'static str,
        set: fn(&mut Settings, &str),
    },
    Stepper {
        min: f32,
        max: f32,
        step: f32,
        unit: &'static str,
        get: fn(&Settings) -> f32,
        set: fn(&mut Settings, f32),
    },
}

struct SettingEntry {
    title: &'static str,
    description: &'static str,
    enabled: fn(&Settings) -> bool,
    control: SettingControl,
}

struct SettingsSectionDef {
    title: &'static str,
    entries: Vec<SettingEntry>,
}

fn sections() -> Vec<SettingsSectionDef> {
    vec![
        SettingsSectionDef {
            title: "Editor",
            entries: vec![
                SettingEntry {
                    title: "Auto Save",
                    description: "Controls when dirty files are saved automatically.",
                    enabled: |_| true,
                    control: SettingControl::Select {
                        options: &[
                            ("off", "Off"),
                            ("afterDelay", "After Delay"),
                            ("onFocusChange", "On Focus Change"),
                            ("onWindowChange", "On Window Change"),
                        ],
                        get: |s| match s.auto_save {
                            AutoSave::Off => "off",
                            AutoSave::AfterDelay => "afterDelay",
                            AutoSave::OnFocusChange => "onFocusChange",
                            AutoSave::OnWindowChange => "onWindowChange",
                        },
                        set: |s, v| {
                            s.auto_save = match v {
                                "afterDelay" => AutoSave::AfterDelay,
                                "onFocusChange" => AutoSave::OnFocusChange,
                                "onWindowChange" => AutoSave::OnWindowChange,
                                _ => AutoSave::Off,
                            }
                        },
                    },
                },
                SettingEntry {
                    title: "Auto Save Delay",
                    description: "Idle time before a dirty file is saved (After Delay only).",
                    enabled: |s| s.auto_save == AutoSave::AfterDelay,
                    control: SettingControl::Stepper {
                        min: 250.0,
                        max: 5000.0,
                        step: 250.0,
                        unit: "ms",
                        get: |s| s.auto_save_delay_ms as f32,
                        set: |s, v| s.auto_save_delay_ms = v as u64,
                    },
                },
            ],
        },
        SettingsSectionDef {
            title: "Appearance",
            entries: vec![SettingEntry {
                title: "Font Size",
                description: "Base font size in pixels; the entire application scales from it.",
                enabled: |_| true,
                control: SettingControl::Stepper {
                    min: 10.0,
                    max: 24.0,
                    step: 1.0,
                    unit: "px",
                    get: |s| s.font_size,
                    set: |s, v| s.font_size = v,
                },
            }],
        },
    ]
}

// ── view ───────────────────────────────────────────────────────────────────────

pub struct SettingsView {
    pub focus_handle: FocusHandle,
}

impl SettingsView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self { focus_handle: cx.focus_handle() }
    }

    fn apply_change(&mut self, f: impl FnOnce(&mut Settings), cx: &mut Context<Self>) {
        let mut s = cx.global::<SettingsStore>().0.clone();
        f(&mut s);
        if let Err(err) = felix_settings::save(&s) {
            eprintln!("felix: can't write settings: {err}");
        }
        cx.set_global(SettingsStore(s));
        apply_settings(cx);
        cx.notify();
    }

    fn render_control(
        &self,
        entry_ix: usize,
        entry: &SettingEntry,
        settings: &Settings,
        t: &RuntimeTheme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let enabled = (entry.enabled)(settings);
        match entry.control {
            SettingControl::Select { options, get, set } => {
                let current = get(settings);
                h_flex()
                    .rounded(px(t.radius_md))
                    .border_1()
                    .border_color(t.border)
                    .children(options.iter().map(|&(value, label)| {
                        let is_current = value == current;
                        div()
                            .id((value, entry_ix))
                            .px_3()
                            .py_1()
                            .text_size(px(t.font_size_caption))
                            .when(is_current, |el| el.bg(t.accent).text_color(t.text_on_accent))
                            .when(!is_current && enabled, |el| {
                                el.text_color(t.text_muted)
                                    .cursor_pointer()
                                    .hover(|el| el.bg(t.line_highlight).text_color(t.text))
                            })
                            .when(!enabled, |el| el.text_color(t.text_disabled))
                            .when(enabled && !is_current, |el| {
                                el.on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |view, _, _, cx| {
                                        view.apply_change(|s| set(s, value), cx)
                                    }),
                                )
                            })
                            .child(label)
                    }))
                    .into_any_element()
            }
            SettingControl::Stepper { min, max, step, unit, get, set } => {
                let value = get(settings);
                let color = if enabled { t.text } else { t.text_disabled };
                let stepper_button = |id: &'static str, label: &'static str, delta: f32| {
                    div()
                        .id((id, entry_ix))
                        .px_2()
                        .py_1()
                        .text_color(color)
                        .when(enabled, |el| {
                            el.cursor_pointer().hover(|el| el.bg(t.line_highlight)).on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |view, _, _, cx| {
                                    view.apply_change(
                                        |s| set(s, (get(s) + delta).clamp(min, max)),
                                        cx,
                                    )
                                }),
                            )
                        })
                        .child(label)
                };
                h_flex()
                    .rounded(px(t.radius_md))
                    .border_1()
                    .border_color(t.border)
                    .text_size(px(t.font_size_caption))
                    .child(stepper_button("dec", "−", -step))
                    .child(
                        div()
                            .px_2()
                            .min_w(px(64.0))
                            .text_center()
                            .text_color(color)
                            .child(format!("{} {unit}", value as i64)),
                    )
                    .child(stepper_button("inc", "+", step))
                    .into_any_element()
            }
        }
    }

    fn render_entry(
        &self,
        entry_ix: usize,
        entry: &SettingEntry,
        settings: &Settings,
        t: &RuntimeTheme,
        cx: &mut Context<Self>,
    ) -> Div {
        h_flex()
            .justify_between()
            .items_start()
            .gap_4()
            .py_3()
            .child(
                v_flex()
                    .gap_1()
                    .max_w(px(420.0))
                    .child(
                        div()
                            .text_size(px(t.font_size_body))
                            .text_color(t.text)
                            .child(entry.title),
                    )
                    .child(
                        div()
                            .text_size(px(t.font_size_caption))
                            .text_color(t.text_muted)
                            .child(entry.description),
                    ),
            )
            .child(self.render_control(entry_ix, entry, settings, t, cx))
    }
}

impl Focusable for SettingsView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for SettingsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.theme().clone();
        let settings = cx.global::<SettingsStore>().0.clone();

        let mut entry_ix = 0;
        let sections: Vec<Div> = sections()
            .into_iter()
            .map(|section| {
                v_flex()
                    .gap_1()
                    .child(
                        div()
                            .text_size(px(t.font_size_heading))
                            .text_color(t.text)
                            .child(section.title),
                    )
                    .child(Divider::horizontal())
                    .children(section.entries.iter().map(|entry| {
                        entry_ix += 1;
                        self.render_entry(entry_ix, entry, &settings, &t, cx)
                    }))
            })
            .collect();

        div()
            .id("settings-scroll")
            .size_full()
            .overflow_y_scroll()
            .bg(t.bg)
            .track_focus(&self.focus_handle)
            .font_family(t.ui_family.clone())
            .child(
                v_flex()
                    .max_w(px(720.0))
                    .mx_auto()
                    .px_8()
                    .py_6()
                    .gap_6()
                    .child(
                        div()
                            .text_size(px(t.font_size_heading * 1.3))
                            .text_color(t.text)
                            .child("Settings"),
                    )
                    .children(sections),
            )
    }
}
