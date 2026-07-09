use faber_settings::{AutoSave, Language, PreviewPosition, Settings};
use gpui::{
    AnyElement, App, Context, Div, FocusHandle, Focusable, Global, IntoElement, Render, Window,
    div, prelude::*, px,
};
use rust_i18n::t;

use crate::theme::{ActiveTheme, RuntimeTheme, apply_settings};
use crate::ui::{Button, Divider, Icon, IconName, Label, h_flex, v_flex};

/// App-wide settings global. The Settings tab is the sole writer; every
/// change persists to disk immediately.
pub struct SettingsStore(pub Settings);

impl Global for SettingsStore {}

// ── setting registry ───────────────────────────────────────────────────────────
// Adding a setting = one field on `Settings` + one entry below.
// Labels are built from t!() at sections() call time so they reflect the active locale.

enum SettingControl {
    Select {
        options: Vec<(&'static str, String)>, // (value_key, display_label)
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
    Toggle {
        get: fn(&Settings) -> bool,
        set: fn(&mut Settings, bool),
    },
}

struct SettingEntry {
    title: String,
    description: String,
    enabled: fn(&Settings) -> bool,
    control: SettingControl,
}

struct SettingsSectionDef {
    title: String,
    entries: Vec<SettingEntry>,
}

fn sections() -> Vec<SettingsSectionDef> {
    // Build language options: "system" first, then one per supported locale.
    let mut lang_options: Vec<(&'static str, String)> =
        vec![("system", t!("settings.language.system").to_string())];
    for &lang in Language::SUPPORTED {
        lang_options.push((lang.key(), lang.autonym().to_string()));
    }

    vec![
        SettingsSectionDef {
            title: t!("settings.section.general").to_string(),
            entries: vec![
                SettingEntry {
                    title: t!("settings.language.title").to_string(),
                    description: t!("settings.language.desc").to_string(),
                    enabled: |_| true,
                    control: SettingControl::Select {
                        options: lang_options,
                        get: |s| s.language.key(),
                        set: |s, v| {
                            s.language = match v {
                                "en" => Language::En,
                                _ => Language::System,
                            }
                        },
                    },
                },
                SettingEntry {
                    title: t!("settings.reopen_last_session.title").to_string(),
                    description: t!("settings.reopen_last_session.desc").to_string(),
                    enabled: |_| true,
                    control: SettingControl::Toggle {
                        get: |s| s.reopen_last_session,
                        set: |s, v| s.reopen_last_session = v,
                    },
                },
                SettingEntry {
                    title: t!("settings.restore_split_layout.title").to_string(),
                    description: t!("settings.restore_split_layout.desc").to_string(),
                    enabled: |s| s.reopen_last_session,
                    control: SettingControl::Toggle {
                        get: |s| s.restore_split_layout,
                        set: |s, v| s.restore_split_layout = v,
                    },
                },
            ],
        },
        SettingsSectionDef {
            title: t!("settings.section.editor").to_string(),
            entries: vec![
                SettingEntry {
                    title: t!("settings.line_numbers.title").to_string(),
                    description: t!("settings.line_numbers.desc").to_string(),
                    enabled: |_| true,
                    control: SettingControl::Toggle {
                        get: |s| s.line_numbers,
                        set: |s, v| s.line_numbers = v,
                    },
                },
                SettingEntry {
                    title: t!("settings.auto_save.title").to_string(),
                    description: t!("settings.auto_save.desc").to_string(),
                    enabled: |_| true,
                    control: SettingControl::Select {
                        options: vec![
                            ("off", t!("settings.auto_save_options.off").to_string()),
                            (
                                "afterDelay",
                                t!("settings.auto_save_options.after_delay").to_string(),
                            ),
                            (
                                "onFocusChange",
                                t!("settings.auto_save_options.on_focus_change").to_string(),
                            ),
                            (
                                "onWindowChange",
                                t!("settings.auto_save_options.on_window_change").to_string(),
                            ),
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
                    title: t!("settings.finder_preview.title").to_string(),
                    description: t!("settings.finder_preview.desc").to_string(),
                    enabled: |_| true,
                    control: SettingControl::Select {
                        options: vec![
                            (
                                "right",
                                t!("settings.finder_preview_options.right").to_string(),
                            ),
                            (
                                "left",
                                t!("settings.finder_preview_options.left").to_string(),
                            ),
                            (
                                "bottom",
                                t!("settings.finder_preview_options.bottom").to_string(),
                            ),
                        ],
                        get: |s| s.file_finder_preview_position.key(),
                        set: |s, v| {
                            s.file_finder_preview_position = match v {
                                "left" => PreviewPosition::Left,
                                "bottom" => PreviewPosition::Bottom,
                                _ => PreviewPosition::Right,
                            }
                        },
                    },
                },
                SettingEntry {
                    title: t!("settings.auto_save_delay.title").to_string(),
                    description: t!("settings.auto_save_delay.desc").to_string(),
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
            title: t!("settings.section.appearance").to_string(),
            entries: vec![
                SettingEntry {
                    title: t!("settings.font_size.title").to_string(),
                    description: t!("settings.font_size.desc").to_string(),
                    enabled: |_| true,
                    control: SettingControl::Stepper {
                        min: 10.0,
                        max: 24.0,
                        step: 1.0,
                        unit: "px",
                        get: |s| s.font_size,
                        set: |s, v| s.font_size = v,
                    },
                },
                SettingEntry {
                    title: t!("settings.scrollbar.title").to_string(),
                    description: t!("settings.scrollbar.desc").to_string(),
                    enabled: |_| true,
                    control: SettingControl::Toggle {
                        get: |s| s.show_scrollbar,
                        set: |s, v| s.show_scrollbar = v,
                    },
                },
                SettingEntry {
                    title: t!("settings.indent_guides.title").to_string(),
                    description: t!("settings.indent_guides.desc").to_string(),
                    enabled: |_| true,
                    control: SettingControl::Toggle {
                        get: |s| s.indent_guides,
                        set: |s, v| s.indent_guides = v,
                    },
                },
            ],
        },
    ]
}

// ── view ───────────────────────────────────────────────────────────────────────

pub struct SettingsView {
    pub focus_handle: FocusHandle,
}

impl SettingsView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
        }
    }

    fn apply_change(&mut self, f: impl FnOnce(&mut Settings), cx: &mut Context<Self>) {
        let mut s = cx.global::<SettingsStore>().0.clone();
        f(&mut s);
        if let Err(err) = faber_settings::save(&s) {
            eprintln!("faber: can't write settings: {err}");
        }
        cx.set_global(SettingsStore(s));
        crate::i18n::apply(cx);
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
            SettingControl::Select {
                ref options,
                get,
                set,
            } => {
                let current = get(settings);
                h_flex()
                    .rounded(px(t.radius_md))
                    .border_1()
                    .border_color(t.border)
                    .children(options.iter().map(|(value, label)| {
                        let value: &'static str = value;
                        let is_current = value == current;
                        let clickable = enabled && !is_current;
                        Button::new((value, entry_ix), label.clone())
                            .list()
                            .caption()
                            .selected(is_current)
                            .when(!clickable, Button::disabled)
                            .when(clickable, |btn| {
                                btn.on_click(cx.listener(move |view, _, _, cx| {
                                    view.apply_change(|s| set(s, value), cx)
                                }))
                            })
                    }))
                    .into_any_element()
            }
            SettingControl::Stepper {
                min,
                max,
                step,
                unit,
                get,
                set,
            } => {
                let value = get(settings);
                let color = if enabled { t.text } else { t.text_disabled };
                let stepper_button = |id: &'static str, icon: IconName, delta: f32| {
                    Button::new((id, entry_ix), "")
                        .list()
                        .caption()
                        .content(Icon::new(icon).size(px(14.)).color(color))
                        .when(!enabled, Button::disabled)
                        .when(enabled, |btn| {
                            btn.on_click(cx.listener(move |view, _, _, cx| {
                                view.apply_change(|s| set(s, (get(s) + delta).clamp(min, max)), cx)
                            }))
                        })
                };
                h_flex()
                    .rounded(px(t.radius_md))
                    .border_1()
                    .border_color(t.border)
                    .child(stepper_button("dec", IconName::Remove, -step))
                    .child(
                        div()
                            .px_2()
                            .min_w(px(64.0))
                            .text_center()
                            .text_size(px(t.font_size_caption))
                            .text_color(color)
                            .child(format!("{} {unit}", value as i64)),
                    )
                    .child(stepper_button("inc", IconName::Add, step))
                    .into_any_element()
            }
            SettingControl::Toggle { get, set } => {
                let on = get(settings);
                let toggle_opts = [
                    ("on", t!("common.on").to_string(), true),
                    ("off", t!("common.off").to_string(), false),
                ];
                h_flex()
                    .rounded(px(t.radius_md))
                    .border_1()
                    .border_color(t.border)
                    .children(toggle_opts.iter().map(|(id, label, value)| {
                        let id: &'static str = id;
                        let value = *value;
                        let is_current = on == value;
                        let clickable = enabled && !is_current;
                        Button::new((id, entry_ix), label.clone())
                            .list()
                            .caption()
                            .selected(is_current)
                            .when(!clickable, Button::disabled)
                            .when(clickable, |btn| {
                                btn.on_click(cx.listener(move |view, _, _, cx| {
                                    view.apply_change(|s| set(s, value), cx)
                                }))
                            })
                    }))
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
        div()
            .flex()
            .flex_wrap()
            .justify_between()
            .items_start()
            .gap_4()
            .py_3()
            .child(
                v_flex()
                    .gap_1()
                    .flex_1()
                    .min_w(px(200.0))
                    .child(Label::new(entry.title.clone()))
                    .child(Label::new(entry.description.clone()).caption().muted()),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .child(self.render_control(entry_ix, entry, settings, t, cx)),
            )
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
                    .child(Label::new(section.title.clone()).heading())
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
                    .w_full()
                    .max_w(px(720.0))
                    .mx_auto()
                    .px_8()
                    .py_6()
                    .gap_6()
                    .child(
                        div()
                            .text_size(px(t.font_size_heading * 1.3))
                            .text_color(t.text)
                            .child(t!("settings.title").to_string()),
                    )
                    .children(sections),
            )
    }
}
