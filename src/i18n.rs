//! Язык интерфейса: русский или английский.
//!
//! Тексты пишутся парами прямо в месте использования: `t!("русский", "english")`
//! для готовой строки и `tf!("русский {}", "english {}", значения…)` для строки
//! с подстановками. Язык хранится отдельно в каждом потоке — так тесты,
//! идущие параллельно, не мешают друг другу.

use std::cell::Cell;

thread_local! {
    static ENGLISH: Cell<bool> = Cell::new(default_is_english());
}

/// Язык при запуске: переменная `FPV_LANG` (`ru` или `en`), иначе язык системы;
/// русский — только если система русская.
fn default_is_english() -> bool {
    let language = std::env::var("FPV_LANG").ok().or_else(sys_locale::get_locale).unwrap_or_default();
    !language.to_lowercase().starts_with("ru")
}

pub fn english() -> bool {
    ENGLISH.with(Cell::get)
}

pub fn set_english(english: bool) {
    ENGLISH.with(|e| e.set(english));
}

/// Строка на текущем языке: `t!("русский", "english")`.
macro_rules! t {
    ($ru:literal, $en:literal) => {
        if $crate::i18n::english() { $en } else { $ru }
    };
}

/// Строка с подстановками на текущем языке: `tf!("русский {}", "english {}", значения…)`.
/// Порядок и число подстановок в обоих вариантах должны совпадать.
macro_rules! tf {
    ($ru:literal, $en:literal $(, $arg:expr)* $(,)?) => {
        if $crate::i18n::english() { format!($en $(, $arg)*) } else { format!($ru $(, $arg)*) }
    };
}
