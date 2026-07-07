use gpui::App;

use crate::settings_view::SettingsStore;

/// Apply the locale from the current SettingsStore and re-register native menus.
/// Call at startup (after SettingsStore is set) and after every language change.
pub fn apply(cx: &mut App) {
    let lang = cx.global::<SettingsStore>().0.language;
    rust_i18n::set_locale(lang.effective_code());
    crate::register_menus(cx);
}
