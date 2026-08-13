use gettextrs::{LocaleCategory, bind_textdomain_codeset, bindtextdomain, setlocale, textdomain};

const GETTEXT_PACKAGE: &str = "diorama";
const DEFAULT_LOCALE_DIR: &str = "/usr/share/locale";

pub fn init() -> Result<(), std::io::Error> {
    setlocale(LocaleCategory::LcAll, "");
    let locale_dir = option_env!("DIORAMA_LOCALEDIR").unwrap_or(DEFAULT_LOCALE_DIR);
    bindtextdomain(GETTEXT_PACKAGE, locale_dir)?;
    bind_textdomain_codeset(GETTEXT_PACKAGE, "UTF-8")?;
    textdomain(GETTEXT_PACKAGE)?;
    Ok(())
}

pub fn gettext(message: &str) -> String {
    gettextrs::gettext(message)
}
