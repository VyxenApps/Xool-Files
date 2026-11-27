use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use windows::core::PCWSTR;
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ, RRF_RT_REG_SZ,
    RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegGetValueW, RegOpenKeyExW, RegSetValueExW,
};
use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND};

const FOLDER_CLASSES: [&str; 2] = [r"Software\Classes\Directory\shell", r"Software\Classes\Drive\shell"];
const VERB: &str = "XoolFiles.OpenFolder";

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain([0]).collect()
}

fn read_default(key: &str) -> Result<Option<String>, String> {
    read_value(key, None)
}

fn read_value(key: &str, name: Option<&str>) -> Result<Option<String>, String> {
    let key = wide(key);
    let name = name.map(wide);
    let name = name.as_ref().map_or(PCWSTR::null(), |name| PCWSTR(name.as_ptr()));
    let mut bytes = [0u16; 512];
    let mut length = (bytes.len() * 2) as u32;
    let status = unsafe {
        RegGetValueW(HKEY_CURRENT_USER, PCWSTR(key.as_ptr()), name, RRF_RT_REG_SZ,
            None, Some(bytes.as_mut_ptr().cast()), Some(&mut length))
    };
    if status == ERROR_FILE_NOT_FOUND || status == ERROR_PATH_NOT_FOUND { return Ok(None); }
    status.ok().map_err(|e| e.to_string())?;
    let end = bytes.iter().position(|&unit| unit == 0).unwrap_or(bytes.len());
    Ok(Some(String::from_utf16_lossy(&bytes[..end])))
}

fn write_default(key: &str, value: Option<&str>) -> Result<(), String> {
    write_value(key, None, value)
}

fn write_value(key: &str, name: Option<&str>, value: Option<&str>) -> Result<(), String> {
    let key = wide(key);
    let name = name.map(wide);
    let name = name.as_ref().map_or(PCWSTR::null(), |name| PCWSTR(name.as_ptr()));
    let mut handle = HKEY::default();
    let status = unsafe {
        match value {
            Some(_) => RegCreateKeyExW(HKEY_CURRENT_USER, PCWSTR(key.as_ptr()), None, PCWSTR::null(),
                REG_OPTION_NON_VOLATILE, KEY_SET_VALUE, None, &mut handle, None),
            None => RegOpenKeyExW(HKEY_CURRENT_USER, PCWSTR(key.as_ptr()), None, KEY_SET_VALUE, &mut handle),
        }
    };
    if value.is_none() && (status == ERROR_FILE_NOT_FOUND || status == ERROR_PATH_NOT_FOUND) { return Ok(()); }
    status.ok().map_err(|e| e.to_string())?;
    let status = unsafe {
        let status = if let Some(value) = value {
            let value: Vec<_> = wide(value).into_iter().flat_map(u16::to_le_bytes).collect();
            RegSetValueExW(handle, name, None, REG_SZ, Some(&value))
        } else {
            RegDeleteValueW(handle, name)
        };
        let _ = RegCloseKey(handle);
        status
    };
    if value.is_none() && status == ERROR_FILE_NOT_FOUND { return Ok(()); }
    status.ok().map_err(|e| e.to_string())
}

const STARTUP_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const STARTUP_NAME: &str = "Xool Files";

pub(crate) fn starts_with_windows() -> Result<bool, String> {
    Ok(read_value(STARTUP_KEY, Some(STARTUP_NAME))?.is_some())
}

pub(crate) fn set_windows_startup(enabled: bool) -> Result<(), String> {
    let command = if enabled {
        let executable = std::env::current_exe().map_err(|error| error.to_string())?;
        Some(format!("\"{}\"", executable.display()))
    } else {
        None
    };
    write_value(STARTUP_KEY, Some(STARTUP_NAME), command.as_deref())
}

fn owned_folder_verb(value: &str) -> bool {
    value == VERB
}

pub fn folder_default() -> bool {
    FOLDER_CLASSES.iter().all(|key| read_default(key).ok().flatten().as_deref().is_some_and(owned_folder_verb))
}

pub fn set_folder_default(enabled: bool) -> Result<(), String> {
    let previous = FOLDER_CLASSES.map(read_default);
    let [left, right] = previous;
    let previous = [left?, right?];
    if enabled {
        if previous.iter().flatten().any(|value| !owned_folder_verb(value)) {
            return Err(crate::i18n::tr("Ya existe un explorador configurado para este usuario.", "A file manager is already configured for this user.").into());
        }
        let executable = std::env::current_exe().map_err(|e| e.to_string())?;
        let command = format!("\"{}\" --open-folder \"%1\\.\"", executable.display());
        for key in FOLDER_CLASSES {
            write_default(&format!(r"{key}\{VERB}\command"), Some(&command))?;
            write_default(&format!(r"{key}\{VERB}"), Some(crate::i18n::tr("Abrir en Xool Files", "Open in Xool Files")))?;
        }
    }
    for (index, key) in FOLDER_CLASSES.into_iter().enumerate() {
        if !enabled && !previous[index].as_deref().is_some_and(owned_folder_verb) { continue; }
        if let Err(error) = write_default(key, enabled.then_some(VERB)) {
            for (prior, old) in FOLDER_CLASSES[..index].iter().zip(&previous) {
                let _ = write_default(prior, old.as_deref());
            }
            return Err(error);
        }
    }
    unsafe {
        use windows::Win32::UI::Shell::{SHCNE_ASSOCCHANGED, SHCNF_IDLIST, SHChangeNotify};
        SHChangeNotify(SHCNE_ASSOCCHANGED, SHCNF_IDLIST, None, None);
    }
    Ok(())
}

pub fn quick_matches(root: &Path, input: &str, ignore_accents: bool, generation: &AtomicU64, token: u64) -> Vec<PathBuf> {
    let input = input.trim().trim_matches('"');
    let path = PathBuf::from(input);
    if path.is_absolute() {
        if path.exists() { return vec![path]; }
    }
    let mut query = crate::search_index::SearchTerms::new(input, ignore_accents);
    let mut pending = vec![(root.to_path_buf(), 0)];
    let mut paths = Vec::new();
    let mut visited = 0;
    while let Some((folder, depth)) = pending.pop() {
        let Ok(entries) = fs::read_dir(folder) else { continue; };
        for entry in entries.flatten() {
            if generation.load(Ordering::Relaxed) != token || visited >= 30_000 || paths.len() >= 35 {
                return paths;
            }
            visited += 1;
            let Ok(kind) = entry.file_type() else { continue; };
            let file = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let Ok(metadata) = entry.metadata() else { continue; };
            if crate::fs_model::is_hidden(&metadata, &name) { continue; }
            let matches = query.matches(&file);
            if matches { paths.push(file.clone()); }
            use std::os::windows::fs::MetadataExt;
            if kind.is_dir() && depth < 9 && metadata.file_attributes() & 0x400 == 0 {
                pending.push((file, depth + 1));
            }
        }
    }
    paths
}
