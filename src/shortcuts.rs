//! Catálogo de atajos de Xool Files.
//!
//! Cada acción tiene un identificador estable, una descripción en español y
//! una combinación predeterminada. La configuración guarda solo las
//! combinaciones que el usuario cambia; el resto se toma de aquí.

use crate::i18n::tr;
use gpui::Keystroke;

pub struct ShortcutDef {
    pub id: &'static str,
    label: (&'static str, &'static str),
    pub default: &'static str,
}

impl ShortcutDef {
    pub(crate) fn label(&self) -> &'static str {
        tr(self.label.0, self.label.1)
    }
}

pub const SHORTCUTS: &[ShortcutDef] = &[
    ShortcutDef { id: "new_tab", label: ("Nueva pestaña", "New tab"), default: "ctrl-t" },
    ShortcutDef { id: "close_tab", label: ("Cerrar pestaña", "Close tab"), default: "ctrl-w" },
    ShortcutDef { id: "next_tab", label: ("Pestaña siguiente", "Next tab"), default: "ctrl-tab" },
    ShortcutDef { id: "prev_tab", label: ("Pestaña anterior", "Previous tab"), default: "ctrl-shift-tab" },
    ShortcutDef { id: "back", label: ("Atrás", "Back"), default: "alt-left" },
    ShortcutDef { id: "forward", label: ("Adelante", "Forward"), default: "alt-right" },
    ShortcutDef { id: "up", label: ("Subir un nivel", "Go up one level"), default: "alt-up" },
    ShortcutDef { id: "refresh", label: ("Actualizar la carpeta", "Refresh folder"), default: "f5" },
    ShortcutDef { id: "focus_address", label: ("Editar la dirección", "Edit address"), default: "ctrl-l" },
    ShortcutDef { id: "focus_search", label: ("Buscar en la carpeta", "Search this folder"), default: "ctrl-f" },
    ShortcutDef { id: "recursive_search", label: ("Buscar en subcarpetas (SUB)", "Search subfolders (SUB)"), default: "ctrl-e" },
    ShortcutDef { id: "toggle_hidden", label: ("Mostrar archivos ocultos", "Show hidden files"), default: "ctrl-h" },
    ShortcutDef { id: "toggle_split", label: ("Dividir el panel", "Split pane"), default: "ctrl-shift-e" },
    ShortcutDef { id: "toggle_preview", label: ("Abrir o cerrar la vista previa", "Toggle preview"), default: "ctrl-shift-p" },
    ShortcutDef { id: "toggle_sidebar", label: ("Mostrar u ocultar la barra lateral", "Toggle sidebar"), default: "ctrl-b" },
    ShortcutDef { id: "new_folder", label: ("Nueva carpeta", "New folder"), default: "ctrl-shift-n" },
    ShortcutDef { id: "rename", label: ("Renombrar en el mismo lugar", "Rename in place"), default: "f2" },
    ShortcutDef { id: "delete", label: ("Enviar a la Papelera", "Move to Recycle Bin"), default: "delete" },
    ShortcutDef { id: "copy", label: ("Copiar", "Copy"), default: "ctrl-c" },
    ShortcutDef { id: "cut", label: ("Cortar", "Cut"), default: "ctrl-x" },
    ShortcutDef { id: "paste", label: ("Pegar", "Paste"), default: "ctrl-v" },
    ShortcutDef { id: "open", label: ("Abrir el elemento seleccionado", "Open selected item"), default: "enter" },
    ShortcutDef { id: "select_all", label: ("Seleccionar todo", "Select all"), default: "ctrl-a" },
    ShortcutDef { id: "settings", label: ("Abrir Ajustes", "Open Settings"), default: "ctrl-," },
    ShortcutDef { id: "toggle_view", label: ("Alternar cuadrícula y lista", "Toggle grid and list"), default: "ctrl-g" },
    ShortcutDef { id: "favorite", label: ("Guardar la carpeta en favoritos", "Add folder to Favorites"), default: "ctrl-d" },
    ShortcutDef { id: "zoom_in", label: ("Acercar", "Zoom in"), default: "ctrl-=" },
    ShortcutDef { id: "zoom_out", label: ("Alejar", "Zoom out"), default: "ctrl--" },
    ShortcutDef { id: "zoom_reset", label: ("Restablecer el zoom", "Reset zoom"), default: "ctrl-0" },
    ShortcutDef { id: "sort_name", label: ("Ordenar por nombre", "Sort by name"), default: "alt-1" },
    ShortcutDef { id: "sort_size", label: ("Ordenar por tamaño", "Sort by size"), default: "alt-2" },
    ShortcutDef { id: "sort_kind", label: ("Ordenar por tipo", "Sort by type"), default: "alt-3" },
    ShortcutDef { id: "sort_modified", label: ("Ordenar por fecha", "Sort by date"), default: "alt-4" },
    ShortcutDef { id: "escape", label: ("Cerrar un menú o cancelar", "Close menu or cancel"), default: "escape" },
];

/// Valor centinela que representa una acción sin atajo. Se guarda en la
/// configuración para distinguirlo de «usar el valor predeterminado».
pub const UNBOUND: &str = "__sin_atajo__";
pub(crate) const QUICK_SEARCH_RECORDING: &str = "quick-search-global";
pub(crate) const QUICK_SEARCH_PRESETS: [Option<&str>; 4] =
    [None, Some("ctrl-space"), Some("shift-space"), Some("ctrl-x")];

pub(crate) fn deserialize_quick_search_shortcut<'de, D: serde::Deserializer<'de>>(
    de: D,
) -> Result<Option<String>, D::Error> {
    use serde::Deserialize;

    #[derive(Deserialize)]
    #[serde(untagged)]
    enum SavedShortcut {
        Keys(String),
        Legacy(u8),
    }

    Ok(match Option::<SavedShortcut>::deserialize(de)? {
        Some(SavedShortcut::Keys(keys)) if !is_unbound(&keys) => Some(normalize(&keys)),
        Some(SavedShortcut::Legacy(1)) => Some("ctrl-space".into()),
        Some(SavedShortcut::Legacy(3)) => Some("shift-space".into()),
        _ => None,
    })
}

pub fn is_unbound(keys: &str) -> bool {
    keys.trim().is_empty() || keys.trim() == UNBOUND
}

pub fn find(id: &str) -> Option<&'static ShortcutDef> {
    SHORTCUTS.iter().find(|def| def.id == id)
}

pub fn default_for(id: &str) -> Option<&'static str> {
    find(id).map(|def| def.default)
}

/// Devuelve la combinación normalizada (`ctrl-shift-t`) para comparar sin
/// depender de mayúsculas ni del orden en que se escribieron los modificadores.
pub fn normalize(keys: &str) -> String {
    let Ok(keystroke) = Keystroke::parse(keys) else {
        return keys.trim().to_lowercase();
    };
    let mut parts = Vec::new();
    if keystroke.modifiers.control {
        parts.push("ctrl");
    }
    if keystroke.modifiers.alt {
        parts.push("alt");
    }
    if keystroke.modifiers.shift {
        parts.push("shift");
    }
    if keystroke.modifiers.platform {
        parts.push("win");
    }
    if keystroke.modifiers.function {
        parts.push("fn");
    }
    let key = keystroke.key.to_lowercase();
    parts.push(&key);
    parts.join("-")
}

/// Comprueba que la combinación se pueda usar como atajo.
pub fn validate(keys: &str) -> Result<(), String> {
    let keys = keys.trim();
    if keys.is_empty() {
        return Err(tr("Escribe una combinación de teclas.", "Enter a key combination.").to_string());
    }
    let keystroke = Keystroke::parse(keys)
        .map_err(|_| tr("Esa combinación no es válida. Usa Ctrl, Alt o una tecla de función.", "Invalid shortcut. Use Ctrl, Alt, or a function key.").to_string())?;
    if keystroke.key.trim().is_empty() {
        return Err(tr("La combinación necesita una tecla final.", "The shortcut needs a final key.").to_string());
    }
    if !keystroke.modifiers.control
        && !keystroke.modifiers.alt
        && !keystroke.modifiers.platform
        && !keystroke.modifiers.function
        && is_printable_key(&keystroke.key)
    {
        return Err(
            tr("Para no interferir al escribir, añade Ctrl o Alt, o usa una tecla como F2 o Supr.", "To avoid interfering with typing, add Ctrl or Alt, or use a key such as F2 or Delete.")
                .to_string(),
        );
    }
    Ok(())
}

fn is_printable_key(key: &str) -> bool {
    let lower = key.to_lowercase();
    if lower.starts_with('f') && lower[1..].parse::<u8>().is_ok() {
        return false;
    }
    !matches!(
        lower.as_str(),
        "escape"
            | "esc"
            | "enter"
            | "return"
            | "tab"
            | "delete"
            | "supr"
            | "backspace"
            | "insert"
            | "home"
            | "end"
            | "pageup"
            | "pagedown"
            | "up"
            | "down"
            | "left"
            | "right"
            | "space"
    )
}

/// Texto legible para mostrar en Ajustes: `Ctrl + Shift + T`.
pub fn display(keys: &str) -> String {
    let Ok(keystroke) = Keystroke::parse(keys) else {
        return keys.to_string();
    };
    let mut parts = Vec::new();
    if keystroke.modifiers.control {
        parts.push("Ctrl".to_string());
    }
    if keystroke.modifiers.alt {
        parts.push("Alt".to_string());
    }
    if keystroke.modifiers.shift {
        parts.push("Shift".to_string());
    }
    if keystroke.modifiers.platform {
        parts.push("Win".to_string());
    }
    if keystroke.modifiers.function {
        parts.push("Fn".to_string());
    }
    parts.push(display_key(&keystroke.key));
    parts.join(" + ")
}

fn display_key(key: &str) -> String {
    match key.to_lowercase().as_str() {
        "left" => "←".to_string(),
        "right" => "→".to_string(),
        "up" => "↑".to_string(),
        "down" => "↓".to_string(),
        "delete" | "supr" => tr("Supr", "Del").to_string(),
        "escape" | "esc" => "Esc".to_string(),
        "enter" | "return" => "Enter".to_string(),
        "tab" => "Tab".to_string(),
        "space" => tr("Espacio", "Space").to_string(),
        "backspace" => tr("Retroceso", "Backspace").to_string(),
        "pageup" => tr("RePág", "PgUp").to_string(),
        "pagedown" => tr("AvPág", "PgDn").to_string(),
        other => other.to_uppercase(),
    }
}

/// Convierte la tecla que llega del sistema en una combinación guardable.
/// Devuelve `None` para las teclas modificadoras solas.
pub fn keystroke_to_string(keystroke: &Keystroke) -> Option<String> {
    let key = keystroke.key.to_lowercase();
    if matches!(
        key.as_str(),
        "control" | "ctrl" | "alt" | "shift" | "win" | "super" | "cmd" | "fn" | "meta"
    ) {
        return None;
    }
    let mut parts = Vec::new();
    if keystroke.modifiers.control {
        parts.push("ctrl");
    }
    if keystroke.modifiers.alt {
        parts.push("alt");
    }
    if keystroke.modifiers.shift {
        parts.push("shift");
    }
    if keystroke.modifiers.platform {
        parts.push("win");
    }
    if keystroke.modifiers.function {
        parts.push("fn");
    }
    parts.push(&key);
    Some(parts.join("-"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_valid_and_unique() {
        let mut seen = std::collections::HashSet::new();
        for def in SHORTCUTS {
            validate(def.default).unwrap();
            assert!(seen.insert(normalize(def.default)), "duplicado: {}", def.default);
        }
    }

    #[test]
    fn display_is_readable() {
        assert_eq!(display("ctrl-shift-t"), "Ctrl + Shift + T");
        assert_eq!(display("alt-left"), "Alt + ←");
        assert_eq!(display("f2"), "F2");
    }

    #[test]
    fn plain_letters_are_rejected_but_function_keys_are_allowed() {
        assert!(validate("a").is_err());
        assert!(validate("f2").is_ok());
        assert!(validate("ctrl-a").is_ok());
        assert!(validate("alt-1").is_ok());
    }

    #[test]
    fn unbound_sentinel_is_recognized() {
        assert!(is_unbound(UNBOUND));
        assert!(is_unbound(""));
        assert!(is_unbound("   "));
        assert!(!is_unbound("ctrl-a"));
    }
}
